use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use color_eyre::eyre::{OptionExt, Report, Result, WrapErr};
use derive_more::From;
use futures_util::FutureExt;
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use jpegxl_rs::image::ToDynamic;
use once_cell::sync::Lazy;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use crate::com::{AnimatedImage, Image, Res, WorkParams};
use crate::config::CONFIG;
use crate::manager::files::{
    is_gif, is_image_crate_supported, is_jxl, is_pixbuf_extension, is_png, is_video_extension,
    is_webp,
};
use crate::pools::handle_panic;
use crate::{Fut, closing};

static LOADING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.loading_threads.get())));

static LOADING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("loading-{u}"))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.loading_threads.get())
        .build()
        .expect("Error creating loading threadpool")
});

static LIMITS: Lazy<Limits> = Lazy::new(|| {
    let mut limits = Limits::default();
    limits.max_alloc = Some(10 * 1024 * 1024 * 1024); // 10GB
    limits
});

#[derive(Debug, Clone)]
pub struct UnscaledImage(pub Image);

impl From<DynamicImage> for UnscaledImage {
    fn from(img: DynamicImage) -> Self {
        Self(img.into())
    }
}


#[derive(Debug, From)]
pub enum ImageOrRes {
    Image(UnscaledImage),
    Res(Res),
}

impl ImageOrRes {
    pub const fn res(&self) -> Res {
        match self {
            Self::Image(uimg) => uimg.0.res,
            Self::Res(r) => *r,
        }
    }

    pub fn should_upscale(&self) -> bool {
        let target = &CONFIG.target_resolution;
        if target.w == 0 && target.h == 0 {
            return false;
        }

        let r = self.res();
        if let Some(minres) = CONFIG.minimum_resolution {
            if r.w < minres.w || r.h < minres.h {
                return true;
            }
        }

        (r.w < target.w || target.w == 0) && (r.h < target.h || target.h == 0)
    }
}


#[derive(Debug)]
pub enum ScanResult {
    // If we needed to convert it to a format the image crate understands.
    // This file will be written when this result is returned, and is owned by the newly created
    // ScannedPage.
    // If the Image is None it means the page was unloaded while scanning, so it needs to be read
    // from scratch.
    ConvertedImage(Arc<Path>, ImageOrRes),
    Image(ImageOrRes),
    // Animations skip the fast path, at least for now.
    Animation(Res),
    Video,
    Invalid(String),
}

// This is so we can consume and drop an image we don't need if the page is unloaded while
// scanning.
pub struct ScanFuture(pub Fut<ScanResult>);

impl ScanFuture {
    // In theory this can be called twice before scanning completes, but in practice it's not worth
    // optimizing for.
    pub fn unload_scanning(&mut self) {
        use ImageOrRes as IOR;
        use ScanResult::*;

        let fut = std::mem::replace(
            &mut self.0,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on an invalid ScanFuture") }.boxed(),
        );

        // The entire Manager runs inside a single LocalSet so this will not panic.
        let h = tokio::task::spawn_local(async move {
            match fut.await {
                ConvertedImage(pb, IOR::Image(uimg)) => ConvertedImage(pb, uimg.0.res.into()),
                Image(IOR::Image(uimg)) => Image(uimg.0.res.into()),
                x @ (ConvertedImage(..) | Image(_) | Animation(_) | Video | Invalid(_)) => x,
            }
        });

        self.0 = h
            .map(|r| match r {
                Ok(r) => r,
                Err(e) => {
                    error!("{e:?}");
                    Invalid(format!("Unexpected error while unloading scanning page: {e}"))
                }
            })
            .boxed()
    }
}

pub async fn scan(path: Arc<Path>, temp_dir: &Path, file_index: usize, load: bool) -> ScanFuture {
    let permit = LOADING_SEM
        .clone()
        .acquire_owned()
        .await
        .expect("Error acquiring scanning permit");

    // It's likely this will not be used in most cases. The tradeoff between making temp_dir an
    // Arc to avoid this is that the Rc is marginally faster on startup in very large directories,
    // where most files will not actually be scanned, compared to one (usually) short-lived
    // allocation per file that is actually scanned.
    //
    // The other uses of Arc in each Page save allocations when responding to user actions,
    // potentially more than once per page.
    let converted = temp_dir.join(format!("{file_index}c.png")).into();

    let (s, r) = oneshot::channel();
    LOADING.spawn_fifo(move || {
        let result = scan_file(path, converted, load);
        let result = match result {
            Ok(sr) => sr,
            Err(e) => ScanResult::Invalid(format!("Error scanning file: {e}")),
        };

        if let Err(_e) = s.send(result) {
            error!("Unexpected channel send failure");
        };
        drop(permit)
    });

    ScanFuture(Box::pin(async move {
        match r.await {
            Ok(sr) => sr,
            Err(e) => {
                error!("{e:?}");
                ScanResult::Invalid(format!("Error scanning file: {e}"))
            }
        }
    }))
}

