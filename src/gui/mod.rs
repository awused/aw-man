use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ahash::AHashMap;
use flume::{Receiver, Sender};
use glium_area::GliumArea;
use gtk::gdk::ModifierType;
use gtk::glib::{ControlFlow, Propagation};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, Align};
use once_cell::unsync::OnceCell;

use self::layout::{LayoutContents, LayoutManager};
use self::prog::Progress;
use super::com::*;
use crate::state_cache::{save_settings, State, STATE};
use crate::{closing, config};

mod glium_area;
mod input;
mod layout;
mod menu;
mod prog;
#[cfg(windows)]
mod windows;

pub static WINDOW_ID: once_cell::sync::OnceCell<String> = once_cell::sync::OnceCell::new();

// The Rc<> ends up more ergonomic in most cases but it's too much of a pain to pass things into
// GObjects.
thread_local!(static GUI: OnceCell<Rc<Gui>> = OnceCell::default());

#[derive(Debug, Copy, Clone, Default)]
struct WindowState {
    maximized: bool,
    fullscreen: bool,
    // This stores the size of the window when it isn't fullscreen or maximized.
    memorized_size: Res,
}

#[derive(Debug, Default)]
enum SpinnerState {
    #[default]
    Hidden,
    Waiting(glib::SourceId),
    Visible,
}

impl SpinnerState {
    pub fn start(&mut self, gui: &Rc<Gui>) {
        match self {
            Self::Waiting(_) | Self::Visible => {}
            Self::Hidden => {
                let g = gui.clone();
                let source = glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    trace!("Show loading spinner");
                    g.spinner_widget.start();
                    g.loading_spinner.replace(Self::Visible);
                });
                *self = Self::Waiting(source);
            }
        }
    }

    pub fn hide(&mut self, gui: &Rc<Gui>) {
        match std::mem::take(self) {
            Self::Hidden => {}
            Self::Waiting(source_id) => {
                source_id.remove();
            }
            Self::Visible => {
                trace!("Hide loading spinner");
                gui.spinner_widget.stop();
            }
        }
    }
}


#[derive(Debug)]
struct Gui {
    window: gtk::ApplicationWindow,
    win_state: Cell<WindowState>,
    overlay: gtk::Overlay,
    canvas: GliumArea,
    menu: OnceCell<menu::GuiMenu>,

    page_num: gtk::Label,
    page_name: gtk::Label,
    archive_name: gtk::Label,
    mode: gtk::Label,
    zoom_level: gtk::Label,
    edge_indicator: gtk::Label,
    progress: RefCell<Progress>,
    bottom_bar: gtk::Box,
    label_updates: RefCell<Option<glib::SourceId>>,

    loading_spinner: RefCell<SpinnerState>,
    spinner_widget: gtk::Spinner,

    state: RefCell<GuiState>,
    bg: Cell<gdk::RGBA>,

    layout_manager: RefCell<LayoutManager>,
    // Called "pad" scrolling to differentiate it with continuous scrolling between pages.
    pad_scrolling: Cell<bool>,
    // While we try to make communication with the manager stateless, it's not perfect.
    // Sometimes there's ongoing work and we want to wait for it to finish before we can apply
    // scrolling.
    pending_scroll: Cell<ScrollMotionTarget>,
    animation_playing: Cell<bool>,

    last_action: Cell<Option<Instant>>,
    first_content_paint: OnceCell<()>,
    open_dialogs: RefCell<input::OpenDialogs>,

    shortcuts: AHashMap<ModifierType, AHashMap<gdk::Key, String>>,

    manager_sender: Sender<MAWithResponse>,

    #[cfg(windows)]
    win32: windows::WindowsEx,
}

pub fn run(manager_sender: Sender<MAWithResponse>, gui_receiver: Receiver<GuiAction>) {
    glium_area::init();

    let application = gtk::Application::new(
        Some("awused.aw-man"),
        gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::NON_UNIQUE,
    );

    let gui_to_manager = Cell::from(Some(manager_sender));
    let gui_receiver = Cell::from(Some(gui_receiver));

    application.connect_activate(move |a| {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(include_str!("style.css"));
        // We give the CssProvider to the default screen so the CSS rules we added
        // can be applied to our window.
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("Error initializing gtk css provider."),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        Gui::new(a, gui_to_manager.take().unwrap(), gui_receiver.take().unwrap());
    });

    // This is a stupid hack around glib trying to exert exclusive control over the command line.
    application.connect_command_line(|a, _| {
        a.activate();
        0
    });

    let _cod = closing::CloseOnDrop::default();
    application.run();
}

