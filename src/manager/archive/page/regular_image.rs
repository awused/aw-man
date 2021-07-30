use core::fmt;
use std::future;
use std::path::PathBuf;
use std::rc::Weak;

use futures_util::FutureExt;
use tokio::select;
use RegularState::*;

use crate::com::{Bgra, Displayable, LoadingParams, Res, ScaledImage};
use crate::manager::archive::Work;
use crate::pools::loading::{self, LoadFuture};
use crate::pools::scanning::BgraOrRes;
use crate::Fut;

#[derive(Debug)]
enum RegularState {
    Unloaded,
    NeedsReload(Bgra),
    Loading(LoadFuture<Bgra, LoadingParams>),
    Reloading(LoadFuture<Bgra, LoadingParams>, Bgra),
    Loaded(Bgra),
    Failed(String),
}

// This represents a static image in a format that the images crate natively understands.
// This file is somewhere on the file system but may not be a temporary file. The file is not owned
// by this struct.
pub(super) struct RegularImage {
    state: RegularState,
    // upscaled: bool,
    // The original resolution of this file, which may be the upscaled file after upcaling.
    original_res: Res,
    last_load: Option<Fut<()>>,
    // The Regular image does _not_ own this file.
    path: Weak<PathBuf>,
}

impl fmt::Debug for RegularImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[i:{:?} {:?}]",
            self.path.upgrade().unwrap_or_default(),
            self.state
        )
    }
}

impl RegularImage {
    pub(super) fn new(bor: BgraOrRes, path: Weak<PathBuf>) -> Self {
        let original_res = bor.res();
        let state = match bor {
            BgraOrRes::Bgra(b) => RegularState::Loaded(b),
            BgraOrRes::Res(_) => RegularState::Unloaded,
        };

        Self {
            state,
            original_res,
            last_load: None,
            path,
        }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unloaded | Loading(_) => Displayable::Nothing,
            Reloading(_, bgra) | Loaded(bgra) | NeedsReload(bgra) => {
                Displayable::Image(ScaledImage {
                    // These clones are cheap
                    bgra: bgra.clone(),
                    original_res: self.original_res,
                })
            }
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: Work) -> bool {
        if !work.load() {
            return false;
        }

        let t_params = match work.params() {
            Some(r) => r,
            None => return false,
        };

        match &self.state {
            Unloaded => true,
            Loading(lf) | Reloading(lf, _) => {
                if work.finalize() {
                    return true;
                }

                // In theory the bgra from "Reloading" could satisfy this, in practice it's very
                // unlikely.
                Self::needs_rescale_loading(self.original_res, t_params, lf.params())
            }
            Loaded(bgra) | NeedsReload(bgra) => {
                // It's at least theoretically possible for this to return false even for
                // NeedsReload.
                Self::needs_rescale_loaded(self.original_res, t_params, bgra.res)
            }
            Failed(_) => false,
        }
    }

    // This will need to be split once target_res includes its fitting strategy
    fn needs_rescale_loading(
        original_res: Res,
        target_params: LoadingParams,
        existing_params: LoadingParams,
    ) -> bool {
        if !existing_params.scale_during_load {
            false
        } else if target_params.target_res.is_zero() {
            original_res != original_res.fit_inside(&existing_params.target_res)
        } else {
            original_res.fit_inside(&target_params.target_res)
                != original_res.fit_inside(&existing_params.target_res)
        }
    }

    fn needs_rescale_loaded(
        original_res: Res,
        target_params: LoadingParams,
        existing_res: Res,
    ) -> bool {
        if target_params.target_res.is_zero() {
            original_res != existing_res
        } else {
            original_res.fit_inside(&target_params.target_res) != existing_res
        }
    }

