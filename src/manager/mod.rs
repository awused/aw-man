use std::cell::RefCell;
use std::cmp::{max, min};
use std::collections::VecDeque;
use std::future::Future;
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::Duration;

use archive::{Archive, Work};
use flume::{Receiver, Sender};
use futures_util::poll;
use indices::PageIndices;
use tempfile::TempDir;
use tokio::select;
use tokio::task::LocalSet;
use tokio::time::{Instant, sleep_until, timeout};

use self::archive::Completion;
use self::files::is_image_crate_supported;
use self::indices::CurrentIndices;
use crate::com::*;
use crate::config::{CONFIG, OPTIONS};
use crate::manager::indices::PI;
use crate::pools::downscaling::Downscaler;
use crate::{closing, spawn_thread};

mod actions;
pub mod archive;
pub mod files;
mod find_next;
mod indices;

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum ManagerWork {
    Current,
    Finalize,
    Downscale,
    Load,
    Upscale,
    Scan,
}

// Downscaling uses a lot of CPU or GPU but also "throws away" the latest load. If the user is
// rapidly changing the resolution (say, dragging a corner in Windows), then it's wasting a bunch
// of processing power for nothing.
#[derive(Debug)]
enum DownscaleDelay {
    Cleared,
    // Having both is marginally more efficient
    NotDelaying(Instant),
    Delaying(Instant),
}

// The minimum time between resolution changes to prevent wasteful downscaling and reloading, even
// for the current page. If many resolution changes come in at once no downscaling will be done
// until this much time passes without a new resolution change. Pages will be loaded and upscaled
// but not downscaled.
static RESOLUTION_DELAY: Duration = Duration::from_millis(250);

impl DownscaleDelay {
    fn delay_downscale(&self) -> bool {
        match self {
            Self::Cleared | Self::NotDelaying(_) => false,
            Self::Delaying(d) => Instant::now() < *d,
        }
    }

    async fn wait_delay(&self) {
        match self {
            Self::Cleared | Self::NotDelaying(_) => unreachable!(),
            Self::Delaying(d) => tokio::time::sleep_until(*d).await,
        }
        trace!("Finished delaying downscaling.");
    }

    fn clear(&mut self) {
        *self = Self::Cleared
    }

    fn mark_resize(&mut self) {
        let now = Instant::now();
        let delay = now + RESOLUTION_DELAY;

        match self {
            Self::Cleared => *self = Self::NotDelaying(delay),
            Self::NotDelaying(d) if now < *d => {
                trace!("Delaying downscaling due to rapid resolution changes.");
                *self = Self::Delaying(delay);
            }
            Self::NotDelaying(d) | Self::Delaying(d) => *d = delay,
        }
    }
}

// In strip mode, we're allowed to load more pages to fill the visible area
#[derive(Debug)]
enum PreloadRangeChange {
    NoChange,
    // We can increase by more than one but that's going to be pretty rare.
    More(usize),
    Fewer(usize),
}

type Archives = Rc<RefCell<VecDeque<Archive>>>;

#[derive(Debug)]
struct Manager {
    archives: Archives,
    temp_dir: TempDir,
    gui_sender: Sender<GuiAction>,

    downscaler: Downscaler,

    target_res: Res,
    modes: Modes,
    preload_ahead: usize,

    old_state: GuiState,
    action_context: GuiActionContext,

    current: CurrentIndices,
    // The next pages to finalize, downscale, load, upscale, or scan. May not be extracted yet.
    finalize: Option<PageIndices>,
    downscale: Option<PageIndices>,
    load: Option<PageIndices>,
    upscale: Option<PageIndices>,
    scan: Option<PageIndices>,

    downscale_delay: DownscaleDelay,
    pending_page_change: Option<Instant>,
    next_id: u16,
    blocking_work: bool,
}

