mod int;
mod scroll;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::convert::TryInto;
use std::rc::Rc;
use std::str::FromStr;
use std::time::{Duration, Instant};

use flume::Sender;
use gtk::gdk::ModifierType;
use gtk::glib::SourceId;
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib, Align};
use once_cell::unsync::OnceCell;

use self::scroll::ScrollState;
use super::com::*;
use crate::gui::scroll::ScrollPos;
use crate::{closing, config};


pub static WINDOW_ID: once_cell::sync::OnceCell<String> = once_cell::sync::OnceCell::new();

#[derive(Debug)]
struct SurfaceContainer {
    // Fields are dropped in FIFO order, ensuring bgra always outlives surface.
    surface: cairo::ImageSurface,
    bgra: Bgra,
}

impl From<&Bgra> for SurfaceContainer {
    fn from(bgra: &Bgra) -> Self {
        let surface;
        let raw_ptr = bgra.as_ptr();
        // Use unsafe to create a cairo::ImageSurface which requires mutable access
        // to the underlying image data without needing to duplicate the entire image
        // in memory.
        // ImageSurface can be used to mutate the underlying data.
        // This is safe because the image data is never mutated in this program.
        let w = if bgra.res.w >= (i16::MAX as u32) {
            error!(
                "Image too wide for cairo, width: {}, max: {}",
                bgra.res.w,
                i16::MAX - 1
            );
            (i16::MAX - 1) as i32
        } else {
            bgra.res.w as i32
        };
        let h = if bgra.res.h >= (i16::MAX as u32) {
            error!(
                "Image too tall for cairo, height: {}, max: {}",
                bgra.res.h,
                i16::MAX - 1
            );
            (i16::MAX - 1) as i32
        } else {
            bgra.res.h as i32
        };
        unsafe {
            let mut_ptr = raw_ptr as *mut u8;
            surface = cairo::ImageSurface::create_for_data_unsafe(
                mut_ptr,
                cairo::Format::ARgb32,
                w,
                h,
                bgra.stride.try_into().expect("Image too big"),
            )
            .expect("Invalid cairo surface state.");
        }
        Self {
            bgra: bgra.clone(),
            surface,
        }
    }
}

#[derive(Debug)]
struct AnimationContainer {
    animated: AnimatedImage,
    surfaces: Vec<Option<SurfaceContainer>>,
    index: usize,
    target_time: Instant,
    timeout_id: Option<SourceId>,
}

impl Drop for AnimationContainer {
    fn drop(&mut self) {
        glib::source_remove(self.timeout_id.take().expect("Animation with no timeout"))
    }
}

// TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
// #[derive(Debug)]
// struct VideoContainer {
//     // Fields are dropped in FIFO order, ensuring the data will always outlive the references.
//     video: gtk::Video,
//     media_file: gtk::MediaFile,
//     input_stream: gio::MemoryInputStream,
//     bytes: glib::Bytes,
//     data: VideoData,
//
//     detached: Cell<bool>,
// }
//
//
// impl VideoContainer {
//     fn new(vd: &VideoData, parent: &gtk::Overlay) -> Self {
//         let data = vd.clone();
//
//         let bytes;
//         let r = data.as_ref() as *const [u8];
//         unsafe {
//             // r is a pointer to a Pin<Arc<Vec<u8>>> which is guaranteed not to move for the
//             // lifetime of the Arc. We hold an immutable reference to the Arc, so it
//             // cannot be destroyed, so this lifetime upcast is safe for this
//             // application. This avoids copying the video data each time it is
//             // displayed.
//             let r = &*r;
//             bytes = glib::Bytes::from_static(r);
//         }
//         let input_stream = gio::MemoryInputStream::from_bytes(&bytes);
//         let media_file = gtk::MediaFile::for_input_stream(&input_stream);
//         let video = gtk::Video::new();
//
//         video.set_halign(Align::Center);
//         video.set_valign(Align::Center);
//
//         video.set_hexpand(false);
//         video.set_vexpand(false);
//
//         video.set_autoplay(true);
//         video.set_loop(true);
//
//         video.set_media_stream(Some(&media_file));
//
//         parent.add_overlay(&video);
//
//         Self {
//             video,
//             media_file,
//             input_stream,
//             bytes,
//             data,
//
//             detached: Cell::new(false),
//         }
//     }
//
//     fn detach(self, parent: &gtk::Overlay) {
//         if let Some(p) = &self.video.parent() {
//             let p = p
//                 .dynamic_cast_ref::<gtk::Overlay>()
//                 .expect("Video attached to non-overlay parent.");
//             p.remove_overlay(&self.video);
//         }
//
//         self.detached.set(true);
//         drop(self);
//     }
// }
//
// impl Drop for VideoContainer {
//     fn drop(&mut self) {
//         if !self.detached.get() {
//             error!("VideoContainer dropped without detaching from parent.");
//             closing::close();
//         }
//     }
// }

