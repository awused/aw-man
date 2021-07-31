use core::fmt;
use std::future;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use tokio::fs::remove_file;
use tokio::select;
use UpscaledState::*;

use super::regular_image::RegularImage;
use crate::com::{Displayable, Res};
use crate::manager::archive::Work;
use crate::pools::scanning::BgraOrRes;
use crate::pools::upscaling::upscale;
use crate::Fut;

enum UpscaledState {
    Unupscaled,
    Upscaling(Fut<Result<Res, String>>),
    Upscaled(RegularImage),
    Failed(String),
}

impl fmt::Debug for UpscaledState {
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
    state: UpscaledState,
    // The original path of the file. Not owned by this struct.
    original_path: Weak<PathBuf>,
    // This file will not be written when the Upscaled is created.
    path: Rc<PathBuf>,
    // Will eventually be used for de-upscaling.
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
    pub(super) fn new(path: PathBuf, original_path: Weak<PathBuf>) -> Self {
        let path = Rc::from(path);
        Self {
            state: Unupscaled,
            original_path,
            path,
            last_upscale: None,
        }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unupscaled | Upscaling(_) => Displayable::Nothing,
            Upscaled(r) => r.get_displayable(),
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: Work) -> bool {
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

    pub(super) async fn do_work(&mut self, work: Work) {
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
                    trace!("Started upscaling {:?}", self);
                    return;
                }
                Upscaling(uf) => {
                    assert!(work.load());
                    match uf.await {
                        Ok(res) => {
                            self.state = Upscaled(RegularImage::new(
                                BgraOrRes::Res(res),
                                Rc::downgrade(&self.path),
                            ));
                            trace!("Finished upscaling {:?}", self);
                        }
                        Err(e) => {
                            trace!("Failed to upscale {:?}: {}", self, e);
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
            if let Upscaled(r) = &mut self.state {
                r.do_work(work).await
            } else {
                unreachable!()
            }
        }
    }

    async fn start_upscale(&mut self) -> UpscaledState {
        let original_path = self
            .original_path
            .upgrade()
            .expect("Failed to upgrade original path.");

        Upscaling(upscale(&*original_path, &*self.path).await)
    }

    async fn try_last_upscale(&mut self) {
        if self.last_upscale.is_none() {
            return;
        }

        // Clear out any past upscales, if they won't block.
        select! {
            biased;
            _ = self.last_upscale.as_mut().unwrap() => {
                self.last_upscale = None
            },
            _ = future::ready(()) => {}
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
                error!("Failed to remove upscaled file {:?}: {:?}", self.path, e)
            }
        }
    }

    pub(super) fn unload(&mut self) {
        if let Upscaled(r) = &mut self.state {
            r.unload();
        }
    }
}