    pub(super) async fn do_work(&mut self, work: Work) {
        self.try_last_load().await;
        assert!(work.load());

        let t_params = work
            .params()
            .unwrap_or_else(|| panic!("Called do_work {:?} on a regular image.", work));

        let path = self
            .path
            .upgrade()
            .expect("Tried to load image after the Page was dropped.");

        let lfut;
        match &mut self.state {
            Unloaded => {
                let lf = loading::static_image::load(path, t_params).await;
                self.state = Loading(lf);
                trace!("Started loading {:?}", self);
                return;
            }
            Loading(lf) => {
                if Self::needs_rescale_loading(self.original_res, t_params, lf.params()) {
                    chain_last_load(&mut self.last_load, lf.cancel());
                    self.state = Unloaded;
                    trace!("Marked to restart loading {:?}", self);
                    return;
                }
                lfut = lf;
            }
            Reloading(lf, bgra) => {
                if !Self::needs_rescale_loaded(self.original_res, t_params, bgra.res) {
                    chain_last_load(&mut self.last_load, lf.cancel());
                    self.state = Loaded(bgra.clone());
                    trace!("Skipped unnecessary rescale for {:?}", self);
                    return;
                }

                if Self::needs_rescale_loading(self.original_res, t_params, lf.params()) {
                    chain_last_load(&mut self.last_load, lf.cancel());
                    self.state = NeedsReload(bgra.clone());
                    trace!("Marked for reloading {:?}", self);
                    return;
                }
                lfut = lf;
            }
            Loaded(bgra) | NeedsReload(bgra) => {
                if !Self::needs_rescale_loaded(self.original_res, t_params, bgra.res) {
                    return;
                }
                if bgra.res == self.original_res {
                    // The image hasn't been scaled so we can scale the loaded image.
                    let lf = loading::static_image::rescale(bgra, t_params).await;
                    self.state = Reloading(lf, bgra.clone());
                    trace!("Started rescaling {:?}", self);
                } else {
                    // We need a full reload because the image is already scaled.
                    let lf = loading::static_image::load(path, t_params).await;
                    self.state = Reloading(lf, bgra.clone());
                    trace!("Started reloading {:?}", self);
                }
                return;
            }
            Failed(_) => unreachable!(),
        }

        assert!(work.finalize());
        match (&mut lfut.fut).await {
            Ok(bgra) => {
                self.state = Loaded(bgra);
                trace!("Finished loading {:?}", self);
            }
            Err(e) => self.state = Failed(e),
        }
    }

    async fn try_last_load(&mut self) {
        if self.last_load.is_none() {
            return;
        }

        // Clear out any past loads, if they won't block.
        select! {
            biased;
            _ = self.last_load.as_mut().unwrap() => {
                self.last_load = None
            },
            _ = future::ready(()) => {}
        }
    }

    pub(super) async fn join(self) {
        match self.state {
            Unloaded | NeedsReload(_) | Loaded(_) | Failed(_) => (),
            Loading(mut lf) | Reloading(mut lf, _) => {
                lf.cancel().await;
            }
        }

        if let Some(last) = self.last_load {
            last.await;
        }
    }

    pub(super) fn unload(&mut self) {
        match &mut self.state {
            Unloaded | Failed(_) => (),
            NeedsReload(_) | Loaded(_) => {
                trace!("Unloaded {:?}", self);
                self.state = Unloaded;
            }
            Loading(lf) | Reloading(lf, _) => {
                let lf = lf.cancel();
                trace!("Unloaded {:?}", self);
                self.state = Unloaded;
                let last_load = self.last_load.take();
                self.last_load = match last_load {
                    Some(fut) => Some(fut.then(|_| lf).boxed_local()),
                    None => Some(lf),
                };
            }
        }
    }
}

fn chain_last_load(last_load: &mut Option<Fut<()>>, new_last: Fut<()>) {
    let old_last = last_load.take();
    *last_load = match old_last {
        Some(fut) => Some(fut.then(|_| new_last).boxed_local()),
        None => Some(new_last),
    };
}
