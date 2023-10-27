use std::cell::RefCell;
use std::path::{PathBuf, MAIN_SEPARATOR};
use std::rc::Rc;

use tempfile::TempDir;

use super::{remove_common_path_prefix, Archive};
use crate::manager::archive::page::Page;

pub(super) fn new_fileset(paths: Vec<PathBuf>, temp_dir: TempDir, id: u16) -> Archive {
    let temp_dir = Rc::from(temp_dir);

    // Try to find any common path-based prefix and remove them.
    let (pages, prefix) = remove_common_path_prefix(paths);

    let prefix = prefix.unwrap_or_else(|| MAIN_SEPARATOR.to_string().into());

    let archive_name = format!("files in {}", prefix.to_string_lossy());

    let pages: Vec<_> = pages
        .into_iter()
        .enumerate()
        .map(|(i, (abs_path, name))| {
            RefCell::new(Page::new_original(
                abs_path,
                // Since archive_name is prefix, name is the relative path except where the UTF-8
                // conversion is lossy. Since rel_path isn't used for anything too important,
                // especially in filesets, this is good enough until something actually breaks.
                PathBuf::from(&*name),
                name,
                i,
                temp_dir.clone(),
            ))
        })
        .collect();

    trace!("Finished constructing fileset with {} pages: {archive_name}", pages.len());

    Archive {
        name: archive_name.into(),
        path: prefix.into(),
        kind: super::Kind::FileSet,
        pages,
        temp_dir: Some(temp_dir),
        id,
        joined: false,
    }
}

// extend:
// If new files don't share a common prefix with the old prefix, we modify the prefix and extend
// the old file paths.
