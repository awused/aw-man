use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use flume::Sender;
use serde_json::{json, Value};
use tokio::{pin, select};

use super::files::is_supported_page_extension;
use super::find_next::SortKeyCache;
use super::indices::{CurrentIndices, PageIndices};
use super::Manager;
use crate::closing;
use crate::com::Direction::{Absolute, Backwards, Forwards};
use crate::com::{CommandResponder, Direction, GuiAction, OneOrTwo};
use crate::config::CONFIG;
use crate::gui::WINDOW_ID;
use crate::manager::archive::Archive;
use crate::manager::indices::AI;
use crate::manager::{find_next, ManagerWork};
use crate::socket::SOCKET_PATH;


impl Manager {
    pub(super) fn move_pages(&mut self, d: Direction, n: usize) {
        // Pessimistically keep the last loaded page loaded. If it's not necessary we'll
        // revert this later before any actual loading happens next time the gui state is built.
        if self.modes.display.strip()
            && n == 1
            && d == Direction::Backwards
            && self.get_displayable(&self.current).layout().res().is_some()
        {
            if let Some(mp) = CONFIG.max_strip_preload_ahead {
                if self.preload_ahead < mp.get() {
                    self.grow_preload(1);
                }
            }
        }

        if !self.modes.manga {
            let nc = self.current.move_clamped_in_archive(d, n);
            if n == 1 && d == Direction::Backwards && self.modes.display.dual_page() {
                self.set_current_page(CurrentIndices::Dual(OneOrTwo::One(nc)));
                return;
            }
            self.set_current_page(CurrentIndices::Single(nc));
            return;
        }

        let mut cache = SortKeyCache::Empty;

        // Try to load additional chapters to satisfy the movement, until we can't.
        // This is currently the only place where an unbounded number of archives can be loaded in
        // response to one user action.
        loop {
            if let Some(pi) = self.current.try_move_pages(d, n) {
                if n == 1 && d == Direction::Backwards && self.modes.display.dual_page() {
                    self.set_current_page(CurrentIndices::Dual(OneOrTwo::One(pi)));
                    return;
                }

                self.set_current_page(CurrentIndices::Single(pi));
                return;
            }

            if let Some(new_cache) = self.open_next_archive(d, cache) {
                cache = new_cache;
            } else {
                let nc = self.current.move_clamped(d, n);
                if d == Direction::Backwards && self.modes.display.dual_page() {
                    // If we moved backwards by 1 in dual page mode, only a single page should be
                    // displayed.
                    if Some(&nc) == self.current.try_move_pages(d, 1).as_ref() {
                        self.set_current_page(CurrentIndices::Dual(OneOrTwo::One(nc)));
                        return;
                    }
                }

                self.set_current_page(CurrentIndices::Single(nc));
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
        self.set_current_page(CurrentIndices::Single(PageIndices::new(
            new_a,
            new_p,
            self.archives.clone(),
        )));
    }

    pub(super) fn move_previous_archive(&mut self) {
        let a = self.current.a();
        if a == AI(0) && self.open_next_archive(Backwards, SortKeyCache::Empty).is_none() {
            return;
        }

        let a = self.current.a();

        let new_a = a.0 - 1;
        let new_p = if self.archives.borrow()[new_a].page_count() > 0 { Some(0) } else { None };

        self.set_current_page(CurrentIndices::Single(PageIndices::new(
            new_a,
            new_p,
            self.archives.clone(),
        )));
    }

    pub(super) fn open(&mut self, files: Vec<PathBuf>, resp: Option<CommandResponder>) {
        let id = self.next_id();

        // TODO -- support opening a set of directories and/or archives.
        // But never mixing regular files and archives.
        let new_archive = match &files[..] {
            [] => Ok(Archive::open_fileset(&[], &self.temp_dir, id)),
            [page, ..] if is_supported_page_extension(page) => {
                Ok(Archive::open_fileset(&files, &self.temp_dir, id))
            }
            [archive] => Ok(Archive::open(archive, &self.temp_dir, id)),
            [..] => {
                let e = "Opening multiple archives is unsupported".to_string();
                error!("{e}");
                Err(e)
            }
        };

        let (a, p) = match new_archive {
            Ok((a, p)) => (a, p),
            Err(e) => {
                if let Some(resp) = resp {
                    if let Err(e) = resp.send(json!({
                        "error": e,
                    })) {
                        error!("Couldn't send to channel: {e}");
                    }
                }
                return;
            }
        };

        for old in self.archives.borrow_mut().drain(..) {
            tokio::task::spawn_local(old.join());
        }

        self.archives.borrow_mut().push_front(a);
        self.current = CurrentIndices::Single(PageIndices::new(0, p, self.archives.clone()));
        // Probably safe to call set_current_page but seems more fragile than I'd like.
        // By closing the old archives we remove the need for the normal cleanup process.
        self.reset_indices();
        // In other circumstances we'd probably want to call maybe_open_new_archives() here, but
        // this is already blocking the GUI waiting for us to start working on whatever we just
        // opened. New archives will still be opened on demand later.
        self.adjust_current_for_dual_page();
    }

    fn set_current_page(&mut self, ci: CurrentIndices) {
        if *self.current == *ci {
            self.adjust_current_for_dual_page();
            self.reset_indices();
            return;
        }
        let oldc = self.current.clone();
        self.current = ci;
        // Reset indices first so that they cannot point to invalid pages.
        self.reset_indices();
        // Important we cleanup before resetting current since it can open/close archives.
        self.cleanup_after_move(oldc);
        self.adjust_current_for_dual_page();
    }

    // Adjusts CurrentIndices for Dual/Single page mode.
    // Single() gets converted to Dual(Two) if both have layouts, otherwise Dual(One).
    // Dual just has the second page, if any, stripped down and reverted to Single.
    pub(super) fn adjust_current_for_dual_page(&mut self) {
        if self.modes.display.dual_page() {
            match &self.current {
                CurrentIndices::Single(c) => {
                    if self.get_displayable(c).layout().res().is_none() {
                        self.current = CurrentIndices::Dual(OneOrTwo::One(c.clone()));
                        return;
                    }

                    let n = self.next_display_page(c, Direction::Forwards);
                    self.current = match n {
                        Some(n) if self.get_displayable(&n).layout().res().is_some() => {
                            CurrentIndices::Dual(OneOrTwo::Two(c.clone(), n))
                        }
                        _ => CurrentIndices::Dual(OneOrTwo::One(c.clone())),
                    };
                }
                CurrentIndices::Dual(OneOrTwo::One(_) | OneOrTwo::Two(..)) => {}
            }
        } else {
            match &self.current {
                CurrentIndices::Single(_) => {}
                CurrentIndices::Dual(OneOrTwo::One(c) | OneOrTwo::Two(c, _)) => {
                    self.current = CurrentIndices::Single(c.clone());
                }
            }
        }
    }

    pub(super) fn reset_indices(&mut self) {
        self.finalize = Some(self.current.clone());
        self.downscale = Some(self.current.clone());
        self.load = Some(self.current.clone());
        self.upscale = self.modes.upscaling.then(|| self.current.clone());
        self.scan = Some(self.current.clone());
    }

    fn start_blocking_work(&mut self) {
        if !self.blocking_work {
            self.blocking_work = true;
            Self::send_gui(&self.gui_sender, GuiAction::BlockingWork);
        }
    }

    fn open_next_archive(&mut self, d: Direction, cache: SortKeyCache) -> Option<SortKeyCache> {
        // Even if this isn't immediately blocking the current image, this could block the next
        // action.
        self.start_blocking_work();

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

        let (a, _) = Archive::open(&next, &self.temp_dir, self.next_id);
        self.next_id = self.next_id.wrapping_add(1);

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

    pub fn cleanup_after_move(&mut self, oldc: PageIndices) {
        let load_range = self.get_range(ManagerWork::Load);
        let unloaditer = oldc.diff_range_with_new(&self.current, &load_range);

        for pi in unloaditer.into_iter().flatten() {
            pi.unload();
        }

        // TODO -- cleanup upscales too, subject to a wider range.
        self.maybe_open_new_archives();
        self.cleanup_unused_archives();
    }

    pub(super) fn grow_preload(&mut self, n: usize) {
        self.preload_ahead += n;
        debug!("Increasing preload_ahead to {}", self.preload_ahead);
        self.reset_indices();

        // For now we explicitly do not open new archives here. It's too niche.
    }

    // This is really only here because it is related to cleanup_after_move but pointedly different
    pub(super) fn shrink_preload(&mut self, n: usize) {
        self.preload_ahead -= n;
        debug!("Decreasing preload_ahead to {}", self.preload_ahead);
        let new_range = self.get_range(ManagerWork::Load);

        // One before the first one to unload
        let Some(mut pi) = self.current.try_move_pages(Forwards, new_range.end().unsigned_abs())
        else {
            return;
        };

        for _ in 0..n {
            let Some(unload) = pi.try_move_pages(Forwards, 1) else {
                break;
            };

            pi = unload;
            pi.unload();
        }

        self.reset_indices();

        // For now we explicitly do not close archives here. It's too niche.
    }

    pub(super) fn maybe_open_new_archives(&mut self) {
        if !self.modes.manga {
            return;
        }

        // If we need to read the disk to load new archives it can be slow enough to miss a frame.
        // Send the state now in case we have anything new to send to keep the UI responsive.
        self.maybe_send_gui_state();

        let load_range = if self.modes.upscaling {
            self.get_range(ManagerWork::Upscale)
        } else {
            self.get_range(ManagerWork::Load)
        };

        // We only ever try to load one in each direction per action.
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
            self.get_range(ManagerWork::Upscale)
        } else {
            self.get_range(ManagerWork::Load)
        };

        let mut start_a =
            self.current.move_clamped(Backwards, load_range.start().unsigned_abs()).a().0;

        while start_a > 0 {
            let a = self.archives.borrow_mut().pop_front().expect("Archive list out of sync");
            tokio::task::spawn_local(a.join());
            self.decrement_archive_indices();
            start_a -= 1;
        }

        let end_a = self.current.move_clamped(Forwards, load_range.end().unsigned_abs()).a().0;

        while end_a < self.archives.borrow().len() - 1 {
            let a = self.archives.borrow_mut().pop_back().expect("Archive list out of sync");
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

    fn get_env(&self, mut gui_env: Vec<(String, OsString)>) -> Vec<(String, OsString)> {
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

        env.append(&mut gui_env);

        env
    }

    pub(super) fn status(&self, gui_env: Vec<(String, OsString)>, resp: Option<CommandResponder>) {
        if let Some(resp) = resp {
            let m = self
                .get_env(gui_env)
                .into_iter()
                .map(|(k, v)| (k, v.to_string_lossy().into()))
                .collect();
            if let Err(e) = resp.send(Value::Object(m)) {
                error!("Unexpected error sending Status to receiver: {e:?}");
            }
        } else {
            warn!("Received Status command but had no way to respond.");
        }
    }

    pub(super) fn list_pages(&self, resp: Option<CommandResponder>) {
        if let Some(resp) = resp {
            let list = self.current.archive().list_pages();
            if let Err(e) = resp.send(Value::Array(list)) {
                error!("Unexpected error sending page list to receiver: {e:?}");
            }
        } else {
            warn!("Received ListPages command but had no way to respond.");
        }
    }

    pub(super) fn execute(
        &self,
        cmd: String,
        gui_env: Vec<(String, OsString)>,
        resp: Option<CommandResponder>,
    ) {
        tokio::task::spawn_local(execute(cmd, self.get_env(gui_env), None, resp));
    }

    pub(super) fn script(
        &self,
        cmd: String,
        gui_env: Vec<(String, OsString)>,
        resp: Option<CommandResponder>,
    ) {
        tokio::task::spawn_local(execute(
            cmd,
            self.get_env(gui_env),
            Some(self.gui_sender.clone()),
            resp,
        ));
    }
}

#[cfg(target_family = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

async fn execute(
    cmdstr: String,
    env: Vec<(String, OsString)>,
    gui_chan: Option<Sender<GuiAction>>,
    resp: Option<CommandResponder>,
) {
    let mut m = serde_json::Map::new();

    let p: &Path = Path::new(cmdstr.as_str());
    let mut comp = p.components();
    if comp.next().is_some() && comp.next().is_some() {
        if let Ok(canon) = p.canonicalize() {
            if !canon.is_absolute() {
                m.insert(
                    "error".into(),
                    format!("Relative paths are not allowed, got: {cmdstr}").into(),
                );
                error!("{m:?}");
                if let Some(resp) = resp {
                    drop(resp.send(Value::Object(m)));
                }
                return;
            }
        } else {
            m.insert("error".into(), format!("Could not get canonical path for {cmdstr}").into());
            error!("{m:?}");
            if let Some(resp) = resp {
                drop(resp.send(Value::Object(m)));
            }
            return;
        }
    }

    let mut cmd = tokio::process::Command::new(cmdstr.clone());

    #[cfg(target_family = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let fut = cmd.envs(env).kill_on_drop(gui_chan.is_some()).output();

    pin!(fut);
    let output = select! {
        output = &mut fut => output,
        _ = closing::closed_fut() => {
            warn!("Waiting to exit for up to 60 seconds until external command completes: {cmdstr}");
            if gui_chan.is_some() {
                drop(tokio::time::timeout(Duration::from_secs(60), fut).await);
            }
            warn!("Command blocking exit completed or killed: {cmdstr}");
            return;
        },
    };


    match output {
        Ok(output) => {
            if output.status.success() {
                let Some(gui_chan) = gui_chan else {
                    return;
                };

                let stdout = String::from_utf8_lossy(&output.stdout);

                for line in stdout.trim().lines() {
                    info!("Running command from script: {line}");
                    // It's possible to get the responses and include them in the JSON output,
                    // but probably unnecessary. This also doesn't wait for any slow/interactive
                    // commands to finish.
                    drop(gui_chan.send(GuiAction::Action(line.to_string(), None)));
                }

                return;
            }
            m.insert(
                "error".into(),
                format!("Executable {cmdstr} exited with error code {:?}", output.status).into(),
            );
            m.insert("stdout".to_string(), String::from_utf8_lossy(&output.stdout).into());
            m.insert("stderr".to_string(), String::from_utf8_lossy(&output.stderr).into());
        }
        Err(e) => {
            m.insert(
                "error".into(),
                format!("Executable {cmdstr} failed to start with error {e:?}").into(),
            );
        }
    }

    let m = Value::Object(m);
    error!("{m:?}");
    if let Some(resp) = resp {
        drop(resp.send(m));
    }
}
