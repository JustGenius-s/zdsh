use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Render, WeakEntity,
    Window, px,
};
use ui::prelude::*;

use crate::{
    DockPosition, MultiWorkspace, ToggleWorkspaceSidebar, Workspace,
    dock::{Panel, PanelEvent},
};

const CHAT_PANEL_KEY: &str = "ChatPanel";

/// Shows the window's thread list in the left dock. Every workspace in a window has one of these,
/// and they all show the same list, which belongs to the window's [`MultiWorkspace`]. Whether the
/// list is open is the multi-workspace's to say, so the panel reports its own visibility back to
/// it and the multi-workspace opens the panel in whichever workspace is active.
pub struct ChatPanel {
    workspace: WeakEntity<Workspace>,
    /// Kept here rather than looked up through the workspace, which the panel is asked about
    /// while the workspace renders.
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
    focus_handle: FocusHandle,
}

impl ChatPanel {
    pub fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            workspace: workspace.weak_handle(),
            multi_workspace: MultiWorkspace::for_window(window, cx)
                .map(|multi_workspace| multi_workspace.downgrade()),
            focus_handle: cx.focus_handle(),
        }
    }

    fn multi_workspace(&self) -> Option<Entity<MultiWorkspace>> {
        self.multi_workspace.as_ref()?.upgrade()
    }

    /// Whether the chat panel is the visible panel of `workspace`'s left dock.
    pub fn is_visible_in(workspace: &Workspace, cx: &App) -> bool {
        workspace
            .left_dock()
            .read(cx)
            .visible_panel()
            .is_some_and(|panel| panel.persistent_name() == Self::persistent_name())
    }
}

impl EventEmitter<PanelEvent> for ChatPanel {}

impl Focusable for ChatPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let thread_list = self
            .multi_workspace()
            .and_then(|multi_workspace| multi_workspace.read(cx).sidebar().map(|s| s.to_any()));
        div()
            .id("chat-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .children(thread_list)
    }
}

impl Panel for ChatPanel {
    fn persistent_name() -> &'static str {
        CHAT_PANEL_KEY
    }

    fn panel_key() -> &'static str {
        CHAT_PANEL_KEY
    }

    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.multi_workspace()
            .and_then(|multi_workspace| {
                multi_workspace
                    .read(cx)
                    .sidebar()
                    .map(|sidebar| sidebar.focus_handle(cx))
            })
            .unwrap_or_else(|| self.focus_handle.clone())
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Left
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(300.)
    }

    fn min_size(&self, _window: &Window, _cx: &App) -> Option<Pixels> {
        Some(px(240.))
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::Chat)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Threads")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleWorkspaceSidebar)
    }

    fn activation_priority(&self) -> u32 {
        // Unique among the panels that can share the left dock: the agent panel has 0, the
        // project panel 1 and the git panel 3.
        2
    }

    fn enabled(&self, cx: &App) -> bool {
        self.multi_workspace().is_some_and(|multi_workspace| {
            let multi_workspace = multi_workspace.read(cx);
            multi_workspace.multi_workspace_enabled(cx) && multi_workspace.sidebar().is_some()
        })
    }

    fn set_active(&mut self, _active: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The dock reports activation even while it is closed, so the multi-workspace reads the
        // dock's actual state. It may be the one opening this panel, so it is updated afterwards.
        let Some(multi_workspace) = self.multi_workspace() else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |_, cx| {
            multi_workspace.update(cx, |multi_workspace, cx| {
                multi_workspace.sync_sidebar_open_from_dock(&workspace, cx);
            });
        });
    }
}
