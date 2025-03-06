use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::canonicalize;
use std::path::{Path, PathBuf, is_separator};
use std::rc::Rc;
use std::sync::Arc;
use std::{fmt, fs, future};

use ExtractionStatus::*;
use ahash::AHashMap;
use color_eyre::Result;
use derive_more::Debug;
use flume::Receiver;
use page::Page;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::oneshot;

use super::files::is_supported_page_extension;
use crate::com::{ContainingPath, Displayable, WorkParams};
use crate::manager::indices::PI;
use crate::natsort::NatKey;
use crate::pools::downscaling::Downscaler;
use crate::pools::extracting::{self, OngoingExtraction};

mod compressed;
mod directory;
mod fileset;
pub mod page;

// Only tracks what stage was completed, not whether it was successful or not.
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum Completion {
    StartScan,
    Scanned,
    // Right now we only care about when scanning is completed
    More,
}

// The booleans are the current upscaling state.
#[derive(Debug, Eq, PartialEq)]
pub enum Work<'a> {
    // Finish (Extracting, Scanning, Upscaling, Loading, Downscaling)
    Finalize(bool, WorkParams, &'a Downscaler),
    // Finish (Extracting, Scanning, Upscaling, Loading) + Start Downscaling
    Downscale(bool, WorkParams, &'a Downscaler),
    // Finish (Extracting, Scanning, Upscaling) + Start Loading
    Load(bool, WorkParams),
    // Finish (Extracting, Scanning) + Start Upscaling
    Upscale,
    // Finish Extracting + Start Scanning
    Scan,
}

impl Work<'_> {
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
            Self::Finalize(u, ..) | Self::Downscale(u, ..) | Self::Load(u, _) => *u,
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

    const fn downscaler(&self) -> Option<&Downscaler> {
        match self {
            Self::Finalize(.., d) | Self::Downscale(.., d) => Some(d),
            Self::Load(..) | Self::Upscale | Self::Scan => None,
        }
    }

    const fn params(&self) -> Option<WorkParams> {
        match self {
            Self::Finalize(_, lp, _) | Self::Downscale(_, lp, _) | Self::Load(_, lp) => Some(*lp),
            Self::Upscale | Self::Scan => None,
        }
    }

    fn extract_early(&self) -> bool {
        self.params().is_some_and(|lp| lp.extract_early)
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
    #[debug("comp")]
    Compressed(ExtractionStatus),
    #[debug("dir")]
    Directory,
    // Ordered collection of files, not specifically in the same directory
    #[debug("set")]
    FileSet,
    Broken(String),
}

pub struct Archive {
    name: Arc<str>,
    path: Arc<Path>,
    kind: Kind,
    pages: Vec<RefCell<Page>>,
    temp_dir: Option<Rc<TempDir>>,
    // Just enough to differentiate between multiple archives with the same name.
    // Nothing really breaks if IDs are repeated. At worst a user with over U16_MAX open archives
    // at once might see odd scrolling behaviour.
    id: u16,
    joined: bool,
}

fn new_broken(path: PathBuf, error: String, id: u16) -> Archive {
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("Broken"))
        .to_string_lossy()
        .into();
    Archive {
        name,
        path: path.into(),
        kind: Kind::Broken(error),
        pages: Vec::default(),
        temp_dir: None,
        id,
        joined: false,
    }
}

// An archive is any collection of pages, even if it's just a directory.
impl Archive {
    // TODO -- clean this up with a closure and ?
    pub(super) fn open(path: &Path, temp_dir: &TempDir, id: u16) -> (Self, Option<usize>) {
        match Self::try_open(path, temp_dir, id) {
            Ok(out) => out,
            Err(e) => {
                let path = canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                (new_broken(path, e.to_string(), id), None)
            }
        }
    }

