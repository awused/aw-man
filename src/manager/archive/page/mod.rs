use std::ffi::OsString;
use std::fmt::{self, Debug};
use std::path::PathBuf;
use std::rc::Rc;

use futures_util::FutureExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::fs::remove_file;
use State::*;

use self::scanned::ScannedPage;
use super::Work;
use crate::com::Displayable;
use crate::pools::scanning::{self, ScanFuture};
use crate::Fut;

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
enum State {
    Extracting(ExtractFuture),
    // This is really only used as the initial state for an original image.
    Unscanned,
    // This will also convert and load but not resize the image, if applicable.
    Scanning(ScanFuture),
    // It has been converted, if necessary, and we know what it is.
    // This is >200 bytes, and will only be used for visited files.
    // Using a box can save a decent chunk of memory at negligible cost.
    Scanned(Box<ScannedPage>),
    Failed(String),
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Extracting(_) => "Extracting",
            Unscanned => "Unscanned",
            Scanning(_) => "Scanning",
            Scanned(_) => "Scanned",
            Failed(_) => "Failed",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug)]
enum Origin {
    // Contains the absolute path of the extracted file.
    Extracted(Rc<PathBuf>),
    // Contains the absolute path of the file.
    Original(Rc<PathBuf>),
}

pub(super) struct Page {
    pub(super) name: String,
    origin: Origin,
    rel_path: PathBuf,
    state: State,
    index: usize,
    temp_dir: Rc<TempDir>,
}

impl Page {
    pub fn new_original(
        abs_path: PathBuf,
        rel_path: PathBuf,
        name: String,
        index: usize,
        temp_dir: Rc<TempDir>,
    ) -> Self {
        Self {
            name,
            origin: Origin::Original(Rc::from(abs_path)),
            rel_path,
            state: Unscanned,
            index,
            temp_dir,
        }
    }

    pub fn new_extracted(
        extracted_path: PathBuf,
        rel_path: PathBuf,
        name: String,
        index: usize,
        temp_dir: Rc<TempDir>,
        extract_future: ExtractFuture,
    ) -> Self {
        Self {
            name,
            origin: Origin::Extracted(Rc::from(extracted_path)),
            rel_path,
            state: Extracting(extract_future),
            index,
            temp_dir,
        }
    }

    pub(super) fn get_displayable(&self, upscaling: bool) -> (Displayable, String) {
        let d = match &self.state {
            Extracting(_) | Unscanned | Scanning(_) => Displayable::Nothing,
            Scanned(s) => s.get_displayable(upscaling),
            Failed(e) => Displayable::Error(e.clone()),
        };

        (d, self.name.clone())
    }

    pub(super) fn has_work(&self, work: Work) -> bool {
        match &self.state {
            Extracting(_) | Unscanned => true,
            Scanning(_) => work != Work::Scan,
            Scanned(i) => i.has_work(work),
            Failed(_) => false,
        }
    }

    // These functions should return after each unit of work is done.
    pub async fn do_work(&mut self, work: Work) {
        if work.extract_early() {
            self.try_jump_extraction_queue();
        }

        match &mut self.state {
            Extracting(f) => {
                let b = (&mut f.fut).await;
                match b {
                    Ok(_) => {
                        self.state = Unscanned;
                        // Since waiting for extraction isn't one of the tracked units of work, we
                        // know for a fact we can try to scan the file now.
                        self.start_scanning(work.load_during_scan()).await
                    }
                    Err(e) => {
                        error!("Failed to extract page {:?}: {}", self, e);
                        self.state = Failed("Failed to extract page.".to_string());
                    }
                }
            }
            Unscanned => self.start_scanning(work.load_during_scan()).await,
            Scanning(f) => {
                assert_ne!(work, Work::Scan);

                let ir = (&mut f.0).await;
                self.state = Scanned(Box::new(ScannedPage::new(self, ir)));
                trace!("Finished scanning {:?}", self);
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
        let p = self.get_absolute_file_path();
        // This clone could be prevented with an Arc but this otherwise enforces that these paths
        // have a single strong owner.
        let p = (**p).clone();
        // Could delay this until it's really necessary but not worth it.
        let converted_path = self.temp_dir.path().join(format!("{}c.png", self.index));

        let f = scanning::scan(p, converted_path, load).await;
        self.state = Scanning(f);
        trace!("Started scanning {:?}", self);
    }

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
                    error!("Failed to remove file {:?}: {:?}", p, e)
                }
            }
        }
    }

    pub fn unload(&mut self) {
        match &mut self.state {
            Extracting(_) | Unscanned | Failed(_) => (),
            Scanning(i) => {
                i.unload_scanning();
                trace!("Unloaded scanning page {:?}", self)
            }
            Scanned(i) => {
                i.unload();
            }
        };
    }

    pub(super) const fn get_absolute_file_path(&self) -> &Rc<PathBuf> {
        match &self.origin {
            Origin::Extracted(p) | Origin::Original(p) => p,
        }
    }

    pub(super) fn get_env(&self) -> Vec<(String, OsString)> {
        let mut e = vec![
            (
                "AWMAN_PAGE_NUMBER".into(),
                (self.index + 1).to_string().into(),
            ),
            (
                "AWMAN_RELATIVE_FILE_PATH".into(),
                self.rel_path.clone().into(),
            ),
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
        json!({
            "path": self.rel_path.to_string_lossy(),
        })
    }
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[p:{} {:?}]", self.name, self.state)
    }
}

fn chain_last_load(last_load: &mut Option<Fut<()>>, new_last: Fut<()>) {
    let old_last = last_load.take();
    *last_load = match old_last {
        Some(fut) => Some(fut.then(|_| new_last).boxed_local()),
        None => Some(new_last),
    };
}
