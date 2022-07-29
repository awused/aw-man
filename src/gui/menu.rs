use std::collections::hash_map::Entry;
use std::rc::Rc;

use ahash::AHashMap;
use gtk::gdk::Rectangle;
use gtk::gio::{Menu, MenuItem, SimpleAction, SimpleActionGroup};
use gtk::glib::{ToVariant, Variant, VariantTy};
use gtk::prelude::ActionMapExt;
use gtk::traits::{EventControllerExt, GestureSingleExt, PopoverExt, RootExt, WidgetExt};
use gtk::{GestureClick, PopoverMenu, PositionType};

use super::Gui;
use crate::com::{DisplayMode, Fit, GuiState};
use crate::config::{ContextMenuGroup, CONFIG};

#[derive(Debug)]
pub(super) struct GuiMenu {
    // Checkboxes
    manga: SimpleAction,
    upscaling: SimpleAction,
    // TODO
    // fullscreen
    // show_ui/hide_ui
    // playing

    // Radio buttons
    fit: SimpleAction,
    display: SimpleAction,

    // Everything else
    command: SimpleAction,
}

// TODO -- this can be redone with enums static mappings
fn action_for(command: &str) -> (&str, Option<Variant>) {
    match command {
        "ToggleMangaMode" => ("manga", None),
        "ToggleUpscaling" => ("upscaling", None),
        "FitToContainer" | "FitToWidth" | "FitToHeight" | "FullSize" => {
            ("fit", Some(command.to_variant()))
        }
        "SinglePage" | "VerticalStrip" | "HorizontalStrip" | "DualPage" | "DualPageReversed" => {
            ("display", Some(command.to_variant()))
        }
        _ => ("action", Some(command.to_variant())),
    }
}


impl GuiMenu {
    pub(super) fn new(gui: &Rc<Gui>) -> Self {
        let manga = SimpleAction::new_stateful("manga", None, &false.to_variant());

        let g = gui.clone();
        manga.connect_activate(move |_, _| {
            g.run_command("ToggleMangaMode", None);
        });

        let upscaling = SimpleAction::new_stateful("upscaling", None, &false.to_variant());

        let g = gui.clone();
        upscaling.connect_activate(move |_, _| {
            g.run_command("ToggleUpscaling", None);
        });


        let fit = SimpleAction::new_stateful(
            "fit",
            Some(VariantTy::new("s").unwrap()),
            &"FitToContainer".to_variant(),
        );

        let g = gui.clone();
        fit.connect_activate(move |_a, v| {
            let cmd = v.unwrap().str().unwrap();
            g.run_command(cmd, None);
        });

        let display = SimpleAction::new_stateful(
            "display",
            Some(VariantTy::new("s").unwrap()),
            &"SinglePage".to_variant(),
        );

        let g = gui.clone();
        display.connect_activate(move |_a, v| {
            let cmd = v.unwrap().str().unwrap();
            g.run_command(cmd, None);
        });

        let command = SimpleAction::new("action", Some(VariantTy::new("s").unwrap()));

        let g = gui.clone();
        command.connect_activate(move |_a, v| {
            let action = v.unwrap().str().unwrap();
            g.run_command(action, None);
        });

        let s = Self { manga, upscaling, fit, display, command };

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
        action_group.add_action(&self.fit);
        action_group.add_action(&self.display);
        action_group.add_action(&self.command);

        gui.window.insert_action_group("context-menu", Some(&action_group));

        let menu = Menu::new();

        let mut submenus = AHashMap::new();
        let mut sections = AHashMap::new();

        for entry in &CONFIG.context_menu {
            let menuitem = MenuItem::new(Some(&entry.name), None);
            let action = action_for(&entry.action);

            menuitem.set_action_and_target_value(
                Some(&("context-menu.".to_owned() + action.0)),
                action.1.as_ref(),
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
        menu.set_position(PositionType::Right);

        let g = gui.clone();
        menu.connect_closed(move |_| {
            // Nested hacks to avoid dropping two scroll events in a row.
            g.drop_next_scroll.set(false);
            // Hack around GTK PopoverMenus taking focus to the grave with them.
            g.window.set_focus(Some(&g.window));
        });

        let right_click = GestureClick::new();
        right_click.set_button(3);

        menu.set_parent(&gui.window);
        right_click.connect_pressed(move |e, _clicked, x, y| {
            let ev = e.current_event().expect("Impossible");
            if ev.triggers_context_menu() {
                let rect = Rectangle::new(x as i32, y as i32, 1, 1);
                menu.set_pointing_to(Some(&rect));
                menu.popup();
            }
        });

        gui.window.add_controller(&right_click);
    }

    pub(super) fn diff_state(&self, old_state: &GuiState, new_state: &GuiState) {
        if old_state.modes.manga != new_state.modes.manga {
            self.manga.set_state(&new_state.modes.manga.to_variant());
        }

        if old_state.modes.upscaling != new_state.modes.upscaling {
            self.upscaling.set_state(&new_state.modes.upscaling.to_variant());
        }

        if old_state.modes.fit != new_state.modes.fit {
            let fit = match new_state.modes.fit {
                Fit::Container => "FitToContainer",
                Fit::Height => "FitToHeight",
                Fit::Width => "FitToWidth",
                Fit::FullSize => "FullSize",
            };
            self.fit.set_state(&fit.to_variant());
        }

        if old_state.modes.display != new_state.modes.display {
            let display = match new_state.modes.display {
                DisplayMode::Single => "Single",
                DisplayMode::VerticalStrip => "VerticalStrip",
                DisplayMode::HorizontalStrip => "HorizontalStrip",
                DisplayMode::DualPage => "DualPage",
                DisplayMode::DualPageReversed => "DualPageReversed",
            };
            self.display.set_state(&display.to_variant());
        }
    }
}
