mod int;
mod renderable;
mod scroll;

use std::cell::{Cell, RefCell, RefMut};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::ops::AddAssign;
use std::rc::Rc;
use std::str::FromStr;
use std::time::{Duration, Instant};

use flume::Sender;
use gtk::gdk::ModifierType;
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, Align};
use once_cell::unsync::OnceCell;

use self::renderable::{AnimationContainer, AnimationStatus, Displayed, SurfaceContainer};
use self::scroll::{ScrollContents, ScrollPos, ScrollState};
use super::com::*;
use crate::{closing, config};


pub static WINDOW_ID: once_cell::sync::OnceCell<String> = once_cell::sync::OnceCell::new();

#[derive(Debug, Default, Copy, Clone)]
enum Position {
    #[default]
    Center,
    Offset(i32),
    // Used when multiple images are visible but scrolling isn't necessary.
    // Contains the overall centering inset (applied before render-time scaling) and the offset in
    // display pixels (applied after scaling).
    OffsetCenter(u32, i32),
}

impl AddAssign<u32> for Position {
    fn add_assign(&mut self, rhs: u32) {
        match self {
            // Negative direction is down/right, so this is "backwards".
            #[allow(clippy::suspicious_op_assign_impl)]
            Self::Offset(ofs) | Self::OffsetCenter(_, ofs) => *ofs -= rhs as i32,
            Self::Center => unreachable!(),
        }
    }
}

