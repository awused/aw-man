use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::FutureExt;
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::oneshot;

use crate::com::{Bgra, Res, WorkParams};
use crate::config::CONFIG;
use crate::pools::loading::UnscaledBgra;
use crate::pools::{handle_panic, resample};
use crate::{Fut, Result};

#[derive(Debug, Clone)]
pub struct ScaledBgra(pub Bgra);

static DOWNSCALING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("scan-load-{}", u))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.downscaling_threads)
        .build()
        .expect("Error creating loading threadpool")
});


pub struct DownscaleFuture<T, R>
where
    T: 'static,
    R: Clone + 'static,
{
    pub fut: Fut<std::result::Result<T, String>>,
    // Not all formats will meaningfully support cancellation.
    cancel_flag: Arc<AtomicBool>,
    extra_info: R,
}

impl<T, R: Clone> DownscaleFuture<T, R> {
    pub fn cancel(&mut self) -> Fut<()> {
        self.cancel_flag.store(true, Ordering::Relaxed);
        let fut = std::mem::replace(
            &mut self.fut,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on a cancelled ScaleFuture") }.boxed(),
        );
        let h = tokio::task::spawn_local(async move {
            drop(fut.await);
        });
        h.map(|_| {}).boxed()
    }
}

impl<T> DownscaleFuture<T, WorkParams> {
    pub const fn params(&self) -> WorkParams {
        self.extra_info
    }
}

impl<T, R: Clone + fmt::Debug> fmt::Debug for DownscaleFuture<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[ScaleFuture {:?}]", self.extra_info)
    }
}

fn spawn_task<F, T>(
    closure: F,
    params: WorkParams,
    cancel_flag: Arc<AtomicBool>,
) -> DownscaleFuture<T, WorkParams>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: fmt::Debug + Send,
{
    let (s, r) = oneshot::channel();

    // Spawn in regular FILO ordering so that our newest job runs first.
    // TODO -- consider spawning most jobs FIFO except for the current rescale
    DOWNSCALING.spawn(move || {
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

    DownscaleFuture {
        fut,
        cancel_flag,
        extra_info: params,
    }
}

pub mod static_image {
    use futures_util::future;

    use super::*;

    pub async fn downscale(
        bgra: &UnscaledBgra,
        params: WorkParams,
    ) -> DownscaleFuture<ScaledBgra, WorkParams> {
        if params.park_before_scale {
            // The current image needs to be processed first, so just park here and wait, the
            // current image will eventually make progress and unblock us for a subsequent call.
            trace!("Parking before downscaling");
            future::pending().await
        }
        // TODO -- consider limiting to two images being scaled at once to avoid tail latency.

        let bgra = bgra.0.clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let closure = move || resize(bgra, params, cancel);

        spawn_task(closure, params, cancel_flag)
    }


    fn resize(bgra: Bgra, params: WorkParams, cancel: Arc<AtomicBool>) -> Result<ScaledBgra> {
        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let img = bgra.clone_image_buffer();

        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let res = Res::from(img.dimensions()).fit_inside(params.target_res);

        let start = Instant::now();

        let resized =
            resample::resize_par_linear(&img, res.w, res.h, resample::FilterType::CatmullRom);

        trace!(
            "Finished scaling image in {}ms",
            start.elapsed().as_millis()
        );

        Ok(ScaledBgra(resized.into()))
    }
}
