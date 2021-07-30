use std::sync::Arc;

use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::Semaphore;

use crate::config::CONFIG;

pub mod extracting;
pub mod loading;
pub mod scanning;

static _UPSCALING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("upscale-{}", u))
        .num_threads(CONFIG.upscaling_threads)
        .build()
        .expect("Error creating upscaling threadpool")
});

static _UPSCALING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.upscaling_threads)));
