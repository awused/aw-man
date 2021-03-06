use core::fmt;
use std::path::PathBuf;
use std::rc::Weak;

use State::*;

use crate::com::{Displayable, Image, ImageWithRes, Res, WorkParams};
use crate::manager::archive::page::{chain_last_load, try_last_load};
use crate::manager::archive::Work;
use crate::pools::downscaling::{self, DownscaleFuture};
use crate::pools::loading::{self, ImageOrRes, LoadFuture, UnscaledImage};
use crate::Fut;

#[derive(Debug)]
enum State {
    Unloaded,
    Loading(LoadFuture<UnscaledImage, WorkParams>),
    Reloading(LoadFuture<UnscaledImage, WorkParams>, Image),
    Scaling(DownscaleFuture<Image, WorkParams>, UnscaledImage),
    Loaded(UnscaledImage),
    Scaled(Image),
    Failed(String),
}

// This represents a static image in a format that the images crate natively understands.
// This file is somewhere on the file system but may not be a temporary file. The file is not owned
// by this struct.
pub(super) struct RegularImage {
    state: State,
    // The original resolution of this file, which may be the upscaled file after upcaling.
    original_res: Res,
    last_load: Option<Fut<()>>,
    // The Regular image does _not_ own this file.
    path: Weak<PathBuf>,
}

impl fmt::Debug for RegularImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[i:{:?} {:?}]", self.path.upgrade().unwrap_or_default(), self.state)
    }
}

