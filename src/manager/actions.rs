use std::cmp::Ordering;
use std::ffi::OsString;
use std::process;

use serde_json::Value;
use tokio::{pin, select};

use super::find_next::SortKeyCache;
use super::indices::PageIndices;
use super::{get_range, Manager};
use crate::closing;
use crate::com::Direction::{Absolute, Backwards, Forwards};
use crate::com::{CommandResponder, Direction};
use crate::gui::WINDOW_ID;
use crate::manager::archive::Archive;
use crate::manager::indices::AI;
use crate::manager::{find_next, ManagerWork};
use crate::socket::SOCKET_PATH;

pub(super) enum Action {
    Status,
    ListPages,
    Execute(String),
}

impl Manager {
    pub(super) fn move_pages(&mut self, d: Direction, n: usize) {
        if !self.modes.manga {
            self.set_current_page(self.current.move_clamped_in_archive(d, n));
            return;
        }

        let mut cache = SortKeyCache::Empty;

        // Try to load additional chapters, until we can't.
        loop {
            if let Some(pi) = self.current.try_move_pages(d, n) {
                self.set_current_page(pi);
                return;
            }

            if let Some(new_cache) = self.open_next_archive(d, cache) {
                cache = new_cache;
            } else {
                self.set_current_page(self.current.move_clamped(d, n));
                return;
            }
        }
    }

    pub(super) fn move_next_archive(&mut self) {
        let a = self.current.a();
        let alen = self.archives.borrow().len();
        if a == AI(alen - 1) && self.open_next_archive(Forwards, SortKeyCache::Empty).is_none() {
            return;
        }

        let new_a = a.0 + 1;
        let new_p = if self.archives.borrow()[new_a].page_count() > 0 { Some(0) } else { None };
        self.set_current_page(PageIndices::new(new_a, new_p, self.archives.clone()))
    }

    pub(super) fn move_previous_archive(&mut self) {
        let a = self.current.a();
        if a == AI(0) && self.open_next_archive(Backwards, SortKeyCache::Empty).is_none() {
            return;
        }

        let a = self.current.a();

        let new_a = a.0 - 1;
        let new_p = if self.archives.borrow()[new_a].page_count() > 0 { Some(0) } else { None };

        self.set_current_page(PageIndices::new(new_a, new_p, self.archives.clone()))
    }

    fn set_current_page(&mut self, pi: PageIndices) {
        if self.current == pi {
            self.reset_indices();
            return;
        }
        let oldc = self.current.clone();
        self.current = pi;
        self.reset_indices();
        self.cleanup_after_move(oldc);
    }

    pub(super) fn reset_indices(&mut self) {
        self.finalize = Some(self.current.clone());
        self.downscale = Some(self.current.clone());
        self.load = Some(self.current.clone());
        self.scan = Some(self.current.clone());
        if self.modes.upscaling {
            self.upscale = Some(self.current.clone());
        } else {
            self.upscale = None;
        }
    }

    fn open_next_archive(&mut self, d: Direction, cache: SortKeyCache) -> Option<SortKeyCache> {
        let (ai, ord) = match d {
            Forwards => (PageIndices::last(self.archives.clone()), Ordering::Greater),
            Backwards => (PageIndices::first(self.archives.clone()), Ordering::Less),
            Absolute => unreachable!(),
        };

        let a = ai.archive();
        if !a.allow_multiple_archives() {
            return None;
        }

        let path = a.path();

        let (next, cache) = find_next::for_path(path, ord, cache)?;
        drop(a);

        let (a, _) = Archive::open(next, &self.temp_dir);

        match d {
            Absolute => unreachable!(),
            Forwards => self.archives.borrow_mut().push_back(a),
            Backwards => {
                self.archives.borrow_mut().push_front(a);
                self.increment_archive_indices();
            }
        }
        Some(cache)
    }

    pub(super) fn cleanup_after_move(&mut self, oldc: PageIndices) {
        let load_range = get_range(ManagerWork::Load);
        let unloaditer = oldc.diff_range_with_new(&self.current, &load_range);

        for pi in unloaditer.into_iter().flatten() {
            pi.unload();
        }

        // TODO -- cleanup upscales too, subject to a wider range.
        //
        //
        self.maybe_open_new_archives();
        self.cleanup_unused_archives();
    }

    pub(super) fn maybe_open_new_archives(&mut self) {
        if !self.modes.manga {
            return;
        }

        // If we need to read the disk to load new archives it can be slow enough to miss a frame.
        // Send the state now in case we have anything new to send to keep the UI responsive.
        self.maybe_send_gui_state();

        let load_range = if self.modes.upscaling {
            get_range(ManagerWork::Upscale)
        } else {
            get_range(ManagerWork::Load)
        };

        if self.current.try_move_pages(Forwards, load_range.end().unsigned_abs()).is_none() {
            self.open_next_archive(Forwards, SortKeyCache::Empty);
        }
        if self
            .current
            .try_move_pages(Backwards, load_range.start().unsigned_abs())
            .is_none()
        {
            self.open_next_archive(Backwards, SortKeyCache::Empty);
        }
    }