#[instrument(level = "error", skip(converted, load), err(Debug))]
fn scan_file(path: Arc<Path>, converted: Arc<Path>, load: bool) -> Result<ScanResult> {
    use ScanResult::*;
    const READ_ERR: &str = "Error reading file, trying again with pixbuf";

    if is_gif(&path) {
        let f = BufReader::new(File::open(&path)?);
        let mut decoder = GifDecoder::new(f)?;
        decoder.set_limits(LIMITS.clone())?;
        let mut frames = decoder.into_frames();

        let first_frame = frames.next();
        let second_frame = if first_frame.is_some() { frames.next() } else { None };

        match (first_frame, second_frame) {
            (Some(Ok(first)), None) => {
                if load {
                    let img = DynamicImage::ImageRgba8(first.into_buffer());
                    return Ok(Image(UnscaledImage::from(img).into()));
                }
                return Ok(Image(Res::from(first.buffer().dimensions()).into()));
            }
            (Some(Ok(first)), Some(Ok(_))) => {
                return Ok(Animation(first.buffer().dimensions().into()));
            }
            (Some(Err(e)), _) | (Some(Ok(_)), Some(Err(e))) => {
                error!("{:?}", Report::new(e).wrap_err(READ_ERR));
            }
            (None, _) => return Ok(ScanResult::Invalid("Read gif with no frames".to_string())),
        }
    } else if is_png(&path) {
        let f = BufReader::new(File::open(&path)?);

        match PngDecoder::new(f).wrap_err(READ_ERR) {
            Ok(mut decoder) => {
                decoder.set_limits(LIMITS.clone())?;
                if decoder.is_apng()? {
                    return Ok(Animation(decoder.dimensions().into()));
                }

                if load {
                    let img = DynamicImage::from_decoder(decoder)?;
                    return Ok(Image(UnscaledImage::from(img).into()));
                }
                return Ok(Image(Res::from(decoder.dimensions()).into()));
            }
            Err(e) => error!("{e:?}"),
        }
    } else if is_webp(&path) {
        let f = BufReader::new(File::open(&path)?);

        match WebPDecoder::new(f).wrap_err(READ_ERR) {
            Ok(mut decoder) => {
                decoder.set_limits(LIMITS.clone())?;
                if decoder.has_animation() {
                    return Ok(Animation(decoder.dimensions().into()));
                }

                if load {
                    let img = DynamicImage::from_decoder(decoder)?;
                    return Ok(Image(UnscaledImage::from(img).into()));
                }
                return Ok(Image(Res::from(decoder.dimensions()).into()));
            }
            Err(e) => error!("{e:?}"),
        }
    } else if is_image_crate_supported(&path) {
        let mut reader = ImageReader::open(&path)?;
        reader.limits(LIMITS.clone());

        if load {
            match reader.decode().wrap_err(READ_ERR) {
                Ok(img) => {
                    return Ok(Image(UnscaledImage::from(img).into()));
                }
                Err(e) => error!("{e:?}"),
            }
        } else {
            match reader.into_dimensions().wrap_err(READ_ERR) {
                Ok(dims) => {
                    return Ok(Image(Res::from(dims).into()));
                }
                Err(e) => error!("{e:?}"),
            }
        }
    }

    if is_jxl(&path) {
        // No public APIs to just get resolution, so read and decode the entire image.
        let data = fs::read(&path)?;

        // TODO -- allow fall-through once the pixbuf loader is fixed?
        let decoder = jpegxl_rs::decoder_builder().build()?;
        let img = decoder
            .decode_to_image(&data)?
            .ok_or_eyre("Failed to convert jpeg-xl to DynamicImage")?;
        if load {
            return Ok(Image(UnscaledImage::from(img).into()));
        }
        return Ok(Image(Res::from(img).into()));
    }

    if is_pixbuf_extension(&path) {
        let pb = gtk::gdk_pixbuf::Pixbuf::from_file(&path)?;
        let pngvec = pb.save_to_bufferv("png", &[("compression", "1")])?;
        let (w, h) = (pb.width(), pb.height());

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let mut f = File::create(&converted)?;
        f.write_all(&pngvec)?;
        drop(f);

        debug!("Converted to {converted:?}");

        if !load {
            return Ok(ConvertedImage(converted, Res::from((w, h)).into()));
        }

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let img = image::load_from_memory_with_format(&pngvec, ImageFormat::Png)?;
        return Ok(ConvertedImage(converted, UnscaledImage::from(img).into()));
    }


    if is_video_extension(&path) {
        return Ok(Video);
    }

    Ok(ScanResult::Invalid("not yet implemented".to_string()))
}


// This is so we can unload and drop a load while it's happening.
pub struct LoadFuture<T, R>
where
    T: 'static,
    R: Clone + 'static,
{
    pub fut: Fut<std::result::Result<T, String>>,
    // Not all formats will meaningfully support cancellation.
    cancel_flag: Arc<AtomicBool>,
    extra_info: R,
}

impl<T: Send, R: Clone> LoadFuture<T, R> {
    pub fn cancel(&mut self) -> Fut<()> {
        self.cancel_flag.store(true, Ordering::Relaxed);
        let fut = std::mem::replace(
            &mut self.fut,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on a cancelled LoadFuture") }.boxed(),
        );
        let h = tokio::task::spawn_local(async move { drop(fut.await) });
        h.map(|_| {}).boxed()
    }
}