impl Gui {
    pub fn new(
        application: &gtk::Application,
        manager_sender: Sender<MAWithResponse>,
        gui_receiver: Receiver<GuiAction>,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(application);

        let rc = Rc::new_cyclic(|weak| Self {
            window,
            win_state: Cell::default(),
            overlay: gtk::Overlay::new(),
            canvas: GliumArea::new(),
            menu: OnceCell::default(),

            page_num: gtk::Label::new(None),
            page_name: gtk::Label::new(None),
            archive_name: gtk::Label::new(None),
            mode: gtk::Label::new(None),
            zoom_level: gtk::Label::new(Some("100%")),
            edge_indicator: gtk::Label::new(None),
            progress: RefCell::default(),
            bottom_bar: gtk::Box::new(gtk::Orientation::Horizontal, 15),
            label_updates: RefCell::default(),

            loading_spinner: RefCell::default(),
            spinner_widget: gtk::Spinner::new(),

            state: RefCell::default(),
            bg: Cell::new(config::CONFIG.background_colour.unwrap_or(gdk::RGBA::BLACK)),

            layout_manager: RefCell::new(LayoutManager::new(weak.clone())),
            pad_scrolling: Cell::default(),
            pending_scroll: Cell::default(),
            animation_playing: Cell::new(true),

            last_action: Cell::default(),
            first_content_paint: OnceCell::default(),
            open_dialogs: RefCell::default(),

            shortcuts: Self::parse_shortcuts(),

            manager_sender,

            #[cfg(windows)]
            win32: windows::WindowsEx::default(),
        });


        rc.menu.set(menu::GuiMenu::new(&rc)).unwrap();

        let g = rc.clone();
        GUI.with(|cell| cell.set(g).expect("Trying to set OnceCell twice"));

        #[cfg(windows)]
        let g = rc.clone();
        application.connect_shutdown(move |_a| {
            info!("Shutting down application");
            #[cfg(windows)]
            g.win32.teardown();

            closing::close();
        });

        // We only support running once so this should never panic.
        // If there is a legitimate use for activating twice, send on the other channel.
        // There are also cyclical references that are annoying to clean up so this Gui object will
        // live forever, but that's fine since the application will exit when the Gui exits.
        let g = rc.clone();
        let ctx = glib::MainContext::ref_thread_default();
        ctx.spawn_local_with_priority(glib::Priority::HIGH, async move {
            while let Ok(gu) = gui_receiver.recv_async().await {
                g.handle_update(gu);
            }
        });

        rc.setup();

        // Grab the window ID to be passed to external commands.
        #[cfg(target_os = "linux")]
        {
            if let Ok(xsuf) = rc.window.surface().dynamic_cast::<gdk4_x11::X11Surface>() {
                WINDOW_ID.set(xsuf.xid().to_string()).unwrap();
            }
        }

        // Hack around https://github.com/gtk-rs/gtk4-rs/issues/520
        #[cfg(windows)]
        rc.win32.setup(rc.clone());

        rc.loading_spinner.borrow_mut().start(&rc);
        rc
    }

    fn setup(self: &Rc<Self>) {
        self.layout();
        self.setup_interaction();


        let g = self.clone();
        self.canvas.connect_resize(move |_, width, height| {
            // Resolution change is also a user action.
            g.last_action.set(Some(Instant::now()));

            assert!(width >= 0 && height >= 0, "Can't have negative width or height");

            let s = g.state.borrow();
            let t_res = (width, height, s.modes.fit, s.modes.display).into();
            g.update_scroll_container(t_res);

            g.send_manager((
                ManagerAction::Resolution(t_res.res),
                GuiActionContext::default(),
                None,
            ));
        });

        let g = self.clone();
        self.window.connect_close_request(move |w| {
            let s = g.win_state.get();
            let size = if s.maximized || s.fullscreen {
                s.memorized_size
            } else {
                (w.width(), w.height()).into()
            };
            save_settings(State { size, maximized: w.is_maximized() });
            Propagation::Proceed
        });

        let g = self.clone();
        self.window.connect_maximized_notify(move |_w| {
            g.window_state_changed();
        });

        let g = self.clone();
        self.window.connect_fullscreened_notify(move |_w| {
            g.window_state_changed();
        });

        self.window.set_visible(true);
    }

    fn window_state_changed(self: &Rc<Self>) {
        let mut s = self.win_state.get();

        #[cfg(unix)]
        let fullscreen = self.window.is_fullscreen();
        #[cfg(windows)]
        let fullscreen = self.win32.is_fullscreen();

        let maximized = self.window.is_maximized();

        // These callbacks run after the state has changed.
        if !s.maximized && !s.fullscreen {
            s.memorized_size = (self.window.width(), self.window.height()).into();
        }

        s.maximized = maximized;
        s.fullscreen = fullscreen;
        self.win_state.set(s);
    }

