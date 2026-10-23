use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, px,
};
use gpui_kit::component::h_flex;
use theme::ActiveTheme as _;
use ui::{Icon, IconName};
use workspace::{Item, Panel as _, Workspace, dock::Dock, item::ItemEvent};

use crate::{git_panel::GitPanel, project_diff::ProjectDiff};

/// Width of the change lists and commit box beside the diff.
const LISTS_WIDTH: f32 = 300.;

/// The repository's changes as a tab: the git panel's change lists, history and commit box
/// beside the diff of the file picked in them.
pub struct ChangesView {
    git_panel: Entity<GitPanel>,
    project_diff: Entity<ProjectDiff>,
}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        // The git panel is drawn in this tab, so anything that opens it in a dock opens the tab.
        for dock in [
            workspace.left_dock().clone(),
            workspace.right_dock().clone(),
            workspace.bottom_dock().clone(),
        ] {
            cx.observe_in(&dock, window, ChangesView::take_git_panel_from_dock)
                .detach();
        }
    })
    .detach();
}

impl ChangesView {
    pub(crate) fn project_diff(&self) -> &Entity<ProjectDiff> {
        &self.project_diff
    }

    /// Activates the changes tab, opening one when there is none.
    pub fn deploy(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let existing = workspace.items_of_type::<Self>(cx).next();
        if let Some(existing) = existing {
            workspace.activate_item(&existing, true, true, window, cx);
            return;
        }
        let Some(git_panel) = workspace.panel::<GitPanel>(cx) else {
            return;
        };
        let workspace_handle = cx.entity();
        let project_diff = cx
            .new(|cx| ProjectDiff::new(workspace.project().clone(), workspace_handle, window, cx));
        if let Some(repository) = workspace.project().read(cx).active_repository(cx) {
            project_diff.update(cx, |project_diff, cx| {
                project_diff.set_repo(Some(repository), cx)
            });
        }
        let changes = cx.new(|_| Self {
            git_panel,
            project_diff,
        });
        workspace.add_item_to_active_pane(Box::new(changes), None, true, window, cx);
    }

    fn take_git_panel_from_dock(
        _workspace: &mut Workspace,
        dock: Entity<Dock>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let shows_git_panel = {
            let dock = dock.read(cx);
            dock.is_open()
                && dock
                    .visible_panel()
                    .is_some_and(|panel| panel.to_any().downcast::<GitPanel>().is_ok())
        };
        if !shows_git_panel {
            return;
        }
        // The dock is mid-update while it notifies, so close it once that is done.
        cx.defer_in(window, move |workspace, window, cx| {
            dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
            Self::deploy(workspace, window, cx);
        });
    }
}

impl EventEmitter<ItemEvent> for ChangesView {}

impl Focusable for ChangesView {
    /// Focuses the panel the way opening it in a dock does, such as on an expanded commit message.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.git_panel.read(cx).activation_focus_handle(cx)
    }
}

impl Item for ChangesView {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitBranch))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Changes".into()
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for ChangesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        h_flex()
            .size_full()
            .items_stretch()
            .child(
                div()
                    .w(px(LISTS_WIDTH))
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(colors.border_variant)
                    .child(self.git_panel.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.project_diff.clone()),
            )
    }
}
