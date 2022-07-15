use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::canonicalize;
use std::path::{is_separator, Path, PathBuf};
use std::rc::Rc;
use std::{fmt, fs, future};

use ahash::{AHashMap, AHashSet};
use flume::{Receiver, Sender};
use page::Page;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::oneshot;
use ExtractionStatus::*;

use super::files::is_supported_page_extension;
use crate::com::{Displayable, WorkParams};
use crate::manager::indices::PI;
use crate::natsort;
use crate::pools::extracting::{self, OngoingExtraction};

mod compressed;
mod directory;
mod fileset;
pub mod page;

// The booleans are the current upscaling state.
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum Work {
    // Finish (Extracting, Scanning, Upscaling, Loading, Downscaling)
    Finalize(bool, WorkParams),
    // Finish (Extracting, Scanning, Upscaling, Loading) + Start Downscaling
    Downscale(bool, WorkParams),
    // Finish (Extracting, Scanning, Upscaling) + Start Loading
    Load(bool, WorkParams),
    // Finish (Extracting, Scanning) + Start Upscaling
    Upscale,
    // Finish Extracting + Start Scanning
    Scan,
}

impl Work {
    const fn finalize(&self) -> bool {
        match self {
            Self::Finalize(..) => true,
            Self::Downscale(..) | Self::Load(..) | Self::Upscale | Self::Scan => false,
        }
    }

    const fn load(&self) -> bool {
        match self {
            Self::Finalize(..) | Self::Downscale(..) | Self::Load(..) => true,
            Self::Upscale | Self::Scan => false,
        }
    }

    const fn upscale(&self) -> bool {
        match self {
            Self::Finalize(u, _) | Self::Downscale(u, _) | Self::Load(u, _) => *u,
            Self::Upscale => true,
            Self::Scan => false,
        }
    }

    const fn downscale(&self) -> bool {
        match self {
            Self::Finalize(..) | Self::Downscale(..) => true,
            Self::Load(..) | Self::Upscale | Self::Scan => false,
        }
    }

    const fn params(&self) -> Option<WorkParams> {
        match self {
            Self::Finalize(_, lp) | Self::Downscale(_, lp) | Self::Load(_, lp) => Some(*lp),
            Self::Upscale | Self::Scan => None,
        }
    }

    fn extract_early(&self) -> bool {
        self.params().map_or(false, |lp| lp.extract_early)
    }

    const fn load_during_scan(&self) -> bool {
        !self.upscale()
    }
}

pub struct PageExtraction {
    pub ext_path: PathBuf,
    pub completion: oneshot::Sender<Result<(), String>>,
}

pub struct PendingExtraction {
    pub ext_map: AHashMap<String, PageExtraction>,
    // Used to jump ahead in the queue and extract a single file, perhaps several, early.
    pub jump_receiver: Receiver<String>,
    pub jump_sender: Sender<String>,
}

enum ExtractionStatus {
    // This helps in the case where many archives are opened and scanned by a long jump.
    Unextracted(Option<PendingExtraction>),
    // We never need to move an archive out of the extracting state.
    Extracting(OngoingExtraction),
}

impl fmt::Debug for ExtractionStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Unextracted(_) => write!(f, "Unextracted"),
            Extracting(_) => write!(f, "Extracting"),
        }
    }
}

#[derive(Debug)]
enum Kind {
    Compressed(ExtractionStatus),
    Directory,
    // Ordered collection of files, not specifically in the same directory
    FileSet,
    Broken(String),
}

pub struct Archive {
    name: String,
    path: PathBuf,
    kind: Kind,
    pages: Vec<RefCell<Page>>,
    temp_dir: Option<Rc<TempDir>>,
}

fn new_broken(path: PathBuf, error: String) -> Archive {
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("Broken"))
        .to_string_lossy()
        .to_string();
    Archive {
        name,
        path,
        kind: Kind::Broken(error),
        pages: Vec::default(),
        temp_dir: None,
    }
}

