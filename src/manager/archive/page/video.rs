use core::fmt;
use std::path::Path;
use std::sync::Weak;

use State::*;

use crate::Fut;
use crate::com::Displayable;
use crate::manager::archive::{Completion, Work};

// TODO -- https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
#[allow(dead_code)]
#[derive(Debug)]
enum State {
    Unloaded,
    Failed(String),
}

// This represents a static image in a format that the images crate natively understands.
// This file is somewhere on the file system but may not be a temporary file. The file is not owned
// by this struct.
pub(super) struct Video {
    state: State,
    last_load: Option<Fut<()>>,
    // The video does _not_ own this file.
    path: Weak<Path>,
}

impl fmt::Debug for Video {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[video:{:?} {:?}]",
            self.path.upgrade().as_deref().unwrap_or_else(|| Path::new("")),
            self.state
        )
    }
}

impl Video {
    pub(super) fn new(path: Weak<Path>) -> Self {
        Self { state: Unloaded, last_load: None, path }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unloaded => {
                let pb = self
                    .path
                    .upgrade()
                    .expect("Called get_displayable on video after page was dropped");
                Displayable::Video(pb)
            }
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    #[allow(clippy::all, unused_variables, clippy::unused_self)]
    pub(super) const fn has_work(&self, work: &Work) -> bool {
        // TODO -- https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
        return false;
        // if !work.load() {
        //     return false;
        // }
        //
        // match &self.state {
        //     Unloaded => true,
        //     Loading(_) => work.finalize(),
        //     Loaded(_) | Failed(_) => false,
        // }
    }

    #[allow(clippy::all, unused_variables, clippy::unused_self)]
    pub(super) async fn do_work(&mut self, work: Work<'_>) -> Completion {
        // try_last_load(&mut self.last_load).await;
        unreachable!();
        // assert!(work.load());
        //
        // let t_params = work
        //     .params()
        //     .unwrap_or_else(|| panic!("Called do_work {:?} on a video.", work));
        //
        // let path = self
        //     .path
        //     .upgrade()
        //     .expect("Tried to load video after the Page was dropped.");
        //
        // let lfut;
        // match &mut self.state {
        //     Unloaded => {
        //         let lf = loading::animation::load(path, t_params).await;
        //         self.state = Loading(lf);
        //         trace!("Started loading {:?}", self);
        //         return;
        //     }
        //     Loading(lf) => {
        //         lfut = lf;
        //     }
        //     Loaded(_) | Failed(_) => unreachable!(),
        // }
        //
        // assert!(work.finalize());
        // match (&mut lfut.fut).await {
        //     Ok(ai) => {
        //         self.state = Loaded(ai);
        //         trace!("Finished loading {:?}", self);
        //     }
        //     Err(e) => self.state = Failed(e),
        // }
    }

    pub(super) async fn join(self) {
        match self.state {
            Unloaded | Failed(_) => (),
            // Loading(mut lf) => {
            //     lf.cancel().await;
            // }
        }

        if let Some(last) = self.last_load {
            last.await;
        }
    }

    pub(super) fn unload(&mut self) {
        match &mut self.state {
            Unloaded | Failed(_) => (),
            // Loading(lf) => {
            //     chain_last_load(&mut self.last_load, lf.cancel());
            //     trace!("Unloaded {:?}", self);
            //     self.state = Unloaded;
            // }
        }
    }
}
