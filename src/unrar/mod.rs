use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use flume::Sender;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::config::CONFIG;
use crate::manager::archive::{PageExtraction, PendingExtraction};
use crate::Result;

pub static HAS_UNRAR: Lazy<bool> = Lazy::new(|| {
    if !CONFIG.allow_external_extractors {
        return false;
    }

    // Just probe it, the user has explicitly allowed us to try.
    Command::new("unrar")
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .is_ok()
});

static FILE_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^ *[^ ]+ +(\d+) +[^ ]+ +[^ ]+ +(.*)\n").unwrap());

#[instrument(level = "error", skip_all)]
pub fn reader(
    source: Arc<Path>,
    mut jobs: PendingExtraction,
    completed_jobs: Sender<(PageExtraction, Vec<u8>)>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    info!("Starting extraction");
    let start = Instant::now();
    let files = read_files(&source)?;

    let mut process = Command::new("unrar")
        .args(["p", "-inul", "--"])
        .arg(&*source)
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = process.stdout.as_mut().unwrap();
    let mut buf = BufReader::new(stdout);
    let reader = &mut buf;

    for (name, size) in &files {
        if cancel.load(Ordering::Relaxed) {
            // Wait with output to avoid a deadlock if the process is trying to write to stdout.
            process.kill()?;
            process.wait_with_output()?;
            return Ok(());
        }

        // Allow the file the user is currently viewing to jump ahead of the archive order.
        if let Ok(path) = jobs.jump_receiver.try_recv() {
            if let Some(page_ext) = jobs.ext_map.remove(&path) {
                extract_single_file(&source, path, page_ext, &completed_jobs)?;
            }
        }

        let mut data = Vec::with_capacity(*size);
        reader.take(*size as u64).read_to_end(&mut data)?;

        if let Some((_, job)) = jobs.ext_map.remove_entry(name) {
            completed_jobs.send((job, data))?;
        }
    }

    process.wait()?;
    trace!("Done extracting archive in {:?}", start.elapsed());
    Ok(())
}


#[instrument(level = "error", skip(source, job, completed_jobs))]
fn extract_single_file(
    source: &Path,
    relpath: String,
    job: PageExtraction,
    completed_jobs: &Sender<(PageExtraction, Vec<u8>)>,
) -> Result<()> {
    debug!("Extracting early");

    let process = Command::new("unrar")
        .args(["p", "-inul", "--"])
        .arg(source)
        .arg(&relpath)
        .stdout(Stdio::piped())
        .spawn()?;

    match process.wait_with_output() {
        Ok(output) => {
            completed_jobs.send((job, output.stdout))?;
        }
        Err(e) => {
            // A file that's missing from an archive is not a fatal error.
            error!("Failed to find or extract file: {e}");
        }
    }

    Ok(())
}

#[instrument(level = "error", skip_all)]
pub fn read_files<P: AsRef<Path>>(source: P) -> Result<Vec<(String, usize)>> {
    let mut process = Command::new("unrar")
        .args(["l", "--"])
        .arg(source.as_ref())
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = process.stdout.as_mut().unwrap();
    let mut stdout = BufReader::new(stdout);

    let mut output = Vec::new();

    let mut line = String::new();
    while 0 != stdout.read_line(&mut line)? {
        if let Some(cap) = FILE_LINE_RE.captures(&line) {
            let size = cap[1].parse::<usize>()?;
            output.push((cap[2].to_owned(), size));
        }
        line.truncate(0);
    }

    process.wait()?;

    Ok(output)
}