    fn layout(self: &Rc<Self>) {
        self.window.remove_css_class("background");
        self.window.set_title(Some("aw-man"));
        self.window.set_default_size(800, 600);

        if let Some(saved) = &*STATE {
            // Don't create very tiny windows.
            if saved.size.w >= 100 && saved.size.h >= 100 {
                self.window.set_default_size(saved.size.w as i32, saved.size.h as i32);
                let mut ws = self.win_state.get();
                ws.memorized_size = saved.size;
                self.win_state.set(ws);
            }

            if saved.maximized {
                self.window.set_maximized(true);
            }
        }

        // TODO -- three separate indicators?
        self.mode.set_width_chars(3);
        self.mode.set_xalign(1.0);

        self.canvas.set_hexpand(true);
        self.canvas.set_vexpand(true);

        self.overlay.set_child(Some(&self.canvas));

        self.bottom_bar.add_css_class("background");
        self.bottom_bar.add_css_class("bottom-bar");

        self.page_name.set_wrap(true);
        self.archive_name.set_wrap(true);

        // Left side -- right to left
        self.bottom_bar.prepend(&self.page_name);
        self.bottom_bar.prepend(&gtk::Label::new(Some("|")));
        self.bottom_bar.prepend(&self.archive_name);
        self.bottom_bar.prepend(&gtk::Label::new(Some("|")));
        self.bottom_bar.prepend(&self.page_num);

        self.progress.borrow_mut().layout(self);

        // TODO -- replace with center controls ?
        // self.edge_indicator.set_hexpand(true);
        self.edge_indicator.set_halign(Align::End);

        self.spinner_widget.set_halign(Align::Start);
        self.spinner_widget.set_valign(Align::Start);

        self.spinner_widget.set_size_request(50, 50);

        self.overlay.add_overlay(&self.spinner_widget);

        // Right side - left to right
        self.bottom_bar.append(&self.edge_indicator);
        self.bottom_bar.append(&self.zoom_level);
        self.bottom_bar.append(&gtk::Label::new(Some("|")));
        self.bottom_bar.append(&self.mode);

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);

        vbox.prepend(&self.overlay);
        vbox.append(&self.bottom_bar);

