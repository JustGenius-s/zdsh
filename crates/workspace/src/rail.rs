use std::sync::Arc;

use agent_settings::{AgentSettings, WindowLayout};
use fs::Fs;
use gpui::{
    Action, Anchor, App, Context, Focusable as _, IntoElement, ParentElement, Pixels, Styled,
    Window, prelude::FluentBuilder as _, px,
};
use gpui_kit::component::{
    Icon as KitIcon, IconName as KitIconName,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    sidebar::{Sidebar as KitSidebar, SidebarMenu, SidebarMenuItem},
};
use project::DisableAiSettings;
use settings::{Settings as _, update_settings_file};
use theme::{ActiveTheme as _, Appearance, SystemAppearance, ThemeRegistry};
use theme_settings::{ThemeAppearanceMode, ThemeSelection, ThemeSettings};
use ui::{IconName, v_flex};
use util::ResultExt as _;

use crate::{UseAgenticLayout, UseClassicLayout, Workspace};

/// The width of GPUI Kit's collapsed sidebar, which the rail is.
pub(crate) const RAIL_WIDTH: Pixels = px(48.);

impl Workspace {
    /// The rail beside the left dock. It stays when the dock collapses, and switches the dock
    /// between its panels.
    pub(crate) fn render_rail(
        &self,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement + use<> {
        let dock = self.left_dock.read(cx);
        let is_open = dock.is_open();
        let active_index = dock.active_panel_index();
        let panel_items = dock
            .panels()
            .enumerate()
            .filter_map(|(index, panel)| {
                if !panel.enabled(cx) {
                    return None;
                }
                let icon = panel.icon(window, cx)?;
                let tooltip = panel.icon_tooltip(window, cx)?;
                let workspace = self.weak_handle();
                Some(
                    SidebarMenuItem::new(tooltip)
                        .icon(rail_icon(icon))
                        .active(is_open && active_index == Some(index))
                        .on_click(move |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    workspace.toggle_rail_panel(index, window, cx)
                                })
                                .log_err();
                        }),
                )
            })
            .collect::<Vec<_>>();
        let fs = self.app_state().fs.clone();

        KitSidebar::new("rail")
            .collapsed(true)
            // The rail and the panel beside it read as one surface, whatever the theme gives
            // GPUI Kit's sidebar.
            .bg(cx.theme().colors().panel_background)
            .border_r_0()
            .child(SidebarMenu::new().children(panel_items))
            .child(
                SidebarMenu::new().child(
                    SidebarMenuItem::new("Extensions")
                        .icon(rail_icon(IconName::Blocks))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(
                                zed_actions::Extensions::default().boxed_clone(),
                                cx,
                            )
                        }),
                ),
            )
            .footer(
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("rail-theme")
                            .ghost()
                            .icon(KitIconName::Palette)
                            .tooltip("Theme")
                            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, cx| {
                                theme_menu(menu, fs.clone(), cx)
                            }),
                    )
                    .child(
                        Button::new("rail-settings")
                            .ghost()
                            .icon(KitIconName::Settings)
                            .tooltip("Settings")
                            .dropdown_menu_with_anchor(Anchor::BottomLeft, settings_menu),
                    ),
            )
    }

    /// Shows the left dock's panel at `panel_index`, or collapses the dock when that panel is
    /// already showing.
    fn toggle_rail_panel(
        &mut self,
        panel_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock = self.left_dock.clone();
        let (is_open, active_index) = {
            let dock = dock.read(cx);
            (dock.is_open(), dock.active_panel_index())
        };
        if is_open && active_index == Some(panel_index) {
            let had_focus = dock.focus_handle(cx).contains_focused(window, cx);
            dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
            if had_focus {
                let pane_focus = self.active_pane.read(cx).focus_handle(cx);
                window.focus(&pane_focus, cx);
            }
        } else {
            dock.update(cx, |dock, cx| {
                dock.activate_panel(panel_index, window, cx);
                dock.set_open(true, window, cx);
            });
            if let Some(panel) = dock.read(cx).active_panel() {
                let panel_focus = panel.activation_focus_handle(cx);
                window.focus(&panel_focus, cx);
            }
        }
        self.serialize_workspace(window, cx);
        cx.notify();
    }
}