#[derive(Debug)]
pub struct Gui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    canvas: gtk::DrawingArea,
    surface: RefCell<Option<SurfaceContainer>>,
    animation: RefCell<Option<AnimationContainer>>,
    video: RefCell<Option<gtk::Video>>,
    progress: gtk::Label,
    page_name: gtk::Label,
    archive_name: gtk::Label,
    mode: gtk::Label,
    bottom_bar: gtk::Box,

    state: RefCell<GuiState>,
    bg: Cell<gdk::RGBA>,

    scroll_state: RefCell<ScrollState>,
    continuous_scrolling: Cell<bool>,
    scroll_motion_target: Cell<ScrollPos>,

    last_action: Cell<Option<Instant>>,
    first_content_paint: OnceCell<()>,
    open_dialogs: RefCell<HashMap<int::Dialogs, gtk::Window>>,

    shortcuts: HashMap<ModifierType, HashMap<u32, String>>,

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

        let rc = Rc::new(Self {
            window,
            overlay: gtk::Overlay::new(),
            canvas: gtk::DrawingArea::default(),
            surface: RefCell::default(),
            animation: RefCell::default(),
            video: RefCell::default(),
            progress: gtk::Label::new(None),
            page_name: gtk::Label::new(None),
            archive_name: gtk::Label::new(None),
            mode: gtk::Label::new(None),
            bottom_bar: gtk::Box::new(gtk::Orientation::Horizontal, 15),

            state: RefCell::default(),
            bg: Cell::new(
                config::CONFIG
                    .background_colour
                    .unwrap_or_else(|| gdk::RGBA::from_str("#00ff0055").unwrap()),
            ),

            scroll_state: RefCell::default(),
            continuous_scrolling: Cell::default(),
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

        #[cfg(target_family = "unix")]
        {
            if let Ok(xsuf) = rc
                .window
                .surface()
                .expect("Window displayed without surface")
                .dynamic_cast::<gdk4_x11::X11Surface>()
            {
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

            if width < 0 || height < 0 {
                panic!("Can't have negative width or height");
            }

            let s = g.state.borrow();
            let t_res = (width, height, s.modes.fit).into();
            g.scroll_state.borrow_mut().update_container(t_res);

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

        self.mode.set_hexpand(true);
        self.mode.set_halign(Align::End);

        self.canvas.set_hexpand(true);
        self.canvas.set_vexpand(true);

        self.overlay.set_child(Some(&self.canvas));

        self.bottom_bar.add_css_class("background");
        self.bottom_bar.add_css_class("bottom-bar");

        self.bottom_bar.prepend(&self.page_name);
        self.bottom_bar.prepend(&gtk::Label::new(Some("|")));
        self.bottom_bar.prepend(&self.archive_name);
        self.bottom_bar.prepend(&gtk::Label::new(Some("|")));
        self.bottom_bar.prepend(&self.progress);
        self.bottom_bar.append(&self.mode);

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);

        vbox.prepend(&self.overlay);
        vbox.append(&self.bottom_bar);

        self.window.set_child(Some(&vbox));
    }

    fn maybe_draw_surface(self: &Rc<Self>, new_bgra: &Bgra) {
        if let Some(sc) = &*self.surface.borrow() {
            if &sc.bgra == new_bgra {
                return;
            }
        }

        self.surface.replace(Some(new_bgra.into()));
    }

    fn paint_surface(
        self: &Rc<Self>,
        surface: &cairo::ImageSurface,
        original_res: Res,
        current_res: Res,
        target_res: TargetRes,
        cr: &cairo::Context,
    ) {
        let res = original_res.fit_inside(target_res);
        if res.is_zero_area() {
            warn!("Attempted to draw 0 sized image");
            return;
        }

        cr.set_operator(cairo::Operator::Over);
        let mut ofx = ((target_res.res.w.saturating_sub(res.w)) / 2) as f64;
        let mut ofy = ((target_res.res.h.saturating_sub(res.h)) / 2) as f64;

        if res.w != current_res.w {
            debug!(
                "Needed to scale image at draw time. {:?} -> {:?}",
                current_res, res
            );
            let scale = res.w as f64 / current_res.w as f64;
            cr.scale(scale, scale);
            ofx /= scale;
            ofy /= scale;
        }

        let scrolling = self.scroll_state.borrow();
        ofx -= scrolling.x as f64;
        ofy -= scrolling.y as f64;
        drop(scrolling);

        cr.set_source_surface(surface, ofx, ofy)
            .expect("Invalid cairo surface state.");
        cr.paint().expect("Invalid cairo surface state");
    }

    fn canvas_draw(self: &Rc<Self>, cr: &cairo::Context, w: i32, h: i32) {
        use Displayable::*;

        cr.save().expect("Invalid cairo context state");
        GdkCairoContextExt::set_source_rgba(cr, &self.bg.get());
        cr.set_operator(cairo::Operator::Source);
        cr.paint().expect("Invalid cairo surface state");

        let mut drew_something = false;

        let s = self.state.borrow();
        match &s.displayable {
            Image(scaled) => {
                drew_something = true;
                self.maybe_draw_surface(&scaled.bgra);
                let sc = self.surface.borrow();
                let sf = &sc.as_ref().expect("Surface unexpectedly not set").surface;
                let da_t_res = (w, h, s.modes.fit).into();

                self.paint_surface(sf, scaled.original_res, scaled.bgra.res, da_t_res, cr);
            }
            Animation(_) => {
                drew_something = true;
                self.surface.replace(None);
                let mut ac_borrow = self.animation.borrow_mut();
                let ac = ac_borrow.as_mut().expect("AnimationContainer not set");
                let frame = &ac.animated.frames()[ac.index].0;
                let sf = if let Some(sc) = &ac.surfaces[ac.index] {
                    &sc.surface
                } else {
                    ac.surfaces[ac.index].replace(frame.into());
                    &ac.surfaces[ac.index].as_ref().expect("Impossible").surface
                };

                let da_t_res = (w, h, Fit::Container).into();
                let original_res: Res = frame.res;

                self.paint_surface(sf, original_res, original_res, da_t_res, cr);
            }
            Video(_) | Error(_) | Nothing => {
                self.surface.replace(None);
            }
        };

        if drew_something {
            let old_now = self.last_action.take();
            if let Some(old_now) = old_now {
                let dur = old_now.elapsed();

                if dur > Duration::from_secs(10) {
                    // Probably wasn't an action that changed anything. Don't log anything.
                } else if dur > Duration::from_millis(100) {
                    info!(
                        "Took {} milliseconds from action to drawable change.",
                        dur.as_millis()
                    );
                } else if dur > Duration::from_millis(20) {
                    debug!(
                        "Took {} milliseconds from action to drawable change.",
                        dur.as_millis()
                    );
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
                    _ => self
                        .window
                        .set_title(Some(&(s.archive_name.clone() + " - aw-man"))),
                };

                self.progress
                    .set_text(&format!("{} / {}", s.page_num, s.archive_len));
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
            self.scroll_state
                .borrow_mut()
                .update_container(new_s.target_res);
            self.canvas.queue_draw();
        }

        if old_s.displayable != new_s.displayable {
            if Nothing == new_s.displayable
                && (new_s.archive_name == old_s.archive_name || old_s.archive_name.is_empty())
            {
                new_s.displayable = old_s.displayable;
                return;
            }

            if let Image(si) = &new_s.displayable {
                let pos = self.scroll_motion_target.replace(ScrollPos::Maintain);

                self.scroll_state
                    .borrow_mut()
                    .update_contents(si.original_res, pos);
            } else {
                // Nothing else scrolls right now
                self.scroll_motion_target.set(ScrollPos::Maintain);
                self.scroll_state.borrow_mut().zero();
            }

            self.animation.replace(None);
            if let Some(vid) = self.video.replace(None) {
                self.overlay.remove_overlay(&vid);
            }

            match &new_s.displayable {
                Image(_) | Nothing => (),
                Animation(a) => {
                    let g = self.clone();
                    let timeout_id = glib::timeout_add_local_once(a.frames()[0].1, move || {
                        g.advance_animation()
                    });
                    let mut surfaces = Vec::with_capacity(a.frames().len());
                    surfaces.resize_with(a.frames().len(), || None);
                    let ac = AnimationContainer {
                        animated: a.clone(),
                        surfaces,
                        index: 0,
                        target_time: Instant::now(),
                        timeout_id: Some(timeout_id),
                    };
                    self.animation.replace(Some(ac));
                }
                Video(v) => {
                    // TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
                    let mf = gtk::MediaFile::for_filename(&v.to_string_lossy());
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

                    self.video.replace(Some(vid));
                }
                Error(_) => {
                    // TODO
                }
            }
            self.canvas.queue_draw();
        }
    }

    fn advance_animation(self: Rc<Self>) {
        let mut acb = self.animation.borrow_mut();
        let ac = acb
            .as_mut()
            .expect("Called advance_animation with no animation.");

        while ac.target_time < Instant::now() {
            ac.index = (ac.index + 1) % ac.animated.frames().len();
            let mut dur = ac.animated.frames()[ac.index].1;
            if dur.is_zero() {
                dur = Duration::from_millis(100);
            }
            let tt = ac.target_time.checked_add(dur).unwrap_or_else(|| {
                Instant::now()
                    .checked_add(Duration::from_secs(1))
                    .expect("End of time")
            });
            ac.target_time = tt;
        }

        self.canvas.queue_draw();


        let g = self.clone();
        ac.timeout_id = Some(glib::timeout_add_local_once(
            ac.target_time.saturating_duration_since(Instant::now()),
            move || g.advance_animation(),
        ));
    }
}