impl<T, R: Clone + fmt::Debug> fmt::Debug for LoadFuture<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[LoadFuture {:?}]", self.extra_info)
    }
}

/// closure should return None if the task was cancelled
fn spawn_task<F, T>(
    closure: F,
    params: WorkParams,
    cancel_flag: Arc<AtomicBool>,
    permit: OwnedSemaphorePermit,
) -> LoadFuture<T, WorkParams>
where
    F: FnOnce() -> Result<Option<T>> + Send + 'static,
    T: fmt::Debug + Send,
{
    let (s, r) = oneshot::channel();

    LOADING.spawn_fifo(move || {
        let result = closure();
        let result = match result {
            Ok(Some(sr)) => Ok(sr),
            Ok(None) => {
                debug!("Cancelled loading file");
                Err("Cancelled".to_string())
            }
            Err(e) => Err(format!("Error loading file {e}")),
        };

        if let Err(_e) = s.send(result) {
            error!("Unexpected channel send failure");
        };
        drop(permit)
    });

    let fut = r
        .map(|r| match r {
            Ok(nested) => nested,
            Err(e) => {
                error!("{e:?}");
                Err(format!("Unexpected error loading file {e}"))
            }
        })
        .boxed_local();

    LoadFuture { fut, cancel_flag, extra_info: params }
}

pub mod static_image {
    use super::*;

    pub async fn load(
        path: Arc<Path>,
        params: WorkParams,
    ) -> LoadFuture<UnscaledImage, WorkParams> {
        let permit = LOADING_SEM
            .clone()
            .acquire_owned()
            .await
            .expect("Error acquiring loading permit");

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let closure = move || load_image(path, params, cancel);

        spawn_task(closure, params, cancel_flag, permit)
    }

    #[instrument(level = "error", skip(_params, cancel), err(Debug))]
    fn load_image(
        path: Arc<Path>,
        _params: WorkParams,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<UnscaledImage>> {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }

        let img = if is_jxl(&path) {
            let data = fs::read(&path)?;
            let decoder = jpegxl_rs::decoder_builder().build()?;

            decoder
                .decode_to_image(&data)?
                .ok_or_eyre("Failed to convert jpeg-xl to DynamicImage")?
        } else if is_image_crate_supported(&path) {
            let mut reader = ImageReader::open(&path)?;
            reader.limits(LIMITS.clone());
            reader.decode()?
        } else {
            unreachable!();
        };


        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }

        Ok(Some(UnscaledImage::from(img)))
    }
}

pub mod animation {

    use std::hash::{Hash, Hasher};

    use ahash::AHasher;
    use color_eyre::eyre::bail;

    use super::*;

    pub async fn load(
        path: Arc<Path>,
        params: WorkParams,
    ) -> LoadFuture<AnimatedImage, WorkParams> {
        let permit = LOADING_SEM
            .clone()
            .acquire_owned()
            .await
            .expect("Error acquiring loading permit");

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let closure = move || load_animation(path, cancel);

        spawn_task(closure, params, cancel_flag, permit)
    }

    // TODO -- benchmark whether it's actually worthwhile to parallelize the conversions.
    fn adecoder_to_frames<'a, D: AnimationDecoder<'a>>(
        dec: D,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<(Image, Duration, u64)>> {
        let raw_frames: std::result::Result<Vec<_>, _> =
            dec.into_frames().take_while(|_| !cancel.load(Ordering::Relaxed)).collect();
        // TODO -- could just index and sort these, needs to be benchmarked

        Ok(raw_frames?
            .into_par_iter()
            .filter_map(|frame| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }

                let dur = frame.delay().into();
                let img = frame.into_buffer();

                let mut h = AHasher::default();
                img.hash(&mut h);
                let hash = h.finish();

                Some((DynamicImage::ImageRgba8(img).into(), dur, hash))
            })
            .collect())
    }

    #[instrument(level = "error", skip(cancel), err(Debug))]
    fn load_animation(path: Arc<Path>, cancel: Arc<AtomicBool>) -> Result<Option<AnimatedImage>> {
        let frames = if is_gif(&path) {
            let f = BufReader::new(File::open(&path)?);
            let decoder = GifDecoder::new(f)?;

            adecoder_to_frames(decoder, &cancel)?
        } else if is_png(&path) {
            let f = BufReader::new(File::open(&path)?);
            let decoder = PngDecoder::new(f)?.apng()?;

            adecoder_to_frames(decoder, &cancel)?
        } else if is_webp(&path) {
            let f = BufReader::new(File::open(&path)?);
            let decoder = WebPDecoder::new(f)?;

            adecoder_to_frames(decoder, &cancel)?
        } else {
            bail!("Animation type not yet implemented");
        };


        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }

        if frames.is_empty() {
            bail!("Empty animation");
        }

        Ok(AnimatedImage::new(frames).into())
    }
}