impl RegularImage {
    pub(super) fn new(ior: ImageOrRes, path: Weak<PathBuf>) -> Self {
        let original_res = ior.res();
        let state = match ior {
            ImageOrRes::Image(b) => State::Loaded(b),
            ImageOrRes::Res(_) => State::Unloaded,
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
            Unloaded | Loading(_) => Displayable::Pending(self.original_res),
            Reloading(_, img)
            | Loaded(UnscaledImage(img))
            | Scaling(_, UnscaledImage(img))
            | Scaled(img) => {
                Displayable::Image(ImageWithRes {
                    // These clones are cheap
                    img: img.clone(),
                    original_res: self.original_res,
                })
            }
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: Work) -> bool {
        let t_params = match work.params() {
            Some(r) => r,
            None => return false,
        };

        match &self.state {
            Unloaded => true,
            Loading(_) | Reloading(..) => {
                // TODO -- change this when "downscaling"/premultiplying isn't required.
                work.downscale()
                // In theory the scaled image from "Reloading" could satisfy this, in practice it's
                // very unlikely and offers minimal savings.
            }
            Loaded(UnscaledImage(img)) => {
                if !work.downscale() {
                    return false;
                }

                Self::needs_rescale_loaded(self.original_res, t_params, img.res)
            }
            Scaling(sf, UnscaledImage { .. }) => {
                if work.finalize() {
                    return true;
                }

                if !work.downscale() {
                    return false;
                }

                // In theory the image from "Reloading" could satisfy this, in practice it's very
                // unlikely.
                Self::needs_rescale_scaling(self.original_res, t_params, sf.params())
            }
            Scaled(img) => Self::needs_rescale_loaded(self.original_res, t_params, img.res),
            Failed(_) => false,
        }
    }

    fn needs_rescale_scaling(
        original_res: Res,
        target_params: WorkParams,
        existing_params: WorkParams,
    ) -> bool {
        original_res.fit_inside(target_params.target_res)
            != original_res.fit_inside(existing_params.target_res)
    }

    fn needs_rescale_loaded(
        original_res: Res,
        target_params: WorkParams,
        existing_res: Res,
    ) -> bool {
        original_res.fit_inside(target_params.target_res) != existing_res
    }

    pub(super) async fn do_work(&mut self, work: Work) {
        try_last_load(&mut self.last_load).await;
        assert!(work.load());

        let t_params = work
            .params()
            .unwrap_or_else(|| panic!("Called do_work {:?} on a regular image.", work));

        let path = self.path.upgrade().expect("Tried to load image after the Page was dropped.");

        let l_fut;
        let s_fut;
        match &mut self.state {
            Unloaded => {
                let lf = loading::static_image::load(path, t_params).await;
                self.state = Loading(lf);
                trace!("Started loading {:?}", self);
                return;
            }
            Loading(lf) => {
                l_fut = Some(lf);
                s_fut = None;
            }
            Reloading(lf, simg) => {
                if !Self::needs_rescale_loaded(self.original_res, t_params, simg.res) {
                    chain_last_load(&mut self.last_load, lf.cancel());
                    self.state = Scaled(simg.clone());
                    trace!("Skipped unnecessary reload for {:?}", self);
                    return;
                }

                l_fut = Some(lf);
                s_fut = None;
            }
            Loaded(uimg) => {
                assert!(work.downscale());

                let sf = downscaling::static_image::downscale_and_premultiply(uimg, t_params).await;
                self.state = Scaling(sf, uimg.clone());
                trace!("Started downscaling for {:?}", self);
                return;
            }
            Scaling(sf, uimg) => {
                if !Self::needs_rescale_loaded(self.original_res, t_params, uimg.0.res) {
                    chain_last_load(&mut self.last_load, sf.cancel());
                    self.state = Loaded(uimg.clone());
                    trace!("Cancelled unnecessary downscale for {:?}", self);
                    return;
                }

                if Self::needs_rescale_scaling(self.original_res, t_params, sf.params()) {
                    chain_last_load(&mut self.last_load, sf.cancel());
                    self.state = Loaded(uimg.clone());
                    trace!("Marked to restart scaling for {:?}", self);
                    return;
                }

                s_fut = Some(sf);
                l_fut = None;
            }
            Scaled(simg) => {
                assert!(Self::needs_rescale_loaded(self.original_res, t_params, simg.res));
                // We need a full reload because the image is already scaled.
                let lf = loading::static_image::load(path, t_params).await;
                self.state = Reloading(lf, simg.clone());
                trace!("Started reloading {:?}", self);
                return;
            }
            Failed(_) => unreachable!(),
        }

        match (l_fut, s_fut) {
            (Some(lf), None) => match (&mut lf.fut).await {
                Ok(uimg) => {
                    self.state = Loaded(uimg);
                    trace!("Finished loading {:?}", self);
                }
                Err(e) => self.state = Failed(e),
            },
            (None, Some(sf)) => match (&mut sf.fut).await {
                Ok(simg) => {
                    self.state = Scaled(simg);
                    trace!("Finished scaling {:?}", self);
                }
                Err(e) => self.state = Failed(e),
            },
            _ => unreachable!(),
        }
    }

    pub(super) async fn join(self) {
        match self.state {
            Unloaded | Loaded(_) | Failed(_) | Scaled(_) => (),
            Loading(mut lf) | Reloading(mut lf, _) => {
                lf.cancel().await;
            }
            Scaling(mut sf, _) => sf.cancel().await,
        }

        if let Some(last) = self.last_load {
            last.await;
        }
    }

    pub(super) fn unload(&mut self) {
        match &mut self.state {
            Unloaded | Failed(_) => (),
            Loaded(_) | Scaled(_) => {
                self.state = Unloaded;
                trace!("Unloaded {:?}", self);
            }
            Loading(lf) => {
                chain_last_load(&mut self.last_load, lf.cancel());
                self.state = Unloaded;
                trace!("Unloaded {:?}", self);
            }
            Reloading(lf, _) => {
                chain_last_load(&mut self.last_load, lf.cancel());
                self.state = Unloaded;
                trace!("Unloaded {:?}", self);
            }
            Scaling(sf, _) => {
                chain_last_load(&mut self.last_load, sf.cancel());
                self.state = Unloaded;
                trace!("Unloaded {:?}", self);
            }
        }
    }
}