/// GPUI Kit sizes a sidebar item's icon to its small text, which is a size smaller than the
/// footer buttons' icons, so the rail sets the buttons' size on its items too.
fn rail_icon(icon: IconName) -> KitIcon {
    KitIcon::empty().path(icon.path()).size_4()
}

fn theme_menu(menu: PopupMenu, fs: Arc<dyn Fs>, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let current_mode = match &ThemeSettings::get_global(cx).theme {
        ThemeSelection::Dynamic { mode, .. } => Some(*mode),
        ThemeSelection::Static(_) => None,
    };
    let current_theme = cx.theme().name.clone();
    let mut themes = ThemeRegistry::global(cx).list();
    themes.sort_by(|a, b| a.name.cmp(&b.name));

    let mut menu = menu.scrollable(true).label("Appearance");
    for (label, mode) in [
        ("System", ThemeAppearanceMode::System),
        ("Light", ThemeAppearanceMode::Light),
        ("Dark", ThemeAppearanceMode::Dark),
    ] {
        let fs = fs.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(current_mode == Some(mode))
                .on_click(move |_, _, cx| set_theme_mode(fs.clone(), mode, cx)),
        );
    }
    menu = menu.separator().label("Theme");
    for theme in themes {
        let fs = fs.clone();
        let name = theme.name.clone();
        let appearance = theme.appearance;
        menu = menu.item(
            PopupMenuItem::new(theme.name.clone())
                .checked(theme.name == current_theme)
                .on_click(move |_, _, cx| set_theme(fs.clone(), name.clone(), appearance, cx)),
        );
    }
    menu.separator().menu(
        "More Themes…",
        zed_actions::theme_selector::Toggle::default().boxed_clone(),
    )
}

fn settings_menu(menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let ai_enabled = !DisableAiSettings::get_global(cx).disable_ai;
    menu.menu("Settings", zed_actions::OpenSettings.boxed_clone())
        .menu("Keymap", Box::new(zed_actions::OpenKeymap))
        .menu(
            "Themes…",
            zed_actions::theme_selector::Toggle::default().boxed_clone(),
        )
        .menu(
            "Icon Themes…",
            zed_actions::icon_theme_selector::Toggle::default().boxed_clone(),
        )
        .menu(
            "Extensions",
            zed_actions::Extensions::default().boxed_clone(),
        )
        .when(ai_enabled, |menu| {
            menu.separator()
                .submenu("Panel Layout", window, cx, |menu, _, cx| {
                    let layout = AgentSettings::get_layout(cx);
                    menu.menu_with_check(
                        "Classic",
                        matches!(layout, WindowLayout::Editor(_)),
                        Box::new(UseClassicLayout),
                    )
                    .menu_with_check(
                        "Agentic",
                        matches!(layout, WindowLayout::Agent(_)),
                        Box::new(UseAgenticLayout),
                    )
                    .when(matches!(layout, WindowLayout::Custom(_)), |menu| {
                        menu.item(PopupMenuItem::new("Custom").checked(true).disabled(true))
                    })
                })
        })
}

fn set_theme(fs: Arc<dyn Fs>, name: gpui::SharedString, appearance: Appearance, cx: &mut App) {
    let system_appearance = SystemAppearance::global(cx).0;
    update_settings_file(fs, cx, move |content, _| {
        theme_settings::set_theme(content, name.to_string(), appearance, system_appearance);
    });
}

fn set_theme_mode(fs: Arc<dyn Fs>, mode: ThemeAppearanceMode, cx: &mut App) {
    let current_theme = cx.theme().name.clone();
    let current_appearance = cx.theme().appearance;
    update_settings_file(fs, cx, move |content, _| {
        let theme = content.theme.as_mut();
        if let Some(settings::ThemeSelection::Dynamic {
            mode: current_mode, ..
        }) = theme.theme.as_mut()
        {
            *current_mode = mode;
            return;
        }
        // A fixed theme keeps its place for its own appearance, next to the default theme for the
        // other one.
        let (light, dark) = match current_appearance {
            Appearance::Light => (
                current_theme.to_string(),
                settings::DEFAULT_DARK_THEME.to_string(),
            ),
            Appearance::Dark => (
                settings::DEFAULT_LIGHT_THEME.to_string(),
                current_theme.to_string(),
            ),
        };
        theme.theme = Some(settings::ThemeSelection::Dynamic {
            mode,
            light: settings::ThemeName(light.into()),
            dark: settings::ThemeName(dark.into()),
        });
    });
}