    #[instrument(level = "error", skip(temp_dir, id), err(Debug))]
    fn try_open(path: &Path, temp_dir: &TempDir, id: u16) -> Result<(Self, Option<usize>)> {
        // Convert relative paths to absolute.
        let path = canonicalize(path)?;

        // Check if it's a directory, file, or missing
        let meta = fs::metadata(&path)?;

        // Each archive gets its own temporary directory which can be cleaned up independently.
        let temp_dir = tempfile::Builder::new().prefix("archive").tempdir_in(temp_dir)?;

        let a = if meta.is_dir() {
            directory::new_archive(path, temp_dir, id)?
        } else if is_supported_page_extension(&path) && path.parent().is_some() {
            let parent = path.parent().unwrap().to_path_buf();

            let a = directory::new_archive(parent, temp_dir, id)?;

            let child = path.file_name().unwrap();
            let child = NatKey::from(child);

            let r = a.pages.binary_search_by(|page| {
                NatKey::from(page.borrow().get_rel_path()).partial_cmp(&child).unwrap()
            });

            if let Ok(i) = r {
                return Ok((a, Some(i)));
            }
            error!("Could not find initial file in {:?}", path.parent().unwrap());
            a
        } else {
            compressed::new_archive(path, temp_dir, id)?
        };

        // Only really meaningful on initial load.
        let p = if !a.pages.is_empty() { Some(0) } else { None };

        Ok((a, p))
    }

    pub(super) fn open_fileset(
        paths: &[PathBuf],
        temp_dir: &TempDir,
        id: u16,
    ) -> (Self, Option<usize>) {
        match Self::try_open_fileset(paths, temp_dir, id) {
            Ok(out) => out,
            Err(e) => (new_broken(PathBuf::new(), e.to_string(), id), None),
        }
    }

    #[instrument(level = "error", skip_all, fields(len = paths.len()), err(Debug))]
    pub(super) fn try_open_fileset(
        paths: &[PathBuf],
        temp_dir: &TempDir,
        id: u16,
    ) -> Result<(Self, Option<usize>)> {
        if paths.is_empty() {
            let temp_dir = tempfile::Builder::new().prefix("archive").tempdir_in(temp_dir)?;
            return Ok((fileset::new_fileset(Vec::new(), temp_dir, id), None));
        }

        let meta = fs::metadata(&paths[0])?;
        if meta.is_dir() || !is_supported_page_extension(&paths[0]) {
            if paths.len() > 1 {
                warn!(
                    "Opening multiple archives is unsupported, opening {:?} as an archive",
                    paths[0]
                );
            }
            return Self::try_open(&paths[0], temp_dir, id);
        }

        let paths: Vec<Arc<Path>> = paths
            .iter()
            .filter_map(|p| canonicalize(p).map(Into::into).ok())
            .filter(|p: &Arc<Path>| is_supported_page_extension(p))
            .collect();

        // Each archive gets its own temporary directory which can be cleaned up independently.
        let temp_dir = tempfile::Builder::new().prefix("archive").tempdir_in(temp_dir)?;

        let files = fileset::new_fileset(paths, temp_dir, id);
        // page_count can be 0 even if paths isn't empty
        let p = if files.page_count() > 0 { Some(0) } else { None };
        Ok((files, p))
    }

    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn name(&self) -> Arc<str> {
        self.name.clone()
    }