        self.window.set_child(Some(&vbox));
    }

    fn handle_update(self: &Rc<Self>, gu: GuiAction) -> ControlFlow {
        use crate::com::GuiAction::*;

        match gu {
            State(s, actx) => {
                match self.window.title() {
                    Some(t) if t.as_str().strip_suffix(" - aw-man") == Some(&s.archive_name) => {}
                    _ => self.window.set_title(Some(&format!("{} - aw-man", s.archive_name))),
                };

                // TODO -- determine true visible pages. Like "1-3/20" or even "19/20 - 3/10"
                let old_s = self.state.replace(s);
                let mut new_s = self.state.borrow_mut();

                self.menu.get().unwrap().diff_state(&old_s, &new_s);

                self.update_displayable(old_s, &mut new_s, actx);
                drop(new_s);


                // Label updates add significant latency, especially with longer file names.
                // Drawing the current page is more important, as is maintaining smooth
                // scrolling.
                let mut lub = self.label_updates.borrow_mut();
                if lub.is_none() {
                    let g = self.clone();

                    let update = move || {
                        g.update_labels();
                        g.label_updates.take().unwrap();
                    };

                    let id = if self.is_scrolling() {
                        // Allow updates to go through after a delay so as not to introduce
                        // unnecessary judder. May have gotten worse with a gtk4 update.
                        glib::timeout_add_local_once(Duration::from_millis(500), update)
                    } else {
                        glib::idle_add_local_once(update)
                    };

                    *lub = Some(id);
                }
            }
            Action(a, fin) => {
                self.run_command(&a, fin);
            }
            BlockingWork => {
                self.loading_spinner.borrow_mut().start(self);
            }
            IdleUnload => {
                self.canvas.inner().idle_unload();
            }
            Quit => {
                self.window.close();
                closing::close();
                return ControlFlow::Break;
            }
        }
        ControlFlow::Continue
    }

    fn update_displayable(
        self: &Rc<Self>,
        old_s: GuiState,
        new_s: &mut GuiState,
        actx: GuiActionContext,
    ) {
        use Displayable::*;
        use GuiContent as GC;

        if old_s.target_res != new_s.target_res {
            self.update_scroll_container(new_s.target_res);
            self.canvas.queue_draw();
        }

        if new_s.content.ongoing_work() {
            self.loading_spinner.borrow_mut().start(self);
        } else {
            self.loading_spinner.borrow_mut().hide(self);
        }

        let scroll_motion = if actx.scroll_motion_target != ScrollMotionTarget::Maintain {
            actx.scroll_motion_target
        } else {
            self.pending_scroll.take()
        };

        if old_s.content == new_s.content && old_s.modes.display == new_s.modes.display {
            return;
        }

        // Keep something visible, at least for this basic case.
        // Gets a little trickier with dual pages or strip mode.
        if let GC::Single { current: Pending | Loading { .. }, .. } = new_s.content {
            if new_s.archive_id == old_s.archive_id {
                new_s.content = old_s.content;
                self.pending_scroll.set(scroll_motion);
                return;
            }
        }

        match &new_s.content {
            // https://github.com/rust-lang/rust/issues/51114
            GC::Single { current: d, .. } | GC::Dual { visible: OneOrTwo::One(d), .. }
                if d.layout().res().is_some() =>
            {
                self.update_scroll_contents(
                    LayoutContents::Single(d.unwrap_res()),
                    scroll_motion,
                    new_s.modes.display,
                );
            }
            GC::Dual {
                visible: OneOrTwo::Two(first, second), ..
            } => {
                self.update_scroll_contents(
                    LayoutContents::Dual(first.unwrap_res(), second.unwrap_res()),
                    scroll_motion,
                    new_s.modes.display,
                );
            }
            GC::Strip { current_index, visible, .. } if visible[0].layout().res().is_some() => {
                let visible = visible.iter().map(Displayable::unwrap_res).collect();

                self.update_scroll_contents(
                    LayoutContents::Strip { current_index: *current_index, visible },
                    scroll_motion,
                    new_s.modes.display,
                );
            }
            // We should never have two visible pages where the first doesn't have a layout_res
            GC::Dual { visible, .. } if visible.second().is_some() => unreachable!(),
            // We should never have a strip with more than one element when the first has no
            // layout_res
            GC::Strip { visible, .. } if visible.len() > 1 => unreachable!(),
            GC::Single { .. } | GC::Dual { .. } | GC::Strip { .. }
                if new_s.content.ongoing_work() =>
            {
                // We don't have any layout information to work with, but there is ongoing work.
                // Store the scroll motion target for later.
                self.pending_scroll.set(scroll_motion);
            }
            GC::Single { .. } | GC::Dual { .. } | GC::Strip { .. } => {
                self.zero_scroll();
            }
        }

        self.canvas.inner().update_displayed(&new_s.content);

        self.canvas.queue_draw();
    }

    fn update_labels(&self) {
        let new_s = self.state.borrow();
        self.page_num.set_text(&format!("{} / {}", new_s.page_num, new_s.archive_len));
        self.archive_name.set_text(&new_s.archive_name);
        self.page_name.set_text(&new_s.page_name);
        self.mode.set_text(&new_s.modes.gui_str());

        let zoom = self.get_zoom_level();

        let zoom = format!("{zoom:>3}%");
        if zoom != self.zoom_level.text().as_str() {
            self.zoom_level.set_text(&zoom);
        }
    }

    fn get_zoom_level(&self) -> f64 {
        use Displayable::*;
        use GuiContent::*;

        let db = self.state.borrow();

        let (current, index) = match &db.content {
            Single { current, .. } => (current, 0),
            Dual { visible, .. } => (visible.first(), 0),
            Strip { visible, current_index, .. } => (&visible[*current_index], *current_index),
        };

        let width = match current {
            Error(_) | Pending => return 100.0,
            Image(img) => img.original_res.w,
            Animation(ac) => ac.frames()[0].0.res.w,
            Loading { original_res: r, .. } => r.w,
            Video(_vid) => {
                // TODO -- just scan videos even if preloading isn't ready.
                return 100.0;

                // Special case until videos are scanned and available for regular layout.
                // let mut t_res = self.state.borrow().target_res;
                // t_res.fit = Fit::Container;

                // return if vid.width() == 0 {
                //     100.0
                // } else {
                //     let stream = vid.media_stream().unwrap();
                //     let ores: Res = (stream.intrinsic_width(), stream.intrinsic_height()).into();
                //     let res = ores.fit_inside(t_res);
                //     (res.w as f64 / ores.w as f64 * 100.0).round()
                // };
            }
        };

        let layout = self.layout_manager.borrow().layout_iter().nth(index).unwrap();

        if width == 0 {
            100.0
        } else {
            (layout.2.w as f64 / width as f64 * 100.0).round()
        }
    }

    fn send_manager(&self, val: MAWithResponse) {
        if let Err(e) = self.manager_sender.send(val) {
            if !closing::closed() {
                // This should never happen
                closing::fatal(format!("Sending to manager unexpectedly failed. {e}"));
                self.window.close();
            }
        }
    }
}
