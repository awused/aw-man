use std::cell::Cell;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::rc::Rc;
use std::str::FromStr;
use std::time::Instant;

use ahash::AHashMap;
use gtk::gdk::{DragAction, FileList, Key, ModifierType, RGBA};
use gtk::gio::File;
use gtk::prelude::*;
use once_cell::sync::Lazy;
use regex::{self, Regex};
use serde_json::Value;

use super::Gui;
use crate::closing;
use crate::com::{
    CommandResponder, Direction, DisplayMode, Fit, GuiActionContext, GuiContent, LayoutCount,
    ManagerAction, OffscreenContent, ScrollMotionTarget, Toggle,
};
use crate::config::CONFIG;
use crate::gui::layout::Edge;

// These are only accessed from one thread but it's cleaner to use sync::Lazy
static JUMP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Jump (\+|-)?(\d+)$").unwrap());
static OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Open (.+)$").unwrap());

#[derive(Debug, Default)]
pub(super) struct OpenDialogs {
    background: Option<gtk::ColorChooserDialog>,
    jump: Option<gtk::Dialog>,
    file: Option<gtk::FileChooserNative>,
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

            g.send_manager((ManagerAction::Open(paths), ScrollMotionTarget::Start.into(), None));

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

        if let Some((cmd, arg)) = s.split_once(' ') {
            // For now only toggles work here
            let arg: Toggle = arg.trim_start().try_into().ok()?;

            return match cmd {
                "MangaMode" => Some((Manga(arg), GuiActionContext::default())),
                "Upscaling" => Some((Upscaling(arg), GuiActionContext::default())),
                _ => None,
            };
        }

