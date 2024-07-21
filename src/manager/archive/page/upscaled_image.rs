use core::fmt;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use futures_util::poll;
use tokio::fs::remove_file;
use State::*;

use super::regular_image::RegularImage;
use crate::com::{Displayable, Res};
use crate::manager::archive::Work;
use crate::pools::loading::ImageOrRes;
use crate::pools::upscaling::upscale;
use crate::Fut;

enum State {
    Unupscaled,
    Upscaling(Fut<Result<Res, String>>),
    Upscaled(RegularImage),
    Failed(String),
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Unupscaled => "Unupscaled",
                Upscaling(_) => "Upscaling",
                Upscaled(_) => "Upscaled",
                Failed(_) => "Failed",
            }
        )
    }
}

pub(super) struct UpscaledImage {
    state: State,
    // The original path of the file. Not owned by this struct.
    original_path: Weak<PathBuf>,
    // Resolution before upscaling.
    original_res: Res,
    // This file will not be written when the Upscaled is created.
    path: Rc<PathBuf>,
    // Will eventually be used for de-upscaling to save disk space/tmpfs ram.
    last_upscale: Option<Fut<()>>,
}

impl fmt::Debug for UpscaledImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[ui:{:?} {:?}]",
            self.original_path.upgrade().unwrap_or_default(),
            self.state
        )
    }
}

impl UpscaledImage {
    pub(super) fn new(path: PathBuf, original_path: Weak<PathBuf>, original_res: Res) -> Self {
        let path = Rc::from(path);
        Self {
            state: Unupscaled,
            original_path,
            original_res,
            path,
            last_upscale: None,
        }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unupscaled | Upscaling(_) => Displayable::Pending,
            Upscaled(r) => r.get_displayable(Some(self.original_res)),
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: &Work) -> bool {
        if !work.upscale() {
            return false;
        }

        match &self.state {
            Unupscaled => true,
            Upscaling(_) => work.load(),
            Upscaled(r) => r.has_work(work),
            Failed(_) => false,
        }
    }

    // #[instrument(level = "trace", skip_all, name = "upscaled")]
    pub(super) async fn do_work(&mut self, work: Work<'_>) {
        self.try_last_upscale().await;

        if work.upscale() {
            match &mut self.state {
                Unupscaled => {
                    // The last upscale COULD, in theory, be writing the file right now.
                    if let Some(lu) = &mut self.last_upscale {
                        lu.await;
                        self.last_upscale = None;
                    }
                    self.state = self.start_upscale().await;
                    trace!("Started upscaling");
                    return;
                }
                Upscaling(uf) => {
                    assert!(work.load());
                    match uf.await {
                        Ok(res) => {
                            self.state = Upscaled(RegularImage::new(
                                ImageOrRes::Res(res),
                                Rc::downgrade(&self.path),
                            ));
                            trace!("Finished upscaling");
                        }
                        Err(e) => {
                            error!("Failed to upscale: {e}");
                            self.state = Failed(e);
                        }
                    }
                    return;
                }
                Upscaled(_) => {}
                Failed(_) => unreachable!(),
            }
        }

        if work.load() {
            let Upscaled(r) = &mut self.state else {
                unreachable!()
            };
            r.do_work(work).await
        }
    }

    async fn start_upscale(&mut self) -> State {
        let original_path = self.original_path.upgrade().expect("Failed to upgrade original path.");

        Upscaling(upscale(&*original_path, &*self.path).await)
    }

    async fn try_last_upscale(&mut self) {
        if self.last_upscale.is_none() {
            return;
        }

        // Clear out any past upscales, if they won't block.
        if poll!(self.last_upscale.as_mut().unwrap()).is_ready() {
            self.last_upscale = None
        }
    }

    pub(super) async fn join(self) {
        if let Some(last) = self.last_upscale {
            last.await;
        }

        let upscaled = match self.state {
            Unupscaled | Failed(_) => false,
            Upscaling(f) => f.await.is_ok(),
            Upscaled(r) => {
                r.join().await;
                true
            }
        };

        if upscaled {
            if let Err(e) = remove_file(&*self.path).await {
                error!("Failed to remove upscaled file {:?}: {e:?}", self.path)
            }
        }
    }

    pub(super) fn unload(&mut self) {
        if let Upscaled(r) = &mut self.state {
            r.unload();
        }
    }
}
