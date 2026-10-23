use crate::{icon_for_remote_connection, open_remote_project};
use fuzzy_nucleo::{StringMatch, StringMatchCandidate, match_strings};
use gpui::{
    Action, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Subscription,
    Task, WeakEntity, Window,
};
use gpui_icons::Icon as KitIcon;
use gpui_kit::component::{
    IndexPath,
    button::{Button, ButtonVariants},
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
};
use remote::RemoteConnectionOptions;
use settings::Settings;
use ui::prelude::*;
use util::{ResultExt, paths::PathExt};
use workspace::{
    MultiWorkspaceWindow, OpenMode, OpenOptions, RecentWorkspace, SerializedWorkspaceLocation,
    Workspace, WorkspaceDb, notifications::DetachAndPromptErr,
};
use zed_actions::OpenRemote;

pub struct SidebarRecentProjects {
    list: Entity<ListState<SidebarRecentProjectsDelegate>>,
    _subscription: Subscription,
}

impl SidebarRecentProjects {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fs = workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).app_state().fs.clone());
        let list = cx.new(|cx| {
            ListState::new(
                SidebarRecentProjectsDelegate {
                    workspace,
                    workspaces: Vec::new(),
                    filtered_workspaces: Vec::new(),
                    selected_index: None,
                    query: String::new(),
                },
                window,
                cx,
            )
            .searchable(true)
        });
        let subscription = cx.subscribe(&list, |_, _, event, cx| {
            if matches!(event, ListEvent::Confirm(_) | ListEvent::Cancel) {
                cx.emit(DismissEvent);
            }
        });
        let db = WorkspaceDb::global(cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(fs) = fs else { return };
            let workspaces = db
                .recent_project_workspaces(fs.as_ref())
                .await
                .log_err()
                .unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                this.list.update(cx, |list, cx| {
                    list.delegate_mut().workspaces = workspaces;
                    let query = list.delegate().query.clone();
                    list.delegate_mut()
                        .perform_search(&query, window, cx)
                        .detach();
                    list.set_selected_index(Some(IndexPath::default()), window, cx);
                    cx.notify();
                });
            })
            .log_err();
        })
        .detach();
        list.focus_handle(cx).focus(window, cx);
        Self {
            list,
            _subscription: subscription,
        }
    }
}
impl EventEmitter<DismissEvent> for SidebarRecentProjects {}
impl Focusable for SidebarRecentProjects {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.list.focus_handle(cx)
    }
}
impl Render for SidebarRecentProjects {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_kit::base::v_flex()
            .w(rems(22.))
            .gap_1()
            .child(List::new(&self.list).h(rems(20.)))
            .child(
                Button::new("open-local-folder")
                    .ghost()
                    .label("Open Local Folders")
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(
                            workspace::Open {
                                create_new_window: Some(false),
                            }
                            .boxed_clone(),
                            cx,
                        );
                        cx.emit(DismissEvent);
                    })),
            )
            .child(
                Button::new("open-remote-folder")
                    .ghost()
                    .label("Open Remote Folder")
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(
                            OpenRemote {
                                from_existing_connection: false,
                                create_new_window: Some(false),
                            }
                            .boxed_clone(),
                            cx,
                        );
                        cx.emit(DismissEvent);
                    })),
            )
    }
}

