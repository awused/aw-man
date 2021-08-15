use std::fs::File;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use std::{fmt, fs};

use futures_util::FutureExt;
use image::gif::GifDecoder;
use image::imageops::FilterType;
use image::{AnimationDecoder, DynamicImage, GenericImageView};
use jpegxl_rs::image::ToDynamic;
use once_cell::sync::Lazy;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

use crate::com::{AnimatedImage, Bgra, LoadingParams, Res};
use crate::config::CONFIG;
use crate::manager::files::{is_gif, is_jxl, is_natively_supported_image, is_webp};
use crate::pools::handle_panic;
use crate::{Fut, Result};

static LOADING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("load-{}", u))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.loading_threads)
        .build()
        .expect("Error creating loading threadpool")
});

static LOADING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.loading_threads)));

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

impl<T, R: Clone> LoadFuture<T, R> {
    pub fn cancel(&mut self) -> Fut<()> {
        self.cancel_flag.store(true, Ordering::Relaxed);
        let fut = std::mem::replace(
            &mut self.fut,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on a cancelled LoadFuture") }.boxed(),
        );
        let h = tokio::task::spawn_local(async move {
            drop(fut.await);
        });
        h.map(|_| {}).boxed()
    }
}

impl<T> LoadFuture<T, LoadingParams> {
    pub const fn params(&self) -> LoadingParams {
        self.extra_info
    }
}

impl<T, R: Clone + fmt::Debug> fmt::Debug for LoadFuture<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[LoadFuture {:?}]", self.extra_info)
    }
}

fn spawn_task<F, T>(
    closure: F,
    params: LoadingParams,
    cancel_flag: Arc<AtomicBool>,
    permit: OwnedSemaphorePermit,
) -> LoadFuture<T, LoadingParams>
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

    pub async fn load(path: Rc<PathBuf>, params: LoadingParams) -> LoadFuture<Bgra, LoadingParams> {
        let permit = LOADING_SEM
            .clone()
            .acquire_owned()
            .await
            .expect("Error acquiring loading permit");

        let path = (*path).clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let closure = move || load_and_maybe_scale(path, params, cancel);

        spawn_task(closure, params, cancel_flag, permit)
    }

    pub async fn rescale(bgra: &Bgra, params: LoadingParams) -> LoadFuture<Bgra, LoadingParams> {
        let permit = LOADING_SEM
            .clone()
            .acquire_owned()
            .await
            .expect("Error acquiring loading permit");

        let bgra = bgra.clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let closure = move || resize(bgra, params, cancel);

        spawn_task(closure, params, cancel_flag, permit)
    }


    fn load_and_maybe_scale(
        path: PathBuf,
        params: LoadingParams,
        cancel: Arc<AtomicBool>,
    ) -> Result<Bgra> {
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
            image::open(&path)?
        } else {
            unreachable!();
        };


        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        if params.scale_during_load {
            let res = Res::from(img.dimensions()).fit_inside(params.target_res);

            if res != Res::from(img.dimensions()) {
                let start = Instant::now();
                let resized = img.resize_exact(res.w, res.h, FilterType::CatmullRom);
                trace!(
                    "Finished scaling image in {}ms",
                    start.elapsed().as_millis()
                );

                return Ok(Bgra::from(resized));
            }
        }
        Ok(Bgra::from(img))
    }

    fn resize(bgra: Bgra, params: LoadingParams, cancel: Arc<AtomicBool>) -> Result<Bgra> {
        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let img: DynamicImage = bgra.into();

        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let res = Res::from(img.dimensions()).fit_inside(params.target_res);

        let start = Instant::now();
        let resized = img.resize_exact(res.w, res.h, FilterType::CatmullRom);
        trace!(
            "Finished scaling image in {}ms",
            start.elapsed().as_millis()
        );

        Ok(Bgra::from(resized))
    }
}

pub mod animation {

    use super::*;

    pub async fn load(
        path: Rc<PathBuf>,
        params: LoadingParams,
    ) -> LoadFuture<AnimatedImage, LoadingParams> {
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

    fn load_animation(path: PathBuf, cancel: Arc<AtomicBool>) -> Result<AnimatedImage> {
        let frames = if is_gif(&path) {
            let f = File::open(&path)?;
            let decoder = GifDecoder::new(f)?;
            let gif_frames: std::result::Result<Vec<_>, _> = decoder
                .into_frames()
                .take_while(|_| !cancel.load(Ordering::Relaxed))
                .collect();

            gif_frames?
                .into_par_iter()
                .filter_map(|frame| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }

                    let dur = frame.delay().into();
                    let img = DynamicImage::ImageRgba8(frame.into_buffer());
                    Some((img.into(), dur))
                })
                .collect()
        } else {
            return Err("Not yet implemented".into());
        };


        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }

        return Ok(AnimatedImage::new(frames));
    }
}
