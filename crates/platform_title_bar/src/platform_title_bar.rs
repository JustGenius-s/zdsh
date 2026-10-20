pub mod platforms;
mod system_window_tabs;

use gpui::{
    Action, AnyElement, App, Context, Decorations, Entity, Hsla, InteractiveElement, IntoElement,
    ParentElement, Styled, WeakEntity, Window, WindowButtonLayout, div,
};
use gpui_kit::component::TitleBar;
use project::DisableAiSettings;
use settings::Settings;
use smallvec::SmallVec;
use std::mem;
use ui::{prelude::*, utils::platform_title_bar_height};
use workspace::{MultiWorkspace, SidebarRenderState, SidebarSide};

use crate::{
    platforms::{platform_linux, platform_windows},
    system_window_tabs::SystemWindowTabs,
};

pub use system_window_tabs::{
    DraggedWindowTab, MergeAllWindows, MoveTabToNewWindow, ShowNextWindowTab, ShowPreviousWindowTab,
};

/// With client-side decorations, the title bar is pulled over the window frame's
/// border to avoid a transparent gap in the rounded corners, and a border in the
/// title bar's own color insets its content back by the same amount. Headers that
/// render window controls in place of the title bar use this to apply the same
/// insets as padding, so the controls stay on the same pixel.
///
/// `pad_bottom` should be false when the header already has a bottom border of
/// its own occupying that space.
pub fn apply_title_bar_insets<E: Styled + FluentBuilder>(
    header: E,
    left_window_controls: bool,
    right_window_controls: bool,
    pad_bottom: bool,
) -> E {
    let inset = theme::CLIENT_SIDE_DECORATION_BORDER;
    header
        .mt(-inset)
        .pt(inset)
        .when(pad_bottom, |header| header.pb(inset))
        .when(left_window_controls, |header| header.pl(inset))
        .when(right_window_controls, |header| header.pr(inset))
}

pub struct PlatformTitleBar {
    id: ElementId,
    children: SmallVec<[AnyElement; 2]>,
    system_window_tabs: Entity<SystemWindowTabs>,
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
}

impl PlatformTitleBar {
    pub fn new(id: impl Into<ElementId>, cx: &mut Context<Self>) -> Self {
        let system_window_tabs = cx.new(|_cx| SystemWindowTabs::new());

        Self {
            id: id.into(),
            children: SmallVec::new(),
            system_window_tabs,
            multi_workspace: None,
        }
    }

    pub fn with_multi_workspace(mut self, multi_workspace: WeakEntity<MultiWorkspace>) -> Self {
        self.multi_workspace = Some(multi_workspace);
        self
    }

    pub fn set_multi_workspace(&mut self, multi_workspace: WeakEntity<MultiWorkspace>) {
        self.multi_workspace = Some(multi_workspace);
    }

    pub fn title_bar_color(&self, window: &mut Window, cx: &mut Context<Self>) -> Hsla {
        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            if window.is_window_active() {
                cx.theme().colors().title_bar_background
            } else {
                cx.theme().colors().title_bar_inactive_background
            }
        } else {
            cx.theme().colors().title_bar_background
        }
    }

    pub fn set_children<T>(&mut self, children: T)
    where
        T: IntoIterator<Item = AnyElement>,
    {
        self.children = children.into_iter().collect();
    }

    pub fn init(cx: &mut App) {
        SystemWindowTabs::init(cx);
    }

    fn sidebar_render_state(&self, cx: &App) -> SidebarRenderState {
        self.multi_workspace
            .as_ref()
            .and_then(|mw| mw.upgrade())
            .map(|mw| mw.read(cx).sidebar_render_state(cx))
            .unwrap_or_default()
    }

    pub fn is_multi_workspace_enabled(cx: &App) -> bool {
        !DisableAiSettings::get_global(cx).disable_ai
    }
}

