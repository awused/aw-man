use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
        .spawn()
        .is_ok()
});

static FILE_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^ *[^ ]+ +(\d+) +[^ ]+ +[^ ]+ +(.*)\n").unwrap());


pub fn reader(
    source: PathBuf,
    mut jobs: PendingExtraction,
    completed_jobs: Sender<(PageExtraction, Vec<u8>)>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let files = read_files(&source)?;

    let mut buf = content_reader(&source)?;
    let reader = &mut buf;

    for (name, size) in &files {
        if cancel.load(Ordering::Relaxed) {
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

    Ok(())
}


fn content_reader<P: AsRef<Path>>(source: P) -> Result<BufReader<ChildStdout>> {
    let process = Command::new("unrar")
        .args(&["p", "-inul", "--"])
        .arg(source.as_ref())
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = process.stdout.expect("Impossible");
    Ok(BufReader::new(stdout))
}

fn extract_single_file<P: AsRef<Path>>(
    source: P,
    relpath: String,
    job: PageExtraction,
    completed_jobs: &Sender<(PageExtraction, Vec<u8>)>,
) -> Result<()> {
    debug!("Extracting {} early", relpath);

    let process = Command::new("unrar")
        .args(&["p", "-inul", "--"])
        .arg(source.as_ref())
        .arg(&relpath)
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = process.stdout.expect("Impossible");
    let mut stdout = BufReader::new(stdout);

    let mut output = Vec::new();

    match stdout.read_to_end(&mut output) {
        Ok(_) => {
            completed_jobs.send((job, output))?;
        }
        Err(e) => {
            // A file that's missing from an archive is not a fatal error.
            error!("Failed to find or extract file {}: {:?}", relpath, e);
        }
    }

    Ok(())
}

pub fn read_files<P: AsRef<Path>>(source: P) -> Result<Vec<(String, usize)>> {
    let process = Command::new("unrar")
        .args(&["l", "--"])
        .arg(source.as_ref())
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = process.stdout.expect("Impossible");
    let mut stdout = BufReader::new(stdout);

    let mut output = Vec::new();


    let mut line = String::new();
    while 0 != stdout.read_line(&mut line)? {
        if let Some(cap) = FILE_LINE_RE.captures(&line) {
            let size = cap
                .get(1)
                .expect("Invalid capture")
                .as_str()
                .parse::<usize>()?;
            output.push((
                cap.get(2).expect("Invalid capture").as_str().to_owned(),
                size,
            ));
        }
        line.truncate(0);
    }
    Ok(output)
}
