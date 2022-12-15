use std::fs::File;
use std::io::{BufReader, Write};
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
use crate::pools::handle_panic;
use crate::{unrar, Result};

static EXTRACTION: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("extract-{u}"))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.extraction_threads.get())
        .build()
        .expect("Error creating extraction threadpool")
});

static WRITERS: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("writer-{u}"))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.extraction_threads.get() * (PERMITS - 1))
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

fn decode(input: &[u8]) -> compress_tools::Result<String> {
    Ok(String::from_utf8_lossy(input).to_string())
}

pub fn extract(source: PathBuf, jobs: PendingExtraction) -> OngoingExtraction {
    let sem = Arc::new(Semaphore::new(PERMITS));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Allow two files per writer thread to be queued for writing.
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
) -> Result<()> {
    if let Some(ext) = source.extension() {
        let ext = ext.to_ascii_lowercase();
        if (ext == "rar" || ext == "cbr") && CONFIG.allow_external_extractors && *unrar::HAS_UNRAR {
            return unrar::reader(source, jobs, completed_jobs, cancel);
        }
    }

    let start = Instant::now();
    let file = BufReader::new(File::open(&source)?);

    let iter = compress_tools::ArchiveIterator::from_read_with_encoding(file, decode)?;

    let mut relpath: String = String::default();
    let mut data: Vec<u8> = Vec::with_capacity(1_048_576);
    let mut in_file = false;

    for cont in iter {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Allow the file the user is currently viewing to jump ahead of the archive order.
        if !in_file {
            if let Ok(path) = jobs.jump_receiver.try_recv() {
                if let Some(page_ext) = jobs.ext_map.remove(&path) {
                    extract_single_file(&source, path, page_ext, &completed_jobs)?;
                }
            }
        }


        match cont {
            ArchiveContents::StartOfEntry(s, _) => {
                relpath = s;
                in_file = true;
            }
            ArchiveContents::DataChunk(d) => data.extend(d),
            ArchiveContents::EndOfEntry => {
                let current_file = data;
                data = Vec::with_capacity(1_048_576);
                if let Some((_, job)) = jobs.ext_map.remove_entry(&relpath) {
                    completed_jobs.send((job, current_file))?;
                }
                in_file = false;
            }
            ArchiveContents::Err(e) => return Err(Box::new(e)),
        }
    }
    trace!("Done extracting file {:?} in {:?}ms", source, start.elapsed().as_millis());

    Ok(())
}

fn extract_single_file<P: AsRef<Path>>(
    source: P,
    relpath: String,
    job: PageExtraction,
    completed_jobs: &Sender<(PageExtraction, Vec<u8>)>,
) -> Result<()> {
    debug!("Extracting {} early", relpath);

    let mut target = Vec::new();

    let file = BufReader::new(File::open(&source)?);

    match compress_tools::uncompress_archive_file_with_encoding(file, &mut target, &relpath, decode)
    {
        Ok(_) => {
            completed_jobs.send((job, target))?;
        }
        Err(e) => {
            // A file that's missing from an archive is not a fatal error.
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
                job.completion
                    .send(Err(e.to_string()))
                    .unwrap_or_else(|e| error!("Failed sending to oneshot channel {:?}", e));
                continue;
            }
        };

        match file.write_all(&data) {
            Ok(_) => {
                job.completion
                    .send(Ok(()))
                    .unwrap_or_else(|e| error!("Failed sending to oneshot channel {:?}", e));
            }
            Err(e) => {
                error!("Failed to write file {:?}: {:?}", job.ext_path, e);
                job.completion
                    .send(Err(e.to_string()))
                    .unwrap_or_else(|e| error!("Failed sending to oneshot channel {:?}", e));
            }
        }
    }
}
