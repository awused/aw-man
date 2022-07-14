use std::cell::RefCell;
use std::cmp::max;
use std::collections::VecDeque;
use std::future::Future;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::Duration;

use archive::{Archive, Work};
use flume::Receiver;
use gtk::glib;
use indices::PageIndices;
use tempfile::TempDir;
use tokio::select;
use tokio::task::LocalSet;

use self::files::is_natively_supported_image;
use crate::com::*;
use crate::config::{CONFIG, OPTIONS};
use crate::manager::actions::Action;
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

type Archives = Rc<RefCell<VecDeque<Archive>>>;

#[derive(Debug)]
struct Manager {
    archives: Archives,
    temp_dir: TempDir,
    gui_sender: glib::Sender<GuiAction>,

    target_res: Res,
    modes: Modes,

    old_state: GuiState,
    action_context: GuiActionContext,

    current: PageIndices,
    // The next pages to finalize, downscale, load, upscale, or scan. May not be extracted yet.
    finalize: Option<PageIndices>,
    downscale: Option<PageIndices>,
    load: Option<PageIndices>,
    upscale: Option<PageIndices>,
    scan: Option<PageIndices>,
}

pub fn run_manager(
    manager_receiver: Receiver<MAWithResponse>,
    gui_sender: glib::Sender<GuiAction>,
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
        let m = Manager::new(gui_sender, tmp_dir);
        run_local(m.run(manager_receiver));
        trace!("Exited manager thread");
    })
}

#[tokio::main(flavor = "current_thread")]
async fn run_local(f: impl Future<Output = ()>) {
    // Set up a LocalSet so that spawn_local can be used for cleanup tasks.
    let local = LocalSet::new();
    local.run_until(f).await;
    local.await;
}

impl Manager {
    fn new(gui_sender: glib::Sender<GuiAction>, temp_dir: TempDir) -> Self {
        let modes = Modes {
            manga: OPTIONS.manga,
            upscaling: OPTIONS.upscale,
            fit: Fit::Container,
            display: DisplayMode::default(),
        };
        let mut gui_state: GuiState = GuiState::default();

        // If we think the first file is an image, load it quickly before scanning the directory.
        // Scanning large, remote directories with a cold cache can be very slow.
        let mut try_early_open = |first_file: &PathBuf| {
            if is_natively_supported_image(first_file) {
                if let Ok(img) = image::open(first_file) {
                    // The alpha will not be premultiplied here.
                    // This is a practical tradeoff to display something as fast as possible, since
                    // most images do not have transparency and large images with transparency will
                    // be damaged by cairo's downscaling anyway.
                    let bgra = Bgra::from(img);
                    let img = ScaledImage { original_res: bgra.res, bgra };
                    gui_state.content = GuiContent::Single(Displayable::Image(img));
                    Self::send_gui(
                        &gui_sender,
                        GuiAction::State(gui_state.clone(), GuiActionContext::default()),
                    );
                }
            }
        };

        let (a, p) = match &OPTIONS.file_names[..] {
            [file] => {
                try_early_open(file);
                Archive::open(file.clone(), &temp_dir)
            }
            files @ [first, ..] => {
                try_early_open(first);
                Archive::open_fileset(files, &temp_dir)
            }
            [] => panic!("File name must be specified."),
        };

        let mut archives = VecDeque::new();
        archives.push_back(a);
        let archives = Rc::new(RefCell::from(archives));

        let current = PageIndices::new(0, p, archives.clone());

        let mut m = Self {
            archives,
            temp_dir,
            gui_sender,

            target_res: (0, 0).into(),
            modes,
            old_state: gui_state,
            action_context: GuiActionContext::default(),

            finalize: Some(current.clone()),
            downscale: Some(current.clone()),
            load: Some(current.clone()),
            upscale: modes.upscaling.then(|| current.clone()),
            scan: Some(current.clone()),
            current,
        };

        m.maybe_send_gui_state();

        m
    }

