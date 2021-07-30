use std::future;
use std::path::PathBuf;
use std::rc::Weak;

use tokio::fs::remove_file;
use tokio::select;
use UpscaledState::*;

use super::regular_image::RegularImage;
use crate::com::{Displayable, Res};
use crate::manager::archive::Work;
use crate::Fut;

enum UpscaledState {
    Unupscaled,
    _Upscaling(Fut<Result<Res, String>>),
    _Upscaled(RegularImage),
    Failed(String),
}

pub(super) struct UpscaledImage {
    state: UpscaledState,
    // The original path of the file. Not owned by this struct.
    _original_path: Weak<PathBuf>,
    // This file will not be written when the Upscaled is created.
    path: PathBuf,
    // Will eventually be used for de-upscaling.
    last_upscale: Option<Fut<()>>,
}

impl UpscaledImage {
    pub(super) fn new(path: PathBuf, original_path: Weak<PathBuf>) -> Self {
        Self {
            state: Unupscaled,
            _original_path: original_path,
            path,
            last_upscale: None,
        }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unupscaled | _Upscaling(_) => Displayable::Nothing,
            _Upscaled(r) => r.get_displayable(),
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: Work) -> bool {
        let load = match &work {
            Work::Finalize(true, _) | Work::Load(true, _) => true,
            Work::Finalize(false, _) | Work::Load(false, _) | Work::Scan => return false,
            Work::Upscale => false,
        };

        match &self.state {
            Unupscaled => true,
            _Upscaling(_) => load,
            _Upscaled(r) => r.has_work(work),
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
                    todo!();
                }
                _Upscaling(uf) => {
                    assert!(work.load());
                    match uf.await {
                        Ok(_) => todo!(),
                        Err(e) => self.state = Failed(e),
                    }
                }
                _Upscaled(_) => {}
                Failed(_) => unreachable!(),
            }
        }

        if work.load() {
            if let _Upscaled(r) = &mut self.state {
                r.do_work(work).await
            } else {
                unreachable!()
            }
        }
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
            _Upscaling(f) => f.await.is_ok(),
            _Upscaled(r) => {
                r.join().await;
                true
            }
        };

        if upscaled {
            if let Err(e) = remove_file(&self.path).await {
                error!("Failed to remove upscaled file {:?}: {:?}", self.path, e)
            }
        }
    }

    pub(super) fn unload(&mut self) {
        if let _Upscaled(r) = &mut self.state {
            r.unload();
        }
    }
}
