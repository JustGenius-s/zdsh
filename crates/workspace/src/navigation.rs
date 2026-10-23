use std::{sync::Arc, time::Duration};

use agent_settings::AgentSettings;
use fs::Fs;
use gpui::{
    Action, Animation, AnimationExt as _, AnyElement, AnyView, App, Context, Div, Focusable as _,
    IntoElement, Styled, div, ease_out_quint, px,
};
use gpui::{ParentElement as _, prelude::FluentBuilder as _};
use gpui_icons::IconName as MenuIcon;
use gpui_kit::component::{
    Icon as KitIcon,
    menu::{PopupMenu, PopupMenuItem},
};
use settings::{Settings as _, update_settings_file};
use theme::{ActiveTheme as _, Appearance};
use theme_settings::{ThemeAppearanceMode, ThemeSelection, ThemeSettings};
use ui::IconName;

use crate::{ItemHandle, Workspace, item::TabCategory};

const ENTRANCE_DURATION: Duration = Duration::from_millis(560);
/// How far a workspace slides in when the window switches to it.
const ENTRANCE_OFFSET: f32 = 48.;

/// The sidebar at a workspace's left edge. The app provides it because its contents (threads,
/// terminals, git changes) live in crates the workspace doesn't depend on.
pub struct WorkspaceNavigation {
    pub view: AnyView,
    /// Drawn inside the content block, right of the right dock, such as the project's file tree
    /// the sidebar opens.
    pub trailing_view: AnyView,
    pub needs_attention: Box<dyn Fn(&App) -> bool>,
}

/// A tool view, such as diagnostics or the debugger, listed in the sidebar's workspace menu. The
/// app adds these because the views live in crates the workspace doesn't depend on.
pub struct RailItem {
    pub icon: IconName,
    pub tooltip: &'static str,
    pub action: Box<dyn Action>,
    pub is_visible: fn(&App) -> bool,
    pub is_active: fn(&dyn ItemHandle) -> bool,
}

impl Workspace {
    pub fn add_rail_item(&mut self, item: RailItem, cx: &mut Context<Self>) {
        self.rail_items.push(item);
        cx.notify();
    }

    /// Slides the workspace in, from the right when its project's pill is right of the one the
    /// window switched from.
    pub fn play_entrance(&mut self, from_right: bool, cx: &mut Context<Self>) {
        let count = self.entrance.map_or(1, |(count, _)| count + 1);
        self.entrance = Some((count, from_right));
        cx.notify();
    }

    pub(crate) fn with_entrance(&self, content: Div) -> AnyElement {
        let Some((count, from_right)) = self.entrance else {
            return content.into_any_element();
        };
        let direction = if from_right { 1. } else { -1. };
        content
            .relative()
            .with_animation(
                ("workspace-entrance", count),
                Animation::new(ENTRANCE_DURATION).with_easing(ease_out_quint()),
                move |content, delta| {
                    content
                        .left(px(direction * ENTRANCE_OFFSET * (1. - delta)))
                        .opacity(0.4 + 0.6 * delta)
                },
            )
            .into_any_element()
    }

    pub(crate) fn render_navigation(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let navigation = self.navigation.as_ref()?;
        // The sidebar sets its own width.
        Some(
            div()
                .flex_none()
                .h_full()
                .bg(cx.theme().sidebar_backdrop())
                .child(navigation.view.clone()),
        )
    }

    /// The navigation's view right of the right dock, which sets its own width.
    pub(crate) fn render_trailing_navigation(&self) -> Option<AnyView> {
        Some(self.navigation.as_ref()?.trailing_view.clone())
    }

    /// The tool views listed in the sidebar's workspace menu.
    pub fn tools_menu(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let mut menu = menu.action_context(self.active_pane.read(cx).focus_handle(cx));
        for item in self.rail_items.iter().filter(|item| (item.is_visible)(cx)) {
            menu = menu.menu_with_icon(
                item.tooltip,
                crate::menu_icon(KitIcon::empty().path(item.icon.path())),
                item.action.boxed_clone(),
            );
        }
        menu
    }

