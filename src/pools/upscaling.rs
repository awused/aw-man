use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use aw_upscale::Upscaler;
use futures_util::FutureExt;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{Semaphore, oneshot};

use crate::Fut;
use crate::com::Res;
use crate::config::CONFIG;
use crate::pools::handle_panic;

static UPSCALING: LazyLock<ThreadPool> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("upscale-{u}"))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.upscaling_threads.get())
        .build()
        .expect("Error creating upscaling threadpool")
});

static UPSCALING_SEM: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(CONFIG.upscaling_threads.get())));

static UPSCALER: LazyLock<Upscaler> = LazyLock::new(|| {
    let mut u = Upscaler::new(CONFIG.alternate_upscaler.clone());
    u.set_denoise(Some(1))
        .set_target_width(CONFIG.target_resolution.w)
        .set_target_height(CONFIG.target_resolution.h)
        .set_timeout(CONFIG.upscale_timeout.map(|s| Duration::from_secs(s.get())));

    if let Some(mres) = CONFIG.minimum_resolution {
        u.set_min_width(mres.w).set_min_height(mres.h);
    }

    u
});

pub async fn upscale<P: AsRef<Path>>(source: P, dest: P) -> Fut<Result<Res, String>> {
    let permit = UPSCALING_SEM
        .clone()
        .acquire_owned()
        .await
        .expect("Error acquiring upscaling permit");

    let (s, r) = oneshot::channel();

    let source = source.as_ref().to_owned();
    let dest = dest.as_ref().to_owned();

    UPSCALING.spawn_fifo(move || {
        let result = match do_upscale(source, dest) {
            Ok(r) => Ok(r),
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = s.send(result) {
            error!("Unexpected error after upscaling: {:?}", e);
        }
        drop(permit);
    });

    r.map(|r| match r {
        Ok(nested) => nested,
        Err(e) => Err(e.to_string()),
    })
    .boxed_local()
}

fn do_upscale(source: PathBuf, dest: PathBuf) -> color_eyre::Result<Res> {
    Ok(Res::from(UPSCALER.run(source, dest)?))
}
