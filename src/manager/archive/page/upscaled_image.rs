use core::fmt;
use std::path::Path;
use std::sync::{Arc, Weak};

use State::*;
use futures_util::poll;
use tokio::fs::remove_file;

use super::regular_image::RegularImage;
use crate::Fut;
use crate::com::{Displayable, Res};
use crate::manager::archive::Work;
use crate::pools::loading::ImageOrRes;
use crate::pools::upscaling::upscale;

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
    original_path: Weak<Path>,
    // Resolution before upscaling.
    original_res: Res,
    // This file will not be written when the Upscaled is created.
    path: Arc<Path>,
    // Will eventually be used for de-upscaling to save disk space/tmpfs ram.
    last_upscale: Option<Fut<()>>,
}

impl fmt::Debug for UpscaledImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[ui:{:?} {:?}]",
            self.original_path.upgrade().as_deref().unwrap_or_else(|| Path::new("")),
            self.state
        )
    }
}

impl UpscaledImage {
    pub(super) fn new(path: Arc<Path>, original_path: Weak<Path>, original_res: Res) -> Self {
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
        if !work.perform_upscale() {
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
        debug_assert!(work.perform_upscale());

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
                            Arc::downgrade(&self.path),
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

        if work.load() {
            let Upscaled(r) = &mut self.state else {
                unreachable!()
            };
            r.do_work(work).await
        }
    }

    async fn start_upscale(&self) -> State {
        let original_path = self.original_path.upgrade().expect("Failed to upgrade original path.");

        Upscaling(upscale(&*original_path, &*self.path).await)
    }

    // Clear out any past upscales, if they won't block.
    async fn try_last_upscale(&mut self) {
        let Some(lu) = self.last_upscale.as_mut() else {
            return;
        };

        if poll!(lu).is_ready() {
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
