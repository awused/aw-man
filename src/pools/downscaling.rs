use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use color_eyre::Result;
use futures_util::{FutureExt, future};
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use crate::Fut;
use crate::com::{Image, Res, ScalingParams};
use crate::config::CONFIG;
use crate::pools::handle_panic;
use crate::pools::loading::UnscaledImage;


static DOWNSCALING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("downscaling-{u}"))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.downscaling_threads.get())
        .build()
        .expect("Error creating downscaling threadpool")
});

// Allow two non-current images to be downscaling at any one time to keep throughput up while
// keeping tail latency reasonable.
static DOWNSCALING_SEM: Lazy<Arc<Semaphore>> = Lazy::new(|| Arc::new(Semaphore::new(2)));

// This was made generic to support animations, but it's been years, probably better to clean it
// up.
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
            async { unreachable!("Waited on a cancelled DownscaleFuture") }.boxed(),
        );
        let h = tokio::task::spawn_local(async move { drop(fut.await) });
        h.map(|_| {}).boxed()
    }
}

impl<T> DownscaleFuture<T, ScalingParams> {
    pub const fn params(&self) -> &ScalingParams {
        &self.extra_info
    }
}

impl<T, R: Clone + fmt::Debug> fmt::Debug for DownscaleFuture<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[ScaleFuture {:?}]", self.extra_info)
    }
}


