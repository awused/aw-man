use std::cell::{Ref, RefCell, RefMut};
use std::cmp::max;
use std::collections::VecDeque;
use std::future::Future;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::thread::JoinHandle;

use archive::{Archive, Work};
use flume::Receiver;
use futures_util::FutureExt;
use gtk::glib;
use indices::{PageIndices, AI};
use tempfile::TempDir;
use tokio::select;
use tokio::task::LocalSet;

use self::files::is_natively_supported_image;
use self::indices::PI;
use crate::com::*;
use crate::config::{CONFIG, FILE_NAME, OPTIONS};
use crate::manager::actions::Action;
use crate::{closing, spawn_thread, Fut};

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

    current: PageIndices,
    // The next pages to finalize, load, upscale, or scan, which may not be extracted yet.
    finalize: Option<PageIndices>,
    downscale: Option<PageIndices>,
    load: Option<PageIndices>,
    upscale: Option<PageIndices>,
    scan: Option<PageIndices>,
}

pub(super) fn run_manager(
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
    })
}

#[tokio::main(flavor = "current_thread")]
async fn run_local<F: Future<Output = Fut<()>>>(f: F) {
    // Set up a LocalSet so that spawn_local can be used for cleanup tasks.
    let local = LocalSet::new();
    let cleanup = local.run_until(f).await;
    local.await;
    // Cleaning up reuses normal code paths that can spawn_local themselves.
    // Wrap it in a local_set so they can do their jobs.
    let cleanup_set = LocalSet::new();
    cleanup_set.run_until(cleanup).await;
    cleanup_set.await;
}

impl Manager {
    fn new(gui_sender: glib::Sender<GuiAction>, temp_dir: TempDir) -> Self {
        let modes = Modes {
            manga: OPTIONS.manga,
            upscaling: OPTIONS.upscale,
            fit: Fit::Container,
        };
        let mut gui_state: GuiState = GuiState::default();

        // If we think the first file is an image, load it quickly before scanning the directory.
        // Scanning large, remote directories with a cold cache can be very slow.
        if is_natively_supported_image(&*FILE_NAME) {
            if let Ok(img) = image::open(&*FILE_NAME) {
                let bgra = Bgra::from(img);
                let img = ScaledImage {
                    original_res: bgra.res,
                    bgra,
                };
                gui_state.displayable = Displayable::Image(img);
                Self::send_gui(&gui_sender, GuiAction::State(gui_state.clone()));
            }
        }

        let (a, p) = Archive::open(FILE_NAME.to_path_buf(), &temp_dir);

        let mut archives = VecDeque::new();
        archives.push_back(a);
        let archives = Rc::new(RefCell::from(archives));

        let current = PageIndices::new(0, p, archives.clone());

        let nu = if modes.upscaling {
            Some(current.clone())
        } else {
            None
        };

        let mut m = Self {
            archives,
            temp_dir,
            gui_sender,

            target_res: (0, 0).into(),
            modes,
            old_state: gui_state,

            finalize: Some(current.clone()),
            downscale: Some(current.clone()),
            load: Some(current.clone()),
            upscale: nu,
            scan: Some(current.clone()),
            current,
        };

        m.maybe_send_gui_state();

        m
    }

