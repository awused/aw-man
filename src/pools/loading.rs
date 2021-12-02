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
use image_23::codecs::gif::GifDecoder;
use image_23::codecs::png::PngDecoder;
use image_23::{AnimationDecoder, DynamicImage, ImageFormat};
use jpegxl_rs::image::ToDynamic;
use once_cell::sync::Lazy;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

use crate::com::{AnimatedImage, Bgra, Frames, Res, WorkParams};
use crate::config::{CONFIG, MINIMUM_RES, TARGET_RES};
use crate::manager::files::{
    is_gif, is_jxl, is_natively_supported_image, is_pixbuf_extension, is_png, is_video_extension,
    is_webp,
};
use crate::pools::handle_panic;
use crate::{closing, Fut, Result};

#[derive(Debug, Clone)]
pub struct UnscaledBgra(pub Bgra);

static LOADING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.loading_threads)));

static LOADING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("loading-{}", u))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.loading_threads)
        .build()
        .expect("Error creating loading threadpool")
});

#[derive(Debug, From)]
pub enum BgraOrRes {
    Bgra(UnscaledBgra),
    Res(Res),
}

impl BgraOrRes {
    pub const fn res(&self) -> Res {
        match self {
            BgraOrRes::Bgra(bgra) => bgra.0.res,
            BgraOrRes::Res(r) => *r,
        }
    }

    pub fn should_upscale(&self) -> bool {
        if TARGET_RES.is_zero() {
            return false;
        }

        let r = self.res();
        ((r.w < TARGET_RES.w || TARGET_RES.w == 0) && (r.h < TARGET_RES.h || TARGET_RES.h == 0))
            || r.w < MINIMUM_RES.w
            || r.h < MINIMUM_RES.h
    }
}


#[derive(Debug)]
pub enum ScanResult {
    // If we needed to convert it to a format the image crate understands.
    // This file will be written when this result is returned, and is owned by the newly created
    // ScannedPage.
    // If the Bgra is None it means the page was unloaded while scanning, so it needs to be read
    // from scratch.
    ConvertedImage(PathBuf, BgraOrRes),
    Image(BgraOrRes),
    // Animations skip the fast path, at least for now.
    Animation,
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
        use BgraOrRes as BOR;
        use ScanResult::*;

