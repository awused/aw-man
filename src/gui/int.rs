use gtk::gdk::keys::Key;
use once_cell::sync::Lazy;
use regex::{self, Regex};
use serde_json::Value;

use super::*;
use crate::com::Direction;

// These are only accessed from one thread but it's cleaner to use sync::Lazy
static SET_BACKGROUND_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^SetBackground ([^ ]+)$").unwrap());
static JUMP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Jump (\+|-)?(\d+)$").unwrap());
static EXECUTE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Execute (.+)$").unwrap());

#[derive(Debug, Hash, Eq, PartialEq)]
pub(super) enum Dialogs {
    Background,
    Jump,
}

fn command_error<T: std::fmt::Display>(e: T, fin: Option<CommandResponder>) {
    let e = format!("{}", e);
    error!("{}", e);
    if let Some(s) = fin {
        s.send(serde_json::Value::String(e))
            .expect("Oneshot channel unexpected failed to send.");
    }
}

fn command_info<T: std::fmt::Display>(e: T, fin: Option<CommandResponder>) {
    let e = format!("{}", e);
    info!("{}", e);
    if let Some(s) = fin {
        s.send(serde_json::Value::String(e))
            .expect("Oneshot channel unexpected failed to send.");
    }
}

impl Gui {
    pub(super) fn setup_interaction(self: &Rc<Self>) {
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
        );

        let g = self.clone();
        scroll.connect_scroll(move |_e, _x, y| {
            g.handle_scroll(y);
            gtk::Inhibit(false)
        });

        self.canvas.add_controller(&scroll);

        let key = gtk::EventControllerKey::new();

        let g = self.clone();
        key.connect_key_pressed(move |_e, a, _b, c| {
            if let Some(s) = g.shortcut_from_key(a, c) {
                g.run_command(s, None);
            }
            gtk::Inhibit(false)
        });

