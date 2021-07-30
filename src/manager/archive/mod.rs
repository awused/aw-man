use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::canonicalize;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{fmt, fs, future};

use flume::{Receiver, Sender};
use page::Page;
use tempdir::TempDir;
use tokio::sync::oneshot;
use ExtractionStatus::*;

use super::files::is_supported_page_extension;
use crate::com::{Displayable, LoadingParams};
use crate::manager::indices::PI;
use crate::natsort;
use crate::pools::extracting::{self, OngoingExtraction};

mod compressed;
mod directory;
pub mod page;

// The booleans are the current upscaling state.
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum Work {
    // Finish (Extracting, Scanning, Upscaling, Loading)
    Finalize(bool, LoadingParams),
    // Finish (Extracting + Scanning, Converting | Upscaling)?) + Start Loading
    Load(bool, LoadingParams),
    // Finish (Extracting + Scanning) + Start Upscaling
    Upscale,
    // Finish Extracting + Start Scanning
    Scan,
}

impl Work {
    const fn finalize(&self) -> bool {
        match self {
            Work::Finalize(..) => true,
            Work::Load(..) | Work::Upscale | Work::Scan => false,
        }
    }

    const fn load(&self) -> bool {
        match self {
            Work::Finalize(..) | Work::Load(..) => true,
            Work::Upscale | Work::Scan => false,
        }
    }

    const fn upscale(&self) -> bool {
        match self {
            Work::Finalize(u, _) | Work::Load(u, _) => *u,
            Work::Upscale => true,
            Work::Scan => false,
        }
    }

    const fn params(&self) -> Option<LoadingParams> {
        match self {
            Work::Finalize(_, lp) | Work::Load(_, lp) => Some(*lp),
            Work::Upscale | Work::Scan => None,
        }
    }

    fn extract_early(&self) -> bool {
        self.params().map_or(false, |lp| lp.extract_early)
    }
}

pub struct PageExtraction {
    pub ext_path: PathBuf,
    pub completion: oneshot::Sender<Result<(), String>>,
}

pub struct PendingExtraction {
    pub ext_map: HashMap<String, PageExtraction>,
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
    // Zip,
    // Rar,
    // SevenZip,
    Directory,
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
    Archive {
        name: "Broken".to_string(),
        path,
        kind: Kind::Broken(error),
        pages: Default::default(),
        temp_dir: None,
    }
}

// An archive is any collection of pages, even if it's just a directory.
impl Archive {
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
        let temp_dir = match TempDir::new_in(temp_dir, "archive") {
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

            let child = path.file_name().unwrap().to_string_lossy().to_string();

            let r = a.pages.binary_search_by_key(&natsort::key(&child), |page| {
                natsort::key(&page.borrow().name)
            });

            if let Ok(i) = r {
                return (a, Some(i));
            }
            error!(
                "Could not find file {:?} in directory {:?}",
                child,
                path.parent().unwrap()
            );
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

    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn name(&self) -> String {
        self.name.to_string()
    }

    pub(super) const fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Directory)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn get_displayable(&self, p: Option<PI>, upscaling: bool) -> (Displayable, String) {
        if let Kind::Broken(e) = &self.kind {
            return (Displayable::Error(e.clone()), "".to_string());
        }

        if p.is_none() {
            let e = format!("Found nothing to display in {:?}", self);
            return (Displayable::Error(e), "".to_string());
        }

        self.get_page(p.unwrap())
            .borrow()
            .get_displayable(upscaling)
    }

    pub(super) fn start_extraction(&mut self, _p: Option<PI>) {
        if let Kind::Compressed(Unextracted(jobs)) = &mut self.kind {
            let jobs = std::mem::take(jobs).expect("Impossible to double extract");
            self.kind = Kind::Compressed(Extracting(extracting::extract(self.path.clone(), jobs)));
        }
    }

    pub(super) fn has_work(&self, p: PI, work: Work) -> bool {
        match self.kind {
            Kind::Compressed(Unextracted(_))
            | Kind::Compressed(Extracting(_))
            | Kind::Directory => (),
            Kind::Broken(_) => return false,
        };

        self.get_page(p).borrow().has_work(work)
    }

    pub(super) async fn do_work(&self, p: PI, work: Work) {
        if matches!(self.kind, Kind::Compressed(Unextracted(_))) {
            // Calling start_extracting() out of band with the "work" chain means we can
            // simplify the code and not need to worry about borrowing the same archive mutably
            // more than once when dealing with multiple pages.
            panic!(
                "Called has_work on {:?} which hasn't started extracting",
                self
            );
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
            panic!(
                "Tried to get non-existent page {:?} in archive {:?}",
                p, self
            )
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
            Kind::Directory | Kind::Broken(_) => (),
        }

        for p in self.pages.drain(..) {
            p.into_inner().join().await;
        }

        if let Some(td) = &self.temp_dir {
            if Rc::strong_count(td) != 1 {
                error!(
                    "Archive temp dir for {:?} leaked reference counts.",
                    self.path
                )
            }
        }

        trace!("Cleaned up archive");
    }

    pub(super) fn get_env(&self, p: Option<PI>) -> Vec<(String, OsString)> {
        let mut env;
        if let Some(p) = p {
            env = self.get_page(p).borrow().get_env();
        } else {
            env = Vec::new();
        }

        env.push(("AWMAN_ARCHIVE".into(), self.path.clone().into()));

        let k = match self.kind {
            Kind::Compressed(_) => "archive",
            Kind::Directory => "directory",
            Kind::Broken(_) => "unknown",
        };
        env.push(("AWMAN_ARCHIVE_TYPE".into(), k.into()));

        env
    }
}

impl fmt::Debug for Archive {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[a:{} {:?} {}]", self.name, self.kind, self.pages.len())
    }
}
