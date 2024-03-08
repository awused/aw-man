use std::collections::hash_map::Entry;
use std::rc::Rc;

use ahash::AHashMap;
use gtk::gdk::prelude::ActionExt;
use gtk::gdk::Rectangle;
use gtk::gio::{Menu, MenuItem, SimpleAction, SimpleActionGroup};
use gtk::glib::{Variant, VariantTy};
use gtk::prelude::*;
use gtk::{GestureClick, PopoverMenu, PositionType};

use super::Gui;
use crate::com::{DisplayMode, Fit, GuiState, Toggle};
use crate::config::{ContextMenuGroup, CONFIG};

#[derive(Debug)]
pub(super) struct GuiMenu {
    // Checkboxes
    manga: SimpleAction,
    upscaling: SimpleAction,
    // TODO
    // fullscreen
    // show_ui/hide_ui
    playing: SimpleAction,

    // Radio buttons
    fit: SimpleAction,
    display: SimpleAction,

    // Everything else
    command: SimpleAction,
}

enum GuiCommand {
    Manga,
    Upscaling,
    Playing,
    Fit(Variant),
    Display(Variant),
    Action(Variant),
}

impl From<&str> for GuiCommand {
    fn from(mut command: &str) -> Self {
        if let Some((cmd, arg)) = command.split_once(' ') {
            if let Ok(arg) = Toggle::try_from(arg.trim_start()) {
                match arg {
                    Toggle::Change => command = cmd,
                    // These don't work nicely with checkboxes, can't be bothered to figure them
                    // out.
                    Toggle::On | Toggle::Off => {}
                }
            }
        }

        match command {
            "ToggleMangaMode" | "MangaMode" => Self::Manga,
            "ToggleUpscaling" | "Upscaling" => Self::Upscaling,
            "TogglePlaying" | "Playing" => Self::Playing,
            "FitToContainer" | "FitToWidth" | "FitToHeight" | "FullSize" => {
                Self::Fit(command.to_variant())
            }
            "SinglePage" | "VerticalStrip" | "HorizontalStrip" | "DualPage"
            | "DualPageReversed" => Self::Display(command.to_variant()),
            _ => Self::Action(command.to_variant()),
        }
    }
}

impl GuiCommand {
    const fn action(&self) -> &'static str {
        match self {
            Self::Manga => "manga",
            Self::Upscaling => "upscaling",
            Self::Playing => "playing",
            Self::Fit(_) => "fit",
            Self::Display(_) => "display",
            Self::Action(_) => "action",
        }
    }

    const fn variant(&self) -> Option<&Variant> {
        match self {
            Self::Manga | Self::Upscaling | Self::Playing => None,
            Self::Fit(v) | Self::Display(v) | Self::Action(v) => Some(v),
        }
    }
}


impl GuiMenu {
    pub(super) fn new(gui: &Rc<Gui>) -> Self {
        let manga =
            SimpleAction::new_stateful(GuiCommand::Manga.action(), None, &false.to_variant());
        let g = gui.clone();
        manga.connect_activate(move |_, _| {
            g.run_command("MangaMode toggle", None);
        });


        let upscaling =
            SimpleAction::new_stateful(GuiCommand::Upscaling.action(), None, &false.to_variant());
        let g = gui.clone();
        upscaling.connect_activate(move |_, _| {
            g.run_command("Upscaling toggle", None);
        });


        let playing =
            SimpleAction::new_stateful(GuiCommand::Playing.action(), None, &true.to_variant());
        let g = gui.clone();
        playing.connect_activate(move |_, _| {
            g.run_command("Playing toggle", None);
        });


        let fit = SimpleAction::new_stateful(
            GuiCommand::Fit(().to_variant()).action(),
            Some(VariantTy::new("s").unwrap()),
            &"FitToContainer".to_variant(),
        );
        let g = gui.clone();
        fit.connect_activate(move |_a, v| {
            let cmd = v.unwrap().str().unwrap();
            g.run_command(cmd, None);
        });


        let display = SimpleAction::new_stateful(
            GuiCommand::Display(().to_variant()).action(),
            Some(VariantTy::new("s").unwrap()),
            &"SinglePage".to_variant(),
        );
        let g = gui.clone();
        display.connect_activate(move |_a, v| {
            let cmd = v.unwrap().str().unwrap();
            g.run_command(cmd, None);
        });


        let command = SimpleAction::new(
            GuiCommand::Action(().to_variant()).action(),
            Some(VariantTy::new("s").unwrap()),
        );
        let g = gui.clone();
        command.connect_activate(move |_a, v| {
            let action = v.unwrap().str().unwrap();
            g.run_command(action, None);
        });

        let s = Self {
            manga,
            upscaling,
            playing,
            fit,
            display,
            command,
        };

        s.setup(gui);
        s
    }