struct SidebarRecentProjectsDelegate {
    workspace: WeakEntity<Workspace>,
    workspaces: Vec<RecentWorkspace>,
    filtered_workspaces: Vec<StringMatch>,
    selected_index: Option<usize>,
    query: String,
}
impl ListDelegate for SidebarRecentProjectsDelegate {
    type Item = ListItem;
    fn items_count(&self, _: usize, _: &App) -> usize {
        self.filtered_workspaces.len()
    }
    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = index.map(|index| index.row);
    }
    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.query = query.to_owned();
        let query = query.trim_start();
        let case = fuzzy_nucleo::Case::smart_if_uppercase_in(query);
        let is_empty_query = query.is_empty();

        let current_workspace_id = self
            .workspace
            .upgrade()
            .and_then(|ws| ws.read(cx).database_id());

        let candidates: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, workspace)| Some(workspace.workspace_id) != current_workspace_id)
            .map(|(id, workspace)| {
                let combined_string = workspace
                    .identity_paths
                    .ordered_paths()
                    .map(|path| path.compact().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .concat();
                StringMatchCandidate::new(id, &combined_string)
            })
            .collect();

        if is_empty_query {
            self.filtered_workspaces = candidates
                .into_iter()
                .map(|candidate| StringMatch {
                    candidate_id: candidate.id,
                    score: 0.0,
                    positions: Vec::new(),
                    string: candidate.string,
                })
                .collect();
        } else {
            self.filtered_workspaces = match_strings(
                &candidates,
                query,
                case,
                fuzzy_nucleo::LengthPenalty::On,
                100,
            );
        }

        self.selected_index = Some(0);
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(hit) = self
            .selected_index
            .and_then(|index| self.filtered_workspaces.get(index))
        else {
            return;
        };
        let Some(recent_workspace) = self.workspaces.get(hit.candidate_id) else {
            return;
        };

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        match &recent_workspace.location {
            SerializedWorkspaceLocation::Local => {
                if let Some(handle) = MultiWorkspaceWindow::from_any(window.window_handle()) {
                    let paths = recent_workspace.paths.paths().to_vec();
                    cx.defer(move |cx| {
                        if let Some(task) = handle
                            .update(cx, |multi_workspace, window, cx| {
                                multi_workspace.open_project(paths, OpenMode::Activate, window, cx)
                            })
                            .log_err()
                        {
                            task.detach_and_log_err(cx);
                        }
                    });
                }
            }
            SerializedWorkspaceLocation::Remote(connection) => {
                let mut connection = connection.clone();
                workspace.update(cx, |workspace, cx| {
                    let app_state = workspace.app_state().clone();
                    let replace_window = MultiWorkspaceWindow::from_any(window.window_handle());
                    let open_options = OpenOptions {
                        requesting_window: replace_window,
                        ..Default::default()
                    };
                    if let RemoteConnectionOptions::Ssh(connection) = &mut connection {
                        crate::RemoteSettings::get_global(cx)
                            .fill_connection_options_from_settings(connection);
                    };
                    let paths = recent_workspace.paths.paths().to_vec();
                    cx.spawn_in(window, async move |_, cx| {
                        open_remote_project(connection.clone(), paths, app_state, open_options, cx)
                            .await
                    })
                    .detach_and_prompt_err(
                        "Failed to open project",
                        window,
                        cx,
                        |_, _, _| None,
                    );
                });
            }
        }
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let hit = self.filtered_workspaces.get(index.row)?;
        let workspace = self.workspaces.get(hit.candidate_id)?;
        let label = workspace
            .identity_paths
            .ordered_paths()
            .map(|path| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
            })
            .collect::<Vec<_>>()
            .join(", ");
        let path = workspace
            .identity_paths
            .ordered_paths()
            .map(|path| path.compact().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        let connection = match &workspace.location {
            SerializedWorkspaceLocation::Local => None,
            SerializedWorkspaceLocation::Remote(connection) => Some(connection),
        };
        let subtitle = connection
            .map(|connection| format!("{} · {}", connection.display_name(), path))
            .unwrap_or(path);
        Some(
            ListItem::new(index.row).child(
                gpui_kit::base::h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(
                        KitIcon::empty()
                            .id("project-icon")
                            .path(icon_for_remote_connection(connection).path())
                            .size_4(),
                    )
                    .child(
                        gpui_kit::base::v_flex()
                            .min_w_0()
                            .child(div().truncate().child(label))
                            .child(div().text_sm().opacity(0.65).truncate().child(subtitle)),
                    ),
            ),
        )
    }
}
