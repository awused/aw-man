use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use derive_more::From;
use futures_util::FutureExt;
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::io::{Limits, Reader};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat};
use jpegxl_rs::image::ToDynamic;
use once_cell::sync::Lazy;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

use crate::com::{AnimatedImage, Image, Res, WorkParams};
use crate::config::CONFIG;
use crate::manager::files::{
    is_gif, is_image_crate_supported, is_jxl, is_pixbuf_extension, is_png, is_video_extension,
    is_webp,
};
use crate::pools::handle_panic;
use crate::{closing, Fut, Result};

static LOADING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.loading_threads.get())));

static LOADING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("loading-{}", u))
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

// TODO -- unwrap
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
        if target.is_zero() {
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
    ConvertedImage(PathBuf, ImageOrRes),
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
                    let e = format!("Unexpected error while unloading scanning page: {:?}", e);
                    error!("{}", e);
                    Invalid(e)
                }
            })
            .boxed()
    }
}

pub async fn scan(path: PathBuf, conv: PathBuf, load: bool) -> ScanFuture {
    let permit = LOADING_SEM
        .clone()
        .acquire_owned()
        .await
        .expect("Error acquiring scanning permit");


    let (s, r) = oneshot::channel();
    LOADING.spawn_fifo(move || {
        let result = scan_file(path, conv, load);
        let result = match result {
            Ok(sr) => sr,
            Err(e) => {
                let e = format!("Error scanning file: {:?}", e);
                if !e.ends_with("\"Cancelled\"") {
                    error!("{}", e);
                } else {
                    debug!("Cancelled scanning file.");
                }
                ScanResult::Invalid(e)
            }
        };

        if let Err(e) = s.send(result) {
            error!("Unexpected error scanning file {:?}", e);
        };
        drop(permit)
    });

    ScanFuture(Box::pin(async move {
        match r.await {
            Ok(sr) => sr,
            Err(e) => {
                let e = format!("Error scanning file {:?}", e);
                error!("{}", e);
                ScanResult::Invalid(e)
            }
        }
    }))
}

