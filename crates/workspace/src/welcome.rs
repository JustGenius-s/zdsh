use crate::{
    Open, OpenMode, PathList, RecentWorkspace, SerializedWorkspaceLocation, Workspace,
    WorkspaceSettings,
    item::{Item, ItemEvent},
    persistence::WorkspaceDb,
};
use git::Clone as GitClone;
use gpui::WeakEntity;
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    ParentElement, Render, Styled, Task, TaskExt, Window, actions,
};
use menu::{SelectNext, SelectPrevious};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::{DefaultOpenBehavior, Settings};
use ui::{KeyBinding, prelude::*};
use util::{ResultExt, paths::PathExt as _};
use zed_actions::{OpenDevContainer, OpenRemote};

#[derive(PartialEq, Clone, Debug, Deserialize, Serialize, JsonSchema, Action)]
#[action(namespace = welcome)]
#[serde(transparent)]
pub struct OpenRecentProject {
    pub index: usize,
}

actions!(
    zed,
    [
        /// Show the Zed welcome screen
        ShowWelcome
    ]
);

/// Width of the start page's column.
const COLUMN_WIDTH: f32 = 520.;
const ACTION_HEIGHT: f32 = 52.;
const RECENT_ROW_HEIGHT: f32 = 32.;
const RECENT_PROJECTS_SHOWN: usize = 5;

/// The page a workspace without a project shows: ways to open one, and recent projects.
pub struct WelcomePage {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    recent_workspaces: Option<Vec<RecentWorkspace>>,
}

impl WelcomePage {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();

        let fs = workspace
            .upgrade()
            .map(|ws| ws.read(cx).app_state().fs.clone());
        let db = WorkspaceDb::global(cx);
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let Some(fs) = fs else { return };
            let workspaces = db
                .recent_project_workspaces(fs.as_ref())
                .await
                .log_err()
                .unwrap_or_default();

