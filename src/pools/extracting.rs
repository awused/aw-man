use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;
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

// Experimentally determined three writers was a good balance.
// No hard data.
const WRITER_COUNT: usize = 3;
const PERMITS: usize = WRITER_COUNT + 1;

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
        .num_threads(CONFIG.extraction_threads.get() * WRITER_COUNT)
        .build()
        .expect("Error creating writer threadpool")
});

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

pub fn extract(source: Arc<Path>, jobs: PendingExtraction) -> OngoingExtraction {
    let sem = Arc::new(Semaphore::new(PERMITS));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Allow two files per writer thread to be queued for writing.
    let (s, receiver) = flume::bounded(WRITER_COUNT * 2);

    let cancel = cancel_flag.clone();
    let permit = sem.clone().try_acquire_owned().unwrap();
    EXTRACTION.spawn_fifo(move || {
        let _p = permit;
        if let Err(e) = reader(source, jobs, s, cancel) {
            error!("Error extracting archive: {e}");
        }
    });

    for _ in 0..WRITER_COUNT {
        let permit = sem.clone().try_acquire_owned().unwrap();
        let r = receiver.clone();
        WRITERS.spawn_fifo(move || {
            let _p = permit;
            writer(r);
        });
    }

    OngoingExtraction { cancel_flag, sem }
}

fn reader(
    source: Arc<Path>,
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
    let mut data: Vec<u8> = Vec::new();
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
            ArchiveContents::StartOfEntry(path, header) => {
                relpath = path;
                in_file = true;
                if header.st_size > 0 {
                    data = Vec::with_capacity(header.st_size as usize);
                }
            }
            ArchiveContents::DataChunk(d) => data.extend(d),
            ArchiveContents::EndOfEntry => {
                let current_file = data;
                data = Vec::new();
                if let Some((_, job)) = jobs.ext_map.remove_entry(&relpath) {
                    completed_jobs.send((job, current_file))?;
                }
                in_file = false;
            }
            ArchiveContents::Err(e) => return Err(Box::new(e)),
        }
    }
    trace!("Done extracting file {source:?} in {:?}ms", start.elapsed().as_millis());

    Ok(())
}

fn extract_single_file(
    source: &Path,
    relpath: String,
    job: PageExtraction,
    completed_jobs: &Sender<(PageExtraction, Vec<u8>)>,
) -> Result<()> {
    debug!("Extracting {relpath} early");

    let mut target = Vec::new();

    let file = BufReader::new(File::open(source)?);

    match compress_tools::uncompress_archive_file_with_encoding(file, &mut target, &relpath, decode)
    {
        Ok(_) => {
            completed_jobs.send((job, target))?;
        }
        Err(e) => {
            // A file that's missing from an archive is not a fatal error.
            error!("Failed to find or extract file {relpath}: {e}");
        }
    }

    Ok(())
}

fn writer(completed_jobs: Receiver<(PageExtraction, Vec<u8>)>) {
    for (job, data) in completed_jobs {
        let mut file = match File::create(&job.ext_path) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to create file {:?}: {e:?}", job.ext_path);
                job.completion
                    .send(Err(e.to_string()))
                    .unwrap_or_else(|e| error!("Failed sending to oneshot channel {e:?}"));
                continue;
            }
        };

        let res = match file.write_all(&data) {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to write file {:?}: {e}", job.ext_path);
                Err(e.to_string())
            }
        };
        job.completion
            .send(res)
            .unwrap_or_else(|e| error!("Failed sending to oneshot channel {e:?}"));
    }
}