    pub(super) const fn id(&self) -> u16 {
        self.id
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

    // The path that contains the archive or all files in the archive.
    pub(super) fn containing_path(&self) -> ContainingPath {
        let p = self.path.clone();
        match self.kind {
            Kind::Compressed(_) => ContainingPath::AssertParent(p),
            Kind::Directory | Kind::Broken(_) => ContainingPath::TryParent(p),
            Kind::FileSet => ContainingPath::Current(p),
        }
    }

    pub(super) fn get_displayable(&self, p: Option<PI>, upscaling: bool) -> Displayable {
        if let Kind::Broken(e) = &self.kind {
            return Displayable::Error(e.clone());
        }

        if let Some(p) = p {
            self.get_page(p).borrow().get_displayable(upscaling)
        } else if matches!(self.kind, Kind::FileSet) {
            let e = format!("Empty fileset in {}", self.path().to_string_lossy());
            Displayable::Error(e)
        } else {
            let e = format!("Found nothing to display in {}", self.name());
            Displayable::Error(e)
        }
    }

    pub(super) fn get_page_name_and_path(&self, p: Option<PI>) -> Option<(Arc<str>, Arc<Path>)> {
        if let Kind::Broken(_) = &self.kind {
            return None;
        }

        p.map(|p| {
            let p = self.get_page(p).borrow();
            (p.name.clone(), p.get_absolute_file_path().clone())
        })
    }

    pub(super) fn start_extraction(&mut self) {
        if let Kind::Compressed(Unextracted(jobs)) = &mut self.kind {
            let jobs = std::mem::take(jobs).expect("Impossible to double extract");
            self.kind = Kind::Compressed(Extracting(extracting::extract(self.path.clone(), jobs)));
        }
    }

    pub(super) fn has_work(&self, p: PI, work: &Work) -> bool {
        match self.kind {
            Kind::Compressed(Unextracted(_) | Extracting(_)) | Kind::Directory | Kind::FileSet => {}
            Kind::Broken(_) => return false,
        };

        self.get_page(p).borrow().has_work(work)
    }

    #[instrument(level = "debug", skip_all, fields(a = ?self, p = p.0), name = "")]
    pub(super) async fn do_work(&self, p: PI, work: Work<'_>) -> Completion {
        if matches!(self.kind, Kind::Compressed(Unextracted(_))) {
            // Calling start_extracting() out of band with the "work" chain means we can
            // simplify the code and not need to worry about borrowing the same archive mutably
            // more than once when dealing with multiple pages.
            panic!("Called has_work on {self:?} which hasn't started extracting");
        }

        if let Ok(mut page) = self.get_page(p).try_borrow_mut() {
            page.do_work(work).await
        } else {
            // One of the other promise chains beat us to this page. It will return to the main
            // loop when something happens for us.
            future::pending().await
        }
    }

    #[instrument(level = "trace")]
    pub(super) fn unload(&self, p: PI) {
        self.get_page(p).borrow_mut().unload()
    }

    fn get_page(&self, p: PI) -> &RefCell<Page> {
        self.pages
            .get(p.0)
            .unwrap_or_else(|| panic!("Tried to get non-existent page {p:?} in archive {self:?}"))
    }

    #[instrument(level = "error")]
    pub(super) async fn join(mut self) {
        debug!("Joined archive");
        self.joined = true;

        match &mut self.kind {
            Kind::Compressed(Extracting(fut)) => {
                fut.cancel().await;
            }
            Kind::Compressed(Unextracted(_))
            | Kind::Directory
            | Kind::FileSet
            | Kind::Broken(_) => (),
        }

        for p in self.pages.drain(..) {
            p.into_inner().join().await;
        }

        if let Some(td) = self.temp_dir.take() {
            match Rc::try_unwrap(td) {
                Ok(td) => {
                    td.close().unwrap_or_else(|e| error!("Error deleting temp dir: {e:?}"));
                }
                Err(_) => {
                    error!("Archive temp dir leaked reference counts.")
                }
            }
        }

        trace!("Cleaned up");
    }

    pub(super) fn get_env(&self, p: Option<PI>) -> Vec<(String, OsString)> {
        let mut env = p.map_or_else(Vec::new, |p| self.get_page(p).borrow().get_env());

        env.push(("AWMAN_ARCHIVE".into(), self.path.to_path_buf().into()));

        let k = match self.kind {
            Kind::Compressed(_) => "archive",
            Kind::Directory => "directory",
            Kind::FileSet => "fileset",
            Kind::Broken(_) => "unknown",
        };
        env.push(("AWMAN_ARCHIVE_TYPE".into(), k.into()));
        env.push(("AWMAN_PAGE_COUNT".into(), self.page_count().to_string().into()));

        env
    }

    pub(super) fn list_pages(&self) -> Vec<Value> {
        self.pages.iter().map(|p| p.borrow().page_info()).collect()
    }
}

impl fmt::Debug for Archive {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {:?} {}", self.name, self.kind, self.pages.len())
    }
}

// While archives should still clean themselves up properly on drop without joining, this isn't
// good.
impl Drop for Archive {
    fn drop(&mut self) {
        if !self.joined {
            error!("Dropped unjoined archive for {self:?}");
        }
    }
}

// Returns the unmodified version and the stripped version of each name and the prefix, if any.
fn remove_common_path_prefix<T: AsRef<Path>>(
    pages: Vec<T>,
) -> (Vec<(T, Arc<str>)>, Option<PathBuf>) {
    let mut prefix: Option<PathBuf> = pages.first().map_or_else(
        || None,
        |name| {
            PathBuf::from(name.as_ref())
                .parent()
                .map_or_else(|| None, |p| Some(p.to_path_buf()))
        },
    );

    for p in &pages {
        while let Some(pfx) = &prefix {
            if p.as_ref().starts_with(pfx) {
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
                let name = if let Some(prefix) = &prefix {
                    path.as_ref().strip_prefix(prefix).unwrap().to_string_lossy()
                } else {
                    path.as_ref().to_string_lossy()
                };

                let name = name.strip_prefix(is_separator).unwrap_or(&name).into();
                // Arc<str> shouldn't really be slower than the old String code, since it was
                // always needing to allocate
                (path, name)
            })
            .collect(),
        prefix,
    )
}