fn scan_file(path: PathBuf, conv: PathBuf, load: bool) -> Result<ScanResult> {
    use ScanResult::*;

    if is_gif(&path) {
        let f = File::open(&path)?;
        let mut decoder = GifDecoder::new(f)?;
        decoder.set_limits(LIMITS.clone())?;
        let mut frames = decoder.into_frames();

        let first_frame = frames.next();
        let second_frame = frames.next();

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
            (Some(Err(e)), _) => {
                error!("Error {:?} while trying to read {:?}, trying again with pixbuf.", e, path)
            }
            _ => {}
        }
    } else if is_png(&path) {
        let f = File::open(&path)?;

        match PngDecoder::new(f) {
            Ok(mut decoder) => {
                decoder.set_limits(LIMITS.clone())?;
                if decoder.is_apng() {
                    return Ok(Animation(decoder.dimensions().into()));
                }

                if load {
                    let img = DynamicImage::from_decoder(decoder)?;
                    return Ok(Image(UnscaledImage::from(img).into()));
                }
                return Ok(Image(Res::from(decoder.dimensions()).into()));
            }
            Err(e) => {
                error!("Error {:?} while trying to read {:?}, trying again with pixbuf.", e, path)
            }
        }
    } else if is_image_crate_supported(&path) {
        let mut reader = Reader::open(&path)?;
        reader.limits(LIMITS.clone());

        if load {
            match reader.decode() {
                Ok(img) => {
                    return Ok(Image(UnscaledImage::from(img).into()));
                }
                Err(e) => {
                    error!(
                        "Error {:?} while trying to read {:?}, trying again with pixbuf.",
                        e, path
                    )
                }
            }
        } else {
            match reader.into_dimensions() {
                Ok(dims) => {
                    return Ok(Image(Res::from(dims).into()));
                }
                Err(e) => {
                    error!(
                        "Error {:?} while trying to read {:?}, trying again with pixbuf.",
                        e, path
                    )
                }
            }
        }
    }

    if is_jxl(&path) {
        // No public APIs to just get resolution, so read and decode the entire image.
        let data = fs::read(&path)?;

        // TODO -- allow fall-through once the pixbuf loader is fixed?
        let decoder = jpegxl_rs::decoder_builder().build()?;
        let img = decoder
            .decode(&data)?
            .into_dynamic_image()
            .ok_or("Failed to convert jpeg-xl to DynamicImage")?;
        if load {
            return Ok(Image(UnscaledImage::from(img).into()));
        }
        return Ok(Image(Res::from(img).into()));
    }

    if is_webp(&path) {
        // Read the entire file into memory but don't necessarily decode all of it.
        let data = fs::read(&path)?;

        let features = webp::BitstreamFeatures::new(&data).ok_or("Could not read webp.")?;
        if features.has_animation() {
            return Ok(Animation((features.width(), features.height()).into()));
        } else if load {
            let decoded = webp::Decoder::new(&data).decode().ok_or("Could not decode webp")?;
            return Ok(Image(UnscaledImage::from(decoded.to_image()).into()));
        }
        return Ok(Image(Res::from((features.width(), features.height())).into()));
    }

    if is_pixbuf_extension(&path) {
        let pb = gtk::gdk_pixbuf::Pixbuf::from_file(&path)?;
        let pngvec = pb.save_to_bufferv("png", &[("compression", "1")])?;
        let (w, h) = (pb.width(), pb.height());

        // TODO -- remove once https://github.com/strukturag/libheif/issues/509 is in a libheif
        // release.
        unsafe {
            let pb: gtk::glib::Object = gtk::glib::Cast::upcast(pb);
            if gtk::glib::ObjectExt::ref_count(&pb) == 2 {
                error!(
                    "Newly allocated Pixbuf for {path:?} has a refcount of 2. Manually \
                     decrementing to avoid leaks."
                );
                // This _will_ leak if we don't unref it manually.
                // SAFETY: We created the pixbuf, we hold one reference to it.
                // If another reference exists it means it has been leaked, so we must clean it up.
                gtk::glib::gobject_ffi::g_object_unref(gtk::glib::ObjectType::as_ptr(&pb));
            }
            drop(pb);
        }

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let mut f = File::create(&conv)?;
        f.write_all(&pngvec)?;
        drop(f);

        debug!("Converted {:?} to {:?}", path, conv);

        if !load {
            return Ok(ConvertedImage(conv, Res::from((w, h)).into()));
        }

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let img = image::load_from_memory_with_format(&pngvec, ImageFormat::Png)?;
        return Ok(ConvertedImage(conv, UnscaledImage::from(img).into()));
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

fn spawn_task<F, T>(
    closure: F,
    params: WorkParams,
    cancel_flag: Arc<AtomicBool>,
    permit: OwnedSemaphorePermit,
) -> LoadFuture<T, WorkParams>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: fmt::Debug + Send,
{
    let (s, r) = oneshot::channel();

    LOADING.spawn_fifo(move || {
        let result = closure();
        let result = match result {
            Ok(sr) => Ok(sr),
            Err(e) => {
                let e = format!("Error loading file {:?}", e);
                if !e.ends_with("\"Cancelled\"") {
                    error!("{}", e);
                } else {
                    debug!("Cancelled loading file.");
                }
                Err(e)
            }
        };

        if let Err(e) = s.send(result) {
            error!("Unexpected error loading file {:?}", e);
        };
        drop(permit)
    });

    let fut = r
        .map(|r| match r {
            Ok(nested) => nested,
            Err(e) => {
                error!("Unexpected error loading file {:?}", e);
                Err(e.to_string())
            }
        })
        .boxed_local();

    LoadFuture { fut, cancel_flag, extra_info: params }
}

pub mod static_image {
    use super::*;

    pub async fn load(
        path: Rc<PathBuf>,
        params: WorkParams,
    ) -> LoadFuture<UnscaledImage, WorkParams> {
        let permit = LOADING_SEM
            .clone()
            .acquire_owned()
            .await
            .expect("Error acquiring loading permit");

        let path = (*path).clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let closure = move || load_image(path, params, cancel);

        spawn_task(closure, params, cancel_flag, permit)
    }

    fn load_image(
        path: PathBuf,
        _params: WorkParams,
        cancel: Arc<AtomicBool>,
    ) -> Result<UnscaledImage> {
        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let img = if is_webp(&path) {
            let data = fs::read(&path)?;

            webp::Decoder::new(&data).decode().ok_or("Could not decode webp")?.to_image()
        } else if is_jxl(&path) {
            let data = fs::read(&path)?;
            let decoder = jpegxl_rs::decoder_builder().build()?;

            decoder
                .decode(&data)?
                .into_dynamic_image()
                .ok_or("Failed to convert jpeg-xl to DynamicImage")?
        } else if is_image_crate_supported(&path) {
            let mut reader = Reader::open(&path)?;
            reader.limits(LIMITS.clone());
            reader.decode()?
        } else {
            unreachable!();
        };


        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        Ok(UnscaledImage::from(img))
    }
}

// TODO -- consider supporting downscaling here.
pub mod animation {

    use std::hash::{Hash, Hasher};

    use ahash::AHasher;

    use super::*;

    pub async fn load(
        path: Rc<PathBuf>,
        params: WorkParams,
    ) -> LoadFuture<AnimatedImage, WorkParams> {
        let permit = LOADING_SEM
            .clone()
            .acquire_owned()
            .await
            .expect("Error acquiring loading permit");

        let path = (*path).clone();
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
        // TODO -- could just index and sort these

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

    fn load_animation(path: PathBuf, cancel: Arc<AtomicBool>) -> Result<AnimatedImage> {
        let frames = if is_gif(&path) {
            let f = File::open(&path)?;
            let decoder = GifDecoder::new(f)?;

            adecoder_to_frames(decoder, &cancel)?
        } else if is_png(&path) {
            let f = File::open(&path)?;
            let decoder = PngDecoder::new(f)?.apng();

            adecoder_to_frames(decoder, &cancel)?
        } else if is_webp(&path) {
            let data = fs::read(&path)?;
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".into());
            }

            let decoder = webp_animation::Decoder::new(&data).map_err(|e| format!("{:?}", e))?;

            let mut last_frame = 0;

            let webp_frames: std::result::Result<Vec<_>, _> = decoder
                .into_iter()
                .map_while(|frame| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }

                    let d = frame.timestamp() - last_frame;
                    last_frame = frame.timestamp();
                    let d = Duration::from_millis(d.saturating_abs() as u64);
                    Some(frame.into_image().map(|img| (img, d)))
                })
                .collect();


            let webp_frames = webp_frames.map_err(|e| format!("{:?}", e))?;

            webp_frames
                .into_par_iter()
                .filter_map(|(img, dur)| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }

                    let mut h = AHasher::default();
                    img.hash(&mut h);
                    let hash = h.finish();

                    Some((DynamicImage::ImageRgba8(img).into(), dur, hash))
                })
                .collect()
        } else {
            return Err("Animation type not yet implemented".into());
        };


        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }

        if frames.is_empty() {
            return Err("Empty animation".into());
        }

        Ok(AnimatedImage::new(frames))
    }
}
