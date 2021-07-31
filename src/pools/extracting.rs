use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use compress_tools::ArchiveContents;
use flume::{Receiver, Sender};
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::Semaphore;

use crate::config::CONFIG;
use crate::manager::archive::{PageExtraction, PendingExtraction};
use crate::unrar;

static EXTRACTION: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("extract-{}", u))
        .num_threads(CONFIG.extraction_threads)
        .build()
        .expect("Error creating extraction threadpool")
});

static WRITERS: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("writer-{}", u))
        .num_threads(CONFIG.extraction_threads * (PERMITS as usize - 1))
        .build()
        .expect("Error creating writer threadpool")
});

// One for extraction + 3 for writing.
const PERMITS: usize = 4;

pub struct OngoingExtraction {
    cancel_flag: Arc<AtomicBool>,
    sem: Arc<Semaphore>,
}

impl OngoingExtraction {
    pub async fn cancel(&mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        drop(self.sem.acquire_many(PERMITS as u32).await);
    }
}

pub fn extract(source: PathBuf, jobs: PendingExtraction) -> OngoingExtraction {
    let sem = Arc::new(Semaphore::new(PERMITS));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let (s, receiver) = flume::bounded((PERMITS - 1) * 2);

    let cancel = cancel_flag.clone();
    let permit = sem.clone().try_acquire_owned().expect("Impossible");
    EXTRACTION.spawn_fifo(move || {
        let _p = permit;
        match reader(source, jobs, s, cancel) {
            Ok(_) => (),
            Err(e) => error!("Error extracting archive: {}", e),
        }
    });

    for _ in 0..PERMITS - 1 {
        let permit = sem.clone().try_acquire_owned().expect("Impossible");
        let r = receiver.clone();
        WRITERS.spawn_fifo(move || {
            let _p = permit;
            writer(r);
        });
    }

    OngoingExtraction { cancel_flag, sem }
}

fn reader(
    source: PathBuf,
    mut jobs: PendingExtraction,
    completed_jobs: Sender<(PageExtraction, Vec<u8>)>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    if let Some(ext) = source.extension() {
        let ext = ext.to_ascii_lowercase();
        if (ext == "rar" || ext == "cbr") && CONFIG.allow_external_extractors && *unrar::HAS_UNRAR {
            return unrar::reader(source, jobs, completed_jobs, cancel);
        }
    }

    let start = Instant::now();
    let file = File::open(&source).map_err(|e| e.to_string())?;

    let iter = compress_tools::ArchiveIterator::from_read(file).map_err(|e| e.to_string())?;

    let mut relpath: String = String::default();
    let mut data: Vec<u8> = Vec::with_capacity(1_048_576);
    let mut in_file = false;

    for cont in iter {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Allow files to jump ahead of the natural order.
        if !in_file {
            if let Ok(path) = jobs.jump_receiver.try_recv() {
                if let Some(page_ext) = jobs.ext_map.remove(&path) {
                    extract_single_file(&source, path, page_ext, &completed_jobs)?;
                }
            }
        }


        match cont {
            ArchiveContents::StartOfEntry(s) => {
                relpath = s;
                in_file = true;
            }
            ArchiveContents::DataChunk(d) => data.extend(d),
            ArchiveContents::EndOfEntry => {
                let current_file = data;
                data = Vec::with_capacity(1_048_576);
                if let Some((_, job)) = jobs.ext_map.remove_entry(&relpath) {
                    completed_jobs
                        .send((job, current_file))
                        .map_err(|e| e.to_string())?;
                }
                in_file = false;
            }
            ArchiveContents::Err(e) => return Err(e.to_string()),
        }
    }
    trace!(
        "Done extracting file {:?} in {:?}ms",
        source,
        start.elapsed().as_millis()
    );

    Ok(())
}

fn extract_single_file<P: AsRef<Path>>(
    source: P,
    relpath: String,
    job: PageExtraction,
    completed_jobs: &Sender<(PageExtraction, Vec<u8>)>,
) -> Result<(), String> {
    debug!("Extracting {} early", relpath);

    let mut target = Vec::new();

    let file = File::open(&source).map_err(|e| e.to_string())?;

    match compress_tools::uncompress_archive_file(file, &mut target, &relpath) {
        Ok(_) => {
            completed_jobs
                .send((job, target))
                .map_err(|e| e.to_string())?;
        }
        Err(e) => {
            // A file that's missing from an archive is not a true error.
            error!("Failed to find or extract file {}: {:?}", relpath, e);
        }
    }

    Ok(())
}

fn writer(completed_jobs: Receiver<(PageExtraction, Vec<u8>)>) {
    for (job, data) in completed_jobs {
        let mut file = match File::create(&job.ext_path) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to create file {:?}: {:?}", job.ext_path, e);
                let _ = job
                    .completion
                    .send(Err(e.to_string()))
                    .map_err(|e| error!("Failed sending to oneshot channel {:?}", e));
                continue;
            }
        };

        match file.write_all(&data) {
            Ok(_) => {
                let _ = job
                    .completion
                    .send(Ok(()))
                    .map_err(|e| error!("Failed sending to oneshot channel {:?}", e));
            }
            Err(e) => {
                error!("Failed to write file {:?}: {:?}", job.ext_path, e);
                let _ = job
                    .completion
                    .send(Err(e.to_string()))
                    .map_err(|e| error!("Failed sending to oneshot channel {:?}", e));
            }
        }
    }
}
