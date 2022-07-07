mod int;
mod renderable;
mod scroll;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;
use std::time::{Duration, Instant};

use flume::Sender;
use gtk::gdk::ModifierType;
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, Align};
use once_cell::unsync::OnceCell;

use self::renderable::{AnimationContainer, DisplayedContent, Renderable, SurfaceContainer};
use self::scroll::{ScrollContents, ScrollState};
use super::com::*;
use crate::{closing, config};


pub static WINDOW_ID: once_cell::sync::OnceCell<String> = once_cell::sync::OnceCell::new();


#[derive(Debug)]
pub struct Gui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    canvas: gtk::DrawingArea,

    displayed: RefCell<DisplayedContent>,

    progress: gtk::Label,
    page_name: gtk::Label,
    archive_name: gtk::Label,
    mode: gtk::Label,
    zoom_level: gtk::Label,
    edge_indicator: gtk::Label,
    bottom_bar: gtk::Box,

    state: RefCell<GuiState>,
    bg: Cell<gdk::RGBA>,

    scroll_state: RefCell<ScrollState>,
    // Called "pad" scrolling to differentiate it with continuous scrolling between pages.
    // TODO -- could move into the scroll_state enum
    pad_scrolling: Cell<bool>,
    drop_next_scroll: Cell<bool>,
    animation_playing: Cell<bool>,

    last_action: Cell<Option<Instant>>,
    first_content_paint: OnceCell<()>,
    open_dialogs: RefCell<HashMap<int::Dialogs, gtk::Window>>,

    shortcuts: HashMap<ModifierType, HashMap<gdk::Key, String>>,

    manager_sender: Rc<Sender<MAWithResponse>>,
}