        match s {
            "NextPage" => {
                let state = self.state.borrow();
                let pages = match state.modes.display {
                    DisplayMode::DualPage | DisplayMode::DualPageReversed => match &state.content {
                        GuiContent::Single { .. } => unreachable!(),
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
                        GuiContent::Single { .. } => unreachable!(),
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
            "ToggleUpscaling" | "Upscaling" => {
                Some((Upscaling(Toggle::Change), GuiActionContext::default()))
            }
            "ToggleMangaMode" | "MangaMode" => {
                Some((Manga(Toggle::Change), GuiActionContext::default()))
            }
            "Status" => Some((Status(self.get_env()), GuiActionContext::default())),
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

        // It's enough, for now, to just set this at dialog spawn time.
        #[cfg(windows)]
        dialog.add_css_class(self.win32.dpi_class());

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

        self.open_dialogs.borrow_mut().background = Some(dialog);
    }

    fn jump_dialog(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().jump {
            command_info("Jump dialog already open", fin);
            d.present();
            return;
        }

        let dialog = gtk::Dialog::builder().transient_for(&self.window).build();
        dialog.set_title(Some("Jump"));

        // It's enough, for now, to just set this at dialog spawn time.
        #[cfg(windows)]
        dialog.add_css_class(self.win32.dpi_class());

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
            let t = "Jump ".to_string() + &e.text();
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

        self.open_dialogs.borrow_mut().jump = Some(dialog);
    }

    fn file_open_dialog(self: &Rc<Self>, folders: bool, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().file {
            command_info("Open file dialog already open", fin);
            d.show();
            return;
        }

        let dialog = gtk::FileChooserNative::new(
            None,
            Some(&self.window),
            if folders {
                gtk::FileChooserAction::SelectFolder
            } else {
                gtk::FileChooserAction::Open
            },
            None,
            None,
        );

        // For now, only one directory at a time
        dialog.set_select_multiple(!folders);

        let dir = gtk::gio::File::for_path(&self.state.borrow().current_dir);
        drop(dialog.set_current_folder(Some(&dir)));

        let g = self.clone();
        dialog.run_async(move |d, a| {
            if a == gtk::ResponseType::Accept {
                let files = d
                    .files()
                    .into_iter()
                    .filter_map(|f| f.dynamic_cast::<File>().ok())
                    .filter_map(|f| f.path())
                    .collect();

                g.send_manager((ManagerAction::Open(files), ScrollMotionTarget::Start.into(), fin));
            }

            d.destroy();
            g.open_dialogs.borrow_mut().file.take();
        });


        self.open_dialogs.borrow_mut().file = Some(dialog);
    }

    pub(super) fn run_command(self: &Rc<Self>, cmd: &str, fin: Option<CommandResponder>) {
        let cmd = cmd.trim();

        trace!("Started running command {}", cmd);
        self.last_action.set(Some(Instant::now()));

        if let Some((gtm, actx)) = self.simple_sends(cmd) {
            self.send_manager((gtm, actx, fin));
            return;
        }

        if let Some((cmd, arg)) = cmd.split_once(' ') {
            let arg = arg.trim_start();

            let _ = match cmd {
                "SetBackground" => {
                    match RGBA::from_str(arg) {
                        Ok(rgba) => {
                            self.bg.set(rgba);
                            self.canvas.inner().set_bg(rgba);
                            self.canvas.queue_draw();
                        }
                        Err(e) => command_error(format!("{:?}", e), fin),
                    }
                    return;
                }
                "Execute" => {
                    return self.send_manager((
                        ManagerAction::Execute(arg.to_string(), self.get_env()),
                        GuiActionContext::default(),
                        fin,
                    ));
                }

                _ => true,
            };

            // For now only toggles work here. Some of the regexes could be eliminated instead.
            if let Ok(arg) = Toggle::try_from(arg) {
                let _ = match cmd {
                    "UI" => {
                        return arg.run_if_change(
                            self.bottom_bar.is_visible(),
                            || self.bottom_bar.show(),
                            || self.bottom_bar.hide(),
                        );
                    }
                    "Fullscreen" => {
                        #[cfg(windows)]
                        return arg.run_if_change(
                            self.win32.is_fullscreen(),
                            || {
                                self.win32.fullscreen(self);
                                self.window.set_decorated(false);
                                self.window.add_css_class("nodecorations");
                            },
                            || {
                                self.win32.unfullscreen(self);
                                self.window.set_decorated(true);
                                self.window.remove_css_class("nodecorations");
                            },
                        );

                        #[cfg(unix)]
                        return arg.run_if_change(
                            self.window.is_fullscreen(),
                            || {
                                self.window.set_fullscreened(true);
                                // TODO -- store is_decorated or use a self.decorations to save
                                // that state
                                self.window.set_decorated(false);
                                self.window.add_css_class("nodecorations");
                            },
                            || {
                                self.window.set_fullscreened(false);
                                self.window.set_decorated(true);
                                self.window.remove_css_class("nodecorations");
                            },
                        );
                    }
                    "Playing" => {
                        return if arg.apply_cell(&self.animation_playing) {
                            self.canvas.inner().set_playing(self.animation_playing.get());
                            self.menu.get().unwrap().set_playing(self.animation_playing.get());
                        };
                    }
                    _ => true,
                };
            }
        }

        let _ = match cmd {
            "Quit" => {
                closing::close();
                return self.window.close();
            }
            "ToggleUI" | "UI" => {
                if self.bottom_bar.is_visible() {
                    self.bottom_bar.hide();
                } else {
                    self.bottom_bar.show();
                }
                return;
            }
            "SetBackground" => return self.background_picker(fin),
            "Jump" => return self.jump_dialog(fin),
            "Open" => return self.file_open_dialog(false, fin),
            "OpenFolder" => return self.file_open_dialog(true, fin),
            "ToggleFullscreen" | "Fullscreen" => {
                return self.window.set_fullscreened(!self.window.is_fullscreen());
            }
            "TogglePlaying" | "Playing" => {
                self.animation_playing.set(!self.animation_playing.get());
                self.menu.get().unwrap().set_playing(self.animation_playing.get());
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

            _ => true,
        };

        if let Some(c) = JUMP_RE.captures(cmd) {
            let num_res = c[2].parse::<usize>();

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
        } else if let Some(c) = OPEN_RE.captures(cmd) {
            // These files may be quoted but we don't parse escaped paths.
            let mut files = c[1].trim();
            let mut paths = Vec::new();

            while !files.is_empty() {
                let split = match files.chars().next().unwrap() {
                    q @ ('"' | '\'') => {
                        files = &files[1..];
                        files.split_once(q)
                    }
                    // We don't handle internal quoting or escaping
                    _ => files.split_once(' '),
                };

                match split {
                    Some((file, rest)) => {
                        paths.push(file.into());
                        files = rest.trim_start();
                    }
                    None => {
                        paths.push(files.into());
                        break;
                    }
                }
            }

            self.send_manager((ManagerAction::Open(paths), ScrollMotionTarget::Start.into(), fin))
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

    fn get_env(&self) -> Vec<(String, OsString)> {
        vec![
            #[cfg(unix)]
            ("AWMAN_FULLSCREEN".into(), self.window.is_fullscreen().to_string().into()),
            #[cfg(windows)]
            ("AWMAN_FULLSCREEN".into(), self.win32.is_fullscreen().to_string().into()),
            (
                "AWMAN_ANIMATION_PLAYING".into(),
                self.animation_playing.get().to_string().into(),
            ),
            ("AWMAN_UI_VISIBLE".into(), self.bottom_bar.is_visible().to_string().into()),
            ("AWMAN_BACKGROUND".into(), self.bg.get().to_string().into()),
        ]
    }
}
