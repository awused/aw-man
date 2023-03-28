use core::fmt;
use std::path::PathBuf;
use std::rc::Weak;

use derive_more::DebugCustom;
use State::*;

use crate::com::{Displayable, Image, ImageWithRes, Res, WorkParams};
use crate::manager::archive::page::{chain_last_load, try_last_load};
use crate::manager::archive::Work;
use crate::pools::downscaling::DownscaleFuture;
use crate::pools::loading::{self, ImageOrRes, LoadFuture, UnscaledImage};
use crate::Fut;

#[derive(DebugCustom)]
enum State {
    Unloaded,
    #[debug(fmt = "Loading")]
    Loading(LoadFuture<UnscaledImage, WorkParams>),
    #[debug(fmt = "Reloading {_1:?}")]
    Reloading(LoadFuture<UnscaledImage, WorkParams>, Image),
    #[debug(fmt = "Reloading {_1:?}")]
    Scaling(DownscaleFuture<Image, WorkParams>, UnscaledImage),
    #[debug(fmt = "Loaded {_0:?}")]
    Loaded(UnscaledImage),
    #[debug(fmt = "Scaled {_0:?}")]
    Scaled(Image),
    #[debug(fmt = "Failed {_0:?}")]
    Failed(String),
}


// This represents a static image in a format that the images crate natively understands.
// This file is somewhere on the file system but may not be a temporary file. The file is not owned
// by this struct.
pub(super) struct RegularImage {
    state: State,
    // The resolution of this file, which may be the upscaled file after upcaling.
    file_res: Res,
    last_load: Option<Fut<()>>,
    // The Regular image does _not_ own this file.
    path: Weak<PathBuf>,
}

impl fmt::Debug for RegularImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[i:{:?} {:?} {:?}]",
            self.path.upgrade().unwrap_or_default(),
            self.file_res,
            self.state
        )
    }
}

impl RegularImage {
    pub(super) fn new(ior: ImageOrRes, path: Weak<PathBuf>) -> Self {
        let file_res = ior.res();
        let state = match ior {
            ImageOrRes::Image(b) => Loaded(b),
            ImageOrRes::Res(_) => Unloaded,
        };

        Self { state, file_res, last_load: None, path }
    }

    pub(super) fn get_displayable(&self, original_res: Option<Res>) -> Displayable {
        match &self.state {
            Unloaded | Loading(_) => Displayable::Pending {
                file_res: self.file_res,
                original_res: original_res.unwrap_or(self.file_res),
            },
            Reloading(_, img)
            | Loaded(UnscaledImage(img))
            | Scaling(_, UnscaledImage(img))
            | Scaled(img) => {
                Displayable::Image(ImageWithRes {
                    // These clones are cheap
                    img: img.clone(),
                    file_res: self.file_res,
                    original_res: original_res.unwrap_or(self.file_res),
                })
            }
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) fn has_work(&self, work: &Work) -> bool {
        let Some(t_params) = work.params() else { return false };

        match &self.state {
            Unloaded => true,
            Loading(_) | Reloading(..) => {
                work.downscale()
                // In theory the scaled image from "Reloading" could satisfy this, in practice it's
                // very unlikely and offers minimal savings.
            }
            Loaded(UnscaledImage(img)) => {
                if !work.downscale() {
                    return false;
                }

                Self::needs_rescale_loaded(self.file_res, t_params, img.res)
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
                Self::needs_rescale_scaling(self.file_res, t_params, sf.params())
            }
            Scaled(img) => Self::needs_rescale_loaded(self.file_res, t_params, img.res),
            Failed(_) => false,
        }
    }

    fn needs_rescale_scaling(
        file_res: Res,
        target_params: WorkParams,
        existing_params: WorkParams,
    ) -> bool {
        file_res.fit_inside(target_params.target_res)
            != file_res.fit_inside(existing_params.target_res)
    }

    fn needs_rescale_loaded(file_res: Res, target_params: WorkParams, existing_res: Res) -> bool {
        file_res.fit_inside(target_params.target_res) != existing_res
    }

    pub(super) async fn do_work(&mut self, work: Work<'_>) {
        try_last_load(&mut self.last_load).await;
        assert!(work.load());

        let t_params = work
            .params()
            .unwrap_or_else(|| panic!("Called do_work {work:?} on a regular image."));

        let path = self.path.upgrade().expect("Tried to load image after the Page was dropped.");

        let l_fut;
        let s_fut;
        match &mut self.state {
            Unloaded => {
                let lf = loading::static_image::load(path, t_params).await;
                self.state = Loading(lf);
                trace!("Started loading {self:?}");
                return;
            }
            Loading(lf) => {
                l_fut = Some(lf);
                s_fut = None;
            }
            Reloading(lf, simg) => {
                if !Self::needs_rescale_loaded(self.file_res, t_params, simg.res) {
                    chain_last_load(&mut self.last_load, lf.cancel());
                    self.state = Scaled(simg.clone());
                    trace!("Skipped unnecessary reload for {self:?}");
                    return;
                }

                l_fut = Some(lf);
                s_fut = None;
            }
            Loaded(uimg) => {
                assert!(work.downscale());

                let sf = work.downscaler().unwrap().downscale_and_premultiply(uimg, t_params).await;
                self.state = Scaling(sf, uimg.clone());
                trace!("Started downscaling for {self:?}");
                return;
            }
            Scaling(sf, uimg) => {
                if !Self::needs_rescale_loaded(self.file_res, t_params, uimg.0.res) {
                    chain_last_load(&mut self.last_load, sf.cancel());
                    self.state = Loaded(uimg.clone());
                    trace!("Cancelled unnecessary downscale for {self:?}");
                    return;
                }

                if Self::needs_rescale_scaling(self.file_res, t_params, sf.params()) {
                    chain_last_load(&mut self.last_load, sf.cancel());
                    self.state = Loaded(uimg.clone());
                    trace!("Marked to restart scaling for {self:?}");
                    return;
                }

                s_fut = Some(sf);
                l_fut = None;
            }
            Scaled(simg) => {
                assert!(Self::needs_rescale_loaded(self.file_res, t_params, simg.res));
                // We need a full reload because the image is already scaled.
                let lf = loading::static_image::load(path, t_params).await;
                self.state = Reloading(lf, simg.clone());
                trace!("Started reloading {self:?}");
                return;
            }
            Failed(_) => unreachable!(),
        }

        match (l_fut, s_fut) {
            (Some(lf), None) => match (&mut lf.fut).await {
                Ok(uimg) => {
                    self.state = Loaded(uimg);
                    trace!("Finished loading {self:?}");
                }
                Err(e) => self.state = Failed(e),
            },
            (None, Some(sf)) => match (&mut sf.fut).await {
                Ok(simg) => {
                    self.state = Scaled(simg);
                    trace!("Finished scaling {self:?}");
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
            Unloaded | Failed(_) => return,
            Loaded(_) | Scaled(_) => {}
            Loading(lf) | Reloading(lf, _) => {
                chain_last_load(&mut self.last_load, lf.cancel());
            }
            Scaling(sf, _) => {
                chain_last_load(&mut self.last_load, sf.cancel());
            }
        }
        trace!("Unloaded {self:?}");
        self.state = Unloaded;
    }
}
