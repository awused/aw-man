mod glium_area;
mod input;
mod layout;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use ahash::AHashMap;
use flume::Sender;
use glium_area::GliumArea;
use gtk::gdk::ModifierType;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, Align};
use once_cell::unsync::OnceCell;

use self::layout::{LayoutContents, LayoutManager};
use super::com::*;
use crate::{closing, config};

pub static WINDOW_ID: once_cell::sync::OnceCell<String> = once_cell::sync::OnceCell::new();

// The Rc<> ends up more ergonomic in most cases but it's too much of a pain to pass things into
// GObjects.
thread_local!(static GUI: OnceCell<Rc<Gui>> = OnceCell::default());

#[derive(Debug)]
pub struct Gui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    canvas: GliumArea,

    progress: gtk::Label,
    page_name: gtk::Label,
    archive_name: gtk::Label,
    mode: gtk::Label,
    zoom_level: gtk::Label,
    edge_indicator: gtk::Label,
    bottom_bar: gtk::Box,

    state: RefCell<GuiState>,
    bg: Cell<gdk::RGBA>,

    layout_manager: RefCell<LayoutManager>,
    // Called "pad" scrolling to differentiate it with continuous scrolling between pages.
    pad_scrolling: Cell<bool>,
    drop_next_scroll: Cell<bool>,
    animation_playing: Cell<bool>,

    last_action: Cell<Option<Instant>>,
    first_content_paint: OnceCell<()>,
    open_dialogs: RefCell<AHashMap<input::Dialogs, gtk::Window>>,

    shortcuts: AHashMap<ModifierType, AHashMap<gdk::Key, String>>,

    manager_sender: Rc<Sender<MAWithResponse>>,
}