pub fn run(
    manager_receiver: Receiver<MAWithResponse>,
    gui_sender: Sender<GuiAction>,
) -> JoinHandle<()> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("aw-man");
    let tmp_dir = CONFIG
        .temp_directory
        .as_ref()
        .map_or_else(|| builder.tempdir(), |d| builder.tempdir_in(d))
        .expect("Error creating temporary directory");

    spawn_thread("manager", move || {
        let _cod = closing::CloseOnDrop::default();
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| {
            run_local(async {
                let m = Manager::new(gui_sender, tmp_dir);
                m.run(manager_receiver).await
            })
        })) {
            closing::fatal(format!("Manager thread panicked unexpectedly: {e:?}"));
        }
        trace!("Exited manager thread");
    })
}

#[tokio::main(flavor = "current_thread")]
async fn run_local(f: impl Future<Output = TempDir>) {
    // Set up a LocalSet so that spawn_local can be used for cleanup tasks.
    let mut local = LocalSet::new();
    let tdir = local.run_until(f).await;

    // The local set really should be empty by now, if not, something was missed.
    // The only legitimate case is a subprocess that is still open, which will time out after 60
    // seconds.
    if poll!(&mut local).is_pending() {
        error!(
            "Manager exited but some cleanup wasn't awaited in join() or a subprocess is still \
             running."
        );

        if let Err(e) = timeout(Duration::from_secs(600), local).await {
            error!("Unable to finish cleaning up in {e}, something is stuck.");
        }
    }

    // By now, all archive joins, even those spawned in separate tasks, are done.
    tdir.close()
        .unwrap_or_else(|e| error!("Error dropping manager temp dir: {e:?}"));
}

impl Manager {
    fn new(gui_sender: Sender<GuiAction>, temp_dir: TempDir) -> Self {
        let modes = Modes {
            manga: OPTIONS.manga,
            upscaling: OPTIONS.upscale,
            fit: OPTIONS.fit,
            display: OPTIONS.display,
        };
        let mut gui_state = GuiState::default();
        let mut blocking_work = false;

        // If we think the first file is an image, load it quickly before scanning the directory.
        // Scanning large, remote directories with a cold cache can be very slow.
        //
        // More could be done here to reuse this in place of a ScanResult but it's likely not worth
        // it in the vast majority of cases.
        let mut try_early_open = |first_file: &PathBuf| {
            if !is_image_crate_supported(first_file) {
                return;
            }

            let Ok(img) = image::open(first_file) else {
                return;
            };

            let img = Image::from(img);
            let iwr = ImageWithRes {
                file_res: img.res,
                original_res: img.res,
                img,
            };
            gui_state.content = GuiContent::Single {
                current: Displayable::Image(iwr),
                preload: None,
            };
            Self::send_gui(
                &gui_sender,
                GuiAction::State(gui_state.clone(), GuiActionContext::default()),
            );
        };

        let file_names = &OPTIONS.file_names;
        let (a, p) = match &file_names[..] {
            [] => Archive::open_fileset(file_names, &temp_dir, 0),
            [file] if !OPTIONS.fileset => {
                try_early_open(file);
                // This might start scanning a directory, which can be slow in some cases.
                blocking_work = true;
                Self::send_gui(&gui_sender, GuiAction::BlockingWork);
                Archive::open(file, &temp_dir, 0)
            }
            [first, ..] /* if is page extension once archive sets exist */=> {
                try_early_open(first);
                Archive::open_fileset(file_names, &temp_dir, 0)
            }
        };

        let mut archives = VecDeque::new();
        archives.push_back(a);
        let archives = Rc::new(RefCell::from(archives));

        let current = PageIndices::new(0, p, archives.clone());

        let mut m = Self {
            archives,
            temp_dir,
            gui_sender,
            downscaler: Downscaler::default(),

            target_res: (0, 0).into(),
            modes,
            preload_ahead: CONFIG.preload_ahead,

            old_state: gui_state,
            action_context: GuiActionContext::default(),

            finalize: Some(current.clone()),
            downscale: Some(current.clone()),
            load: Some(current.clone()),
            upscale: modes.upscaling.then(|| current.clone()),
            scan: Some(current.clone()),
            current: CurrentIndices::Single(current),

            downscale_delay: DownscaleDelay::Cleared,
            pending_page_change: None,
            next_id: 1,
            blocking_work,
        };

        m.adjust_current_for_dual_page();
        m.maybe_send_gui_state();

        // Now that initial archive state has been sent to the GUI, we can run initial commands and
        // have some hope they'll mostly work.
        if let Err(e) = m.startup_commands() {
            error!("Error reading initial startup commands: {e}");
        }

        m
    }