    async fn run(mut self, receiver: Receiver<MAWithResponse>) -> Fut<()> {
        if self.modes.manga {
            self.maybe_open_new_archives();
        }

        loop {
            use ManagerWork::*;

            self.find_next_work();

            // Check and start any extractions synchronously.
            // This will never block.
            self.start_extractions();

            self.maybe_send_gui_state();

            let current_work = self.has_work(Current);

            select! {
                biased;
                _ = closing::closed_fut() => break,
                mtg = receiver.recv_async() => {
                    match mtg {
                        Ok((mtg, r)) => {
                            debug!("{:?}", mtg);
                            self.handle_action(mtg, r);
                        }
                        Err(_e) => {},
                    }
                }
                _ = self.do_work(Current, true), if current_work => {},
                _ = self.do_work(Finalize, current_work), if self.has_work(Finalize) => {},
                _ = self.do_work(Downscale, current_work), if self.has_work(Downscale) => {},
                _ = self.do_work(Load, current_work), if self.has_work(Load) => {},
                _ = self.do_work(Upscale, current_work), if self.has_work(Upscale) => {},
                _ = self.do_work(Scan, current_work), if self.has_work(Scan) => {},
            };
        }

        closing::close();
        self.join().boxed_local()
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
        }
    }

    fn build_gui_state(&self) -> GuiState {
        let archive = self.get_archive(self.current.a());
        let p = self.current.p();

        let (displayable, page_name) = archive.get_displayable(p, self.modes.upscaling);
        let mut page_num = p.unwrap_or(PI(0)).0;

        if archive.page_count() > 0 {
            page_num += 1;
        }

        GuiState {
            displayable,
            page_num,
            page_name,
            archive_len: archive.page_count(),
            archive_name: archive.name(),
            modes: self.modes,
            target_res: TargetRes {
                res: self.target_res,
                fit: self.modes.fit,
            },
        }
    }

    fn maybe_send_gui_state(&mut self) {
        let gs = self.build_gui_state();
        if gs != self.old_state {
            Self::send_gui(&self.gui_sender, GuiAction::State(gs.clone()));
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
        let p = self.current.p();
        self.get_archive_mut(self.current.a()).start_extraction(p);

        // False positive
        #[allow(clippy::manual_flatten)]
        for pi in [
            &self.finalize,
            &self.downscale,
            &self.load,
            &self.upscale,
            &self.scan,
        ] {
            if let Some(pi) = pi {
                self.get_archive_mut(pi.a()).start_extraction(pi.p());
            }
        }
    }

    fn get_archive(&self, a: AI) -> Ref<Archive> {
        Ref::map(self.archives.borrow(), |archives| {
            archives
                .get(a.0)
                .unwrap_or_else(|| panic!("Tried to get non-existent archive {:?}", a))
        })
    }

    fn get_archive_mut(&self, a: AI) -> RefMut<Archive> {
        RefMut::map(self.archives.borrow_mut(), |archives| {
            archives
                .get_mut(a.0)
                .unwrap_or_else(|| panic!("Tried to get non-existent archive {:?}", a))
        })
    }

    async fn join(self) {
        for a in self.archives.take().drain(..) {
            a.join().await;
        }
        self.temp_dir
            .close()
            .unwrap_or_else(|e| error!("Error dropping manager temp dir: {:?}", e));
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
            let (_, work) = self.get_work_for_type(w, false);

            if pi.is_none() {
                continue;
            }

            let range = if self.modes.manga {
                self.current.wrapping_range(Self::get_range(w))
            } else {
                self.current.wrapping_range_in_archive(Self::get_range(w))
            };

            for npi in range {
                if let Some(page) = npi.p() {
                    if self.get_archive(npi.a()).has_work(page, work) {
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

    fn get_range(work: ManagerWork) -> RangeInclusive<isize> {
        use ManagerWork::*;

        let behind = CONFIG
            .preload_behind
            .try_into()
            .map_or(isize::MIN, isize::saturating_neg);

        let ahead = match work {
            Current => unreachable!(),
            Finalize | Downscale | Load | Scan => {
                CONFIG.preload_ahead.try_into().unwrap_or(isize::MAX)
            }
            Upscale => max(CONFIG.preload_ahead, CONFIG.prescale)
                .try_into()
                .unwrap_or(isize::MAX),
        };
        behind..=ahead
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
            if let Some(p) = pi.p() {
                self.get_archive(pi.a()).has_work(p, w)
            } else {
                false
            }
        } else {
            false
        }
    }

    async fn do_work(&self, work: ManagerWork, current_work: bool) {
        let (pi, w) = self.get_work_for_type(work, current_work);

        if let Some(pi) = pi {
            if let Some(p) = pi.p() {
                self.get_archive(pi.a()).do_work(p, w).await
            } else {
                unreachable!();
            }
        } else {
            unreachable!();
        }
    }

    const fn get_work_for_type(
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
                        target_res: TargetRes {
                            res: self.target_res,
                            fit: self.modes.fit,
                        },
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
                        target_res: TargetRes {
                            res: self.target_res,
                            fit: self.modes.fit,
                        },
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
                        target_res: TargetRes {
                            res: self.target_res,
                            fit: self.modes.fit,
                        },
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
                        target_res: TargetRes {
                            res: self.target_res,
                            fit: self.modes.fit,
                        },
                    },
                ),
            ),
            Upscale => (self.upscale.as_ref(), Work::Upscale),
            Scan => (self.scan.as_ref(), Work::Scan),
        }
    }
}
