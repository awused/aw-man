use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::{future, FutureExt};
use ocl::ProQue;
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::select;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::task::{spawn_blocking, JoinHandle};

use crate::com::{Image, WorkParams};
use crate::config::CONFIG;
use crate::pools::handle_panic;
use crate::pools::loading::UnscaledImage;
use crate::{Fut, Result};

static DOWNSCALING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("downscaling-{}", u))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.downscaling_threads.get())
        .build()
        .expect("Error creating downscaling threadpool")
});

// Allow two non-current images to be downscaling at any one time to keep throughput up while
// keeping tail latency reasonable.
static DOWNSCALING_SEM: Lazy<Arc<Semaphore>> = Lazy::new(|| Arc::new(Semaphore::new(2)));

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

impl<T: Send, R: Clone> DownscaleFuture<T, R> {
    pub fn cancel(&mut self) -> Fut<()> {
        self.cancel_flag.store(true, Ordering::Relaxed);
        let fut = std::mem::replace(
            &mut self.fut,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on a cancelled ScaleFuture") }.boxed(),
        );
        let h = tokio::task::spawn_local(async move { drop(fut.await) });
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

#[derive(Debug)]
enum OpenCLQueue {
    Uninitialized,
    Initializing(JoinHandle<Option<ProQue>>),
    Ready(ProQue),
    Failed,
}

impl Default for OpenCLQueue {
    fn default() -> Self {
        Self::Uninitialized
    }
}

impl OpenCLQueue {
    fn init(&mut self) {
        if let Self::Uninitialized = *self {
            *self = Self::Initializing(spawn_blocking(move || {
                let resample_src = include_str!("../resample.cl");

                let start = Instant::now();
                match ProQue::builder().src(resample_src).build() {
                    Ok(r) => {
                        trace!("Finished constructing ProQue in {:?}", start.elapsed());
                        Some(r)
                    }
                    Err(e) => {
                        error!("Failed to initialize OpenCL context: {}", e);
                        None
                    }
                }
            }));
        }
    }
}

#[derive(Default, Debug)]
pub struct Downscaler {
    open_cl: RefCell<OpenCLQueue>,
}

impl PartialEq for Downscaler {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Downscaler {
    fn assert_receiver_is_total_eq(&self) {}
}

impl Downscaler {
    pub fn init(&self) {
        self.open_cl.borrow_mut().init();
    }

    // Non-blocking despite being async
    async fn get_or_init_queue(&self) -> Option<ProQue> {
        let mut q = if let Ok(q) = self.open_cl.try_borrow_mut() {
            q
        } else {
            // Only called for the current page, which will always go first.
            unreachable!()
        };

        match &mut *q {
            OpenCLQueue::Uninitialized => {
                q.init();
                None
            }
            OpenCLQueue::Initializing(handle) => {
                select! {
                    biased;
                    r = handle => {
                        if let Ok(Some(pq)) = r {
                            *q = OpenCLQueue::Ready(pq.clone());
                            Some(pq)
                        } else {
                            *q = OpenCLQueue::Failed;
                            None
                        }
                    }
                    _ = future::ready(()) => None,
                }
            }
            OpenCLQueue::Ready(pq) => Some(pq.clone()),
            OpenCLQueue::Failed => None,
        }
    }

    async fn await_queue(&self) -> Option<ProQue> {
        let mut q = if let Ok(q) = self.open_cl.try_borrow_mut() {
            q
        } else {
            // Another page is already being downscaled. It will make progress and unblock us for a
            // subsequent call.
            trace!("Parking before awaiting OpenCL");
            return future::pending().await;
        };

        loop {
            match &mut *q {
                OpenCLQueue::Uninitialized => {
                    q.init();
                }
                OpenCLQueue::Initializing(handle) => {
                    if let Ok(Some(pq)) = handle.await {
                        *q = OpenCLQueue::Ready(pq.clone());
                        return Some(pq);
                    }
                    *q = OpenCLQueue::Failed;
                    return None;
                }
                OpenCLQueue::Ready(pq) => return Some(pq.clone()),
                OpenCLQueue::Failed => return None,
            }
        }
    }
}

fn spawn_task<F, T>(
    closure: F,
    params: WorkParams,
    cancel_flag: Arc<AtomicBool>,
    permit: Option<OwnedSemaphorePermit>,
) -> DownscaleFuture<T, WorkParams>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: fmt::Debug + Send,
{
    let (s, r) = oneshot::channel();

    let closure = move || {
        let result = closure();
        drop(permit);
        let result = match result {
            Ok(sr) => Ok(sr),
            Err(e) => {
                let e = format!("Error downscaling file {:?}", e);
                error!("{}", e);
                Err(e)
            }
        };

        if let Err(e) = s.send(result) {
            error!("Unexpected error downscaling file {:?}", e);
        };
    };

    if params.jump_downscaling_queue {
        DOWNSCALING.spawn(closure);
    } else {
        DOWNSCALING.spawn_fifo(closure);
    }

    let fut = r
        .map(|r| match r {
            Ok(nested) => nested,
            Err(e) => {
                error!("Unexpected error downscaling file {:?}", e);
                Err(e.to_string())
            }
        })
        .boxed_local();

    DownscaleFuture { fut, cancel_flag, extra_info: params }
}

pub mod static_image {

    use super::*;

    impl Downscaler {
        pub async fn downscale_and_premultiply(
            &self,
            uimg: &UnscaledImage,
            params: WorkParams,
        ) -> DownscaleFuture<Image, WorkParams> {
            if params.park_before_scale {
                // The current image needs to be processed first, so just park here and wait, the
                // current image will eventually make progress and unblock us for a subsequent call.
                trace!("Parking before downscaling");
                future::pending().await
            }

            let permit = if params.jump_downscaling_queue {
                None
            } else {
                Some(
                    DOWNSCALING_SEM
                        .clone()
                        .acquire_owned()
                        .await
                        .expect("Error acquiring downscaling permit"),
                )
            };

            error!("Trying to get queue, sync: {}", params.jump_downscaling_queue);
            let maybe_queue = if params.jump_downscaling_queue {
                self.get_or_init_queue().await
            } else {
                self.await_queue().await
            };

            error!("Got queue? {:?}", maybe_queue);


            let img = uimg.0.clone();
            let cancel_flag = Arc::new(AtomicBool::new(false));
            let cancel = cancel_flag.clone();
            let closure = move || process(img, params, cancel);


            spawn_task(closure, params, cancel_flag, permit)
        }
    }


    fn process(img: Image, params: WorkParams, cancel: Arc<AtomicBool>) -> Result<Image> {
        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("Cancelled").into());
        }

        let resize_res = img.res.fit_inside(params.target_res);

        let start = Instant::now();

        let resized = img.downscale(resize_res);

        trace!("Finished scaling image in {}ms", start.elapsed().as_millis());
        Ok(resized)
    }
}