pub fn run(manager_sender: flume::Sender<MAWithResponse>, gui_receiver: glib::Receiver<GuiAction>) {
    glium_area::init();

    let application = gtk::Application::new(
        Some("awused.aw-man"),
        gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::NON_UNIQUE,
    );

    let gui_to_manager = Rc::from(manager_sender);
    let gui_receiver = Rc::from(Cell::from(Some(gui_receiver)));
    application.connect_activate(move |a| {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(include_bytes!("style.css"));
        // We give the CssProvider to the default screen so the CSS rules we added
        // can be applied to our window.
        gtk::StyleContext::add_provider_for_display(
            &gdk::Display::default().expect("Error initializing gtk css provider."),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        Gui::new(a, gui_to_manager.clone(), gui_receiver.clone());
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
        manager_sender: Rc<flume::Sender<MAWithResponse>>,
        gui_receiver: Rc<Cell<Option<glib::Receiver<GuiAction>>>>,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(application);

        let rc = Rc::new_cyclic(|weak| Self {
            window,
            overlay: gtk::Overlay::new(),
            canvas: GliumArea::new(),

            progress: gtk::Label::new(None),
            page_name: gtk::Label::new(None),
            archive_name: gtk::Label::new(None),
            mode: gtk::Label::new(None),
            zoom_level: gtk::Label::new(Some("100%")),
            edge_indicator: gtk::Label::new(None),
            bottom_bar: gtk::Box::new(gtk::Orientation::Horizontal, 15),

            state: RefCell::default(),
            bg: Cell::new(config::CONFIG.background_colour.unwrap_or(gdk::RGBA::BLACK)),

            layout_manager: RefCell::new(LayoutManager::new(weak.clone())),
            pad_scrolling: Cell::default(),
            drop_next_scroll: Cell::default(),
            animation_playing: Cell::new(true),

            last_action: Cell::default(),
            first_content_paint: OnceCell::default(),
            open_dialogs: RefCell::default(),

            shortcuts: Self::parse_shortcuts(),

            manager_sender,
        });

        let g = rc.clone();
        GUI.with(|cell| cell.set(g).expect("Trying to set OnceCell twice"));

        application.connect_shutdown(move |_a| {
            info!("Shutting down application");
            closing::close();
        });

        // We only support running once so this should never panic.
        // If there is a legitimate use for activating twice, send on the other channel.
        // There are also cyclical references that are annoying to clean up so this Gui object will
        // live forever, but that's fine since the application will exit when the Gui exits.
        let g = rc.clone();
        gui_receiver
            .take()
            .expect("Activated application twice. This should never happen.")
            .attach(None, move |gu| g.handle_update(gu));

        rc.setup();

        // Grab the window ID to be passed to external commands.
        #[cfg(target_family = "unix")]
        {
            if let Ok(xsuf) = rc.window.surface().dynamic_cast::<gdk4_x11::X11Surface>() {
                WINDOW_ID.set(xsuf.xid().to_string()).expect("Impossible");
            }
        }

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

            g.manager_sender
                .send((ManagerAction::Resolution(t_res.res), GuiActionContext::default(), None))
                .expect("Sending from Gui to Manager unexpectedly failed");
        });

        self.window.show();
    }

    fn layout(self: &Rc<Self>) {
        self.window.remove_css_class("background");
        self.window.set_default_size(800, 600);
        self.window.set_title(Some("aw-man"));

        // TODO -- three separate indicators?
        self.mode.set_width_chars(3);
        self.mode.set_xalign(1.0);

        self.canvas.set_hexpand(true);
        self.canvas.set_vexpand(true);

        self.overlay.set_child(Some(&self.canvas));

        self.bottom_bar.add_css_class("background");
        self.bottom_bar.add_css_class("bottom-bar");

        // Left side -- right to left
        self.bottom_bar.prepend(&self.page_name);
        self.bottom_bar.prepend(&gtk::Label::new(Some("|")));
        self.bottom_bar.prepend(&self.archive_name);
        self.bottom_bar.prepend(&gtk::Label::new(Some("|")));
        self.bottom_bar.prepend(&self.progress);

        // TODO -- replace with center controls ?
        self.edge_indicator.set_hexpand(true);
        self.edge_indicator.set_halign(Align::End);

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

    fn handle_update(self: &Rc<Self>, gu: GuiAction) -> glib::Continue {
        use crate::com::GuiAction::*;

        match gu {
            State(s, actx) => {
                match self.window.title() {
                    Some(t) if t.as_str().strip_suffix(" - aw-man") == Some(&s.archive_name) => {}
                    _ => self.window.set_title(Some(&(s.archive_name.clone() + " - aw-man"))),
                };

                // TODO -- determine true visible pages. Like "1-3/20" or even "19/20 - 3/10"
                self.progress.set_text(&format!("{} / {}", s.page_num, s.archive_len));
                self.archive_name.set_text(&s.archive_name);
                self.page_name.set_text(&s.page_name);
                self.mode.set_text(&s.modes.gui_str());

                let old_s = self.state.replace(s);
                let mut new_s = self.state.borrow_mut();

                self.update_displayable(old_s, &mut new_s, actx);
                drop(new_s);
                self.update_zoom_level();
            }
            Action(a, fin) => {
                self.run_command(&a, Some(fin));
            }
            Quit => {
                self.window.close();
                closing::close();
                return glib::Continue(false);
            }
        }
        glib::Continue(true)
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

        if old_s.content == new_s.content && old_s.modes.display == new_s.modes.display {
            return;
        }

        if let GC::Single(Nothing | Pending(_)) = new_s.content {
            if new_s.archive_name == old_s.archive_name || old_s.archive_name.is_empty() {
                new_s.content = old_s.content;
                return;
            }
        }

        match &new_s.content {
            // https://github.com/rust-lang/rust/issues/51114
            GC::Single(d) if d.layout_res().is_some() => {
                self.update_scroll_contents(
                    LayoutContents::Single(d.layout_res().unwrap()),
                    actx.scroll_motion_target,
                    new_s.modes.display,
                );
            }
            GC::Multiple { current_index, visible, .. } if visible[0].layout_res().is_some() => {
                let visible = visible.iter().map(|v| v.layout_res().unwrap()).collect();

                self.update_scroll_contents(
                    LayoutContents::Multiple { current_index: *current_index, visible },
                    actx.scroll_motion_target,
                    new_s.modes.display,
                );
            }
            GC::Multiple { visible, .. } if visible.len() > 1 => unreachable!(),
            GC::Multiple { .. } | GC::Single(_) => {
                self.zero_scroll();
            }
        }

        self.canvas.inner().update_displayed(&new_s.content);

        self.canvas.queue_draw();
    }

    fn update_zoom_level(self: &Rc<Self>) {
        let zoom = self.get_zoom_level();

        let zoom = format!("{:>3}%", zoom);
        if zoom != self.zoom_level.text().as_str() {
            self.zoom_level.set_text(&zoom);
        }
    }

    fn get_zoom_level(self: &Rc<Self>) -> f64 {
        use Displayable::*;
        use GuiContent::*;

        let db = self.state.borrow();

        let (current, index) = match &db.content {
            Single(r) => (r, 0),
            Multiple { visible, current_index, .. } => (&visible[*current_index], *current_index),
        };

        let width = match current {
            Error(_) | Nothing => return 100.0,
            Image(img) => img.original_res.w,
            Animation(ac) => ac.frames()[0].0.res.w,
            Pending(r) => r.w,
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
}