#[derive(Debug)]
pub struct Gui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    canvas: gtk::DrawingArea,

    displayed: RefCell<Displayed>,

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
    // When moving between pages we want to ensure that the scroll position gets set correctly.
    scroll_motion_target: Cell<ScrollPos>,

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

            displayed: RefCell::new(Displayed::Nothing),

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
            // This is best effort, it can be wrong if the user performs another action right as
            // the manager is sending the previous contents. But defaulting to "maintain" should
            // result in the correct scroll state in every scenario I can foresee.
            scroll_motion_target: Cell::new(ScrollPos::Maintain),

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
        self.canvas.set_draw_func(move |_, cr, width, height| {
            g.canvas_draw(cr, width, height);
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
                .send((ManagerAction::Resolution(t_res.res), None))
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

    fn paint_surface(
        surface: &mut SurfaceContainer,
        target_res: TargetRes,
        h_pos: Position,
        v_pos: Position,
        cr: &cairo::Context,
    ) -> Res {
        let original_res = surface.original_res;
        let current_res = surface.bgra.res;

        let display_res = original_res.fit_inside(target_res);
        if display_res.is_zero_area() {
            warn!("Attempted to draw 0 sized image");
            return display_res;
        }

        cr.set_operator(cairo::Operator::Over);
        // TODO -- handle this image being thinner than other images and not needing scrolling.
        // Current implementation can cause weird jumping.
        let (mut ofx, sx) = match h_pos {
            Position::Center => (((target_res.res.w.saturating_sub(display_res.w)) / 2) as f64, 0),
            Position::Offset(sx) => (0f64, sx),
            Position::OffsetCenter(ofx, sx) => (ofx as f64, sx),
        };
        let (mut ofy, sy) = match v_pos {
            Position::Center => (((target_res.res.h.saturating_sub(display_res.h)) / 2) as f64, 0),
            Position::Offset(sy) => (0f64, sy),
            Position::OffsetCenter(ofy, sy) => (ofy as f64, sy),
        };

        if display_res.w != current_res.w {
            debug!("Needed to scale image at draw time. {:?} -> {:?}", current_res, display_res);
            let scale = display_res.w as f64 / current_res.w as f64;
            cr.scale(scale, scale);
            ofx /= scale;
            ofy /= scale;
        }

        let (sx, sy) = surface.internal_scroll(sx, sy);
        ofx -= sx as f64;
        ofy -= sy as f64;

        cr.set_source_surface(&surface.surface, ofx, ofy)
            .expect("Invalid cairo surface state.");
        cr.paint().expect("Invalid cairo surface state");
        display_res
    }

    fn canvas_draw(self: &Rc<Self>, cr: &cairo::Context, w: i32, h: i32) {
        cr.save().expect("Invalid cairo context state");
        GdkCairoContextExt::set_source_rgba(cr, &self.bg.get());
        cr.set_operator(cairo::Operator::Source);
        cr.paint().expect("Invalid cairo surface state");

        let mut drew_something = false;


        let s = self.state.borrow();
        let (h_pos, v_pos) = self.scroll_state.borrow().as_positions();

        let mut render = |d: &mut Displayed, v_pos| match d {
            Displayed::Image(sf) => {
                drew_something = true;
                let target_res = (w, h, s.modes.fit).into();

                Self::paint_surface(sf, target_res, h_pos, v_pos, cr)
            }
            Displayed::Animation(ac) => {
                drew_something = true;
                let sf = &mut ac.surfaces[ac.index];
                let target_res = (w, h, Fit::Container).into();

                Self::paint_surface(sf, target_res, h_pos, v_pos, cr)
            }
            Displayed::Pending(res) => *res,
            Displayed::Video(_) | Displayed::Error(_) | Displayed::Nothing => (0, 0).into(),
        };

        let mut d = self.displayed.borrow_mut();
        render(&mut *d, v_pos);

        // if render_multiple {
        //for each
        // let r = render(&mut *d, v_pos);
        // v_pos += r.h;
        // }

        // }

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
            State(s) => {
                match self.window.title() {
                    Some(t) if t.to_string().starts_with(&s.archive_name) => {}
                    _ => self.window.set_title(Some(&(s.archive_name.clone() + " - aw-man"))),
                };

                self.progress.set_text(&format!("{} / {}", s.page_num, s.archive_len));
                self.archive_name.set_text(&s.archive_name);
                self.page_name.set_text(&s.page_name);
                self.mode.set_text(&s.modes.gui_str());

                let old_s = self.state.replace(s);
                let mut new_s = self.state.borrow_mut();

                self.update_displayable(old_s, &mut new_s)
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

    fn update_displayable(self: &Rc<Self>, old_s: GuiState, new_s: &mut GuiState) {
        use Displayable::*;

        if old_s.target_res != new_s.target_res {
            self.update_scroll_container(new_s.target_res);
            self.canvas.queue_draw();

            // This won't result in duplicate work because it's impossible for both a resolution
            // update update to arrive at once, but if it does it's very minimal.
            let db = self.displayed.borrow();
            Self::update_zoom_level(new_s, &db, &self.zoom_level);
        }

        if old_s.displayable == new_s.displayable {
            return;
        }

        if (Nothing == new_s.displayable || matches!(new_s.displayable, Pending(_)))
            && (new_s.archive_name == old_s.archive_name || old_s.archive_name.is_empty())
        {
            new_s.displayable = old_s.displayable;
            return;
        }

        match &new_s.displayable {
            Image(ScaledImage { original_res: res, .. }) | Pending(res) => {
                let pos = self.scroll_motion_target.replace(ScrollPos::Maintain);

                self.update_scroll_contents(ScrollContents::Single(*res), pos);
            }
            _ => {
                self.scroll_motion_target.set(ScrollPos::Maintain);
                self.zero_scroll();
            }
        }

        // TODO -- for each displayed being removed.
        let mut db = self.displayed.borrow_mut();
        match &*db {
            Displayed::Image(_)
            | Displayed::Animation(_)
            | Displayed::Pending(_)
            | Displayed::Nothing => (),
            Displayed::Video(vid) => self.overlay.remove_overlay(vid),
            Displayed::Error(e) => self.overlay.remove_overlay(e),
        }

        match &new_s.displayable {
            Image(img) => *db = Displayed::Image(img.into()),
            Animation(a) => {
                let g = self.clone();
                let target_time = Instant::now()
                    .checked_add(a.frames()[0].1)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));
                let timeout_id =
                    ManuallyDrop::new(glib::timeout_add_local_once(a.frames()[0].1, move || {
                        g.advance_animation()
                    }));
                let surfaces = a.frames().map(|f| SurfaceContainer::from_unscaled(&f.0));

                let ac = AnimationContainer {
                    animated: a.clone(),
                    surfaces,
                    index: 0,
                    status: AnimationStatus::Playing { target_time, timeout_id },
                };

                *db = Displayed::Animation(ac);
            }
            Video(v) => {
                // TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
                let mf = gtk::MediaFile::for_filename(&*v.to_string_lossy());
                mf.set_loop(true);
                mf.set_playing(true);

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

                *db = Displayed::Video(vid);
            }
            Pending(res) => *db = Displayed::Pending(*res),
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

                *db = Displayed::Error(error);
            }
            Nothing => *db = Displayed::Nothing,
        }

        Self::update_zoom_level(new_s, &db, &self.zoom_level);
        self.canvas.queue_draw();
    }

    fn update_zoom_level(state: &GuiState, displayed: &Displayed, zoom_label: &gtk::Label) {
        let t_res = state.target_res;
        let zoom = match displayed {
            Displayed::Error(_) | Displayed::Nothing => 100.0,
            Displayed::Image(img) => {
                // TODO -- consider the "true" original resolution of the unupscaled file?
                let res = img.original_res.fit_inside(t_res);
                (res.w as f64 / img.original_res.w as f64 * 100.0).round()
            }
            Displayed::Animation(ac) => {
                let ores = ac.animated.frames()[0].0.res;
                let res = ores.fit_inside(t_res);
                (res.w as f64 / ores.w as f64 * 100.0).round()
            }
            Displayed::Video(vid) => {
                // TODO -- this doesn't work properly when videos are initializing, hence the hack.
                // Not even the MediaStream's intrinsic resolution is set.
                // Will eventually be fixed by scanning videos but it's not worth doing before
                // videos can be preloaded.
                if vid.width() == 0 {
                    100.0
                } else {
                    let stream = vid.media_stream().unwrap();
                    let ores: Res = (stream.intrinsic_width(), stream.intrinsic_height()).into();
                    let res = ores.fit_inside(t_res);
                    (res.w as f64 / ores.w as f64 * 100.0).round()
                }
            }
            Displayed::Pending(p_res) => {
                if p_res.w == 0 {
                    100.0
                } else {
                    // TODO -- consider the "true" original resolution of the unupscaled file?
                    let res = p_res.fit_inside(t_res);
                    (res.w as f64 / p_res.w as f64 * 100.0).round()
                }
            }
        };

        let zoom = format!("{:>3}%", zoom);
        if zoom != zoom_label.text().as_str() {
            zoom_label.set_text(&zoom);
        }
    }

    // Panics if there isn't an animation being played.
    fn borrow_animation<'a>(self: &'a Rc<Self>) -> RefMut<'a, AnimationContainer> {
        RefMut::map(self.displayed.borrow_mut(), |db| {
            if let Displayed::Animation(anim) = &mut *db {
                anim
            } else {
                unreachable!();
            }
        })
    }

    fn advance_animation(self: Rc<Self>) {
        let mut ab = self.borrow_animation();
        let ac = &mut *ab;
        let (target_time, timeout_id) =
            if let AnimationStatus::Playing { target_time, timeout_id } = &mut ac.status {
                (target_time, &mut **timeout_id)
            } else {
                unreachable!()
            };

        while *target_time < Instant::now() {
            ac.index = (ac.index + 1) % ac.animated.frames().len();
            let mut dur = ac.animated.frames()[ac.index].1;
            if dur.is_zero() {
                dur = Duration::from_millis(100);
            }
            *target_time = target_time.checked_add(dur).unwrap_or_else(|| {
                Instant::now().checked_add(Duration::from_secs(1)).expect("End of time")
            });
        }

        self.canvas.queue_draw();


        let g = self.clone();
        *timeout_id = glib::timeout_add_local_once(
            target_time.saturating_duration_since(Instant::now()),
            move || g.advance_animation(),
        );
    }
}
