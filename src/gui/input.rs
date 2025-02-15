// Sure is Gnome
#![allow(deprecated)]

use std::cell::{Cell, Ref};
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::str::FromStr;
use std::time::Instant;
use std::usize;

use ahash::AHashMap;
use gtk::gdk::{DragAction, FileList, Key, ModifierType, RGBA};
use gtk::gio::File;
use gtk::glib::{BoxedAnyObject, Propagation};
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
use crate::config::{CONFIG, Shortcut};
use crate::gui::clipboard;
use crate::gui::layout::Edge;

// These are only accessed from one thread but it's cleaner to use sync::Lazy
static JUMP_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^Jump (\+|-)?(\d+)( ([[:alpha:]]+))?$").unwrap());
static OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Open (.+)$").unwrap());

#[derive(Debug, Default)]
pub(super) struct OpenDialogs {
    background: Option<gtk::ColorChooserDialog>,
    jump: Option<gtk::Window>,
    file: Option<gtk::FileChooserNative>,
    help: Option<gtk::Window>,
}

fn command_error<T: std::fmt::Display>(e: T, fin: Option<CommandResponder>) {
    error!("{e}");
    if let Some(s) = fin {
        if let Err(e) = s.send(Value::String(e.to_string())) {
            error!("Oneshot channel failed to send. {e}");
        }
    }
}