// An archive is any collection of pages, even if it's just a directory.
impl Archive {
    // TODO -- clean this up with a closure and ?
    pub(super) fn open(path: PathBuf, temp_dir: &TempDir) -> (Self, Option<usize>) {
        // Convert relative paths to absolute.
        let path = match canonicalize(&path) {
            Ok(path) => path,
            Err(e) => {
                let s = format!("Error getting absolute path for {:?}: {:?}", path, e);
                error!("{}", s);
                return (new_broken(path, s), None);
            }
        };

        // Check if it's a directory, file, or missing
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                let s = format!("Could not stat file {:?}: {:?}", path, e);
                error!("{}", s);
                return (new_broken(path, s), None);
            }
        };

        // Each archive gets its own temporary directory which can be cleaned up independently.
        let temp_dir = match tempfile::Builder::new().prefix("archive").tempdir_in(temp_dir) {
            Ok(tmp) => tmp,
            Err(e) => {
                let s = format!("Error creating temp_dir for {:?}: {:?}", path, e);
                error!("{}", s);
                return (new_broken(path, s), None);
            }
        };

        let a = if meta.is_dir() {
            match directory::new_archive(path, temp_dir) {
                Ok(a) => a,
                Err((p, s)) => return (new_broken(p, s), None),
            }
        } else if is_supported_page_extension(&path) && path.parent().is_some() {
            let parent = path.parent().unwrap().to_path_buf();

            let a = match directory::new_archive(parent, temp_dir) {
                Ok(a) => a,
                Err((p, s)) => return (new_broken(p, s), None),
            };

            let child = path.file_name().unwrap();

            let r = a.pages.binary_search_by_key(&natsort::key(child), |page| {
                natsort::key(page.borrow().get_rel_path().as_os_str())
            });

            if let Ok(i) = r {
                return (a, Some(i));
            }
            error!("Could not find file {:?} in directory {:?}", child, path.parent().unwrap());
            a
        } else {
            match compressed::new_archive(path, temp_dir) {
                Ok(a) => a,
                Err((p, s)) => return (new_broken(p, s), None),
            }
        };

        // Only really meaningful on initial load.
        let p = if !a.pages.is_empty() { Some(0) } else { None };

        (a, p)
    }

    pub(super) fn open_fileset(paths: &[PathBuf], temp_dir: &TempDir) -> (Self, Option<usize>) {
        // If it's a directory or archive we switch to the normal mechanism.
        // TODO -- support opening a set of directories or archives. But probably never mixing
        // regular files and archives.
        match fs::metadata(&paths[0]) {
            Ok(m) => {
                if m.is_dir() || !is_supported_page_extension(&paths[0]) {
                    return Self::open(paths[0].clone(), temp_dir);
                }
            }
            Err(e) => {
                let s = format!("Could not stat file {:?}: {:?}", paths[0], e);
                error!("{}", s);
                return (new_broken(paths[0].clone(), s), None);
            }
        };

        // TODO -- consider supporting the same image multiple times.
        let mut dedupe = AHashSet::new();

        let paths: Vec<_> = match paths
            .iter()
            .map(canonicalize)
            .filter(|p| match p {
                Ok(p) => is_supported_page_extension(p) && dedupe.insert(p.clone()),
                Err(_) => false,
            })
            .collect()
        {
            Ok(v) => v,
            Err(e) => {
                let s = format!("Error getting absolute path: {:?}", e);
                error!("{}", s);
                return (new_broken(paths[0].clone(), s), None);
            }
        };

        drop(dedupe);

        // Each archive gets its own temporary directory which can be cleaned up independently.
        let temp_dir = match tempfile::Builder::new().prefix("archive").tempdir_in(temp_dir) {
            Ok(tmp) => tmp,
            Err(e) => {
                let s = format!("Error creating temp_dir for fileset: {:?}", e);
                error!("{}", s);
                return (new_broken(paths[0].clone(), s), None);
            }
        };

        (fileset::new_fileset(paths, temp_dir), Some(0))
    }

    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn name(&self) -> String {
        self.name.to_string()
    }

    pub(super) const fn allow_multiple_archives(&self) -> bool {
        // TODO -- consider making this configurable for Directory archives
        match self.kind {
            Kind::Compressed(_) | Kind::Broken(_) => true,
            Kind::Directory | Kind::FileSet => false,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn get_displayable(&self, p: Option<PI>, upscaling: bool) -> (Displayable, String) {
        if let Kind::Broken(e) = &self.kind {
            return (Displayable::Error(e.clone()), "".to_string());
        }

        if let Some(p) = p {
            self.get_page(p).borrow().get_displayable(upscaling)
        } else {
            let e = format!("Found nothing to display in {:?}", self);
            (Displayable::Error(e), "".to_string())
        }
    }

    pub(super) fn start_extraction(&mut self) {
        if let Kind::Compressed(Unextracted(jobs)) = &mut self.kind {
            let jobs = std::mem::take(jobs).expect("Impossible to double extract");
            self.kind = Kind::Compressed(Extracting(extracting::extract(self.path.clone(), jobs)));
        }
    }

    pub(super) fn has_work(&self, p: PI, work: Work) -> bool {
        match self.kind {
            Kind::Compressed(Unextracted(_) | Extracting(_)) | Kind::Directory | Kind::FileSet => {}
            Kind::Broken(_) => return false,
        };

        self.get_page(p).borrow().has_work(work)
    }

    pub(super) async fn do_work(&self, p: PI, work: Work) {
        if matches!(self.kind, Kind::Compressed(Unextracted(_))) {
            // Calling start_extracting() out of band with the "work" chain means we can
            // simplify the code and not need to worry about borrowing the same archive mutably
            // more than once when dealing with multiple pages.
            panic!("Called has_work on {:?} which hasn't started extracting", self);
        }

        if let Ok(mut page) = self.get_page(p).try_borrow_mut() {
            page.do_work(work).await
        } else {
            // One of the other promise chains beat us to this page. It will return to the main
            // loop when something happens for us.
            future::pending().await
        }
    }

    pub(super) fn unload(&self, p: PI) {
        self.get_page(p).borrow_mut().unload()
    }

    fn get_page(&self, p: PI) -> &RefCell<Page> {
        self.pages.get(p.0).unwrap_or_else(|| {
            panic!("Tried to get non-existent page {:?} in archive {:?}", p, self)
        })
    }

    pub(super) async fn join(mut self) {
        trace!("Joined {:?}", self);

        match self.kind {
            Kind::Compressed(Unextracted(pend)) => drop(pend),
            Kind::Compressed(Extracting(mut fut)) => {
                fut.cancel().await;
                drop(fut);
            }
            Kind::Directory | Kind::FileSet | Kind::Broken(_) => (),
        }

        for p in self.pages {
            p.into_inner().join().await;
        }

        if let Some(td) = self.temp_dir.take() {
            match Rc::try_unwrap(td) {
                Ok(td) => {
                    td.close().unwrap_or_else(|e| {
                        error!("Error deleting temp dir for {:?}: {:?}", self.path, e)
                    });
                }
                Err(_) => {
                    error!("Archive temp dir for {:?} leaked reference counts.", self.path)
                }
            }
        }

        trace!("Cleaned up archive {:?}", self.path);
    }

    pub(super) fn get_env(&self, p: Option<PI>) -> Vec<(String, OsString)> {
        let mut env =
            if let Some(p) = p { self.get_page(p).borrow().get_env() } else { Vec::new() };

        env.push(("AWMAN_ARCHIVE".into(), self.path.clone().into()));

        let k = match self.kind {
            Kind::Compressed(_) => "archive",
            Kind::Directory => "directory",
            Kind::FileSet => "fileset",
            Kind::Broken(_) => "unknown",
        };
        env.push(("AWMAN_ARCHIVE_TYPE".into(), k.into()));

        env
    }

    pub(super) fn list_pages(&self) -> Vec<Value> {
        self.pages.iter().map(|p| p.borrow().page_info()).collect()
    }
}

impl fmt::Debug for Archive {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[a:{} {:?} {}]", self.name, self.kind, self.pages.len())
    }
}

// Returns the unmodified version and the stripped version of each name and the prefix, if any.
fn remove_common_path_prefix(pages: Vec<PathBuf>) -> (Vec<(PathBuf, String)>, Option<PathBuf>) {
    let mut prefix: Option<PathBuf> = pages.get(0).map_or_else(
        || None,
        |name| PathBuf::from(name).parent().map_or_else(|| None, |p| Some(p.to_path_buf())),
    );

    for p in &pages {
        while let Some(pfx) = &prefix {
            if p.starts_with(&pfx) {
                break;
            }

            prefix = pfx.parent().map(Path::to_owned);
        }

        if prefix.is_none() {
            break;
        }
    }

    (
        pages
            .into_iter()
            .map(|path| {
                if let Some(prefix) = &prefix {
                    let name = path
                        .strip_prefix(prefix)
                        .expect("Not possible for prefix not to match")
                        .to_string_lossy()
                        .to_string();
                    (path, name)
                } else {
                    let name = path.to_string_lossy().to_string();
                    (path, name)
                }
            })
            .map(|(path, name)| {
                let name = match name.strip_prefix(is_separator) {
                    Some(n) => n.to_string(),
                    None => name,
                };
                (path, name)
            })
            .collect(),
        prefix,
    )
}
