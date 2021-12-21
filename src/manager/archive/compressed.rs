use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::path::{is_separator, Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use aw_man::natsort;
use futures_util::FutureExt;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::Archive;
use crate::config::CONFIG;
use crate::manager::archive::page::{ExtractFuture, Page};
use crate::manager::archive::{ExtractionStatus, PageExtraction, PendingExtraction};
use crate::manager::files::is_supported_page_extension;
use crate::unrar;

pub(super) fn new_archive(path: PathBuf, temp_dir: TempDir) -> Result<Archive, (PathBuf, String)> {
    trace!("Started reading compressed archive {:?}", path);
    let temp_dir = Rc::from(temp_dir);
    let start = Instant::now();

    // Jumping ahead of the queue can slow everything down, so only do it for the current image.
    // If the queue is already full then we just wait for normal extraction.
    let (jump_sender, jump_receiver) = flume::bounded(1);
    let jump_sender = Rc::from(jump_sender);


    let pages = read_files_in_archive(&path)?;

    // Try to find any common path-based prefix and remove them.
    let mut pages = remove_common_path_prefix(pages);

    // Sort by natural order
    pages.sort_by_cached_key(|(_, name)| natsort::key(name));

    let mut ext_map = HashMap::new();

    let pages: Vec<_> = pages
        .into_iter()
        .enumerate()
        .map(|(index, (rel_path, name))| {
            let (page, completion) =
                build_new_page(rel_path.clone(), name, index, &temp_dir, &jump_sender);

            let ext_path = page.borrow().get_absolute_file_path().to_path_buf();

            ext_map.insert(
                rel_path.to_string_lossy().to_string(),
                PageExtraction {
                    ext_path,
                    completion,
                },
            );
            page
        })
        .collect();

    trace!("Finished scanning archive {:?} {:?}", path, start.elapsed());

    let archive_name = path
        .file_name()
        .map_or_else(|| "".to_string(), |p| p.to_string_lossy().to_string());

    let pe = PendingExtraction {
        ext_map,
        jump_receiver,
        jump_sender: (*jump_sender).clone(),
    };

    Ok(Archive {
        name: archive_name,
        path,
        kind: super::Kind::Compressed(ExtractionStatus::Unextracted(Some(pe))),
        pages,
        temp_dir: Some(temp_dir),
    })
}

fn build_new_page(
    rel_path: PathBuf,
    name: String,
    index: usize,
    temp_dir: &Rc<TempDir>,
    jump_queue: &Rc<flume::Sender<String>>,
) -> (RefCell<Page>, oneshot::Sender<Result<(), String>>) {
    let ext = rel_path
        .extension()
        .expect("Path with supported extension has no extension")
        .to_string_lossy();
    let ext_path = temp_dir
        .path()
        .join(format!("{}.{}", index.to_string(), ext));

    let (s, r) = oneshot::channel();

    // Unwrap the inner result.
    let fut = r
        .map(|outer| match outer {
            Ok(inner) => inner,
            Err(e) => Err("Unexpected error extracting page: ".to_string() + &e.to_string()),
        })
        .boxed();

    let ext_fut = ExtractFuture {
        fut,
        jump_queue: Some(jump_queue.clone()),
    };

    (
        RefCell::new(Page::new_extracted(
            ext_path,
            rel_path,
            name,
            index,
            temp_dir.clone(),
            ext_fut,
        )),
        s,
    )
}

// Returns the unmodified version and the stripped version.
fn remove_common_path_prefix(pages: Vec<String>) -> Vec<(PathBuf, String)> {
    let mut prefix: Option<PathBuf> = pages.get(0).map_or_else(
        || None,
        |name| {
            PathBuf::from(name)
                .parent()
                .map_or_else(|| None, |p| Some(p.to_path_buf()))
        },
    );

    for p in &pages {
        while let Some(pfx) = &prefix {
            if Path::new(&p).starts_with(&pfx) {
                break;
            }

            prefix = pfx.parent().map(Path::to_owned);
        }

        if prefix.is_none() {
            break;
        }
    }

    let prefix: Option<String> = prefix.map(|p| p.to_string_lossy().to_string());

    pages
        .into_iter()
        .map(|name| {
            if let Some(prefix) = &prefix {
                (
                    name.clone().into(),
                    name.strip_prefix(prefix)
                        .expect("Not possible for prefix not to match")
                        .to_string(),
                )
            } else {
                (name.clone().into(), name)
            }
        })
        .map(|(path, name)| {
            (
                path,
                name.strip_prefix(is_separator).unwrap_or(&name).to_string(),
            )
        })
        .collect()
}

fn read_files_in_archive(path: &Path) -> std::result::Result<Vec<String>, (PathBuf, String)> {
    if let Some(ext) = path.extension() {
        let ext = ext.to_ascii_lowercase();

        if (ext == "rar" || ext == "cbr") && CONFIG.allow_external_extractors && *unrar::HAS_UNRAR {
            return unrar::read_files(path)
                .map(|vec| {
                    vec.into_iter()
                        .map(|(s, _)| s)
                        .filter(|name| is_supported_page_extension(&name))
                        .collect()
                })
                .map_err(|e| (path.to_owned(), e.to_string()));
        }
    }

    let source = match File::open(path) {
        Ok(src) => src,
        Err(e) => {
            let s = format!("Failed to open archive {:?}: {:?}", path, e);
            error!("{}", s);
            return Err((path.to_owned(), s));
        }
    };

    // Note -- So far, libarchive has at least been able to read the headers of all files, but
    // since it can't read the contents of all rar files there's a risk here.
    let files = match compress_tools::list_archive_files(source) {
        Ok(names) => names,
        Err(e) => {
            let s = format!("Failed to open archive {:?}: {:?}", path, e);
            error!("{}", s);
            return Err((path.to_owned(), s));
        }
    };

    Ok(files
        .into_iter()
        .filter(|name| is_supported_page_extension(&name))
        .collect())
}