        let fut = std::mem::replace(
            &mut self.0,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on an invalid ScanFuture") }.boxed(),
        );
        // The entire Manager runs inside a single LocalSet so this will not panic.
        let h = tokio::task::spawn_local(async move {
            let drop_bgra;
            let out = match fut.await {
                ConvertedImage(pb, BOR::Bgra(bgra)) => {
                    drop_bgra = bgra.0.clone();
                    ConvertedImage(pb, bgra.0.res.into())
                }
                Image(BOR::Bgra(bgra)) => {
                    drop_bgra = bgra.0.clone();
                    Image(bgra.0.res.into())
                }
                x => return x,
            };

            tokio::task::spawn_blocking(move || drop(drop_bgra));
            out
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
                let e = format!("Error scanning file {:?}", e);
                error!("{}", e);
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
        let decoder = GifDecoder::new(f)?;
        let mut frames = decoder.into_frames();

        let first_frame = frames.next();
        let second_frame = frames.next();

        if second_frame.is_none() && first_frame.is_some() {
            let first_frame = first_frame.unwrap()?;
            if load {
                let img = DynamicImage::ImageRgba8(first_frame.into_buffer());
                return Ok(Image(UnscaledBgra(img.into()).into()));
            }
            return Ok(Image(
                Res::from((first_frame.buffer().width(), first_frame.buffer().height())).into(),
            ));
        } else if second_frame.is_some() {
            return Ok(Animation);
        }
    } else if is_png(&path) {
        let f = File::open(&path)?;

        // Fall through to pixbuf in case this image won't load.
        // This is relevant for PNGs with invalid CRCs that pixbuf is tolerant of.
        match PngDecoder::new(f) {
            Ok(decoder) => {
                if decoder.is_apng() {
                    return Ok(Animation);
                }

                let img = DynamicImage::from_decoder(decoder)?;
                return Ok(Image(UnscaledBgra(img.into()).into()));
            }
            Err(e) => error!(
                "Error {:?} while trying to read {:?}, trying again with pixbuf.",
                e, path
            ),
        }
    } else if is_natively_supported_image(&path) {
        let img = image::open(&path);
        // Fall through to pixbuf in case this image won't load.
        // This is relevant for PNGs with invalid CRCs that pixbuf is tolerant of.
        match img {
            Ok(img) => {
                if load {
                    return Ok(Image(UnscaledBgra(img.into()).into()));
                }
                return Ok(Image(Res::from(img).into()));
            }
            Err(e) => error!(
                "Error {:?} while trying to read {:?}, trying again with pixbuf.",
                e, path
            ),
        }
    }

    if is_jxl(&path) {
        let data = fs::read(&path)?;

        // TODO -- allow fall-through once the pixbuf loader is fixed?
        let decoder = jpegxl_rs::decoder_builder().build()?;
        let img = decoder
            .decode(&data)?
            .into_dynamic_image()
            .ok_or("Failed to convert jpeg-xl to DynamicImage")?;
        if load {
            return Ok(Image(UnscaledBgra(img.into()).into()));
        }
        return Ok(Image(Res::from(img).into()));
    }

    if is_webp(&path) {
        let data = fs::read(&path)?;

        let features = webp::BitstreamFeatures::new(&data).ok_or("Could not read webp.")?;
        if features.has_animation() {
            return Ok(Animation);
        } else if load {
            let decoded = webp::Decoder::new(&data)
                .decode()
                .ok_or("Could not decode webp")?;
            return Ok(Image(UnscaledBgra(decoded.to_image().into()).into()));
        }
        return Ok(Image(
            Res::from((features.width(), features.height())).into(),
        ));
    }

    // TODO -- pixbuf loaders often leak or segfault, consider doing this in another process.

    if is_pixbuf_extension(&path) {
        let pb = gtk::gdk_pixbuf::Pixbuf::from_file(&path)?;
        let pngvec = pb.save_to_bufferv("png", &[("compression", "1")])?;

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let mut f = File::create(&conv)?;
        f.write_all(&pngvec)?;
        drop(f);

        debug!("Converted {:?} to {:?}", path, conv);

        if !load {
            return Ok(ConvertedImage(
                conv,
                Res::from((pb.width(), pb.height())).into(),
            ));
        }

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let img = image_23::load_from_memory_with_format(&pngvec, ImageFormat::Png)?;
        return Ok(ConvertedImage(conv, UnscaledBgra(img.into()).into()));
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
        let h = tokio::task::spawn_local(async move {
            let result = fut.await;
            tokio::task::spawn_blocking(move || drop(result));
        });
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
                error!("{}", e);
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

    LoadFuture {
        fut,
        cancel_flag,
        extra_info: params,
    }
}

pub mod static_image {

    use super::*;

    pub async fn load(
        path: Rc<PathBuf>,
        params: WorkParams,
    ) -> LoadFuture<UnscaledBgra, WorkParams> {
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
    ) -> Result<UnscaledBgra> {
        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let img = if is_webp(&path) {
            let data = fs::read(&path)?;

            let decoded = webp::Decoder::new(&data)
                .decode()
                .ok_or("Could not decode webp")?;
            decoded.to_image()
        } else if is_jxl(&path) {
            let data = fs::read(&path)?;
            let decoder = jpegxl_rs::decoder_builder().build()?;
            decoder
                .decode(&data)?
                .into_dynamic_image()
                .ok_or("Failed to convert jpeg-xl to DynamicImage")?
        } else if is_natively_supported_image(&path) {
            image_23::open(&path)?
        } else {
            unreachable!();
        };


        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        Ok(UnscaledBgra(img.into()))
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
    ) -> Result<Frames> {
        let raw_frames: std::result::Result<Vec<_>, _> = dec
            .into_frames()
            .take_while(|_| !cancel.load(Ordering::Relaxed))
            .collect();

        Ok(raw_frames?
            .into_par_iter()
            .filter_map(|frame| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }

                let dur = frame.delay().into();
                let img = image_23::DynamicImage::ImageRgba8(frame.into_buffer());

                let mut h = AHasher::default();
                img.hash(&mut h);
                let hash = h.finish();

                Some((img.into(), dur, hash))
            })
            .collect::<Vec<_>>()
            .into())
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
                .take_while(|_| !cancel.load(Ordering::Relaxed))
                .map(|frame| {
                    let d = frame.timestamp() - last_frame;
                    last_frame = frame.timestamp();
                    let d = Duration::from_millis(d.saturating_abs() as u64);
                    frame.into_image().map(|img| (img, d))
                })
                .collect();


            let webp_frames = webp_frames.map_err(|e| format!("{:?}", e))?;

            webp_frames
                .into_par_iter()
                .filter_map(|(img, dur)| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }

                    let img = image_23::DynamicImage::ImageRgba8(img);

                    let mut h = AHasher::default();
                    img.hash(&mut h);
                    let hash = h.finish();

                    Some((img.into(), dur, hash))
                })
                .collect::<Vec<_>>()
                .into()
        } else {
            return Err("Not yet implemented".into());
        };


        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }

        Ok(AnimatedImage::new(frames))
    }
}