        self.window.add_controller(&key);
    }

    fn handle_scroll(self: &Rc<Self>, y: f64) {
        trace!("Started responding to scroll");
        self.last_action.set(Some(Instant::now()));

        if y > 0.0 {
            self.scroll_down(None);
        } else {
            self.scroll_up(None);
        }
    }

    // False positive: https://github.com/rust-lang/rust-clippy/issues/5787
    #[allow(clippy::needless_lifetimes)]
    fn shortcut_from_key<'a>(self: &'a Rc<Self>, k: Key, mods: ModifierType) -> Option<&'a String> {
        let mods = mods & !ModifierType::LOCK_MASK;
        let upper = k.to_upper();

        self.shortcuts.get(&mods)?.get(&upper)
    }

    fn simple_sends(self: &Rc<Self>, s: &str) -> Option<ManagerAction> {
        use Direction::*;
        use ManagerAction::*;

        match s {
            "NextPage" => {
                self.scroll_motion_target.set(ScrollPos::Start);
                Some(MovePages(Forwards, 1))
            }
            "PreviousPage" => {
                self.scroll_motion_target.set(ScrollPos::End);
                Some(MovePages(Backwards, 1))
            }
            "FirstPage" => {
                self.scroll_motion_target.set(ScrollPos::Start);
                Some(MovePages(Absolute, 0))
            }
            "LastPage" => {
                self.scroll_motion_target.set(ScrollPos::Start);
                Some(MovePages(Absolute, self.state.borrow().archive_len))
            }
            "NextArchive" => {
                self.scroll_motion_target.set(ScrollPos::Start);
                Some(NextArchive)
            }
            "PreviousArchive" => {
                self.scroll_motion_target.set(ScrollPos::Start);
                Some(PreviousArchive)
            }
            "ToggleUpscaling" => Some(ToggleUpscaling),
            "ToggleMangaMode" => Some(ToggleManga),
            "Status" => Some(Status),
            "FullSize" => Some(FitStrategy(Fit::FullSize)),
            "FitToContainer" => Some(FitStrategy(Fit::Container)),
            "FitToWidth" => Some(FitStrategy(Fit::Width)),
            "FitToHeight" => Some(FitStrategy(Fit::Height)),
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
                        .expect("Got event from non-existent widget")
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
        if let Some(d) = self.open_dialogs.borrow().get(&Dialogs::Background) {
            command_info("SetBackground dialog already open", fin);
            d.present();
            return;
        }

        let obg = self.bg.get();

        let dialog =
            gtk::ColorChooserDialog::new(Some("Pick Background Colour"), Some(&self.window));

        self.close_on_quit(&dialog);

        let g = self.clone();
        dialog.connect_rgba_notify(move |d| {
            g.bg.set(d.rgba());
            g.canvas.queue_draw();
        });

        let g = self.clone();
        dialog.run_async(move |d, r| {
            if r != gtk::ResponseType::Ok {
                g.bg.set(obg);
                g.canvas.queue_draw();
            }
            g.open_dialogs.borrow_mut().remove(&Dialogs::Background);
            d.destroy();
            drop(fin)
        });

        self.open_dialogs
            .borrow_mut()
            .insert(Dialogs::Background, dialog.upcast::<gtk::Window>());
    }

    fn jump_dialog(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if let Some(d) = self.open_dialogs.borrow().get(&Dialogs::Jump) {
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
                if lc.is_digit(10) || lc == '-' || lc == '+' {
                    return;
                }
                let lc = lc.to_ascii_uppercase().to_string();
                if let Some(s) = g.shortcut_from_key(Key::from_name(&lc), ModifierType::empty()) {
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
            g.open_dialogs.borrow_mut().remove(&Dialogs::Jump);
            d.content_area().remove(&entry);
            d.destroy();
        });

        self.open_dialogs
            .borrow_mut()
            .insert(Dialogs::Jump, dialog.upcast::<gtk::Window>());
    }

    pub(super) fn run_command(self: &Rc<Self>, cmd: &str, fin: Option<CommandResponder>) {
        trace!("Started running command {}", cmd);
        self.last_action.set(Some(Instant::now()));

        if let Some(gtm) = self.simple_sends(cmd) {
            self.manager_sender
                .send((gtm, fin))
                .expect("Unexpected failed to send from Gui to Manager");
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
            "ScrollDown" => return self.scroll_down(fin),
            "ScrollUp" => return self.scroll_up(fin),
            "ScrollRight" => return self.scroll_right(fin),
            "ScrollLeft" => return self.scroll_left(fin),

            _ => (),
        }

        if let Some(c) = SET_BACKGROUND_RE.captures(cmd) {
            let col = c.get(1).expect("Invalid capture").as_str();
            match gdk::RGBA::from_str(col) {
                Ok(rgba) => {
                    self.bg.set(rgba);
                    self.canvas.queue_draw();
                }
                Err(e) => command_error(format!("{:?}", e), fin),
            }
        } else if let Some(c) = JUMP_RE.captures(cmd) {
            let num_res = c.get(2).expect("Invalid capture").as_str().parse::<usize>();
            let num;
            match num_res {
                Ok(n) => num = n,
                Err(e) => return command_error(e, fin),
            }
            let direction = match c.get(1) {
                None => {
                    self.scroll_motion_target.set(ScrollPos::Start);
                    Direction::Absolute
                }
                Some(m) if m.as_str() == "+" => {
                    self.scroll_motion_target.set(ScrollPos::Start);
                    Direction::Forwards
                }
                Some(m) if m.as_str() == "-" => {
                    self.scroll_motion_target.set(ScrollPos::End);
                    Direction::Backwards
                }
                _ => panic!("Invalid jump capture"),
            };
            self.manager_sender
                .send((ManagerAction::MovePages(direction, num), fin))
                .expect("Unexpected failed to send from Gui to Manager");
        } else if let Some(c) = EXECUTE_RE.captures(cmd) {
            let exe = c.get(1).expect("Invalid capture").as_str().to_string();
            self.manager_sender
                .send((ManagerAction::Execute(exe), fin))
                .expect("Unexpected failed to send from Gui to Manager");
        } else {
            let e = format!("Unrecognized command {:?}", cmd);
            warn!("{}", e);
            if let Some(fin) = fin {
                drop(fin.send(Value::String(e)));
            }
        }
    }

    pub(super) fn parse_shortcuts() -> HashMap<ModifierType, HashMap<u32, String>> {
        let mut shortcuts = HashMap::new();

        for s in &config::CONFIG.shortcuts {
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

            let inner = if let Some(x) = shortcuts.get_mut(&modifiers) {
                x
            } else {
                shortcuts.insert(modifiers, HashMap::new());
                shortcuts.get_mut(&modifiers).unwrap()
            };

            let k = *Key::from_name(&s.key);
            inner.insert(k, s.action.clone());
        }
        shortcuts
    }
}
