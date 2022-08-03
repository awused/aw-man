// All the GUI code dealing with input, whether directly or programmatically.

use std::cell::Cell;
use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::str::FromStr;
use std::time::Instant;

use ahash::AHashMap;
use gtk::gdk::{DragAction, FileList, Key, ModifierType, RGBA};
use gtk::prelude::*;
use once_cell::sync::Lazy;
use regex::{self, Regex};
use serde_json::Value;

use super::Gui;
use crate::closing;
use crate::com::{
    CommandResponder, Direction, DisplayMode, Fit, GuiActionContext, GuiContent, LayoutCount,
    ManagerAction, OffscreenContent, ScrollMotionTarget,
};
use crate::config::CONFIG;
use crate::gui::layout::Edge;

// These are only accessed from one thread but it's cleaner to use sync::Lazy
static SET_BACKGROUND_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^SetBackground ([^ ]+)$").unwrap());
static JUMP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Jump (\+|-)?(\d+)$").unwrap());
static EXECUTE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Execute (.+)$").unwrap());

#[derive(Debug, Default)]
pub(super) struct OpenDialogs {
    background: Option<gtk::Window>,
    jump: Option<gtk::Window>,
}

fn command_error<T: std::fmt::Display>(e: T, fin: Option<CommandResponder>) {
    let e = format!("{}", e);
    error!("{}", e);
    if let Some(s) = fin {
        if let Err(e) = s.send(Value::String(e)) {
            error!("Oneshot channel failed to send. {e}");
        }
    }
}

fn command_info<T: std::fmt::Display>(e: T, fin: Option<CommandResponder>) {
    let e = format!("{}", e);
    info!("{}", e);
    if let Some(s) = fin {
        if let Err(e) = s.send(Value::String(e)) {
            error!("Oneshot channel failed to send. {e}");
        }
    }
}