pub fn run(manager_sender: flume::Sender<MAWithResponse>, gui_receiver: glib::Receiver<GuiAction>) {
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
            canvas: gtk::DrawingArea::default(),

            displayed: RefCell::new(DisplayedContent::Single(Renderable::Nothing)),

            progress: gtk::Label::new(None),
            page_name: gtk::Label::new(None),
            archive_name: gtk::Label::new(None),
            mode: gtk::Label::new(None),
            zoom_level: gtk::Label::new(Some("100%")),
            edge_indicator: gtk::Label::new(None),
            bottom_bar: gtk::Box::new(gtk::Orientation::Horizontal, 15),

            state: RefCell::default(),
            bg: Cell::new(
                config::CONFIG
                    .background_colour
                    .unwrap_or_else(|| gdk::RGBA::from_str("#00ff0055").unwrap()),
            ),

            scroll_state: RefCell::new(ScrollState::new(weak.clone())),
            pad_scrolling: Cell::default(),
            drop_next_scroll: Cell::default(),
            animation_playing: Cell::new(true),

            last_action: Cell::default(),
            first_content_paint: OnceCell::default(),
            open_dialogs: RefCell::default(),

            shortcuts: Self::parse_shortcuts(),

            manager_sender,
        });

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
        self.canvas.set_draw_func(move |_, cr, _width, _height| {
            g.canvas_draw(cr);
        });

        let g = self.clone();
        self.canvas.connect_resize(move |_, width, height| {
            // Resolution change is also a user action.
            g.last_action.set(Some(Instant::now()));

            assert!(width >= 0 && height >= 0, "Can't have negative width or height");

            let s = g.state.borrow();
            let t_res = (width, height, s.modes.fit).into();
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

        // TODO -- replace with center controls
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

    fn paint_surface(surface: &mut SurfaceContainer, layout: (i32, i32, Res), cr: &cairo::Context) {
        cr.save().expect("Invalid cairo context state");
        let current_res = surface.image.bgra.res;

        let display_res = layout.2;
        if display_res.is_zero_area() {
            warn!("Attempted to draw 0 sized image");
            return;
        }

        cr.set_operator(cairo::Operator::Over);

        let (sx, sy) = surface.internal_scroll(layout.0, layout.1);
        let mut ofx = sx as f64;
        let mut ofy = sy as f64;

        if display_res.w != current_res.w {
            debug!("Needed to scale image at draw time. {:?} -> {:?}", current_res, display_res);
            let scale = display_res.w as f64 / current_res.w as f64;
            cr.scale(scale, scale);
            ofx /= scale;
            ofy /= scale;
        }

        cr.set_source_surface(&surface.surface, ofx, ofy)
            .expect("Invalid cairo surface state.");
        cr.paint().expect("Invalid cairo surface state");
        cr.restore().expect("Invalid cairo context state");
    }

    fn canvas_draw(self: &Rc<Self>, cr: &cairo::Context) {
        cr.save().expect("Invalid cairo context state");
        GdkCairoContextExt::set_source_rgba(cr, &self.bg.get());
        cr.set_operator(cairo::Operator::Source);
        cr.paint().expect("Invalid cairo surface state");

        let mut drew_something = false;


        let layout_manager = self.scroll_state.borrow();
        let mut layouts = layout_manager.layout_iter();

        let mut render = |d: &mut Renderable| {
            let layout = layouts.next().expect("Layout not defined for all displayed pages.");
            match d {
                Renderable::Image(sf) => {
                    drew_something = true;

                    Self::paint_surface(sf, layout, cr);
                }
                Renderable::Animation(a) => {
                    drew_something = true;
                    let mut ab = a.borrow_mut();
                    let ac = &mut *ab;
                    let sf = &mut ac.surfaces[ac.index];

                    Self::paint_surface(sf, layout, cr)
                }
                Renderable::Video(_)
                | Renderable::Error(_)
                | Renderable::Pending(_)
                | Renderable::Nothing => {}
            }
        };

        let mut d = self.displayed.borrow_mut();

        match &mut *d {
            DisplayedContent::Single(r) => render(r),
            DisplayedContent::Multiple(visible) => visible.iter_mut().for_each(render),
        }

        if drew_something {
            let old_now = self.last_action.take();
            if let Some(old_now) = old_now {
                let dur = old_now.elapsed();

                if dur > Duration::from_secs(10) {
                    // Probably wasn't an action that changed anything. Don't log anything.
                } else if dur > Duration::from_millis(100) {
                    info!("Took {} milliseconds from action to drawable change.", dur.as_millis());
                } else if dur > Duration::from_millis(20) {
                    debug!("Took {} milliseconds from action to drawable change.", dur.as_millis());
                } else {
                    trace!("Took {} milliseconds from action to drawable change.", dur.as_millis());
                }
            }

            match self.first_content_paint.get() {
                None => {
                    self.first_content_paint.set(()).unwrap();
                    info!("Completed first meaningful paint");
                }
                Some(_) => (),
            }
        }
        cr.restore().expect("Invalid cairo context state");
    }

    fn handle_update(self: &Rc<Self>, gu: GuiAction) -> glib::Continue {
        use crate::com::GuiAction::*;

        match gu {
            State(s, actx) => {
                match self.window.title() {
                    Some(t) if t.to_string().starts_with(&s.archive_name) => {}
                    _ => self.window.set_title(Some(&(s.archive_name.clone() + " - aw-man"))),
                };

                // TODO -- determine true visible pages. Like "1-3/20"
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
        use GuiContent::{Multiple, Single};

        if old_s.target_res != new_s.target_res {
            self.update_scroll_container(new_s.target_res);
            self.canvas.queue_draw();

            // This won't result in duplicate work because it's impossible for both a resolution
            // update and a content update to arrive at once, but if it does it's very minimal.
            self.update_zoom_level();
        }

        if old_s.content == new_s.content {
            return;
        }

        match new_s.content {
            Single(Nothing | Pending(_))
                if new_s.archive_name == old_s.archive_name || old_s.archive_name.is_empty() =>
            {
                new_s.content = old_s.content;
                return;
            }
            // Could do something here for pending multiples, but it's pretty unlikely.
            _ => (),
        }


        match &new_s.content {
            // https://github.com/rust-lang/rust/issues/51114
            Single(d) if d.scroll_res().is_some() => {
                self.update_scroll_contents(
                    ScrollContents::Single(d.scroll_res().unwrap()),
                    actx.scroll_motion_target,
                    new_s.modes.display,
                );
            }
            Single(_) => {
                self.zero_scroll();
            }
            Multiple { previous_scrollable, visible, next } => {
                let visible = visible.iter().map(|v| v.scroll_res().unwrap()).collect();
                let next = if let OffscreenContent::Scrollable(r) = next { Some(*r) } else { None };

                self.update_scroll_contents(
                    ScrollContents::Multiple {
                        prev: *previous_scrollable,
                        visible,
                        next,
                    },
                    actx.scroll_motion_target,
                    new_s.modes.display,
                );
            }
        }

        // TODO -- for each displayed being removed once videos can be scrolled.
        let mut db = self.displayed.borrow_mut();
        match &*db {
            DisplayedContent::Single(Renderable::Video(vid)) => self.overlay.remove_overlay(vid),
            DisplayedContent::Single(Renderable::Error(e)) => self.overlay.remove_overlay(e),
            _ => {}
        }

        let map_displayable = |d: &Displayable| {
            match d {
                Image(img) => Renderable::Image(img.into()),
                Animation(a) => Renderable::Animation(AnimationContainer::new(a, self.clone())),
                Video(v) => {
                    // TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
                    let mf = gtk::MediaFile::for_filename(&*v.to_string_lossy());
                    mf.set_loop(true);
                    mf.set_playing(self.animation_playing.get());

                    let vid = gtk::Video::new();

                    vid.set_halign(Align::Center);
                    vid.set_valign(Align::Center);

                    vid.set_hexpand(false);
                    vid.set_vexpand(false);

                    vid.set_autoplay(true);
                    vid.set_loop(true);

                    vid.set_focusable(false);
                    vid.set_can_focus(false);

                    vid.set_media_stream(Some(&mf));

                    self.overlay.add_overlay(&vid);

                    Renderable::Video(vid)
                }
                Error(e) => {
                    let error = gtk::Label::new(Some(e));

                    error.set_halign(Align::Center);
                    error.set_valign(Align::Center);

                    error.set_hexpand(false);
                    error.set_vexpand(false);

                    error.set_wrap(true);
                    error.set_max_width_chars(120);
                    error.add_css_class("error-label");

                    self.overlay.add_overlay(&error);

                    Renderable::Error(error)
                }
                Pending(res) => Renderable::Pending(*res),
                Nothing => Renderable::Nothing,
            }
        };

        let take_old_renderable = |d: &Displayable, old: &mut Vec<Renderable>| {
            for (i, o) in old.iter_mut().enumerate() {
                if o.matches(d) {
                    return Some(old.swap_remove(i));
                }
            }

            None
        };

        match (&mut *db, &new_s.content) {
            (DisplayedContent::Single(_), Single(d)) => {
                *db = DisplayedContent::Single(map_displayable(d))
            }
            (DisplayedContent::Multiple(old), Single(d)) => {
                let r = take_old_renderable(d, old).unwrap_or_else(|| map_displayable(d));
                *db = DisplayedContent::Single(r);
            }
            (DisplayedContent::Single(s), Multiple { visible, .. }) => {
                let visible = visible
                    .iter()
                    .map(|d| if s.matches(d) { std::mem::take(s) } else { map_displayable(d) })
                    .collect();
                *db = DisplayedContent::Multiple(visible);
            }
            (DisplayedContent::Multiple(old), Multiple { visible, .. }) => {
                let visible = visible
                    .iter()
                    .map(|d| take_old_renderable(d, old).unwrap_or_else(|| map_displayable(d)))
                    .collect();
                *db = DisplayedContent::Multiple(visible);
            }
        }

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
        use DisplayedContent::*;
        use Renderable::*;

        let db = self.displayed.borrow();

        let first = match &*db {
            Single(r) => r,
            Multiple(visible) => &visible[0],
        };

        let first_width = match first {
            Error(_) | Nothing => return 100.0,
            Image(img) => img.image.original_res.w,
            Animation(ac) => ac.borrow().animated.frames()[0].0.res.w,
            Pending(r) => r.w,
            Video(vid) => {
                // Special case until videos are scanned and available for regular layout.
                let mut t_res = self.state.borrow().target_res;
                t_res.fit = Fit::Container;

                return if vid.width() == 0 {
                    100.0
                } else {
                    let stream = vid.media_stream().unwrap();
                    let ores: Res = (stream.intrinsic_width(), stream.intrinsic_height()).into();
                    let res = ores.fit_inside(t_res);
                    (res.w as f64 / ores.w as f64 * 100.0).round()
                };
            }
        };

        let first_layout = self.scroll_state.borrow().layout_iter().next().unwrap();

        if first_width == 0 {
            100.0
        } else {
            (first_layout.2.w as f64 / first_width as f64 * 100.0).round()
        }
    }
}
