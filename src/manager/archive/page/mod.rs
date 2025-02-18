use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use State::*;
use derive_more::Debug;
use futures_util::{FutureExt, poll};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::fs::remove_file;

use self::scanned::ScannedPage;
use super::{Completion, Work};
use crate::Fut;
use crate::com::Displayable;
use crate::pools::loading::{self, ScanFuture};

mod animation;
mod regular_image;
mod scanned;
mod upscaled_image;
mod video;

pub struct ExtractFuture {
    pub fut: Fut<Result<(), String>>,
    pub jump_queue: Option<Rc<flume::Sender<String>>>,
}

// A Page represents a single "page" in the archive, even if that page is animated or a video.
#[derive(Debug)]
enum State {
    #[debug("Extracting")]
    Extracting(ExtractFuture),
    // This is really only used as the initial state for an original image.
    Unscanned,
    // This will also convert and load but not resize the image, if applicable.
    #[debug("Scanning")]
    Scanning(ScanFuture),
    // It has been converted, if necessary, and we know what it is.
    // This is >200 bytes, and will only be used for visited files.
    // Using a box can save a decent chunk of memory at negligible cost.
    #[debug("Scanned")]
    Scanned(Box<ScannedPage>),
    Failed(String),
}


#[derive(fmt::Debug)]
enum Origin {
    // Contains the absolute path of the extracted file.
    Extracted(Arc<Path>),
    // Contains the absolute path of the file.
    Original(Arc<Path>),
}

pub(super) struct Page {
    pub(super) name: Arc<str>,
    origin: Origin,
    rel_path: PathBuf,
    state: State,
    index: usize,
    temp_dir: Rc<TempDir>,
}

impl Page {
    pub const fn new_original(
        abs_path: Arc<Path>,
        rel_path: PathBuf,
        name: Arc<str>,
        index: usize,
        temp_dir: Rc<TempDir>,
    ) -> Self {
        Self {
            name,
            origin: Origin::Original(abs_path),
            rel_path,
            state: Unscanned,
            index,
            temp_dir,
        }
    }

    pub const fn new_extracted(
        extracted_path: Arc<Path>,
        rel_path: PathBuf,
        name: Arc<str>,
        index: usize,
        temp_dir: Rc<TempDir>,
        extract_future: ExtractFuture,
    ) -> Self {
        Self {
            name,
            origin: Origin::Extracted(extracted_path),
            rel_path,
            state: Extracting(extract_future),
            index,
            temp_dir,
        }
    }

    pub(super) fn get_displayable(&self, upscaling: bool) -> Displayable {
        match &self.state {
            Extracting(_) | Unscanned | Scanning(_) => Displayable::Pending,
            Scanned(s) => s.get_displayable(upscaling),
            Failed(e) => Displayable::Error(e.clone()),
        }
    }

    pub(super) fn has_work(&self, work: &Work) -> bool {
        match &self.state {
            Extracting(_) | Unscanned => true,
            Scanning(_) => *work != Work::Scan,
            Scanned(i) => i.has_work(work),
            Failed(_) => false,
        }
    }

    // These functions should return after each unit of work is done.
    #[instrument(level = "error", skip_all, fields(p = ?self),  name = "")]
    pub async fn do_work(&mut self, work: Work<'_>) -> Completion {
        if work.extract_early() {
            self.try_jump_extraction_queue();
        }

        match &mut self.state {
            Extracting(f) => {
                match (&mut f.fut).await {
                    Ok(_) => {
                        self.state = Unscanned;
                        // Since waiting for extraction isn't one of the tracked units of work, we
                        // know for a fact we can try to scan the file now.
                        self.start_scanning(work.load_during_scan()).await;
                    }
                    Err(e) => {
                        error!("Failed to extract page: {e}");
                        self.state = Failed("Failed to extract page.".to_string());
                    }
                }
                Completion::StartScan
            }
            Unscanned => {
                self.start_scanning(work.load_during_scan()).await;
                Completion::StartScan
            }
            Scanning(f) => {
                assert_ne!(work, Work::Scan);

                let ir = (&mut f.0).await;
                self.state = Scanned(Box::new(ScannedPage::new(self, ir)));
                trace!("Finished scanning");
                Completion::Scanned
            }
            Scanned(ip) => ip.do_work(work).await,
            Failed(_) => unreachable!(),
        }
    }