    async fn run(mut self, receiver: Receiver<MAWithResponse>) {
        if self.modes.manga {
            self.maybe_open_new_archives();
        }

        'main: loop {
            use ManagerWork::*;

            self.maybe_send_gui_state();

            self.find_next_work();

            // Check and start any extractions synchronously.
            // This will never block.
            self.start_extractions();

            let current_work = self.has_work(Current);
            let final_work = self.has_work(Finalize);
            let downscale_work = self.has_work(Downscale);
            let load_work = self.has_work(Load);
            let upscale_work = self.has_work(Upscale);
            let scan_work = self.has_work(Scan);

            let no_work = !(current_work
                || final_work
                || downscale_work
                || load_work
                || upscale_work
                || scan_work);

            let mut idle = false;

            'idle: loop {
                select! {
                    biased;
                    _ = closing::closed_fut() => break 'main,
                    mtg = receiver.recv_async() => {
                        match mtg {
                            Ok((mtg, context, r)) => {
                                debug!("{:?} {:?}", mtg, context);
                                self.action_context = context;
                                self.handle_action(mtg, r);
                            }
                            Err(_e) => {},
                        }
                    }
                    _ = self.do_work(Current, true), if current_work => {},
                    _ = self.do_work(Finalize, current_work), if final_work => {},
                    _ = self.do_work(Downscale, current_work), if downscale_work => {},
                    _ = self.do_work(Load, current_work), if load_work => {},
                    _ = self.do_work(Upscale, current_work), if upscale_work => {},
                    _ = self.do_work(Scan, current_work), if scan_work => {},
                    _ = idle_sleep(), if no_work && !idle && CONFIG.idle_timeout.is_some() => {
                        idle = true;
                        debug!("Entering idle mode.");
                        self.idle_unload();
                        continue 'idle;
                    }
                };

                if idle {
                    self.reset_indices();
                }

                break 'idle;
            }
        }

        closing::close();
        // TODO -- timeout here in case a decoder or extractor is stuck
        self.join().await
    }

    fn handle_action(&mut self, ma: ManagerAction, resp: Option<CommandResponder>) {
        use ManagerAction::*;

        match ma {
            Resolution(r) => {
                self.target_res = r;
                self.reset_indices();
            }
            MovePages(d, n) => self.move_pages(d, n),
            NextArchive => self.move_next_archive(),
            PreviousArchive => self.move_previous_archive(),
            Status => self.handle_command(Action::Status, resp),
            ListPages => self.handle_command(Action::ListPages, resp),
            Execute(s) => self.handle_command(Action::Execute(s), resp),
            ToggleUpscaling => {
                self.modes.upscaling = !self.modes.upscaling;
                self.reset_indices();
                self.maybe_open_new_archives();
            }
            ToggleManga => {
                self.modes.manga = !self.modes.manga;
                self.reset_indices();
                self.maybe_open_new_archives();
            }
            FitStrategy(s) => {
                self.modes.fit = s;
                self.reset_indices();
            }
            Display(dm) => {
                self.modes.display = dm;
            }
        }
    }

    fn target_res(&self) -> TargetRes {
        (self.target_res, self.modes.fit, self.modes.display).into()
    }

    // TODO -- this really could use a refactor
    fn build_gui_state(&self) -> GuiState {
        let archive = self.current.archive();
        let manga = self.modes.manga && archive.allow_multiple_archives();
        let p = self.current.p();

        let (displayable, page_name) = archive.get_displayable(p, self.modes.upscaling);
        let page_num = p.map_or(0, |p| p.0 + 1);
        let target_res = self.target_res();

        let move_page = |p: &PageIndices, d| {
            if manga {
                p.try_move_pages(d, 1)
            } else {
                let np = p.move_clamped_in_archive(d, 1);
                if p != &np { Some(np) } else { None }
            }
        };

        let scrollable = |p: &PageIndices| {
            p.archive()
                .get_displayable(p.p(), self.modes.upscaling)
                .0
                .scroll_res()
                .is_some()
        };


        let get_offscreen_content =
            |p: &PageIndices, d, remaining_preload, check_two: bool| match move_page(p, d) {
                None if manga && remaining_preload == 0 => OffscreenContent::Unknown,
                None => OffscreenContent::Nothing,
                Some(next) if scrollable(&next) => {
                    if !check_two {
                        OffscreenContent::Scrollable(ScrollableCount::OneOrMore)
                    } else {
                        match move_page(&next, d) {
                            None if manga && remaining_preload <= 1 => {
                                OffscreenContent::Scrollable(ScrollableCount::OneOrMore)
                            }
                            Some(n2) if scrollable(&n2) => {
                                OffscreenContent::Scrollable(ScrollableCount::TwoOrMore)
                            }
                            None | Some(_) => {
                                OffscreenContent::Scrollable(ScrollableCount::ExactlyOne)
                            }
                        }
                    }
                }
                Some(_) => OffscreenContent::Unscrollable,
            };

        let content = match (self.modes.display, displayable.scroll_res()) {
            (DisplayMode::Single, _)
            | (DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip, None) => {
                GuiContent::Single(displayable)
            }
            (DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip, Some(_)) => {
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

                while let Some(p) = move_page(&c, Direction::Backwards) {
                    let d = p.archive().get_displayable(p.p(), self.modes.upscaling).0;
                    let res = if let Some(res) = d.scroll_res() {
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

                visible.push(displayable);

                let mut c = self.current.clone();
                // This deliberately does not include the current page's scroll height.
                let mut remaining = scroll_dim(self.target_res) + CONFIG.scroll_amount.get();
                let mut forward_pages = 0;

                while let Some(n) = move_page(&c, Direction::Forwards) {
                    let d = n.archive().get_displayable(n.p(), self.modes.upscaling).0;
                    let res = if let Some(res) = d.scroll_res() {
                        res.fit_inside(target_res)
                    } else {
                        break;
                    };

                    visible.push(d);
                    c = n;
                    forward_pages += 1;
                    let sc = scroll_dim(res);
                    if remaining <= sc {
                        break;
                    }
                    remaining -= sc;
                }

                let next = get_offscreen_content(
                    &c,
                    Direction::Forwards,
                    CONFIG.preload_ahead.saturating_sub(forward_pages),
                    false,
                );

                GuiContent::Multiple {
                    prev: OffscreenContent::Unknown,
                    current_index,
                    visible,
                    next,
                }
            }
            (DisplayMode::DualPage | DisplayMode::DualPageReversed, current) => {
                let mut c = self.current.clone();

                let prev =
                    get_offscreen_content(&c, Direction::Backwards, CONFIG.preload_behind, true);

                let mut visible = Vec::with_capacity(2);
                visible.push(displayable);

                let mut preload_ahead = CONFIG.preload_ahead;

                if current.is_some() {
                    if let Some(next) = move_page(&c, Direction::Forwards) {
                        let d = next.archive().get_displayable(next.p(), self.modes.upscaling).0;
                        if d.scroll_res().is_some() {
                            visible.push(d);
                            preload_ahead = preload_ahead.saturating_sub(1);
                            c = next;
                        }
                    }
                }

                // TODO -- for now we really only have one "current" so we only need to check one
                // ahead
                let next = get_offscreen_content(&c, Direction::Forwards, preload_ahead, false);

                GuiContent::Multiple { prev, current_index: 0, visible, next }
            }
        };


        GuiState {
            content,
            page_num,
            page_name,
            archive_len: archive.page_count(),
            archive_name: archive.name(),
            modes: self.modes,
            target_res,
        }
    }

    fn maybe_send_gui_state(&mut self) {
        // Always take the context. If nothing happened we don't want it applying to later updates.
        let context = std::mem::take(&mut self.action_context);
        let gs = self.build_gui_state();

        if gs != self.old_state {
            Self::send_gui(&self.gui_sender, GuiAction::State(gs.clone(), context));
            self.old_state = gs;
        }
    }

    fn send_gui(gui_sender: &glib::Sender<GuiAction>, action: GuiAction) {
        match gui_sender.send(action) {
            Ok(_) => (),
            Err(e) => {
                error!("Sending to gui thread unexpectedly failed, {:?}", e);
                closing::close();
            }
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

    async fn join(self) {
        for a in self.archives.take() {
            a.join().await;
        }
        self.temp_dir
            .close()
            .unwrap_or_else(|e| error!("Error dropping manager temp dir: {:?}", e));
    }

    fn find_next_work(&mut self) {
        // TODO -- could override preload settings in continuous scrolling mode
        let work_pairs = [
            (&self.finalize, ManagerWork::Finalize),
            (&self.downscale, ManagerWork::Downscale),
            (&self.load, ManagerWork::Load),
            (&self.upscale, ManagerWork::Upscale),
            (&self.scan, ManagerWork::Scan),
        ];
        let mut new_values = Vec::new();

        'outer: for (pi, w) in work_pairs {
            let (_, work) = self.get_work_for_type(w, false);

            if pi.is_none() {
                continue;
            }

            let range = if self.modes.manga {
                self.current.wrapping_range(get_range(w))
            } else {
                self.current.wrapping_range_in_archive(get_range(w))
            };

            for npi in range {
                if let Some(p) = npi.p() {
                    if npi.archive().has_work(p, work) {
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
            if let Some(p) = pi.p() { pi.archive().has_work(p, w) } else { false }
        } else {
            false
        }
    }

    async fn do_work(&self, work: ManagerWork, current_work: bool) {
        let (pi, w) = self.get_work_for_type(work, current_work);

        if let Some(pi) = pi {
            if let Some(p) = pi.p() {
                pi.archive().do_work(p, w).await
            } else {
                unreachable!();
            }
        } else {
            unreachable!();
        }
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

    fn idle_unload(&self) {
        let scroll_dim = if self.modes.display.vertical_pagination() {
            |r: Res| r.h
        } else {
            |r: Res| r.w
        };

        let min_pages = match self.modes.display {
            DisplayMode::Single | DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip => 1,
            DisplayMode::DualPage | DisplayMode::DualPageReversed => 2,
        };

        // At least for single and strip modes keeping one backwards is likely to be enough even if
        // it's smaller than the scroll size.
        // Worst case the user sees a visible gap for a bit.
        let mut unload = self.current.try_move_pages(Direction::Backwards, 1);
        for i in 1..=CONFIG.preload_behind {
            match unload.take() {
                Some(pi) => {
                    if i > min_pages {
                        pi.unload();
                    }
                    unload = pi.try_move_pages(Direction::Backwards, 1);
                }
                None => break,
            }
        }

        let target_res = self.target_res();
        let mut remaining = match self.modes.display {
            DisplayMode::Single | DisplayMode::DualPage | DisplayMode::DualPageReversed => 0,
            DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip => {
                scroll_dim(self.target_res) + CONFIG.scroll_amount.get()
            }
        };

        let mut unload = self.current.try_move_pages(Direction::Forwards, 1);
        for i in 1..=CONFIG.preload_ahead {
            match unload.take() {
                Some(pi) => {
                    let consumed = if remaining == 0 {
                        0
                    } else if let Some(res) =
                        pi.archive().get_displayable(pi.p(), self.modes.upscaling).0.scroll_res()
                    {
                        scroll_dim(res.fit_inside(target_res))
                    } else {
                        remaining = 0;
                        0
                    };

                    if i > min_pages && remaining == 0 {
                        pi.unload();
                    } else {
                        remaining = remaining.saturating_sub(consumed);
                    }


                    unload = pi.try_move_pages(Direction::Forwards, 1);
                }
                None => break,
            }
        }
    }
}

async fn idle_sleep() {
    tokio::time::sleep(Duration::from_secs(CONFIG.idle_timeout.unwrap().get())).await
}

fn get_range(work: ManagerWork) -> RangeInclusive<isize> {
    use ManagerWork::*;

    let behind = CONFIG.preload_behind.try_into().map_or(isize::MIN, isize::saturating_neg);

    let ahead = match work {
        Current => unreachable!(),
        Finalize | Downscale | Load | Scan => CONFIG.preload_ahead.try_into().unwrap_or(isize::MAX),
        Upscale => max(CONFIG.preload_ahead, CONFIG.prescale).try_into().unwrap_or(isize::MAX),
    };
    behind..=ahead
}