impl Gui {
    pub(super) fn setup_interaction(self: &Rc<Self>) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);

        let g = self.clone();
        scroll.connect_scroll_begin(move |_e| {
            g.pad_scrolling.set(true);
        });

        let g = self.clone();
        scroll.connect_scroll_end(move |_e| {
            g.pad_scrolling.set(false);
        });

        #[cfg(unix)]
        {
            // This might only be necessary on X11 but this is also a GTK4 regression.
            // Previously this was not necessary with gtk3.
            // This matches the behaviour of Chrome, Firefox, and mcomix so it could be worse.
            let enter = gtk::EventControllerMotion::new();
            let g = self.clone();
            enter.connect_leave(move |_| {
                trace!("Will drop next scroll event to avoid X11/GTK4 bug.");
                g.drop_next_scroll.set(true);
            });

            // Would prefer to put this on the window itself but that just doesn't work.
            self.overlay.add_controller(&enter);
        }

        let g = self.clone();
        scroll.connect_scroll(move |_e, x, y| {
            // X11/GTK scrolling is stupid and broken.
            if g.drop_next_scroll.get() {
                g.drop_next_scroll.set(false);
                debug!("Dropping scroll event because of X11/GTK4 bug.");
                return gtk::Inhibit(false);
            }

            // GTK continuous scrolling start/end is weird.
            // Detect when this is extremely likely to be a discrete device.
            if g.pad_scrolling.get() && x.fract() == 0.0 && y.fract() == 0.0 {
                warn!("Detected discrete scrolling while in touchpad scrolling mode.");
                g.pad_scrolling.set(false);
            }
            // TODO -- could do inverse condition, but may be possible to remove entirely on
            // Wayland

            if g.pad_scrolling.get() {
                g.pad_scroll(x, y);
            } else {
                g.discrete_scroll(x, y);
            }
            gtk::Inhibit(false)
        });


        self.overlay.add_controller(&scroll);

        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);

        let g = self.clone();
        drag.connect_drag_begin(move |_e, _x, _y| {
            g.layout_manager.borrow_mut().start_drag();
        });

        let g = self.clone();
        drag.connect_drag_update(move |_e, x, y| {
            g.drag_update(x * -1.0, y * -1.0);
        });

        self.canvas.add_controller(&drag);

        let key = gtk::EventControllerKey::new();

        let g = self.clone();
        key.connect_key_pressed(move |_e, a, _b, c| {
            if let Some(s) = g.shortcut_from_key(a, c) {
                g.run_command(s, None);
            }
            gtk::Inhibit(false)
        });

        self.window.add_controller(&key);

        let drop_target = gtk::DropTarget::new(FileList::static_type(), DragAction::COPY);

        let g = self.clone();
        drop_target.connect_drop(move |_dt, v, _x, _y| {
            let files = v.get::<FileList>().unwrap().files();
            let paths: Vec<_> = files.into_iter().filter_map(|f| f.path()).collect();

            debug!("Got {paths:?} from drop event");
            g.send_manager((ManagerAction::Open(paths), GuiActionContext::default(), None));

            true
        });

        self.window.add_controller(&drop_target);
    }

    fn shortcut_from_key<'a>(self: &'a Rc<Self>, k: Key, mods: ModifierType) -> Option<&'a String> {
        let mods = mods & !ModifierType::LOCK_MASK;
        let upper = k.to_upper();

        self.shortcuts.get(&mods)?.get(&upper)
    }

    fn simple_sends(self: &Rc<Self>, s: &str) -> Option<(ManagerAction, GuiActionContext)> {
        use Direction::*;
        use ManagerAction::*;
        use ScrollMotionTarget::*;

        match s {
            "NextPage" => {
                let state = self.state.borrow();
                let pages = match state.modes.display {
                    DisplayMode::DualPage | DisplayMode::DualPageReversed => match &state.content {
                        GuiContent::Single(_) => unreachable!(),
                        GuiContent::Multiple { next: OffscreenContent::Nothing, .. } => 0,
                        GuiContent::Multiple { visible, .. } => visible.len(),
                    },
                    DisplayMode::Single
                    | DisplayMode::VerticalStrip
                    | DisplayMode::HorizontalStrip => 1,
                };
                Some((MovePages(Forwards, pages), Start.into()))
            }
            "PreviousPage" => {
                let state = self.state.borrow();
                let pages = match state.modes.display {
                    DisplayMode::DualPage | DisplayMode::DualPageReversed => match state.content {
                        GuiContent::Single(_) => unreachable!(),
                        GuiContent::Multiple { prev: OffscreenContent::Nothing, .. } => 0,
                        GuiContent::Multiple {
                            prev: OffscreenContent::LayoutCompatible(LayoutCount::TwoOrMore),
                            ..
                        } => 2,
                        GuiContent::Multiple { .. } => 1,
                    },
                    DisplayMode::Single
                    | DisplayMode::VerticalStrip
                    | DisplayMode::HorizontalStrip => 1,
                };

                let scroll_target = match state.modes.display {
                    DisplayMode::Single | DisplayMode::DualPage | DisplayMode::DualPageReversed => {
                        End
                    }
                    DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip => Start,
                };

                Some((MovePages(Backwards, pages), scroll_target.into()))
            }
            "FirstPage" => Some((MovePages(Absolute, 0), Start.into())),
            "LastPage" => {
                Some((MovePages(Absolute, self.state.borrow().archive_len), Start.into()))
            }
            "NextArchive" => Some((NextArchive, Start.into())),
            "PreviousArchive" => Some((PreviousArchive, Start.into())),
            "ToggleUpscaling" => Some((ToggleUpscaling, GuiActionContext::default())),
            "ToggleMangaMode" => Some((ToggleManga, GuiActionContext::default())),
            "Status" => Some((Status, GuiActionContext::default())),
            "ListPages" => Some((ListPages, GuiActionContext::default())),
            "FitToContainer" => Some((FitStrategy(Fit::Container), GuiActionContext::default())),
            "FitToWidth" => Some((FitStrategy(Fit::Width), GuiActionContext::default())),
            "FitToHeight" => Some((FitStrategy(Fit::Height), GuiActionContext::default())),
            "FullSize" => Some((FitStrategy(Fit::FullSize), GuiActionContext::default())),
            "VerticalStrip" => {
                Some((Display(DisplayMode::VerticalStrip), GuiActionContext::default()))
            }
            "HorizontalStrip" => {
                Some((Display(DisplayMode::HorizontalStrip), GuiActionContext::default()))
            }
            "DualPage" => Some((Display(DisplayMode::DualPage), GuiActionContext::default())),
            "DualPageReversed" => {
                Some((Display(DisplayMode::DualPageReversed), GuiActionContext::default()))
            }
            "SinglePage" => Some((Display(DisplayMode::Single), GuiActionContext::default())),
            _ => None,
        }
    }

    fn close_on_quit<T: WidgetExt>(self: &Rc<Self>, w: &T) {
        let key = gtk::EventControllerKey::new();
        let g = self.clone();
        key.connect_key_pressed(move |e, a, _b, c| {
            match g.shortcut_from_key(a, c) {
                Some(s) if s == "Quit" => {
                    e.widget()
                        .downcast::<gtk::Window>()
                        .expect("Dialog was somehow not a window")
                        .close();
                }
                _ => (),
            }
            gtk::Inhibit(false)
        });

        w.add_controller(&key);
    }

    fn background_picker(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().background {
            command_info("SetBackground dialog already open", fin);
            d.present();
            return;
        }

        let obg = self.bg.get();

        let dialog =
            gtk::ColorChooserDialog::new(Some("Pick Background Colour"), Some(&self.window));

        dialog.set_rgba(&obg);

        self.close_on_quit(&dialog);

        let g = self.clone();
        dialog.connect_rgba_notify(move |d| {
            g.bg.set(d.rgba());
            g.canvas.inner().set_bg(d.rgba());
            g.canvas.queue_draw();
        });

        let g = self.clone();
        dialog.run_async(move |d, r| {
            if r != gtk::ResponseType::Ok {
                g.bg.set(obg);
                g.canvas.inner().set_bg(obg);
                g.canvas.queue_draw();
            }
            g.open_dialogs.borrow_mut().background.take();
            d.destroy();
            drop(fin)
        });

        let g = self.clone();
        dialog.connect_destroy(move |_| {
            // Nested hacks to avoid dropping two scroll events in a row.
            g.drop_next_scroll.set(false);
        });

        self.open_dialogs.borrow_mut().background = Some(dialog.upcast::<gtk::Window>());
    }

    fn jump_dialog(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().jump {
            command_info("Jump dialog already open", fin);
            d.present();
            return;
        }

        let dialog = gtk::Dialog::builder().transient_for(&self.window).build();
        dialog.set_title(Some("Jump"));

        let entry = gtk::Entry::new();

        let g = self.clone();
        let d = dialog.clone();
        entry.connect_changed(move |e| {
            if let Some(lc) = e.text().chars().last() {
                if lc.is_ascii_digit() || lc == '-' || lc == '+' {
                    return;
                }
                let lc = lc.to_ascii_uppercase().to_string();
                let key = match Key::from_name(&lc) {
                    Some(k) => k,
                    None => return,
                };
                if let Some(s) = g.shortcut_from_key(key, ModifierType::empty()) {
                    if s == "Quit" {
                        d.close();
                    }
                }
            }
        });

        let g = self.clone();
        let d = dialog.clone();
        // In practice this closure will only run once, so the new default value will never
        // be used.
        let fin = Cell::from(fin);
        entry.connect_activate(move |e| {
            let t = "Jump ".to_string() + &e.text().to_string();
            if JUMP_RE.is_match(&t) {
                g.run_command(&t, fin.take());
            }
            d.close();
        });

        dialog.content_area().append(&entry);

        let g = self.clone();
        dialog.run_async(move |d, _r| {
            g.open_dialogs.borrow_mut().jump.take();
            d.content_area().remove(&entry);
            d.destroy();
        });

        let g = self.clone();
        dialog.connect_destroy(move |_| {
            // Nested hacks to avoid dropping two scroll events in a row.
            g.drop_next_scroll.set(false);
        });

        self.open_dialogs.borrow_mut().jump = Some(dialog.upcast::<gtk::Window>());
    }

    pub(super) fn run_command(self: &Rc<Self>, cmd: &str, fin: Option<CommandResponder>) {
        trace!("Started running command {}", cmd);
        self.last_action.set(Some(Instant::now()));

        if let Some((gtm, actx)) = self.simple_sends(cmd) {
            self.send_manager((gtm, actx, fin));
            return;
        }

        match cmd {
            "Quit" => {
                closing::close();
                return self.window.close();
            }
            "ToggleUI" => {
                if self.bottom_bar.is_visible() {
                    self.bottom_bar.hide();
                } else {
                    self.bottom_bar.show();
                }
                return;
            }
            "SetBackground" => return self.background_picker(fin),
            "Jump" => return self.jump_dialog(fin),
            "ToggleFullscreen" => {
                return self.window.set_fullscreened(!self.window.is_fullscreen());
            }
            "TogglePlaying" => {
                self.animation_playing.set(!self.animation_playing.get());
                return self.canvas.inner().set_playing(self.animation_playing.get());
            }
            "ScrollDown" => return self.scroll_down(fin),
            "ScrollUp" => return self.scroll_up(fin),
            "ScrollRight" => return self.scroll_right(fin),
            "ScrollLeft" => return self.scroll_left(fin),

            "SnapBottom" => return self.snap(Edge::Bottom, fin),
            "SnapTop" => return self.snap(Edge::Top, fin),
            "SnapRight" => return self.snap(Edge::Right, fin),
            "SnapLeft" => return self.snap(Edge::Left, fin),

            _ => (),
        }

        if let Some(c) = SET_BACKGROUND_RE.captures(cmd) {
            let col = c.get(1).expect("Invalid capture").as_str();
            match RGBA::from_str(col) {
                Ok(rgba) => {
                    self.bg.set(rgba);
                    self.canvas.queue_draw();
                }
                Err(e) => command_error(format!("{:?}", e), fin),
            }
        } else if let Some(c) = JUMP_RE.captures(cmd) {
            let num_res = c.get(2).expect("Invalid capture").as_str().parse::<usize>();

            let mut num = match num_res {
                Ok(n) => n,
                Err(e) => return command_error(e, fin),
            };

            let (direction, actx) = match c.get(1) {
                None => {
                    // Users will enter 1-based pages, but still accept 0.
                    num = num.saturating_sub(1);
                    (Direction::Absolute, ScrollMotionTarget::Start.into())
                }
                Some(m) if m.as_str() == "+" => {
                    (Direction::Forwards, ScrollMotionTarget::Start.into())
                }
                Some(m) if m.as_str() == "-" => {
                    (Direction::Backwards, ScrollMotionTarget::End.into())
                }
                _ => panic!("Invalid jump capture"),
            };
            self.send_manager((ManagerAction::MovePages(direction, num), actx, fin));
        } else if let Some(c) = EXECUTE_RE.captures(cmd) {
            let exe = c.get(1).expect("Invalid capture").as_str().to_string();
            self.send_manager((ManagerAction::Execute(exe), GuiActionContext::default(), fin));
        } else {
            let e = format!("Unrecognized command {:?}", cmd);
            warn!("{}", e);
            if let Some(fin) = fin {
                if let Err(e) = fin.send(Value::String(e)) {
                    error!("Oneshot channel failed to send. {e}");
                }
            }
        }
    }

    pub(super) fn parse_shortcuts() -> AHashMap<ModifierType, AHashMap<Key, String>> {
        let mut shortcuts = AHashMap::new();

        for s in &CONFIG.shortcuts {
            let mut modifiers: ModifierType = ModifierType::from_bits(0).unwrap();
            if let Some(m) = &s.modifiers {
                let m = m.to_lowercase();
                if m.contains("control") {
                    modifiers |= ModifierType::CONTROL_MASK;
                }
                if m.contains("alt") {
                    modifiers |= ModifierType::ALT_MASK;
                }
                if m.contains("shift") {
                    modifiers |= ModifierType::SHIFT_MASK;
                }
                if m.contains("super") {
                    modifiers |= ModifierType::SUPER_MASK;
                }
                if m.contains("command") {
                    modifiers |= ModifierType::META_MASK;
                }
            };

            let inner = match shortcuts.entry(modifiers) {
                Entry::Occupied(inner) => inner.into_mut(),
                Entry::Vacant(vacant) => vacant.insert(AHashMap::new()),
            };

            let k = Key::from_name(&s.key)
                .unwrap_or_else(|| panic!("{}", format!("Could not decode Key: {}", &s.key)));
            inner.insert(k, s.action.clone());
        }
        shortcuts
    }
}
