use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use rayon::iter::{ParallelBridge, ParallelIterator};
use rayon::slice::ParallelSliceMut;
use tempfile::TempDir;

use super::page::Page;
use super::Archive;
use crate::manager::files::is_supported_page_extension;
use crate::natsort::ParsedString;

pub(super) fn new_archive(path: PathBuf, temp_dir: TempDir) -> Result<Archive, (PathBuf, String)> {
    // TODO -- maybe support recursion, but it will naturally be slower.
    // Probably save time by only statting files without an extension
    let start = Instant::now();
    trace!("Started reading directory {:?}", path);

    let files = fs::read_dir(&path);
    let files = match files {
        Ok(fs) => fs,
        Err(e) => {
            let s = format!("Failed to read files from directory {:?}: {:?}", path, e);
            error!("{}", s);
            return Err((path, s));
        }
    };

    let temp_dir = Rc::from(temp_dir);

    let name = path
        .file_name()
        .map_or_else(|| "".to_string(), |p| p.to_string_lossy().to_string());

    let mut pages: Vec<(PathBuf, ParsedString)> = files
        .par_bridge()
        .filter_map(|rd| {
            let de = rd.ok()?;

            let filepath = de.path();
            let filepath = filepath.strip_prefix(&path).ok()?;

            // Especially in a large directory we don't want to waste time sniffing mime types.
            if is_supported_page_extension(filepath) {
                Some((filepath.to_owned(), de.file_name().into()))
            } else {
                None
            }
        })
        .collect();

    pages.par_sort_by(|(_, a), (_, b)| a.cmp(b));

    let pages = pages
        .into_iter()
        .enumerate()
        .map(|(i, (rel_path, name))| {
            let s = RefCell::new(Page::new_original(
                path.join(&rel_path),
                rel_path,
                name.into_original().to_string_lossy().to_string(),
                i,
                temp_dir.clone(),
            ));
            s
        })
        .collect();

    trace!("Finished reading directory {:?} {:?}", path, start.elapsed());

    Ok(Archive {
        name,
        path,
        kind: super::Kind::Directory,
        pages,
        temp_dir: Some(temp_dir),
    })
}
