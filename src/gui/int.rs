use std::collections::hash_map::Entry;

use ahash::AHashMap;
use gdk::Key;
use once_cell::sync::Lazy;
use regex::{self, Regex};
use serde_json::Value;

use super::*;
use crate::com::Direction;
use crate::config::ContextMenuGroup;

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
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);

        let g = self.clone();
        scroll.connect_scroll_begin(move |_e| {
            g.pad_scrolling.set(true);
        });

        let g = self.clone();
        scroll.connect_scroll_end(move |_e| {
            g.pad_scrolling.set(false);
        });

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
            g.scroll_state.borrow_mut().start_drag();
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

        self.setup_context_menu();
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
            // TODO -- dual page mode handling here.
            "NextPage" => Some((MovePages(Forwards, 1), Start.into())),
            "PreviousPage" => Some((MovePages(Backwards, 1), End.into())),
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
            "FullSize" => Some((FitStrategy(Fit::FullSize), GuiActionContext::default())),
            "FitToContainer" => Some((FitStrategy(Fit::Container), GuiActionContext::default())),
            "FitToWidth" => Some((FitStrategy(Fit::Width), GuiActionContext::default())),
            "FitToHeight" => Some((FitStrategy(Fit::Height), GuiActionContext::default())),
            "VerticalStrip" => {
                Some((Display(DisplayMode::VerticalStrip), GuiActionContext::default()))
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
        if let Some(d) = self.open_dialogs.borrow().get(&Dialogs::Background) {
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

        let g = self.clone();
        dialog.connect_destroy(move |_| {
            // Nested hacks to avoid dropping two scroll events in a row.
            g.drop_next_scroll.set(false);
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
            g.open_dialogs.borrow_mut().remove(&Dialogs::Jump);
            d.content_area().remove(&entry);
            d.destroy();
        });

        let g = self.clone();
        dialog.connect_destroy(move |_| {
            // Nested hacks to avoid dropping two scroll events in a row.
            g.drop_next_scroll.set(false);
        });

        self.open_dialogs
            .borrow_mut()
            .insert(Dialogs::Jump, dialog.upcast::<gtk::Window>());
    }

    pub(super) fn run_command(self: &Rc<Self>, cmd: &str, fin: Option<CommandResponder>) {
        trace!("Started running command {}", cmd);
        self.last_action.set(Some(Instant::now()));

        if let Some((gtm, actx)) = self.simple_sends(cmd) {
            self.manager_sender
                .send((gtm, actx, fin))
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
            "TogglePlaying" => {
                self.animation_playing.set(!self.animation_playing.get());
                return self.displayed.borrow_mut().set_playing(self, self.animation_playing.get());
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
            self.manager_sender
                .send((ManagerAction::MovePages(direction, num), actx, fin))
                .expect("Unexpected failed to send from Gui to Manager");
        } else if let Some(c) = EXECUTE_RE.captures(cmd) {
            let exe = c.get(1).expect("Invalid capture").as_str().to_string();
            self.manager_sender
                .send((ManagerAction::Execute(exe), GuiActionContext::default(), fin))
                .expect("Unexpected failed to send from Gui to Manager");
        } else {
            let e = format!("Unrecognized command {:?}", cmd);
            warn!("{}", e);
            if let Some(fin) = fin {
                drop(fin.send(Value::String(e)));
            }
        }
    }

    pub(super) fn parse_shortcuts() -> HashMap<ModifierType, HashMap<Key, String>> {
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

            let k = Key::from_name(&s.key)
                .unwrap_or_else(|| panic!("{}", format!("Could not decode Key: {}", &s.key)));
            inner.insert(k, s.action.clone());
        }
        shortcuts
    }

    fn setup_context_menu(self: &Rc<Self>) {
        if config::CONFIG.context_menu.is_empty() {
            return;
        }

        let action = gio::SimpleAction::new("action", Some(glib::VariantTy::new("s").unwrap()));

        let g = self.clone();
        action.connect_activate(move |_a, v| {
            let action = v.unwrap().get::<String>().unwrap();

            g.run_command(&action, None);
        });

        let action_group = gio::SimpleActionGroup::new();
        action_group.add_action(&action);

        self.window.insert_action_group("context-menu", Some(&action_group));

        let menu = gio::Menu::new();

        let mut submenus = AHashMap::new();
        let mut sections = AHashMap::new();

        for entry in &config::CONFIG.context_menu {
            let menuitem = gio::MenuItem::new(Some(&entry.name), None);
            menuitem.set_action_and_target_value(
                Some("context-menu.action"),
                Some(&entry.action.to_variant()),
            );

            let menu = match &entry.group {
                Some(ContextMenuGroup::Submenu(sm)) => match submenus.entry(sm.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let submenu = gio::Menu::new();
                        menu.append_submenu(Some(sm), &submenu);
                        e.insert(submenu)
                    }
                },
                Some(ContextMenuGroup::Section(sc)) => match sections.entry(sc.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let section = gio::Menu::new();
                        menu.append_section(Some(sc), &section);
                        e.insert(section)
                    }
                },
                None => &menu,
            };

            menu.append_item(&menuitem);
        }

        let menu = gtk::PopoverMenu::from_model_full(&menu, gtk::PopoverMenuFlags::NESTED);
        menu.set_has_arrow(false);
        menu.set_position(gtk::PositionType::Right);

        let g = self.clone();
        menu.connect_closed(move |_| {
            // Nested hacks to avoid dropping two scroll events in a row.
            g.drop_next_scroll.set(false);
            // Hack around GTK PopoverMenus taking focus to the grave with them.
            g.window.set_focus(Some(&g.window));
        });

        let right_click = gtk::GestureClick::new();
        right_click.set_button(3);

        menu.set_parent(&self.window);
        right_click.connect_pressed(move |e, _clicked, x, y| {
            let ev = e.current_event().expect("Impossible");
            if ev.triggers_context_menu() {
                let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                menu.set_pointing_to(Some(&rect));
                menu.popup();
            }
        });

        self.window.add_controller(&right_click);
    }
}
