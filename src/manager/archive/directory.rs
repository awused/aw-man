use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
use rayon::slice::ParallelSliceMut;
use tempfile::TempDir;

use super::page::Page;
use super::Archive;
use crate::manager::files::is_supported_page_extension;
use crate::natsort::NatKey;

pub(super) fn new_archive(
    path: PathBuf,
    temp_dir: TempDir,
    id: u16,
) -> Result<Archive, (PathBuf, String)> {
    let start = Instant::now();

    // Use a small temporary pool for sorting and converting. Making it too large increases
    // fragmentation for little benefit.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();

    trace!("Started reading directory {:?}", path);

    let files = fs::read_dir(&path);
    let files = match files {
        Ok(fs) => fs,
        Err(e) => {
            let s = format!("Failed to read files from directory {path:?}: {e:?}");
            error!("{s}");
            return Err((path, s));
        }
    };

    let temp_dir = Rc::from(temp_dir);

    let name = path.file_name().map_or_else(|| "".into(), |p| p.to_string_lossy().into());

    let pages: Vec<_> = pool.install(|| {
        let mut pages: Vec<(PathBuf, NatKey<OsString>)> = files
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

        pages
            .into_par_iter()
            .map(|(rel_path, name)| (path.join(&rel_path), rel_path, name.to_string_lossy().into()))
            .collect()
    });

    let pages = pages
        .into_iter()
        .enumerate()
        .map(|(i, (abs_path, rel_path, name))| {
            RefCell::new(Page::new_original(abs_path, rel_path, name, i, temp_dir.clone()))
        })
        .collect();

    drop(pool);
    trace!("Finished reading directory {path:?} {:?}", start.elapsed());

    Ok(Archive {
        name,
        // Kind of wasteful, but this is ultimately very rare
        path: path.into(),
        kind: super::Kind::Directory,
        pages,
        temp_dir: Some(temp_dir),
        id,
        joined: false,
    })
}