    /// Ways to open a new tab of `category`, used by the sidebar's add buttons.
    pub fn tab_creation_menu(
        &self,
        category: TabCategory,
        menu: PopupMenu,
        cx: &mut App,
    ) -> PopupMenu {
        let menu = menu.action_context(self.active_pane.read(cx).focus_handle(cx));
        match category {
            TabCategory::Agent => {
                let has_project = self.project.read(cx).visible_worktrees(cx).next().is_some();
                menu.menu_with_icon_and_disabled(
                    "New Agent Tab",
                    crate::menu_icon(MenuIcon::Agent),
                    Box::new(zed_actions::agent::NewThread),
                    !has_project || !AgentSettings::get_global(cx).enabled(cx),
                )
                .when(!has_project, |menu| {
                    menu.separator().menu_with_icon(
                        "Open Folder…",
                        crate::menu_icon(MenuIcon::FolderOpen),
                        Box::new(crate::Open::default()),
                    )
                })
            }
            TabCategory::Terminal => {
                let mut menu = menu.menu_with_icon_and_disabled(
                    "New Terminal",
                    crate::menu_icon(MenuIcon::TerminalRectangle),
                    Box::new(crate::NewTerminal::default()),
                    self.terminal_provider.is_none(),
                );
                if let Some(provider) = self.terminal_provider.as_ref() {
                    let actions = provider.quick_actions(cx);
                    if !actions.is_empty() {
                        menu = menu.separator();
                        for (label, action) in actions {
                            menu = menu.menu_with_icon(
                                label,
                                crate::menu_icon(MenuIcon::Lightning),
                                action,
                            );
                        }
                    }
                    menu = menu.separator().menu_with_icon(
                        "Manage quick actions…",
                        crate::menu_icon(MenuIcon::Sliders),
                        Box::new(crate::OpenTerminalQuickActions),
                    );
                }
                menu
            }
            TabCategory::File => menu
                .menu_with_icon(
                    "New File",
                    crate::menu_icon(MenuIcon::FilePlus),
                    Box::new(crate::NewFile),
                )
                .menu_with_icon(
                    "Open File…",
                    crate::menu_icon(MenuIcon::File),
                    Box::new(crate::Open::default()),
                ),
            TabCategory::Action => self.tools_menu(menu, cx),
        }
    }
}

/// The appearance and theme choices of the title bar's settings menu.
pub fn theme_menu(menu: PopupMenu, fs: Arc<dyn Fs>, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let current_mode = match &ThemeSettings::get_global(cx).theme {
        ThemeSelection::Dynamic { mode, .. } => Some(*mode),
        ThemeSelection::Static(_) => None,
    };
    let mut menu = menu.item(crate::menu_heading("Appearance"));
    for (label, icon, mode) in [
        (
            "System",
            crate::menu_icon(MenuIcon::Display),
            ThemeAppearanceMode::System,
        ),
        (
            "Light",
            crate::menu_icon(MenuIcon::Sun),
            ThemeAppearanceMode::Light,
        ),
        (
            "Dark",
            crate::menu_icon(MenuIcon::Moon),
            ThemeAppearanceMode::Dark,
        ),
    ] {
        let fs = fs.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(icon)
                .checked(current_mode == Some(mode))
                .on_click(move |_, _, cx| set_theme_mode(fs.clone(), mode, cx)),
        );
    }
    menu.separator()
        .menu_with_icon(
            "Color Theme…",
            crate::menu_icon(MenuIcon::Palette),
            zed_actions::theme_selector::Toggle::default().boxed_clone(),
        )
        .menu_with_icon(
            "Icon Theme…",
            crate::menu_icon(MenuIcon::ShapesSquareCircle),
            zed_actions::icon_theme_selector::Toggle::default().boxed_clone(),
        )
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