    fn try_jump_extraction_queue(&mut self) {
        if let Extracting(ef) = &mut self.state {
            if let Some(s) = ef.jump_queue.take() {
                drop(s.try_send(self.rel_path.to_string_lossy().to_string()));
            }
        }
    }

    async fn start_scanning(&mut self, load: bool) {
        let p = self.get_absolute_file_path().clone();

        let f = loading::scan(p, self.temp_dir.path(), self.index, load).await;
        self.state = Scanning(f);
        trace!("Started scanning");
    }

    #[instrument(level = "error")]
    pub async fn join(self) {
        let written = match self.state {
            Extracting(f) => f.fut.await.is_ok(),
            Unscanned => true,
            Scanning(f) => {
                f.0.await;
                true
            }
            Scanned(i) => {
                i.join().await;
                true
            }
            Failed(_) => return,
        };

        if written {
            if let Origin::Extracted(p) = self.origin {
                if let Err(e) = remove_file(p.as_ref()).await {
                    error!("Failed to remove file {p:?}: {e:?}")
                }
            }
        }
    }

    #[instrument(level = "trace")]
    pub fn unload(&mut self) {
        match &mut self.state {
            Extracting(_) | Unscanned | Failed(_) => (),
            Scanning(i) => {
                i.unload_scanning();
                trace!("Unloaded scanning page")
            }
            Scanned(i) => {
                i.unload();
            }
        };
    }

    pub(super) const fn get_absolute_file_path(&self) -> &Arc<Path> {
        match &self.origin {
            Origin::Extracted(p) | Origin::Original(p) => p,
        }
    }

    pub(super) const fn get_rel_path(&self) -> &PathBuf {
        &self.rel_path
    }

    pub(super) fn get_env(&self) -> Vec<(String, OsString)> {
        let mut e = vec![
            ("AWMAN_PAGE_NUMBER".into(), (self.index + 1).to_string().into()),
            ("AWMAN_RELATIVE_FILE_PATH".into(), self.rel_path.clone().into()),
        ];

        match self.state {
            Extracting(_) | Failed(_) => (),
            Unscanned | Scanning(_) | Scanned(_) => e.push((
                "AWMAN_CURRENT_FILE".into(),
                self.get_absolute_file_path().as_os_str().to_owned(),
            )),
        }

        e
    }

    pub(super) fn page_info(&self) -> Value {
        let mut val = json!({
            "path": self.rel_path.to_string_lossy(),
            "index":  (self.index + 1),
        });

        match self.state {
            Extracting(_) | Failed(_) => (),
            Unscanned | Scanning(_) | Scanned(_) => {
                val.as_object_mut().unwrap().insert(
                    "abs_path".to_string(),
                    Value::String(self.get_absolute_file_path().to_string_lossy().to_string()),
                );
            }
        };

        // TODO -- more page info? Or a separate info command for details?

        val
    }
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {:?}", self.name, self.state)
    }
}

// Build a chain of previous loads that have been cancelled.
// This doesn't affect when they're cleaned up, the LocalSet will run the cleanup code, but this is
// necessary to track when all pending operations have finished.
fn chain_last_load(last_load: &mut Option<Fut<()>>, new_last: Fut<()>) {
    let old_last = last_load.take();
    *last_load = match old_last {
        Some(fut) => Some(fut.then(|_| new_last).boxed_local()),
        None => Some(new_last),
    };
}

// Clear out any past loads, if they won't block.
async fn try_last_load(last_load: &mut Option<Fut<()>>) {
    let Some(last) = last_load.as_mut() else {
        return;
    };

    if poll!(last).is_ready() {
        *last_load = None
    }
}