    fn cleanup_unused_archives(&mut self) {
        let load_range = if self.modes.upscaling {
            get_range(ManagerWork::Upscale)
        } else {
            get_range(ManagerWork::Load)
        };

        let mut start_a =
            self.current.move_clamped(Backwards, load_range.start().unsigned_abs()).a().0;

        while start_a > 0 {
            let a = self.archives.borrow_mut().pop_front().expect("Archive list out of sync");
            debug!("Closing archive {:?}", a);
            tokio::task::spawn_local(a.join());
            self.decrement_archive_indices();
            start_a -= 1;
        }

        let end_a = self.current.move_clamped(Forwards, load_range.end().unsigned_abs()).a().0;

        while end_a < self.archives.borrow().len() - 1 {
            let a = self.archives.borrow_mut().pop_back().expect("Archive list out of sync");
            debug!("Closing archive {:?}", a);
            tokio::task::spawn_local(a.join());
        }
    }

    fn increment_archive_indices(&mut self) {
        self.current.increment_archive();

        [
            &mut self.finalize,
            &mut self.downscale,
            &mut self.load,
            &mut self.scan,
            &mut self.upscale,
        ]
        .into_iter()
        .flatten()
        .for_each(PageIndices::increment_archive)
    }

    fn decrement_archive_indices(&mut self) {
        self.current.decrement_archive();

        [
            &mut self.finalize,
            &mut self.downscale,
            &mut self.load,
            &mut self.scan,
            &mut self.upscale,
        ]
        .into_iter()
        .flatten()
        .for_each(PageIndices::decrement_archive)
    }

    fn get_env(&self) -> Vec<(String, OsString)> {
        let mut env = self.current.archive().get_env(self.current.p());
        env.push(("AWMAN_PID".into(), process::id().to_string().into()));
        env.push((
            "AWMAN_DISPLAY_MODE".into(),
            self.modes.display.to_string().to_lowercase().into(),
        ));
        env.push(("AWMAN_FIT_MODE".into(), self.modes.fit.to_string().to_lowercase().into()));
        env.push(("AWMAN_MANGA_MODE".into(), self.modes.manga.to_string().into()));
        env.push(("AWMAN_UPSCALING_ENABLED".into(), self.modes.upscaling.to_string().into()));

        if let Some(wid) = WINDOW_ID.get() {
            env.push(("AWMAN_WINDOW".into(), wid.into()))
        }

        if let Some(p) = SOCKET_PATH.get() {
            env.push(("AWMAN_SOCKET".into(), p.into()))
        }

        env
    }

    pub(super) fn handle_command(&self, action: Action, resp: Option<CommandResponder>) {
        match action {
            Action::Status => {
                if let Some(resp) = resp {
                    let m = self
                        .get_env()
                        .into_iter()
                        .map(|(k, v)| (k, v.to_string_lossy().into()))
                        .collect();
                    if let Err(e) = resp.send(Value::Object(m)) {
                        error!("Unexpected error sending Status to receiver: {:?}", e);
                    }
                } else {
                    warn!("Received Status command but had no way to respond.");
                }
            }
            Action::ListPages => {
                if let Some(resp) = resp {
                    let list = self.current.archive().list_pages();
                    if let Err(e) = resp.send(Value::Array(list)) {
                        error!("Unexpected error sending page list to receiver: {:?}", e);
                    }
                } else {
                    warn!("Received Status command but had no way to respond.");
                }
            }
            Action::Execute(cmd) => {
                tokio::task::spawn_local(execute(cmd, self.get_env(), resp));
            }
        }
    }
}

#[cfg(target_family = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

async fn execute(cmdstr: String, env: Vec<(String, OsString)>, resp: Option<CommandResponder>) {
    let mut m = serde_json::Map::new();
    let mut cmd = tokio::process::Command::new(cmdstr.clone());

    #[cfg(target_family = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let cmd = cmd.envs(env).spawn();

    // https://github.com/rust-lang/rust/issues/48594
    #[allow(clippy::never_loop)]
    'outer: loop {
        let cmd = match cmd {
            Ok(cmd) => cmd,
            Err(e) => {
                m.insert(
                    "error".into(),
                    format!("Executable {} failed to start with error {:?}", cmdstr, e).into(),
                );
                break 'outer;
            }
        };

        let fut = cmd.wait_with_output();
        pin!(fut);
        let output = select! {
            output = &mut fut => output,
            _ = closing::closed_fut() => {
                warn!("Waiting to exit until external command completes: {cmdstr}");
                drop(fut.await);
                warn!("Command blocking exit completed: {cmdstr}");
                return;
            },
        };


        match output {
            Ok(output) => {
                if output.status.success() {
                    return;
                }
                m.insert(
                    "error".into(),
                    format!("Executable {} exited with error code {:?}", cmdstr, output.status)
                        .into(),
                );
                m.insert("stdout".to_string(), String::from_utf8_lossy(&output.stdout).into());
                m.insert("stderr".to_string(), String::from_utf8_lossy(&output.stderr).into());
            }
            Err(e) => {
                m.insert(
                    "error".into(),
                    format!("Executable {} failed to start with error {:?}", cmdstr, e).into(),
                );
            }
        }

        break;
    }

    let m = Value::Object(m);
    error!("{:?}", m);
    if let Some(resp) = resp {
        drop(resp.send(m));
    }
}