/// Renders the platform-appropriate left-side window controls (e.g. Ubuntu/GNOME close button).
///
/// Only relevant on Linux with client-side decorations when the window manager
/// places controls on the left.
pub fn render_left_window_controls(
    button_layout: Option<WindowButtonLayout>,
    close_action: Box<dyn Action>,
    window: &Window,
) -> Option<AnyElement> {
    if PlatformStyle::platform() != PlatformStyle::Linux {
        return None;
    }
    if !matches!(window.window_decorations(), Decorations::Client { .. }) {
        return None;
    }
    let button_layout = button_layout?;
    if button_layout.left[0].is_none() {
        return None;
    }
    Some(
        platform_linux::LinuxWindowControls::new(
            "left-window-controls",
            button_layout.left,
            close_action,
        )
        .into_any_element(),
    )
}

/// Renders the platform-appropriate right-side window controls (close, minimize, maximize).
///
/// Returns `None` on Mac or when the platform doesn't need custom controls
/// (e.g. Linux with server-side decorations).
pub fn render_right_window_controls(
    button_layout: Option<WindowButtonLayout>,
    close_action: Box<dyn Action>,
    window: &Window,
) -> Option<AnyElement> {
    let decorations = window.window_decorations();
    let height = platform_title_bar_height(window);

    match PlatformStyle::platform() {
        PlatformStyle::Linux => {
            if !matches!(decorations, Decorations::Client { .. }) {
                return None;
            }
            let button_layout = button_layout?;
            if button_layout.right[0].is_none() {
                return None;
            }
            Some(
                platform_linux::LinuxWindowControls::new(
                    "right-window-controls",
                    button_layout.right,
                    close_action,
                )
                .into_any_element(),
            )
        }
        PlatformStyle::Windows => {
            Some(platform_windows::WindowsWindowControls::new(height).into_any_element())
        }
        PlatformStyle::Mac => None,
    }
}

impl Render for PlatformTitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let decorations = window.window_decorations();
        let height = platform_title_bar_height(window);
        let titlebar_color = self.title_bar_color(window, cx);
        let children = mem::take(&mut self.children);
        let sidebar = self.sidebar_render_state(cx);
        let is_fullscreen = window.is_fullscreen() || window.is_simple_fullscreen();
        // GPUI Kit's title bar moves the window, handles double clicks and draws the window
        // controls on Windows and Linux.
        let title_bar = TitleBar::new()
            .w_full()
            .h(height)
            // Flat and without a divider, as one surface with the rail; the content block below
            // has its own edge.
            .bg(titlebar_color)
            .border_b_0()
            .on_close_window(|_, window, cx| {
                window.dispatch_action(Box::new(workspace::CloseWindow), cx)
            })
            // In fullscreen GPUI Kit pads the bar's content itself.
            .when(is_fullscreen, |title_bar| title_bar.pl_0())
            .map(|title_bar| match decorations {
                Decorations::Server => title_bar,
                Decorations::Client { tiling, .. } => title_bar
                    .when(
                        !(tiling.top || tiling.right)
                            && !(sidebar.open && sidebar.side == SidebarSide::Right),
                        |title_bar| title_bar.rounded_tr(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                    )
                    .when(
                        !(tiling.top || tiling.left)
                            && !(sidebar.open && sidebar.side == SidebarSide::Left),
                        |title_bar| title_bar.rounded_tl(theme::CLIENT_SIDE_DECORATION_ROUNDING),
                    )
                    // Keep in sync with `apply_title_bar_insets`.
                    .mt(-theme::CLIENT_SIDE_DECORATION_BORDER)
                    .mb(-theme::CLIENT_SIDE_DECORATION_BORDER)
                    .border(theme::CLIENT_SIDE_DECORATION_BORDER)
                    .border_color(titlebar_color),
            })
            .child(
                div()
                    .id(self.id.clone())
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .overflow_x_hidden()
                    .w_full()
                    .h_full()
                    .children(children),
            );

        v_flex()
            .w_full()
            .child(title_bar)
            .child(self.system_window_tabs.clone().into_any_element())
    }
}

impl ParentElement for PlatformTitleBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}