    async fn run(mut self, receiver: Receiver<MAWithResponse>) -> TempDir {
        let mut pending_manga_open = self.modes.manga;

        'main: loop {
            use ManagerWork::*;

            let preload_change = self.maybe_send_gui_state();

            match preload_change {
                PreloadRangeChange::NoChange => {}
                PreloadRangeChange::More(n) => self.grow_preload(n),
                PreloadRangeChange::Fewer(n) => self.shrink_preload(n),
            }


            self.find_next_work();

            // Check and start any extractions synchronously.
            // This will never block.
            self.start_extractions();

            let delay_downscale = self.downscale_delay.delay_downscale();

            let current_work = !delay_downscale && self.has_work(Current);
            let final_work = !delay_downscale && self.has_work(Finalize);
            let downscale_work = !delay_downscale && self.has_work(Downscale);
            let load_work = self.has_work(Load);
            let upscale_work = self.has_work(Upscale);
            let scan_work = self.has_work(Scan);

            if pending_manga_open && !current_work {
                pending_manga_open = false;

                if self.modes.manga {
                    self.reset_indices();
                    self.maybe_open_new_archives();
                    continue;
                }
            }

            let no_work = !(current_work
                || final_work
                || downscale_work
                || load_work
                || upscale_work
                || scan_work
                || delay_downscale);

            let mut idle = false;

            'idle: loop {
                select! {
                    biased;
                    _ = closing::closed_fut() => break 'main,
                    mtg = receiver.recv_async() => {
                        match mtg {
                            Ok((mtg, context, r)) => {
                                debug!("{mtg:?} {context:?}");
                                self.action_context = context;
                                let interactive = self.handle_action(mtg, r);
                                if !interactive {
                                    continue 'idle;
                                }
                            }
                            Err(_e) => {
                                closing::fatal("Gui -> Manager channel disconnected");
                                break 'main;
                            },
                        }
                    }

                    // Schedule the next piece of work once it is ready
                    _ = self.do_work(Current, true), if current_work => {},
                    comp = self.do_work(Finalize, current_work), if final_work =>
                        self.handle_completion(comp, self.finalize.clone().unwrap()),
                    comp = self.do_work(Downscale, current_work), if downscale_work =>
                        self.handle_completion(comp, self.downscale.clone().unwrap()),
                    comp = self.do_work(Load, current_work), if load_work =>
                        self.handle_completion(comp, self.load.clone().unwrap()),
                    comp = self.do_work(Upscale, current_work), if upscale_work =>
                        self.handle_completion(comp, self.upscale.clone().unwrap()),
                    _ = self.do_work(Scan, current_work), if scan_work => {},

                    _ = self.downscale_delay.wait_delay(), if delay_downscale => {
                        self.downscale_delay.clear();
                    },

                    _ = async { sleep_until(self.pending_page_change.unwrap()).await },
                            if self.pending_page_change.is_some() => {
                        self.pending_page_change = None;
                        // trace!("Running deferred page change command");

                        Self::send_gui(
                            &self.gui_sender,
                            GuiAction::Action(CONFIG.page_change_command.clone().unwrap(), None));

                        continue 'idle;
                    }

                    _ = idle_sleep(), if no_work && !idle && CONFIG.idle_timeout.is_some() => {
                        idle = true;
                        debug!("Entering idle mode.");
                        self.run_optional_command(&CONFIG.idle_command);
                        self.idle_unload();
                        continue 'idle;
                    }
                };

                if idle {
                    self.reset_indices();
                    self.run_optional_command(&CONFIG.unidle_command);
                }

                break 'idle;
            }
        }

        closing::close();
        if let Err(e) = timeout(Duration::from_secs(600), self.join()).await {
            error!("Failed to exit cleanly in {e}, something is probably stuck.");
        }
        self.temp_dir
    }

    // Returns true if the action is "interactive" and should interrupt the idle state
    fn handle_action(&mut self, ma: ManagerAction, resp: Option<CommandResponder>) -> bool {
        use ManagerAction::*;

        match ma {
            Resolution(r) => {
                self.downscale_delay.mark_resize();
                self.target_res = r;
                self.reset_indices();
            }
            MovePages(d, n) => self.move_pages(d, n),
            NextArchive => self.move_next_archive(),
            PreviousArchive => self.move_previous_archive(),
            Open(files) => self.open(files, resp),
            Status(env) => {
                self.status(env, resp);
                return false;
            }
            ListPages => {
                self.list_pages(resp);
                return false;
            }
            Execute(s, env) => {
                self.execute(s, env, resp);
                return false;
            }
            Script(s, env) => {
                self.script(s, env, resp);
                return false;
            }
            Upscaling(toggle) => {
                if toggle.apply(&mut self.modes.upscaling) {
                    self.reset_indices();
                    // Upscaling may have different bounds, but we don't proactively close archives
                    // at this point.
                    self.maybe_open_new_archives();
                }
            }
            Manga(toggle) => {
                if toggle.apply(&mut self.modes.manga) {
                    self.reset_indices();
                    // We can open new archives if manga mode is toggled, but we don't proactively
                    // close them as soon as manga mode is turned off.
                    self.maybe_open_new_archives();

                    // If we're potentially straddling the end of an archive (or could be),
                    // readjust the dual page mode.
                    if self.modes.display.dual_page() {
                        let a = self.current.a();
                        let pages = self.archives.borrow()[a.0].page_count();
                        if pages > 0 && Some(PI(pages - 1)) == self.current.p() {
                            self.current = CurrentIndices::Single(self.current.clone());
                            self.adjust_current_for_dual_page();
                        }
                    }
                }
            }
            FitStrategy(s) => {
                self.modes.fit = s;
                self.reset_indices();
            }
            Display(dm) => {
                if self.modes.display.strip()
                    && !dm.strip()
                    && self.preload_ahead != CONFIG.preload_ahead
                {
                    self.shrink_preload(self.preload_ahead - CONFIG.preload_ahead)
                }

                self.modes.display = dm;
                self.adjust_current_for_dual_page();
                self.reset_indices();
            }
            CleanExit => {
                // The quit command, if any, was sent and processed before this, so it's now fine
                // to begin shutting down.
                closing::close();
            }
        }

        true
    }

    fn target_res(&self) -> TargetRes {
        (self.target_res, self.modes.fit, self.modes.display).into()
    }

    fn next_display_page(&self, p: &PageIndices, d: Direction) -> Option<PageIndices> {
        if self.modes.manga {
            p.try_move_pages(d, 1)
        } else {
            let np = p.move_clamped_in_archive(d, 1);
            if p != &np { Some(np) } else { None }
        }
    }

    fn get_displayable(&self, p: &PageIndices) -> Displayable {
        p.archive().get_displayable(p.p(), self.modes.upscaling)
    }

    fn build_gui_state(&self) -> (GuiState, PreloadRangeChange) {
        use DisplayMode::*;

        let archive = self.current.archive();
        let p = self.current.p();

        let displayable = archive.get_displayable(p, self.modes.upscaling);
        let page_info = archive.get_page_name_and_path(p);
        let page_num = p.map_or(0, |p| p.0 + 1);
        let target_res = self.target_res();

        // Even if we're in manga mode, directories and filesets definitely have nothing past
        // their ends.
        let allow_multiple_archives = self.modes.manga && archive.allow_multiple_archives();

        let mut preload_change = PreloadRangeChange::NoChange;

        let content = match (self.modes.display, displayable.layout().res()) {
            (Single | VerticalStrip | HorizontalStrip, None) => {
                // Not scrollable
                GuiContent::Single { current: displayable, preload: None }
            }
            (Single, Some(_)) => {
                // TODO -- optimization here, this shouldn't trigger for pre-downscale images
                let preload =
                    if let Some(p) = self.next_display_page(&self.current, Direction::Forwards) {
                        let d = self.get_displayable(&p);
                        if d.layout().res().is_some() { Some(d) } else { None }
                    } else {
                        None
                    };

                GuiContent::Single { current: displayable, preload }
            }
            (DualPage | DualPageReversed, _) => {
                let prev = self.get_offscreen_content(
                    &self.current,
                    Direction::Backwards,
                    allow_multiple_archives,
                    CONFIG.preload_behind,
                    true,
                );

                let mut preload_ahead = self.preload_ahead;

                let (visible, n) = match &self.current {
                    CurrentIndices::Single(c) => {
                        // TODO -- make this an assertion
                        error!("CurrentIndices::Single in Dual Page mode");
                        (OneOrTwo::One(displayable), c.clone())
                    }
                    CurrentIndices::Dual(OneOrTwo::One(c)) => {
                        (OneOrTwo::One(displayable), c.clone())
                    }
                    CurrentIndices::Dual(OneOrTwo::Two(_, n)) => {
                        preload_ahead = preload_ahead.saturating_sub(1);
                        (OneOrTwo::Two(displayable, self.get_displayable(n)), n.clone())
                    }
                };

                let next = self.get_offscreen_content(
                    &n,
                    Direction::Forwards,
                    allow_multiple_archives,
                    preload_ahead,
                    false,
                );

                GuiContent::Dual { prev, visible, next }
            }
            (VerticalStrip | HorizontalStrip, Some(_)) => {
                let r = self.build_strip_content(target_res, displayable, allow_multiple_archives);
                preload_change = r.0;
                r.1
            }
        };

        (
            GuiState {
                content,
                page_num,
                page_info,
                archive_len: archive.page_count(),
                archive_name: archive.name(),
                archive_id: archive.id(),
                current_dir: archive.containing_path(),
                modes: self.modes,
                target_res,
            },
            preload_change,
        )
    }

    fn get_offscreen_content(
        &self,
        p: &PageIndices,
        d: Direction,
        allow_multiple_archives: bool,
        remaining_preload: usize,
        check_two: bool,
    ) -> OffscreenContent {
        match self.next_display_page(p, d) {
            None if allow_multiple_archives && remaining_preload == 0 => OffscreenContent::Unknown,
            None => OffscreenContent::Nothing,
            Some(next) => {
                let layout_res = self.get_displayable(&next).layout();

                match layout_res {
                    MaybeLayoutRes::Incompatible => OffscreenContent::LayoutIncompatible,
                    MaybeLayoutRes::Unknown => OffscreenContent::Unknown,
                    MaybeLayoutRes::Res(_) if !check_two => {
                        OffscreenContent::LayoutCompatible(LayoutCount::OneOrMore)
                    }
                    MaybeLayoutRes::Res(_) => match self.next_display_page(&next, d) {
                        None if allow_multiple_archives && remaining_preload <= 1 => {
                            OffscreenContent::LayoutCompatible(LayoutCount::OneOrMore)
                        }
                        Some(n2) if self.get_displayable(&n2).layout().res().is_some() => {
                            OffscreenContent::LayoutCompatible(LayoutCount::TwoOrMore)
                        }
                        None | Some(_) => {
                            OffscreenContent::LayoutCompatible(LayoutCount::ExactlyOne)
                        }
                    },
                }
            }
        }
    }

    fn build_strip_content(
        &self,
        target_res: TargetRes,
        current_displayable: Displayable,
        allow_multiple_archives: bool,
    ) -> (PreloadRangeChange, GuiContent) {
        let scroll_dim = if self.modes.display.vertical_pagination() {
            |r: Res| r.h
        } else {
            |r: Res| r.w
        };

        let mut visible = Vec::new();
        let mut current_index = 0;


        // We at least fill the configured scroll amount in either direction, if possible.
        let mut c = self.current.clone();
        let mut remaining = CONFIG.scroll_amount.get();
        let mut preload_change = PreloadRangeChange::NoChange;

        while let Some(p) = self.next_display_page(&c, Direction::Backwards) {
            let d = self.get_displayable(&p);
            let res = if let Some(res) = d.layout().res() {
                res.fit_inside(target_res)
            } else {
                break;
            };

            // Unless the user has thousands of tiny images and a huge scroll amount this
            // won't matter.
            visible.insert(0, d);
            current_index += 1;
            c = p;
            let sc = scroll_dim(res);
            if remaining <= sc {
                break;
            }
            remaining -= sc;
        }

        visible.push(current_displayable);

        let mut c = self.current.clone();
        // This deliberately does not include the current page's scroll height.
        let mut remaining_pixels = scroll_dim(self.target_res) + CONFIG.scroll_amount.get();
        let mut forward_pages = 0;

        while let Some(n) = self.next_display_page(&c, Direction::Forwards) {
            let d = self.get_displayable(&n);
            let res = if let Some(res) = d.layout().res() {
                res.fit_inside(target_res)
            } else {
                break;
            };

            visible.push(d);
            c = n;
            forward_pages += 1;
            let sc = scroll_dim(res);
            if remaining_pixels <= sc {
                remaining_pixels = 0;
                break;
            }
            remaining_pixels -= sc;
        }

        let remaining_preload_pages = self.preload_ahead.saturating_sub(forward_pages);

        let next = self.get_offscreen_content(
            &c,
            Direction::Forwards,
            allow_multiple_archives,
            remaining_preload_pages,
            false,
        );

        if forward_pages > self.preload_ahead {
            if let Some(mp) = CONFIG.max_strip_preload_ahead {
                let pages = min(forward_pages - self.preload_ahead, mp.get() - self.preload_ahead);

                if pages > 0 {
                    preload_change = PreloadRangeChange::More(pages);
                }
            }
        } else if remaining_pixels > 0 && remaining_preload_pages == 0 {
            match next {
                OffscreenContent::Nothing | OffscreenContent::LayoutIncompatible => {}
                OffscreenContent::LayoutCompatible(_) | OffscreenContent::Unknown => {
                    if let Some(mp) = CONFIG.max_strip_preload_ahead {
                        if self.preload_ahead < mp.get() {
                            preload_change = PreloadRangeChange::More(1);
                        }
                    }
                }
            }
        }

        if remaining_pixels == 0
            && remaining_preload_pages > 0
            && CONFIG.max_strip_preload_ahead.is_some()
        {
            let unload = min(self.preload_ahead - CONFIG.preload_ahead, remaining_preload_pages);
            if unload > 0 {
                preload_change = PreloadRangeChange::Fewer(unload)
            }
        }

        (
            preload_change,
            GuiContent::Strip {
                // We include as much scrollable content backwards as we think we need so no
                // need to check what is beyond that.
                prev: OffscreenContent::Unknown,
                current_index,
                visible,
                next,
            },
        )
    }

    fn maybe_send_gui_state(&mut self) -> PreloadRangeChange {
        // Always take the context. If nothing happened we don't want it applying to later updates.
        let context = std::mem::take(&mut self.action_context);
        let (gs, preload_change) = self.build_gui_state();

        let archive_changed = gs.archive_id != self.old_state.archive_id;
        let page_changed = archive_changed || gs.page_num != self.old_state.page_num;
        let modes_changed = gs.modes != self.old_state.modes;

        if gs != self.old_state || self.blocking_work {
            Self::send_gui(&self.gui_sender, GuiAction::State(gs.clone(), context));
            self.old_state = gs;
            self.blocking_work = false;
        }

        if modes_changed {
            self.run_optional_command(&CONFIG.mode_change_command);
        }

        if archive_changed {
            self.run_optional_command(&CONFIG.archive_change_command);
        }

        if page_changed {
            if let Some(cmd) = &CONFIG.page_change_command {
                if let Some(debounce) = CONFIG.page_change_debounce {
                    self.pending_page_change =
                        Some(Instant::now() + Duration::from_secs(debounce.get() as _));
                } else {
                    Self::send_gui(&self.gui_sender, GuiAction::Action(cmd.clone(), None));
                }
            }
        }

        preload_change
    }

    fn run_optional_command(&self, cmd: &Option<String>) {
        if let Some(cmd) = cmd {
            Self::send_gui(&self.gui_sender, GuiAction::Action(cmd.clone(), None));
        }
    }

    fn send_gui(gui_sender: &Sender<GuiAction>, action: GuiAction) {
        if let Err(e) = gui_sender.send(action) {
            closing::fatal(format!("Sending to gui thread unexpectedly failed, {e:?}"));
        }
    }

    fn start_extractions(&mut self) {
        self.current.archive_mut().start_extraction();

        for pi in [&self.finalize, &self.downscale, &self.load, &self.upscale, &self.scan]
            .into_iter()
            .flatten()
        {
            pi.archive_mut().start_extraction();
        }
    }

    async fn join(&mut self) {
        for a in self.archives.take() {
            a.join().await;
        }
    }

    fn find_next_work(&mut self) {
        let work_pairs = [
            (&self.finalize, ManagerWork::Finalize),
            (&self.downscale, ManagerWork::Downscale),
            (&self.load, ManagerWork::Load),
            (&self.upscale, ManagerWork::Upscale),
            (&self.scan, ManagerWork::Scan),
        ];
        let mut new_values = Vec::new();

        'outer: for (pi, w) in work_pairs {
            let Some(pi) = pi else {
                continue;
            };

            let (_, work) = self.get_work_for_type(w, false);

            if let Some(p) = pi.p() {
                if pi.archive().has_work(p, &work) {
                    continue;
                }
            };

            let range = if self.modes.manga {
                self.current.wrapping_range(self.get_range(w), pi)
            } else {
                self.current.wrapping_range_in_archive(self.get_range(w), pi)
            };

            for npi in range {
                if let Some(p) = npi.p() {
                    if npi.archive().has_work(p, &work) {
                        new_values.push((w, Some(npi)));
                        continue 'outer;
                    }
                }
            }
            new_values.push((w, None));
        }

        for (w, npi) in new_values {
            self.set_next(w, npi);
        }
    }

    fn set_next(&mut self, work: ManagerWork, npi: Option<PageIndices>) {
        use ManagerWork::*;

        match work {
            Current => unreachable!(),
            Finalize => self.finalize = npi,
            Downscale => self.downscale = npi,
            Load => self.load = npi,
            Upscale => self.upscale = npi,
            Scan => self.scan = npi,
        }
    }

    fn has_work(&self, work: ManagerWork) -> bool {
        let (pi, w) = self.get_work_for_type(work, false);

        if let Some(pi) = pi {
            if let Some(p) = pi.p() { pi.archive().has_work(p, &w) } else { false }
        } else {
            false
        }
    }

    #[instrument(level = "trace", skip_all, fields(w = ?work, c = current_work))]
    async fn do_work(&self, work: ManagerWork, current_work: bool) -> Completion {
        let (pi, w) = self.get_work_for_type(work, current_work);

        let pi = pi.unwrap();
        pi.archive().do_work(pi.p().unwrap(), w).await
    }

    fn get_work_for_type(
        &self,
        work: ManagerWork,
        current_work: bool,
    ) -> (Option<&PageIndices>, Work) {
        use ManagerWork::*;

        match work {
            Current => (
                Some(&self.current),
                Work::Finalize(
                    self.modes.upscaling,
                    WorkParams {
                        park_before_scale: false,
                        jump_downscaling_queue: true,
                        extract_early: true,
                        target_res: self.target_res(),
                    },
                    &self.downscaler,
                ),
            ),
            Finalize => (
                self.finalize.as_ref(),
                Work::Finalize(
                    self.modes.upscaling,
                    WorkParams {
                        park_before_scale: current_work,
                        jump_downscaling_queue: false,
                        extract_early: false,
                        target_res: self.target_res(),
                    },
                    &self.downscaler,
                ),
            ),
            Downscale => (
                self.downscale.as_ref(),
                Work::Downscale(
                    self.modes.upscaling,
                    WorkParams {
                        park_before_scale: current_work,
                        jump_downscaling_queue: false,
                        extract_early: false,
                        target_res: self.target_res(),
                    },
                    &self.downscaler,
                ),
            ),
            Load => (
                self.load.as_ref(),
                Work::Load(
                    self.modes.upscaling,
                    WorkParams {
                        park_before_scale: current_work,
                        jump_downscaling_queue: false,
                        extract_early: false,
                        target_res: self.target_res(),
                    },
                ),
            ),
            Upscale => (self.upscale.as_ref(), Work::Upscale),
            Scan => (self.scan.as_ref(), Work::Scan),
        }
    }

    fn handle_completion(&mut self, comp: Completion, pi: PageIndices) {
        if comp == Completion::Scanned {
            if let CurrentIndices::Dual(OneOrTwo::One(c)) = &self.current {
                if Some(pi) == c.try_move_pages(Direction::Forwards, 1) {
                    self.current = CurrentIndices::Single(c.clone());
                    self.adjust_current_for_dual_page();
                }
            }
        }
    }

    fn idle_unload(&mut self) {
        let scroll_dim = if self.modes.display.vertical_pagination() {
            |r: Res| r.h
        } else {
            |r: Res| r.w
        };

        // TODO -- decide if this is good enough.
        self.downscaler.unload();

        // Minimum pages to keep before and after the singular current page
        let min_pages = match self.modes.display {
            DisplayMode::Single | DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip => {
                (1, 1)
            }
            DisplayMode::DualPage | DisplayMode::DualPageReversed => (2, 3),
        };

        // At least for single and strip modes keeping one backwards is likely to be enough even if
        // it's smaller than the scroll size.
        // Worst case the user sees a visible gap for a bit.
        let mut unload = self.current.try_move_pages(Direction::Backwards, 1);
        for i in 1..=CONFIG.preload_behind {
            let Some(pi) = unload.take() else {
                break;
            };

            if i > min_pages.0 {
                pi.unload();
            }
            unload = pi.try_move_pages(Direction::Backwards, 1);
        }

        let target_res = self.target_res();
        let mut remaining = match self.modes.display {
            DisplayMode::Single | DisplayMode::DualPage | DisplayMode::DualPageReversed => 0,
            DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip => {
                scroll_dim(self.target_res) + CONFIG.scroll_amount.get()
            }
        };

        let mut unload = self.current.try_move_pages(Direction::Forwards, 1);
        for i in 1..=self.preload_ahead {
            let Some(pi) = unload.take() else {
                break;
            };

            let consumed = if remaining == 0 {
                0
            } else if let Some(res) = self.get_displayable(&pi).layout().res() {
                scroll_dim(res.fit_inside(target_res))
            } else {
                remaining = 0;
                0
            };

            if i > min_pages.1 && remaining == 0 {
                pi.unload();
            } else {
                remaining = remaining.saturating_sub(consumed);
            }


            unload = pi.try_move_pages(Direction::Forwards, 1);
        }

        Self::send_gui(&self.gui_sender, GuiAction::IdleUnload);
    }

    fn get_range(&self, work: ManagerWork) -> RangeInclusive<isize> {
        use ManagerWork::*;

        let behind = CONFIG.preload_behind.try_into().map_or(isize::MIN, isize::saturating_neg);

        let ahead = match work {
            Current => unreachable!(),
            Finalize | Downscale | Load | Scan => {
                self.preload_ahead.try_into().unwrap_or(isize::MAX)
            }
            Upscale => max(self.preload_ahead, CONFIG.prescale).try_into().unwrap_or(isize::MAX),
        };
        behind..=ahead
    }

    fn next_id(&mut self) -> u16 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

async fn idle_sleep() {
    tokio::time::sleep(Duration::from_secs(CONFIG.idle_timeout.unwrap().get())).await
}
