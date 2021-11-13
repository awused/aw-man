use core::fmt;
use std::future;
use std::path::PathBuf;
use std::rc::Weak;

use tokio::select;
use State::*;

use crate::com::{AnimatedImage, Displayable, WorkParams};
use crate::manager::archive::page::chain_last_load;
use crate::manager::archive::Work;
use crate::pools::loading::{self, LoadFuture};
use crate::Fut;

#[derive(Debug)]
enum State {
    Unloaded,
    Loading(LoadFuture<AnimatedImage, WorkParams>),
    Loaded(AnimatedImage),
    Failed(String),
}

// This represents a static image in a format that the images crate natively understands.
// This file is somewhere on the file system but may not be a temporary file. The file is not owned
// by this struct.
pub(super) struct Animation {
    state: State,
    last_load: Option<Fut<()>>,
    // The animation does _not_ own this file.
    path: Weak<PathBuf>,
}

impl fmt::Debug for Animation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[anim:{:?} {:?}]",
            self.path.upgrade().unwrap_or_default(),
            self.state
        )
    }
}

impl Animation {
    pub(super) fn new(path: Weak<PathBuf>) -> Self {
        Self {
            state: Unloaded,
            last_load: None,
            path,
        }
    }

    pub(super) fn get_displayable(&self) -> Displayable {
        match &self.state {
            Unloaded | Loading(_) => Displayable::Nothing,
            Loaded(ai) => Displayable::Animation(ai.clone()),
            Failed(s) => Displayable::Error(s.clone()),
        }
    }

    pub(super) const fn has_work(&self, work: Work) -> bool {
        if !work.load() {
            return false;
        }

        match &self.state {
            Unloaded => true,
            Loading(_) => work.finalize(),
            Loaded(_) | Failed(_) => false,
        }
    }

    pub(super) async fn do_work(&mut self, work: Work) {
        self.try_last_load().await;
        assert!(work.load());

        let t_params = work
            .params()
            .unwrap_or_else(|| panic!("Called do_work {:?} on an animation.", work));

        let path = self
            .path
            .upgrade()
            .expect("Tried to load animation after the Page was dropped.");

        let lfut;
        match &mut self.state {
            Unloaded => {
                let lf = loading::animation::load(path, t_params).await;
                self.state = Loading(lf);
                trace!("Started loading {:?}", self);
                return;
            }
            Loading(lf) => {
                lfut = lf;
            }
            Loaded(_) | Failed(_) => unreachable!(),
        }

        assert!(work.finalize());
        match (&mut lfut.fut).await {
            Ok(ai) => {
                self.state = Loaded(ai);
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
            Unloaded | Loaded(_) | Failed(_) => (),
            Loading(mut lf) => {
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
            Loaded(ai) => {
                let ai = ai.clone();
                trace!("Unloaded {:?}", self);
                self.state = Unloaded;
                tokio::task::spawn_blocking(move || drop(ai));
            }
            Loading(lf) => {
                chain_last_load(&mut self.last_load, lf.cancel());
                trace!("Unloaded {:?}", self);
                self.state = Unloaded;
            }
        }
    }
}