fn command_info<T: std::fmt::Display>(e: T, fin: Option<CommandResponder>) {
    info!("{e}");
    if let Some(s) = fin {
        if let Err(e) = s.send(Value::String(e.to_string())) {
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


        let g = self.clone();
        scroll.connect_scroll(move |_e, x, y| {
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
            Propagation::Proceed
        });


        self.overlay.add_controller(scroll);

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

        let g = self.clone();
        drag.connect_drag_end(move |_e, _x, _y| {
            g.layout_manager.borrow_mut().end_drag();
        });

        self.canvas.add_controller(drag);

        let key = gtk::EventControllerKey::new();

        let g = self.clone();
        key.connect_key_pressed(move |_e, a, _b, c| {
            if let Some(s) = g.shortcut_from_key(a, c) {
                g.run_command(s, None);
            }
            Propagation::Proceed
        });

        self.window.add_controller(key);

        let click = gtk::GestureClick::new();

        let g = self.clone();
        click.connect_released(move |_c, n, _x, _y| {
            if n == 2 {
                if g.window.is_maximized() {
                    g.window.unmaximize()
                } else {
                    g.window.maximize()
                }
            }
        });

        self.window.add_controller(click);

        let drop_target = gtk::DropTarget::new(FileList::static_type(), DragAction::COPY);

        let g = self.clone();
        drop_target.connect_drop(move |_dt, v, _x, _y| {
            let files = match v.get::<FileList>() {
                Ok(files) => files.files(),
                Err(e) => {
                    error!("Error reading files from drop event: {e}");
                    return true;
                }
            };
            let paths: Vec<_> = files.into_iter().filter_map(|f| f.path()).collect();

            g.send_manager((ManagerAction::Open(paths), ScrollMotionTarget::Start.into(), None));

            true
        });

        self.window.add_controller(drop_target);
    }

    fn shortcut_from_key<'a>(self: &'a Rc<Self>, k: Key, mods: ModifierType) -> Option<&'a String> {
        let mods = mods & !ModifierType::LOCK_MASK;
        let upper = k.to_upper();

        self.shortcuts.get(&mods)?.get(&upper)
    }

    // TODO -- next_jump and prev_jump can be racy during scripting
    fn next_jump(self: &Rc<Self>) -> usize {
        let state = self.state.borrow();
        match &state.content {
            GuiContent::Single { .. } | GuiContent::Strip { .. } => 1,
            // Do our best to maintain alignment. This is a bit inconsistent with strip
            // mode but keeps the user from accidentally breaking things.
            GuiContent::Dual { next: OffscreenContent::Nothing, .. } => 0,
            GuiContent::Dual { visible, .. } => visible.count(),
        }
    }

    fn prev_jump(self: &Rc<Self>) -> usize {
        let state = self.state.borrow();
        match state.content {
            GuiContent::Dual { prev: OffscreenContent::Nothing, .. } => 0,
            GuiContent::Dual {
                prev: OffscreenContent::LayoutCompatible(LayoutCount::TwoOrMore),
                ..
            } => 2,
            GuiContent::Single { .. } | GuiContent::Strip { .. } | GuiContent::Dual { .. } => 1,
        }
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
            "NextPage" => Some((MovePages(Forwards, self.next_jump()), Start.into())),
            "PreviousPage" => {
                let scroll_target = match self.state.borrow().modes.display {
                    DisplayMode::Single | DisplayMode::DualPage | DisplayMode::DualPageReversed => {
                        End
                    }
                    DisplayMode::VerticalStrip | DisplayMode::HorizontalStrip => Start,
                };

                Some((MovePages(Backwards, self.prev_jump()), scroll_target.into()))
            }
            "FirstPage" => Some((MovePages(Absolute, 0), Start.into())),
            "LastPage" => Some((MovePages(Absolute, usize::MAX), Start.into())),
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
                        .and_downcast::<gtk::Window>()
                        .expect("Dialog was somehow not a window")
                        .close();
                }
                _ => (),
            }
            Propagation::Proceed
        });

        w.add_controller(key);
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

        self.open_dialogs.borrow_mut().background = Some(dialog);
    }

    fn jump_dialog(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().jump {
            command_info("Jump dialog already open", fin);
            d.present();
            return;
        }

        let dialog = gtk::Window::builder().title("Jump").transient_for(&self.window).build();

        // It's enough, for now, to just set this at dialog spawn time.
        #[cfg(windows)]
        dialog.add_css_class(self.win32.dpi_class());

        let entry = gtk::Entry::new();

        let g = self.clone();
        let d = dialog.downgrade();
        entry.connect_changed(move |e| {
            if let Some(lc) = e.text().chars().last() {
                if lc.is_ascii_digit() || lc == '-' || lc == '+' {
                    return;
                }
                let lc = lc.to_ascii_uppercase().to_string();
                let Some(key) = Key::from_name(lc) else {
                    return;
                };

                if let Some(s) = g.shortcut_from_key(key, ModifierType::empty()) {
                    if s == "Quit" {
                        if let Some(d) = d.upgrade() {
                            d.close()
                        }
                    }
                }
            }
        });

        let g = self.clone();
        let d = dialog.downgrade();
        // In practice this closure will only run once, so the new default value will never
        // be used.
        let fin = Cell::from(fin);
        entry.connect_activate(move |e| {
            let t = "Jump ".to_string() + &e.text();
            if JUMP_RE.is_match(&t) {
                g.run_command(&t, fin.take());
            }
            if let Some(d) = d.upgrade() {
                d.close()
            }
        });

        dialog.set_child(Some(&entry));

        let g = self.clone();
        dialog.connect_close_request(move |d| {
            g.open_dialogs.borrow_mut().jump.take();
            d.destroy();
            Propagation::Proceed
        });

        dialog.set_visible(true);

        self.open_dialogs.borrow_mut().jump = Some(dialog);
    }

    fn file_open_dialog(self: &Rc<Self>, folders: bool, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().file {
            command_info("Open file dialog already open", fin);
            d.set_visible(true);
            return;
        }

        /*
        // New implementation using FileDialog. Does not work well.
        // See https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/issues/84.
        let dir = gtk::gio::File::for_path(&self.state.borrow().current_dir);

        let dialog = gtk::FileDialog::builder().initial_folder(&dir).build();

        let g = self.clone();
        if !folders {
            dialog.open_multiple(Some(&self.window), None::<&Cancellable>, move |r| {
                match r {
                    Ok(files) => {
                        let files = files
                            .into_iter()
                            .filter_map(Result::ok)
                            .filter_map(|f| f.dynamic_cast::<File>().ok())
                            .filter_map(|f| f.path())
                            .collect();
                        g.send_manager((
                            ManagerAction::Open(files),
                            ScrollMotionTarget::Start.into(),
                            fin,
                        ));
                    }
                    Err(e) => {
                        error!("{e}");
                    }
                }
                g.open_dialogs.borrow_mut().file.take();
            });
        } else {
            dialog.select_folder(Some(&self.window), None::<&Cancellable>, move |r| {
                match r {
                    Ok(folder) => {
                        println!("Selected folder {folder:?}");
                        let Some(folder) = folder.path() else {
                            return;
                        };

                        g.send_manager((
                            ManagerAction::Open(vec![folder]),
                            ScrollMotionTarget::Start.into(),
                            fin,
                        ))
                    }
                    Err(e) => {
                        error!("{e}");
                    }
                }
                g.open_dialogs.borrow_mut().file.take();
            });
        }*/

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

        let dir = gtk::gio::File::for_path(self.state.borrow().current_dir.get());
        drop(dialog.set_current_folder(Some(&dir)));

        let g = self.clone();
        dialog.run_async(move |d, a| {
            if a == gtk::ResponseType::Accept {
                let files = d
                    .files()
                    .into_iter()
                    .filter_map(Result::ok)
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

    fn help_dialog(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if let Some(d) = &self.open_dialogs.borrow().help {
            command_info("Help dialog already open", fin);
            d.present();
            return;
        }

        let dialog = gtk::Window::builder().title("Help").transient_for(&self.window).build();

        self.close_on_quit(&dialog);

        // default size might want to scale based on dpi
        dialog.set_default_width(800);
        dialog.set_default_height(600);

        // It's enough, for now, to just set this at dialog spawn time.
        #[cfg(windows)]
        dialog.add_css_class(self.win32.dpi_class());

        let store = gtk::gio::ListStore::new::<BoxedAnyObject>();

        for s in &CONFIG.shortcuts {
            store.append(&BoxedAnyObject::new(s));
        }

        let modifier_factory = gtk::SignalListItemFactory::new();
        modifier_factory.connect_setup(move |_fact, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::new(None);
            label.set_halign(gtk::Align::Start);
            item.set_child(Some(&label))
        });
        modifier_factory.connect_bind(move |_fact, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let child = item.child().and_downcast::<gtk::Label>().unwrap();
            let entry = item.item().and_downcast::<BoxedAnyObject>().unwrap();
            let s: Ref<&Shortcut> = entry.borrow();
            child.set_text(s.modifiers.as_deref().unwrap_or(""));
        });

        let key_factory = gtk::SignalListItemFactory::new();
        key_factory.connect_setup(move |_fact, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::new(None);
            label.set_halign(gtk::Align::Start);
            item.set_child(Some(&label))
        });
        key_factory.connect_bind(move |_fact, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let child = item.child().and_downcast::<gtk::Label>().unwrap();
            let entry = item.item().and_downcast::<BoxedAnyObject>().unwrap();
            let s: Ref<&Shortcut> = entry.borrow();

            match Key::from_name(&s.key).unwrap().to_unicode() {
                // This should avoid most unprintable/weird characters but still translate
                // question, bracketright, etc into characters.
                Some(c) if !c.is_whitespace() && !c.is_control() => {
                    child.set_text(&c.to_string());
                }
                _ => {
                    child.set_text(&s.key);
                }
            }
        });

        let action_factory = gtk::SignalListItemFactory::new();
        action_factory.connect_setup(move |_fact, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::new(None);
            label.set_halign(gtk::Align::Start);
            item.set_child(Some(&label))
        });
        action_factory.connect_bind(move |_fact, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let child = item.child().and_downcast::<gtk::Label>().unwrap();
            let entry = item.item().and_downcast::<BoxedAnyObject>().unwrap();
            let s: Ref<&Shortcut> = entry.borrow();
            child.set_text(&s.action);
        });

        let modifier_column = gtk::ColumnViewColumn::new(Some("Modifiers"), Some(modifier_factory));
        let key_column = gtk::ColumnViewColumn::new(Some("Key"), Some(key_factory));
        let action_column = gtk::ColumnViewColumn::new(Some("Action"), Some(action_factory));
        action_column.set_expand(true);

        let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(store))));
        view.append_column(&modifier_column);
        view.append_column(&key_column);
        view.append_column(&action_column);

        let scrolled =
            gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).build();

        scrolled.set_child(Some(&view));

        dialog.set_child(Some(&scrolled));

        let g = self.clone();
        dialog.connect_close_request(move |d| {
            g.open_dialogs.borrow_mut().help.take();
            d.destroy();
            Propagation::Proceed
        });

        dialog.set_visible(true);

        self.open_dialogs.borrow_mut().help = Some(dialog);
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
                        Err(e) => command_error(format!("{e:?}"), fin),
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
                "Script" => {
                    return self.send_manager((
                        ManagerAction::Script(arg.to_string(), self.get_env()),
                        GuiActionContext::default(),
                        fin,
                    ));
                }

                "ScrollDown" | "ScrollUp" | "ScrollRight" | "ScrollLeft" => {
                    let scroll = match u32::from_str(arg) {
                        Ok(scroll) => scroll,
                        Err(e) => {
                            return command_error(format!("Invalid scroll amount {arg}: {e}"), fin);
                        }
                    };

                    if scroll == 0 {
                        return;
                    }

                    let scroll = NonZeroU32::new(scroll);

                    match cmd {
                        "ScrollDown" => return self.scroll_down(scroll, fin),
                        "ScrollUp" => return self.scroll_up(scroll, fin),
                        "ScrollRight" => return self.scroll_right(scroll, fin),
                        "ScrollLeft" => return self.scroll_left(scroll, fin),
                        _ => unreachable!(),
                    }
                }

                _ => true,
            };

            // For now only toggles work here. Some of the regexes could be eliminated instead.
            if let Ok(arg) = Toggle::try_from(arg) {
                let _ = match cmd {
                    "UI" => {
                        return arg.run_if_change(
                            self.bottom_bar.is_visible(),
                            || self.bottom_bar.set_visible(true),
                            || self.bottom_bar.set_visible(false),
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

            if let Ok(arg) = ScrollMotionTarget::try_from(arg) {
                let _ = match cmd {
                    "NextPage" => {
                        return self.send_manager((
                            ManagerAction::MovePages(Direction::Forwards, self.next_jump()),
                            arg.into(),
                            fin,
                        ));
                    }
                    "PreviousPage" => {
                        return self.send_manager((
                            ManagerAction::MovePages(Direction::Backwards, self.prev_jump()),
                            arg.into(),
                            fin,
                        ));
                    }
                    "FirstPage" => {
                        return self.send_manager((
                            ManagerAction::MovePages(Direction::Absolute, 0),
                            arg.into(),
                            fin,
                        ));
                    }
                    "LastPage" => {
                        return self.send_manager((
                            ManagerAction::MovePages(Direction::Absolute, usize::MAX),
                            arg.into(),
                            fin,
                        ));
                    }

                    _ => true,
                };
            }
        }

        let _ = match cmd {
            "Quit" => {
                if self.exit_requested.get() || closing::closed() {
                    // Abnormal exit or second attempt
                    closing::close();
                    return self.window.close();
                }

                if let Some(cmd) = &CONFIG.quit_command {
                    self.run_command(cmd, None);
                }
                self.exit_requested.set(true);
                self.send_manager((ManagerAction::CleanExit, GuiActionContext::default(), None));
                return;
            }
            "ToggleUI" | "UI" => {
                if self.bottom_bar.is_visible() {
                    self.bottom_bar.set_visible(false);
                } else {
                    self.bottom_bar.set_visible(true);
                }
                return;
            }
            "SetBackground" => return self.background_picker(fin),
            "Jump" => return self.jump_dialog(fin),
            "Open" => return self.file_open_dialog(false, fin),
            "OpenFolder" => return self.file_open_dialog(true, fin),
            "Help" => return self.help_dialog(fin),
            "ToggleFullscreen" | "Fullscreen" => {
                return self.window.set_fullscreened(!self.window.is_fullscreen());
            }
            "TogglePlaying" | "Playing" => {
                self.animation_playing.set(!self.animation_playing.get());
                self.menu.get().unwrap().set_playing(self.animation_playing.get());
                return self.canvas.inner().set_playing(self.animation_playing.get());
            }
            "ScrollDown" => return self.scroll_down(None, fin),
            "ScrollUp" => return self.scroll_up(None, fin),
            "ScrollRight" => return self.scroll_right(None, fin),
            "ScrollLeft" => return self.scroll_left(None, fin),

            "SnapBottom" => return self.snap(Edge::Bottom, fin),
            "SnapTop" => return self.snap(Edge::Top, fin),
            "SnapRight" => return self.snap(Edge::Right, fin),
            "SnapLeft" => return self.snap(Edge::Left, fin),

            "Copy" => {
                let file = self.state.borrow().page_info.as_ref().map(|(_, p)| p.clone());
                let provider = clipboard::SelectionProvider::new(file);

                if let Err(e) = self.window.clipboard().set_content(Some(&provider)) {
                    command_error(format!("{e:?}"), fin);
                }
                return;
            }

            _ => true,
        };

        if let Some(c) = JUMP_RE.captures(cmd) {
            let num_res = c[2].parse::<usize>();

            let mut num = match num_res {
                Ok(n) => n,
                Err(e) => return command_error(e, fin),
            };

            let smt = if let Some(m) = c.get(4) {
                if let Ok(smt) = ScrollMotionTarget::try_from(m.as_str()) {
                    Some(smt)
                } else {
                    let e = format!("Unrecognized command {cmd:?}");
                    warn!("{e}");
                    if let Some(fin) = fin {
                        if let Err(e) = fin.send(Value::String(e)) {
                            error!("Oneshot channel failed to send. {e}");
                        }
                    }
                    return;
                }
            } else {
                None
            };

            let (direction, actx) = match c.get(1) {
                None => {
                    // Users will enter 1-based pages, but still accept 0.
                    num = num.saturating_sub(1);
                    (Direction::Absolute, smt.unwrap_or(ScrollMotionTarget::Start).into())
                }
                Some(m) if m.as_str() == "+" => {
                    (Direction::Forwards, smt.unwrap_or(ScrollMotionTarget::Start).into())
                }
                Some(m) if m.as_str() == "-" => {
                    (Direction::Backwards, smt.unwrap_or(ScrollMotionTarget::End).into())
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

                if let Some((file, rest)) = split {
                    paths.push(file.into());
                    files = rest.trim_start();
                } else {
                    paths.push(files.into());
                    break;
                }
            }

            self.send_manager((ManagerAction::Open(paths), ScrollMotionTarget::Start.into(), fin))
        } else {
            let e = format!("Unrecognized command {cmd:?}");
            warn!("{e}");
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
                .unwrap_or_else(|| panic!("Could not decode Key: {}", &s.key));
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