    fn setup(&self, gui: &Rc<Gui>) {
        if CONFIG.context_menu.is_empty() {
            return;
        }

        let action_group = SimpleActionGroup::new();
        action_group.add_action(&self.manga);
        action_group.add_action(&self.upscaling);
        action_group.add_action(&self.playing);
        action_group.add_action(&self.fit);
        action_group.add_action(&self.display);
        action_group.add_action(&self.command);

        gui.window.insert_action_group("context-menu", Some(&action_group));

        let menu = Menu::new();

        let mut submenus = AHashMap::new();
        let mut sections = AHashMap::new();

        for entry in &CONFIG.context_menu {
            let menuitem = MenuItem::new(Some(&entry.name), None);
            let cmd = GuiCommand::from(entry.action.trim());

            menuitem.set_action_and_target_value(
                Some(&("context-menu.".to_owned() + cmd.action())),
                cmd.variant(),
            );

            let menu = match &entry.group {
                Some(ContextMenuGroup::Submenu(sm)) => match submenus.entry(sm.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let submenu = Menu::new();
                        menu.append_submenu(Some(sm), &submenu);
                        e.insert(submenu)
                    }
                },
                Some(ContextMenuGroup::Section(sc)) => match sections.entry(sc.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let section = Menu::new();
                        menu.append_section(Some(sc), &section);
                        e.insert(section)
                    }
                },
                None => &menu,
            };

            menu.append_item(&menuitem);
        }

        let menu = PopoverMenu::from_model_full(&menu, gtk::PopoverMenuFlags::NESTED);
        menu.set_has_arrow(false);
        menu.set_parent(&gui.window);
        menu.set_position(PositionType::Right);
        menu.set_valign(gtk::Align::Start);

        let g = gui.clone();
        menu.connect_closed(move |_| {
            // Hack around GTK PopoverMenus taking focus to the grave with them.
            GtkWindowExt::set_focus(&g.window, Some(&g.window));
        });

        let right_click = GestureClick::new();
        right_click.set_button(3);

        right_click.connect_pressed(move |e, _clicked, x, y| {
            let ev = e.current_event().unwrap();
            if ev.triggers_context_menu() {
                let rect = Rectangle::new(x as i32, y as i32, 1, 1);
                menu.set_pointing_to(Some(&rect));
                menu.popup();
            }
        });

        gui.window.add_controller(right_click);
    }

    pub(super) fn diff_state(&self, old_state: &GuiState, new_state: &GuiState) {
        if old_state.modes.manga != new_state.modes.manga {
            self.manga.change_state(&new_state.modes.manga.to_variant());
        }

        if old_state.modes.upscaling != new_state.modes.upscaling {
            self.upscaling.change_state(&new_state.modes.upscaling.to_variant());
        }

        if old_state.modes.fit != new_state.modes.fit {
            let fit = match new_state.modes.fit {
                Fit::Container => "FitToContainer",
                Fit::Height => "FitToHeight",
                Fit::Width => "FitToWidth",
                Fit::FullSize => "FullSize",
            };
            self.fit.change_state(&fit.to_variant());
        }

        if old_state.modes.display != new_state.modes.display {
            let display = match new_state.modes.display {
                DisplayMode::Single => "Single",
                DisplayMode::VerticalStrip => "VerticalStrip",
                DisplayMode::HorizontalStrip => "HorizontalStrip",
                DisplayMode::DualPage => "DualPage",
                DisplayMode::DualPageReversed => "DualPageReversed",
            };
            self.display.change_state(&display.to_variant());
        }
    }

    pub(super) fn set_playing(&self, v: bool) {
        self.playing.change_state(&v.to_variant());
    }
}
