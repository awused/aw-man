use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tempfile::TempDir;

use super::{remove_common_path_prefix, Archive};
use crate::manager::archive::page::Page;

pub(super) fn new_fileset(paths: Vec<PathBuf>, temp_dir: TempDir) -> Archive {
    let temp_dir = Rc::from(temp_dir);

    // Try to find any common path-based prefix and remove them.
    let (pages, prefix) = remove_common_path_prefix(paths);

    let archive_name =
        format!("files in {}", prefix.as_ref().map_or("/".into(), |p| p.to_string_lossy()));

    let pages: Vec<_> = pages
        .into_iter()
        .enumerate()
        .map(|(i, (abs_path, name))| {
            RefCell::new(Page::new_original(
                abs_path,
                name.clone().into(),
                name,
                i,
                temp_dir.clone(),
            ))
        })
        .collect();

    trace!("Finished constructing fileset with {} {}", pages.len(), archive_name,);

    Archive {
        name: archive_name,
        path: prefix.unwrap_or_default(),
        kind: super::Kind::FileSet,
        pages,
        temp_dir: Some(temp_dir),
    }
}
