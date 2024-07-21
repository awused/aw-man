use std::cell::RefCell;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use ahash::AHashMap;
use futures_util::FutureExt;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::Archive;
use crate::config::CONFIG;
use crate::manager::archive::page::{ExtractFuture, Page};
use crate::manager::archive::{
    remove_common_path_prefix, ExtractionStatus, PageExtraction, PendingExtraction,
};
use crate::manager::files::is_supported_page_extension;
use crate::natsort::NatKey;
use crate::unrar;

#[instrument(level = "error", skip_all)]
pub(super) fn new_archive(
    path: PathBuf,
    temp_dir: TempDir,
    id: u16,
) -> Result<Archive, (PathBuf, String)> {
    trace!("Started reading compressed archive {path:?}");
    let temp_dir = Rc::from(temp_dir);
    let start = Instant::now();

    // Jumping ahead of the queue can slow everything down, so only do it for the current image.
    // If the queue is already full then we just wait for normal extraction.
    let (jump_sender, jump_receiver) = flume::bounded(1);
    let jump_sender = Rc::from(jump_sender);


    let pages = read_files_in_archive(&path)?;

    // Try to find any common path-based prefix and remove them.
    let (pages, _) = remove_common_path_prefix(pages);

    // Sort by natural order
    let mut pages: Vec<_> =
        pages.into_iter().map(|(path, name)| (path, NatKey::from_str(name))).collect();
    pages.sort_by(|(_, a), (_, b)| a.cmp(b));

    let mut ext_map = AHashMap::new();

    let pages: Vec<_> = pages
        .into_iter()
        .enumerate()
        .map(|(index, (rel_path, name))| {
            let (page, completion) = build_new_page(
                rel_path.clone(),
                name.into_original(),
                index,
                &temp_dir,
                &jump_sender,
            );

            let ext_path = page.borrow().get_absolute_file_path().to_path_buf();

            ext_map.insert(
                rel_path.to_string_lossy().to_string(),
                PageExtraction { ext_path, completion },
            );
            page
        })
        .collect();

    trace!("Finished scanning archive {path:?} {:?}", start.elapsed());

    let archive_name = path.file_name().map_or_else(|| "".into(), |p| p.to_string_lossy().into());

    let pe = PendingExtraction { ext_map, jump_receiver };

    Ok(Archive {
        name: archive_name,
        // Wasteful but rare
        path: path.into(),
        kind: super::Kind::Compressed(ExtractionStatus::Unextracted(Some(pe))),
        pages,
        temp_dir: Some(temp_dir),
        id,
        joined: false,
    })
}

fn build_new_page(
    rel_path: PathBuf,
    name: Arc<str>,
    index: usize,
    temp_dir: &Rc<TempDir>,
    jump_queue: &Rc<flume::Sender<String>>,
) -> (RefCell<Page>, oneshot::Sender<Result<(), String>>) {
    let ext = rel_path
        .extension()
        .expect("Path with supported extension has no extension")
        .to_string_lossy();
    let ext_path = temp_dir.path().join(format!("{index}.{ext}"));

    let (s, r) = oneshot::channel();

    // Unwrap the inner result.
    let fut = r
        .map(|outer| match outer {
            Ok(inner) => inner,
            Err(e) => Err(format!("Unexpected error extracting page: {e}")),
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


fn decode(input: &[u8]) -> compress_tools::Result<String> {
    Ok(String::from_utf8_lossy(input).to_string())
}


fn read_files_in_archive(path: &Path) -> std::result::Result<Vec<PathBuf>, (PathBuf, String)> {
    if let Some(ext) = path.extension() {
        let ext = ext.to_ascii_lowercase();

        if (ext == "rar" || ext == "cbr") && CONFIG.allow_external_extractors && *unrar::HAS_UNRAR {
            return unrar::read_files(path)
                .map(|vec| {
                    vec.into_iter()
                        .map(|(s, _)| s)
                        .filter(|name| is_supported_page_extension(name))
                        .map(Into::into)
                        .collect()
                })
                .map_err(|e| (path.to_owned(), e.to_string()));
        }
    }

    let source = match File::open(path) {
        Ok(src) => src,
        Err(e) => {
            let s = format!("Failed to open archive {path:?}: {e:?}");
            error!("{s}");
            return Err((path.to_owned(), s));
        }
    };

    // Note -- So far, libarchive has at least been able to read the headers of all files, but
    // since it can't read the contents of all rar files there's a risk here.
    let files = match compress_tools::list_archive_files_with_encoding(source, decode) {
        // let files = match compress_tools::list_archive_files(source) {
        Ok(names) => names,
        Err(e) => {
            let s = format!("Failed to open archive {path:?}: {e:?}");
            error!("{s}");
            return Err((path.to_owned(), s));
        }
    };

    Ok(files
        .into_iter()
        .filter(|name| is_supported_page_extension(name))
        .map(Into::into)
        .collect())
}