            this.update(cx, |this, cx| {
                this.recent_workspaces = Some(workspaces);
                cx.notify();
            })
            .ok();
        })
        .detach();

        WelcomePage {
            workspace,
            focus_handle,
            recent_workspaces: None,
        }
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
        cx.notify();
    }

    fn open_recent_project(
        &mut self,
        action: &OpenRecentProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(recent_workspaces) = &self.recent_workspaces {
            if let Some(workspace) = recent_workspaces.get(action.index) {
                let is_local = matches!(workspace.location, SerializedWorkspaceLocation::Local);

                if is_local {
                    let paths = workspace.paths.paths().to_vec();
                    let open_mode = match WorkspaceSettings::get_global(cx).default_open_behavior {
                        DefaultOpenBehavior::ExistingWindow => OpenMode::Activate,
                        DefaultOpenBehavior::NewWindow => OpenMode::NewWindow,
                    };
                    self.workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .open_workspace_for_paths(open_mode, paths, window, cx)
                                .detach_and_log_err(cx);
                        })
                        .log_err();
                } else {
                    use zed_actions::OpenRecent;
                    window.dispatch_action(OpenRecent::default().boxed_clone(), cx);
                }
            }
        }
    }

    /// Whether the window already shows a workspace for `paths`.
    fn is_open_in_window(&self, paths: &PathList, cx: &App) -> bool {
        let Some(multi_workspace) = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).multi_workspace().cloned())
            .and_then(|multi_workspace| multi_workspace.upgrade())
        else {
            return false;
        };
        multi_workspace
            .read(cx)
            .project_group_keys()
            .iter()
            .any(|key| key.path_list() == paths)
    }

    fn render_action(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        action: Box<dyn Action>,
        cx: &App,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let focus_handle = self.focus_handle.clone();
        let key_binding = KeyBinding::for_action_in(action.as_ref(), &self.focus_handle, cx)
            .size(rems_from_px(12_f32));
        h_flex()
            .id(id)
            .h(rems_from_px(ACTION_HEIGHT))
            .px(rems_from_px(14_f32))
            .gap(rems_from_px(10_f32))
            .rounded(rems_from_px(9_f32))
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.element_background)
            .cursor_pointer()
            .hover(|style| style.bg(colors.element_hover))
            .active(|style| style.bg(colors.element_active))
            .child(Icon::new(icon).color(Color::Muted).size(IconSize::Medium))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(key_binding)
            .on_click(move |_, window, cx| {
                focus_handle.dispatch_action(action.as_ref(), window, cx)
            })
    }

    fn render_recent_project(
        &self,
        index: usize,
        workspace: &RecentWorkspace,
        cx: &App,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (icon, location) = match &workspace.location {
            SerializedWorkspaceLocation::Local => (
                IconName::Folder,
                workspace
                    .identity_paths
                    .paths()
                    .first()
                    .and_then(|path| path.parent())
                    .map(|parent| parent.compact().to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            SerializedWorkspaceLocation::Remote(_) => (IconName::Server, "Remote".to_string()),
        };
        let is_open = self.is_open_in_window(&workspace.identity_paths, cx);
        let focus_handle = self.focus_handle.clone();
        h_flex()
            .id(("recent-project", index))
            .w_full()
            .h(rems_from_px(RECENT_ROW_HEIGHT))
            .px_2()
            .gap(rems_from_px(9_f32))
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .active(|style| style.bg(colors.ghost_element_active))
            .child(Icon::new(icon).color(Color::Muted).size(IconSize::Small))
            .child(
                div()
                    .flex_none()
                    .child(project_name(&workspace.identity_paths)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(colors.text_muted)
                    .child(location),
            )
            .when(is_open, |row| {
                row.child(
                    Label::new("Open")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .on_click(move |_, window, cx| {
                focus_handle.dispatch_action(&OpenRecentProject { index }, window, cx)
            })
    }
}

impl Render for WelcomePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let recent_projects = self
            .recent_workspaces
            .as_ref()
            .into_iter()
            .flatten()
            .take(RECENT_PROJECTS_SHOWN)
            .enumerate()
            .map(|(index, workspace)| self.render_recent_project(index, workspace, cx))
            .collect::<Vec<_>>();
        let actions = [
            self.render_action(
                "open-folder",
                IconName::FolderOpen,
                "Open Folder",
                Open::DEFAULT.boxed_clone(),
                cx,
            )
            .into_any_element(),
            self.render_action(
                "clone-repository",
                IconName::CloudDownload,
                "Clone Repository",
                GitClone.boxed_clone(),
                cx,
            )
            .into_any_element(),
            self.render_action(
                "connect-ssh",
                IconName::Server,
                "Connect to SSH Host",
                OpenRemote::default().boxed_clone(),
                cx,
            )
            .into_any_element(),
            self.render_action(
                "open-dev-container",
                IconName::Box,
                "Open in Dev Container",
                OpenDevContainer.boxed_clone(),
                cx,
            )
            .into_any_element(),
        ];

        h_flex()
            .key_context("Welcome")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_recent_project))
            .size_full()
            .bg(colors.editor_background)
            .justify_center()
            .child(
                v_flex()
                    .id("welcome-content")
                    .w(rems_from_px(COLUMN_WIDTH))
                    .max_w_full()
                    .h_full()
                    .px_4()
                    .pt(rems_from_px(52_f32))
                    .pb_8()
                    .gap(rems_from_px(18_f32))
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(Headline::new("Open a Workspace"))
                            .child(
                                Label::new(
                                    "A workspace keeps a project's threads, files and terminals \
                                     together. Open several and switch between them with ⌃1–9.",
                                )
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(div().grid().grid_cols(2).gap_2().children(actions))
                    .when(!recent_projects.is_empty(), |column| {
                        column.child(
                            v_flex()
                                .child(
                                    div()
                                        .px_2()
                                        .pb_1()
                                        .text_sm()
                                        .text_color(colors.text_muted)
                                        .child("Recent"),
                                )
                                .children(recent_projects),
                        )
                    })
                    .child(
                        Label::new("You can also drag a folder into the window.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
    }
}

impl EventEmitter<ItemEvent> for WelcomePage {}

impl Focusable for WelcomePage {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for WelcomePage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Welcome".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("New Welcome Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(crate::item::ItemEvent)) {
        f(*event)
    }
}

impl crate::SerializableItem for WelcomePage {
    fn serialized_item_kind() -> &'static str {
        "WelcomePage"
    }

    fn cleanup(
        workspace_id: crate::WorkspaceId,
        alive_items: Vec<crate::ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<gpui::Result<()>> {
        crate::delete_unloaded_items(
            alive_items,
            workspace_id,
            "welcome_pages",
            &persistence::WelcomePagesDb::global(cx),
            cx,
        )
    }

    fn deserialize(
        _project: Entity<project::Project>,
        workspace: gpui::WeakEntity<Workspace>,
        workspace_id: crate::WorkspaceId,
        item_id: crate::ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<gpui::Result<Entity<Self>>> {
        if persistence::WelcomePagesDb::global(cx)
            .get_welcome_page(item_id, workspace_id)
            .ok()
            .is_some_and(|is_open| is_open)
        {
            Task::ready(Ok(cx.new(|cx| WelcomePage::new(workspace, window, cx))))
        } else {
            Task::ready(Err(anyhow::anyhow!("No welcome page to deserialize")))
        }
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: crate::ItemId,
        _closing: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<gpui::Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let db = persistence::WelcomePagesDb::global(cx);
        Some(cx.background_spawn(
            async move { db.save_welcome_page(item_id, workspace_id, true).await },
        ))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        event == &ItemEvent::UpdateTab
    }
}

mod persistence {
    use crate::WorkspaceDb;
    use db::{
        query,
        sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
        sqlez_macros::sql,
    };

    pub struct WelcomePagesDb(ThreadSafeConnection);

    impl Domain for WelcomePagesDb {
        const NAME: &str = stringify!(WelcomePagesDb);

        const MIGRATIONS: &[&str] = (&[sql!(
                    CREATE TABLE welcome_pages (
                        workspace_id INTEGER,
                        item_id INTEGER UNIQUE,
                        is_open INTEGER DEFAULT FALSE,

                        PRIMARY KEY(workspace_id, item_id),
                        FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                        ON DELETE CASCADE
                    ) STRICT;
        )]);
    }

    db::static_connection!(WelcomePagesDb, [WorkspaceDb]);

    impl WelcomePagesDb {
        query! {
            pub async fn save_welcome_page(
                item_id: crate::ItemId,
                workspace_id: crate::WorkspaceId,
                is_open: bool
            ) -> Result<()> {
                INSERT OR REPLACE INTO welcome_pages(item_id, workspace_id, is_open)
                VALUES (?, ?, ?)
            }
        }

        query! {
            pub fn get_welcome_page(
                item_id: crate::ItemId,
                workspace_id: crate::WorkspaceId
            ) -> Result<bool> {
                SELECT is_open
                FROM welcome_pages
                WHERE item_id = ? AND workspace_id = ?
            }
        }
    }
}

fn project_name(paths: &PathList) -> String {
    let joined = paths
        .paths()
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        "Untitled".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_name_empty() {
        let paths = PathList::new::<&str>(&[]);
        assert_eq!(project_name(&paths), "Untitled");
    }

    #[test]
    fn test_project_name_single() {
        let paths = PathList::new(&["/home/user/my-project"]);
        assert_eq!(project_name(&paths), "my-project");
    }

    #[test]
    fn test_project_name_multiple() {
        // PathList sorts lexicographically, so filenames appear in alpha order
        let paths = PathList::new(&["/home/user/zed", "/home/user/api"]);
        assert_eq!(project_name(&paths), "api, zed");
    }

    #[test]
    fn test_project_name_root_path_filtered() {
        // A bare root "/" has no file_name(), falls back to "Untitled"
        let paths = PathList::new(&["/"]);
        assert_eq!(project_name(&paths), "Untitled");
    }
}
