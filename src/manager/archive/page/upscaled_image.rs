use core::fmt;
use std::path::Path;
use std::sync::{Arc, Weak};

use State::*;
use derive_more::Debug;
use futures_util::poll;
use tokio::fs::remove_file;

use super::regular_image::RegularImage;
use crate::Fut;
use crate::com::{Displayable, Res};
use crate::manager::archive::{Completion, Work};
use crate::pools::loading::ImageOrRes;
use crate::pools::upscaling::{estimate_upscaled_resolution, upscale};

#[derive(Debug)]
enum State {
    #[debug("Unupscaled({_0:?})")]
    Unupscaled(Res),
    #[debug("Upscaling({_1:?})")]
    Upscaling(Fut<Result<Res, String>>, Res),
    #[debug("Upscaled")]
    Upscaled(RegularImage),
    #[debug("Failed")]
    Failed(String),
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
            state: Unupscaled(estimate_upscaled_resolution(original_res)),
            original_path,
            original_res,
            path,
            last_upscale: None,
        }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unupscaled(estimated) | Upscaling(_, estimated) => Displayable::Loading {
                file_res: *estimated,
                original_res: self.original_res,
            },
            Upscaled(r) => r.get_displayable(Some(self.original_res)),
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: &Work) -> bool {
        if !work.perform_upscale() {
            return false;
        }

        match &self.state {
            Unupscaled(_) => true,
            Upscaling(..) => work.load(),
            Upscaled(r) => r.has_work(work),
            Failed(_) => false,
        }
    }

    #[instrument(level = "error", skip_all, name = "", fields(s = ?self.state))]
    pub(super) async fn do_work(&mut self, work: Work<'_>) -> Completion {
        self.try_last_upscale().await;
        debug_assert!(work.perform_upscale());

        match &mut self.state {
            Unupscaled(estimated) => {
                // The last upscale COULD, in theory, be writing the file right now.
                if let Some(lu) = &mut self.last_upscale {
                    lu.await;
                    self.last_upscale = None;
                }
                let estimated = *estimated;
                self.state = self.start_upscale(estimated).await;
                trace!("Started upscaling");
                return Completion::Other;
            }
            Upscaling(uf, _) => {
                assert!(work.load());
                return match uf.await {
                    Ok(res) => {
                        self.state = Upscaled(RegularImage::new(
                            ImageOrRes::Res(res),
                            Arc::downgrade(&self.path),
                        ));
                        trace!("Finished upscaling");
                        Completion::Other
                    }
                    Err(e) => {
                        error!("Failed to upscale: {e}");
                        self.state = Failed(e);
                        Completion::Failed
                    }
                };
            }
            Upscaled(_) => {}
            Failed(_) => unreachable!(),
        }

        if work.load() {
            let Upscaled(r) = &mut self.state else { unreachable!() };
            r.do_work(work).await
        } else {
            Completion::Other
        }
    }

    async fn start_upscale(&self, estimated: Res) -> State {
        let original_path = self.original_path.upgrade().expect("Failed to upgrade original path.");

        Upscaling(upscale(&*original_path, &*self.path).await, estimated)
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
            Unupscaled(_) | Failed(_) => false,
            Upscaling(f, _) => f.await.is_ok(),
            Upscaled(r) => {
                r.join().await;
                true
            }
        };

        if upscaled && let Err(e) = remove_file(&*self.path).await {
            error!("Failed to remove upscaled file {:?}: {e:?}", self.path)
        }
    }

    pub(super) fn unload(&mut self) {
        if let Upscaled(r) = &mut self.state {
            r.unload();
        }
    }
}