// closure should return None when cancelled
fn spawn_task<F, T>(
    closure: F,
    params: ScalingParams,
    cancel_flag: Arc<AtomicBool>,
    permit: Option<OwnedSemaphorePermit>,
    jump_queue: bool,
) -> DownscaleFuture<T, ScalingParams>
where
    F: FnOnce() -> Result<Option<T>> + Send + 'static,
    T: fmt::Debug + Send,
{
    let (s, r) = oneshot::channel();

    let closure = move || {
        let result = closure();
        // Permit must be dropped from the other thread
        drop(permit);
        let result = match result {
            Ok(Some(dr)) => Ok(dr),
            Ok(None) => {
                debug!("Cancelled downscaling file");
                Err("Cancelled".to_string())
            }
            Err(e) => {
                error!("{e:?}");
                Err(format!("Error downscaling file {e}"))
            }
        };

        if let Err(_e) = s.send(result) {
            error!("Unexpected channel send failure");
        };
    };

    if jump_queue {
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


impl Downscaler {
    pub async fn downscale(
        &self,
        uimg: &UnscaledImage,
        params: &ScalingParams,
        jump_queue: bool,
    ) -> DownscaleFuture<Image, ScalingParams> {
        if params.park_before_scale {
            // The current image needs to be processed first, so just park here and wait, the
            // current image will eventually make progress and unblock us for a subsequent call.
            trace!("Parking before downscaling");
            future::pending().await
        }

        let downscaling_permit = if jump_queue {
            None
        } else {
            Some(DOWNSCALING_SEM.clone().acquire_owned().await.unwrap())
        };

        let img = uimg.0.clone();
        let resize_res = uimg.0.res.fit_inside(params.target_res);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();

        #[cfg(feature = "opencl")]
        let closure = {
            let estimated_vram = estimate_vram_mb(uimg.0.res, resize_res);

            let maybe_queue = if *VRAM_LIMIT_MB == 0 {
                None
            } else if estimated_vram > *VRAM_LIMIT_MB as usize {
                warn!(
                    "Downscaling image with resolution {} to {resize_res} is believed to take \
                     {estimated_vram}MB of vram, which is more than what is configured ({}MB), \
                     using CPU instead.",
                    uimg.0.res, *VRAM_LIMIT_MB
                );
                None
            } else if jump_queue {
                // For the current image we're not going to wait or start the process.
                // Starting it means compiling the shader each time (thanks opencl) which can
                // delay rendering operations by stressing the GPU.
                self.get_if_init().await
            } else {
                self.await_queue().await
            };

            let gpu_reservation = if let Some(queue) = maybe_queue {
                // Even the current image doesn't get to skip this, but it should be extremely
                // fast and the current image will be the only waiter.
                trace!("Reserving {estimated_vram}MB of vram for downscaling");
                Some((
                    queue,
                    VRAM_MB_SEM.clone().acquire_many_owned(estimated_vram as u32).await.unwrap(),
                ))
            } else {
                None
            };

            move || static_image::process(img, resize_res, gpu_reservation, cancel)
        };


        #[cfg(not(feature = "opencl"))]
        let closure = move || process(img, resize_res, cancel);

        spawn_task(closure, *params, cancel_flag, downscaling_permit, jump_queue)
    }
}

mod static_image {
    use super::*;

    pub(super) fn process(
        img: Image,
        resize_res: Res,
        #[cfg(feature = "opencl")] gpu_reservation: Option<(ocl::ProQue, OwnedSemaphorePermit)>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<Image>> {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }

        let start = Instant::now();

        #[cfg(feature = "opencl")]
        if let Some((pro_que, permit)) = gpu_reservation {
            let resized = img.downscale_opencl(resize_res, pro_que);
            drop(permit);

            match resized {
                Ok(img) => {
                    trace!("Finished scaling image in {:?} with OpenCL", start.elapsed());
                    return Ok(Some(img));
                }
                Err(e) => {
                    error!(
                        "Failed to downscale image with OpenCL, consider reducing allowed memory \
                         usage: {e}",
                    );
                }
            }
        }


        let resized = img.downscale(resize_res);

        trace!("Finished scaling image in {:?} on CPU", start.elapsed());
        Ok(Some(resized))
    }
}


#[cfg(feature = "opencl")]
mod inner {
    use std::cell::RefCell;
    use std::num::NonZeroU16;
    use std::sync::Arc;
    use std::task::Poll;
    use std::time::Instant;

    use futures_util::{future, poll};
    use ocl::{Device, DeviceType, Platform, ProQue};
    use once_cell::sync::Lazy;
    use tokio::sync::Semaphore;
    use tokio::task::{JoinHandle, spawn_blocking};

    use crate::com::Res;
    use crate::config::CONFIG;

    // Whatever this is, it will definitely fit in a u32.
    pub(super) static VRAM_LIMIT_MB: Lazy<u32> =
        Lazy::new(|| CONFIG.gpu_vram_limit_gb.map_or(0, NonZeroU16::get) as u32 * 1024);

    // Each permit is 1MB of estimated vram usage.
    pub(super) static VRAM_MB_SEM: Lazy<Arc<Semaphore>> =
        Lazy::new(|| Arc::new(Semaphore::new(*VRAM_LIMIT_MB as usize)));

    // For now, don't adjust this for other image formats.
    pub(super) const fn estimate_vram_mb(start: Res, target: Res) -> usize {
        let src_size = start.w as usize * start.h as usize * 4 / 1_048_576;
        // Intermediate image uses one float per channel
        let intermediate_size = start.w as usize * target.h as usize * 4 * 4 / 1_048_576;
        let dst_size = target.w as usize * target.h as usize * 4 / 1_048_576;
        src_size + intermediate_size + dst_size
    }

    pub fn print_gpus() {
        let mut index = 0;
        for platform in Platform::list() {
            println!("Platform: {platform}:\n");

            let devices = Device::list(platform, Some(DeviceType::GPU));
            let Ok(devices) = devices else {
                continue;
            };

            devices.into_iter().for_each(|d| {
                println!(
                    "Device #{index}: {}\n",
                    d.name().unwrap_or_else(|_| "Unnamed GPU".to_string()),
                );
                index += 1;
            });
        }
    }

    // Take the first available matching the prefix, if any.
    // No method to differentiate between identical GPUs but this should be fine.
    pub fn find_best_opencl_device(gpu_prefix: &str) -> Option<(Platform, Device)> {
        for platform in Platform::list() {
            if let Some(device) = Device::list(platform, Some(DeviceType::GPU))
                .iter()
                .flatten()
                .find(|d| d.name().as_deref().unwrap_or("").starts_with(gpu_prefix))
            {
                return Some((platform, *device));
            }
        }

        if !gpu_prefix.is_empty() {
            error!("Could not find matching GPU for prefix \"{gpu_prefix}\", try --show-gpus");
        }

        // The code in resample.rs is faster than running resample.cl on the CPU.
        None
    }

    #[derive(Debug, Default)]
    enum OpenCLQueue {
        #[default]
        Uninitialized,
        Initializing(JoinHandle<Option<ProQue>>),
        Ready(ProQue),
        Failed,
    }

    impl OpenCLQueue {
        fn init(&mut self) {
            if matches!(*self, Self::Uninitialized) {
                *self = Self::Initializing(spawn_blocking(move || {
                    let resample_src = include_str!("../resample.cl");

                    let start = Instant::now();

                    let Some((platform, device)) = find_best_opencl_device(&CONFIG.gpu_prefix)
                    else {
                        warn!("Unable to find suitable GPU for OpenCL");
                        return None;
                    };

                    debug!(
                        "Attemping to construct ProQue\nDevice: {device}\n\nPlatform: {platform} "
                    );

                    let mut builder = ProQue::builder();
                    builder.src(resample_src).platform(platform).device(device);

                    match builder.build() {
                        Ok(r) => {
                            trace!("Finished constructing ProQue in {:?}", start.elapsed());
                            Some(r)
                        }
                        Err(e) => {
                            error!(
                                "Failed to initialize OpenCL context for GPU {}: {}",
                                device.name().unwrap_or_else(|msg| msg.to_string()),
                                e
                            );
                            None
                        }
                    }
                }));
            }
        }

        fn unload(&mut self) {
            match self {
                Self::Initializing(_) | Self::Ready(_) => *self = Self::Uninitialized,
                Self::Failed | Self::Uninitialized => (),
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
        pub fn unload(&mut self) {
            self.open_cl.borrow_mut().unload();
        }

        // Non-blocking despite being async
        pub(super) async fn get_if_init(&self) -> Option<ProQue> {
            let mut q = self.open_cl.borrow_mut();

            match &mut *q {
                OpenCLQueue::Uninitialized | OpenCLQueue::Failed => None,
                OpenCLQueue::Initializing(handle) => match poll!(handle) {
                    Poll::Ready(Ok(Some(pq))) => {
                        *q = OpenCLQueue::Ready(pq.clone());
                        Some(pq)
                    }
                    Poll::Ready(_) => {
                        *q = OpenCLQueue::Failed;
                        None
                    }
                    Poll::Pending => None,
                },
                OpenCLQueue::Ready(pq) => Some(pq.clone()),
            }
        }

        pub(super) async fn await_queue(&self) -> Option<ProQue> {
            let Ok(mut q) = self.open_cl.try_borrow_mut() else {
                // Another page is already being downscaled. It will make progress and unblock us
                // for a subsequent call.
                trace!("Parking before awaiting OpenCL");
                return future::pending().await;
            };

            if matches!(*q, OpenCLQueue::Uninitialized) {
                q.init();
            }

            match &mut *q {
                OpenCLQueue::Uninitialized => unreachable!(),
                OpenCLQueue::Initializing(handle) => {
                    if let Ok(Some(pq)) = handle.await {
                        *q = OpenCLQueue::Ready(pq.clone());
                        Some(pq)
                    } else {
                        *q = OpenCLQueue::Failed;
                        None
                    }
                }
                OpenCLQueue::Ready(pq) => Some(pq.clone()),
                OpenCLQueue::Failed => None,
            }
        }
    }
}

#[cfg(not(feature = "opencl"))]
mod inner {
    pub fn print_gpus() {
        println!("Built without OpenCL support");
    }

    #[derive(Default, Debug, PartialEq, Eq)]
    pub struct Downscaler {}

    impl Downscaler {
        #[allow(clippy::unused_self)]
        pub const fn unload(&self) {}
    }
}

#[cfg(all(test, feature = "opencl"))]
pub use self::inner::find_best_opencl_device;
#[cfg(feature = "opencl")]
use self::inner::*;
pub use self::inner::{Downscaler, print_gpus};
