use std::path::{Path, PathBuf};
use std::sync::Arc;

use aw_upscale::Upscaler;
use futures_util::FutureExt;
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{oneshot, Semaphore};

use crate::com::Res;
use crate::config::{CONFIG, TARGET_RES};
use crate::Fut;

static UPSCALING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("upscale-{}", u))
        .num_threads(CONFIG.upscaling_threads)
        .build()
        .expect("Error creating upscaling threadpool")
});

static UPSCALING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.upscaling_threads)));

static UPSCALER: Lazy<Upscaler> = Lazy::new(|| {
    let mut u = Upscaler::new(CONFIG.alternate_upscaler.clone());
    u.set_denoise(true)
        .set_width(TARGET_RES.w)
        .set_height(TARGET_RES.h);
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

fn do_upscale(source: PathBuf, dest: PathBuf) -> crate::Result<Res> {
    Ok(Res::from(UPSCALER.run(source, dest)?))
}
