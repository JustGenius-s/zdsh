use action_log::DiffStats;
use agent_ui::AgentPanel;
use gpui::AsKeystroke as _;
use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_kit::component::{h_flex, kbd::Kbd, menu::DropdownMenu as _, v_flex};
use icons::IconName;
use project::{ProjectPath, git_store::GitStore};
use theme::ActiveTheme;
use ui::{Color, rems_from_px};
use util::ResultExt as _;
use workspace::{
    ItemHandle, MultiWorkspace, Workspace, WorkspaceNavigation, item::TabCategory,
};

use crate::Sidebar;
use crate::sidebar_row::{
    GROUP_GAP, LIST_INSET, SidebarRow, StatusDot, sidebar_icon, sidebar_icon_button,
};

/// Space before a group's title, lining it up with the icons of the rows below it.
const GROUP_LABEL_PADDING_START: f32 = 9.;
const GROUP_LABEL_PADDING_END: f32 = 4.;
const GROUP_LABEL_HEIGHT: f32 = 28.;
const FILE_GROUP_LABEL_HEIGHT: f32 = 20.;
const HEADER_HEIGHT: f32 = 30.;
/// How many of the repository's changed files the sidebar lists before linking to all of them.
const GIT_CHANGES_SHOWN: usize = 3;

/// The sidebar at a workspace's left edge, showing that workspace's threads, files and
/// terminals.
pub struct WorkspaceNav {
    workspace: WeakEntity<Workspace>,
    /// The window's thread list, which already tracks every project's threads and their status.
    threads: Option<Entity<Sidebar>>,
    needed_attention: bool,
    _threads_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

struct FileRow {
    path: ProjectPath,
    item: Option<Box<dyn ItemHandle>>,
    end: FileRowEnd,
}

enum FileRowEnd {
    None,
    DiffStats(DiffStats),
    Status(&'static str, Color),
}

impl WorkspaceNav {
    /// Creates the sidebar for `workspace` and puts it at the workspace's left edge.
    pub fn install(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let workspace_handle = cx.entity();
        // The workspace is mid-update here, so the sidebar can't read it yet.
        let git_store = workspace.project().read(cx).git_store().clone();
        let nav = cx.new(|cx| Self::new(&workspace_handle, &git_store, window, cx));
        workspace.set_navigation(
            WorkspaceNavigation {
                view: nav.clone().into(),
                needs_attention: Box::new(move |cx| nav.read(cx).needs_attention(cx)),
            },
            cx,
        );
    }

    fn new(
        workspace: &Entity<Workspace>,
        git_store: &Entity<GitStore>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(workspace, |_, _, cx| cx.notify()),
            cx.subscribe(git_store, |_, _, _, cx| cx.notify()),
        ];
        Self {
            workspace: workspace.downgrade(),
            threads: None,
            needed_attention: false,
            _threads_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    fn multi_workspace(&self, cx: &App) -> Option<Entity<MultiWorkspace>> {
        self.workspace
            .upgrade()?
            .read(cx)
            .multi_workspace()?
            .upgrade()
    }

    /// The window registers its thread list after its first workspace exists, so the sidebar
    /// looks for it each time it draws until it finds it.
    fn connect_thread_list(&mut self, cx: &mut Context<Self>) {
        if self.threads.is_some() {
            return;
        }
        let Some(threads) = self.multi_workspace(cx).and_then(|multi_workspace| {
            multi_workspace
                .read(cx)
                .sidebar()?
                .to_any()
                .downcast::<Sidebar>()
                .ok()
        }) else {
            return;
        };
        self._threads_subscription = Some(cx.observe(&threads, |this, _, cx| {
            let needs_attention = this.needs_attention(cx);
            if needs_attention != this.needed_attention {
                this.needed_attention = needs_attention;
                // The title bar flags the project of a thread that waits on the user.
                if let Some(multi_workspace) = this.multi_workspace(cx) {
                    multi_workspace.update(cx, |_, cx| cx.notify());
                }
            }
            cx.notify();
        }));
        self.threads = Some(threads);
    }

    fn needs_attention(&self, cx: &App) -> bool {
        let (Some(threads), Some(workspace)) = (self.threads.as_ref(), self.workspace.upgrade())
        else {
            return false;
        };
        let key = workspace.read(cx).project_group_key(cx);
        threads.read(cx).waiting_thread_count(&key) > 0
    }

    fn render_header(&self, workspace: &Entity<Workspace>, cx: &App) -> impl IntoElement {
        let key = workspace.read(cx).project_group_key(cx);
        let accent_index = self.multi_workspace(cx).and_then(|multi_workspace| {
            multi_workspace
                .read(cx)
                .project_group_keys()
                .iter()
                .position(|group_key| *group_key == key)
        });
        let accent = cx
            .theme()
            .accents()
            .color_for_index(accent_index.unwrap_or_default() as u32);
        let branch = workspace
            .read(cx)
            .project()
            .read(cx)
            .active_repository(cx)
            .and_then(|repository| {
                repository
                    .read(cx)
                    .branch
                    .as_ref()
                    .map(|branch| SharedString::from(branch.name().to_string()))
            });
        let colors = cx.theme().colors();
        let weak_workspace = self.workspace.clone();

        h_flex()
            .w_full()
            .min_h(rems_from_px(HEADER_HEIGHT))
            .px(rems_from_px(LIST_INSET))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rems_from_px(6_f32))
                    .pl(rems_from_px(GROUP_LABEL_PADDING_START))
                    .pr(rems_from_px(GROUP_LABEL_PADDING_END))
                    .child(div().flex_none().size(px(8.)).rounded(px(2.)).bg(accent))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(key.display_name(&Default::default())),
                    )
                    .when_some(branch, |row, branch| {
                        row.child(
                            h_flex()
                                .min_w_0()
                                .gap(rems_from_px(4_f32))
                                .text_sm()
                                .text_color(colors.text_muted)
                                .child(sidebar_icon(IconName::GitBranch).flex_none())
                                .child(div().min_w_0().truncate().child(branch)),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        sidebar_icon_button("workspace-options", IconName::ChevronDown)
                            .tooltip("Workspace Tools")
                            .dropdown_menu(move |menu, _, cx| {
                                let Some(workspace) = weak_workspace.upgrade() else {
                                    return menu;
                                };
                                workspace.read(cx).tools_menu(menu, cx)
                            }),
                    )
                    .child(
                        sidebar_icon_button("workspace-search", IconName::MagnifyingGlass)
                            .tooltip("Search")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(zed_actions::unified_search::Toggle::default()),
                                    cx,
                                )
                            }),
                    ),
            )
    }

    /// A group's title with its count, and buttons that show while the group is hovered.
    fn render_group_label(
        &self,
        id: &'static str,
        title: &'static str,
        count: usize,
        buttons: Vec<AnyElement>,
        cx: &App,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let group = SharedString::from(format!("workspace-nav-{id}"));
        h_flex()
            .id(id)
            .group(group.clone())
            .w_full()
            .min_h(rems_from_px(GROUP_LABEL_HEIGHT))
            .px(rems_from_px(LIST_INSET))
            .child(
                h_flex()
                    .w_full()
                    .gap(rems_from_px(6_f32))
                    .pl(rems_from_px(GROUP_LABEL_PADDING_START))
                    .pr(rems_from_px(GROUP_LABEL_PADDING_END))
                    .text_color(colors.text_muted)
                    .child(title)
                    .child(
                        div()
                            .text_color(colors.text_placeholder)
                            .child(count.to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .children(buttons),
                    ),
            )
    }

    fn render_threads(&self, workspace: &Entity<Workspace>, cx: &App) -> impl IntoElement {
        let key = workspace.read(cx).project_group_key(cx);
        let threads = self
            .threads
            .as_ref()
            .map(|threads| threads.read(cx).project_threads(&key))
            .unwrap_or_default();
        let colors = cx.theme().colors();
        let warning = cx.theme().status().warning;
        let new_thread = sidebar_icon_button("new-thread", IconName::Plus)
            .tooltip("New Thread")
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(zed_actions::agent::NewThread), cx)
            })
            .into_any_element();

        v_flex()
            .w_full()
            .child(self.render_group_label(
                "threads",
                "Threads",
                threads.len(),
                vec![new_thread],
                cx,
            ))
            .children(threads.into_iter().map(|thread| {
                let entry_index = thread.entry_index;
                let end = if thread.status == StatusDot::WaitingForConfirmation {
                    Some(div().text_sm().text_color(warning).child("Approve"))
                } else {
                    thread.updated.map(|updated| {
                        div()
                            .text_sm()
                            .text_color(colors.text_placeholder)
                            .child(updated)
                    })
                };
                let thread_list = self.threads.clone();
                SidebarRow::new(
                    SharedString::from(format!("nav-thread-{entry_index}")),
                    thread.status,
                    div()
                        .truncate()
                        .text_color(if thread.is_active {
                            colors.text
                        } else {
                            colors.text_muted
                        })
                        .child(thread.title),
                )
                .when_some(end, |row, end| row.end(end))
                .selected(thread.is_active)
                .on_click(move |_, window, cx| {
                    if let Some(thread_list) = &thread_list {
                        thread_list.update(cx, |thread_list, cx| {
                            thread_list.activate_project_thread(entry_index, window, cx)
                        });
                    }
                })
            }))
    }

    /// The files at hand: what this workspace's threads changed, what is open, and what git
    /// reports as changed.
    fn file_groups(
        &self,
        workspace: &Entity<Workspace>,
        cx: &App,
    ) -> (Vec<FileRow>, Vec<FileRow>, Vec<FileRow>, usize) {
        let workspace = workspace.read(cx);
        let mut thread_changes: Vec<FileRow> = Vec::new();
        if let Some(agent_panel) = workspace.panel::<AgentPanel>(cx) {
            for conversation in agent_panel.read(cx).conversation_views() {
                let Some(thread_view) = conversation.read(cx).root_thread_view() else {
                    continue;
                };
                let action_log = thread_view.read(cx).thread.read(cx).action_log().clone();
                for (buffer, diff) in action_log.read(cx).changed_buffers(cx) {
                    let Some(file) = buffer.read(cx).file() else {
                        continue;
                    };
                    let path = ProjectPath::from_file(file.as_ref(), cx);
                    let stats = DiffStats::single_file(diff.read(cx));
                    match thread_changes.iter_mut().find(|row| row.path == path) {
                        Some(FileRow {
                            end: FileRowEnd::DiffStats(existing),
                            ..
                        }) => {
                            existing.lines_added += stats.lines_added;
                            existing.lines_removed += stats.lines_removed;
                        }
                        Some(_) => {}
                        None => thread_changes.push(FileRow {
                            path,
                            item: None,
                            end: FileRowEnd::DiffStats(stats),
                        }),
                    }
                }
            }
        }

        let mut open_files: Vec<FileRow> = Vec::new();
        for pane in workspace.panes() {
            for item in pane.read(cx).items() {
                if item.tab_category(cx) != TabCategory::File {
                    continue;
                }
                let Some(path) = item.project_path(cx) else {
                    continue;
                };
                if thread_changes.iter().any(|row| row.path == path)
                    || open_files.iter().any(|row| row.path == path)
                {
                    continue;
                }
                open_files.push(FileRow {
                    path,
                    item: Some(item.boxed_clone()),
                    end: FileRowEnd::None,
                });
            }
        }

        let mut git_changes = Vec::new();
        let mut git_change_count = 0;
        if let Some(repository) = workspace.project().read(cx).active_repository(cx) {
            let repository = repository.read(cx);
            let status = cx.theme().status();
            for entry in repository.cached_status() {
                git_change_count += 1;
                if git_changes.len() >= GIT_CHANGES_SHOWN {
                    continue;
                }
                let Some(path) = repository.repo_path_to_project_path(&entry.repo_path, cx) else {
                    continue;
                };
                let (letter, color) = if entry.status.is_conflicted() {
                    ("!", Color::Custom(status.conflict))
                } else if entry.status.is_deleted() {
                    ("D", Color::Custom(status.deleted))
                } else if entry.status.is_created() || entry.status.is_untracked() {
                    ("A", Color::Custom(status.created))
                } else {
                    ("M", Color::Custom(status.modified))
                };
                git_changes.push(FileRow {
                    path,
                    item: None,
                    end: FileRowEnd::Status(letter, color),
                });
            }
        }

        (thread_changes, open_files, git_changes, git_change_count)
    }

    fn render_file_row(&self, row: FileRow, cx: &App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let name: SharedString = row
            .path
            .path
            .file_name()
            .unwrap_or_default()
            .to_string()
            .into();
        let directory: SharedString = row
            .path
            .path
            .parent()
            .map(|parent| parent.as_unix_str().to_string())
            .unwrap_or_default()
            .into();
        let end = match row.end {
            FileRowEnd::None => None,
            FileRowEnd::DiffStats(stats) => Some(
                h_flex()
                    .gap(rems_from_px(4_f32))
                    .text_sm()
                    .child(
                        div()
                            .text_color(status.created)
                            .child(format!("+{}", stats.lines_added)),
                    )
                    .child(
                        div()
                            .text_color(status.deleted)
                            .child(format!("−{}", stats.lines_removed)),
                    )
                    .into_any_element(),
            ),
            FileRowEnd::Status(letter, color) => Some(
                div()
                    .text_sm()
                    .text_color(color.color(cx))
                    .child(letter)
                    .into_any_element(),
            ),
        };
        let path = row.path.clone();
        let item = row.item;
        let workspace = self.workspace.clone();
        SidebarRow::new(
            SharedString::from(format!(
                "nav-file-{}-{}",
                row.path.worktree_id.to_usize(),
                row.path.path.as_unix_str()
            )),
            sidebar_icon(IconName::File).text_color(colors.icon_muted),
            h_flex()
                .min_w_0()
                .gap(rems_from_px(6_f32))
                .child(div().flex_none().text_color(colors.text_muted).child(name))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(colors.text_placeholder)
                        .child(directory),
                ),
        )
        .when_some(end, |row, end| row.end(end))
        .on_click(move |_, window, cx| {
            workspace
                .update(cx, |workspace, cx| match &item {
                    Some(item) => {
                        workspace.activate_item(item.as_ref(), true, true, window, cx);
                    }
                    None => workspace
                        .open_path(path.clone(), None, true, window, cx)
                        .detach_and_log_err(cx),
                })
                .log_err();
        })
    }

    fn render_file_group_label(&self, title: &'static str, cx: &App) -> impl IntoElement {
        div()
            .w_full()
            .min_h(rems_from_px(FILE_GROUP_LABEL_HEIGHT))
            .flex()
            .items_center()
            .px(rems_from_px(LIST_INSET))
            .pl(rems_from_px(LIST_INSET + GROUP_LABEL_PADDING_START))
            .text_xs()
            .text_color(cx.theme().colors().text_placeholder)
            .child(title)
    }

    fn render_files(&self, workspace: &Entity<Workspace>, cx: &App) -> impl IntoElement {
        let (thread_changes, open_files, git_changes, git_change_count) =
            self.file_groups(workspace, cx);
        let count = thread_changes.len() + open_files.len() + git_changes.len();
        let colors = cx.theme().colors();
        let weak_workspace = self.workspace.clone();
        let buttons = vec![
            sidebar_icon_button("project-tree", IconName::ListTree)
                .tooltip("Project Files")
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(zed_actions::project_panel::ToggleFocus), cx)
                })
                .into_any_element(),
            sidebar_icon_button("open-file", IconName::Plus)
                .tooltip("Open File")
                .dropdown_menu(move |menu, _, cx| {
                    let Some(workspace) = weak_workspace.upgrade() else {
                        return menu;
                    };
                    workspace.update(cx, |workspace, cx| {
                        workspace.tab_creation_menu(TabCategory::File, menu, cx)
                    })
                })
                .into_any_element(),
        ];
        let hidden_git_changes = git_change_count.saturating_sub(git_changes.len());

        v_flex()
            .w_full()
            .mt(rems_from_px(GROUP_GAP))
            .child(self.render_group_label("files", "Files", count, buttons, cx))
            .when(!thread_changes.is_empty(), |group| {
                group
                    .child(self.render_file_group_label("Thread Changes", cx))
                    .children(
                        thread_changes
                            .into_iter()
                            .map(|row| self.render_file_row(row, cx)),
                    )
            })
            .when(!open_files.is_empty(), |group| {
                group
                    .child(self.render_file_group_label("Open", cx))
                    .children(open_files.into_iter().map(|row| self.render_file_row(row, cx)))
            })
            .when(!git_changes.is_empty(), |group| {
                group
                    .child(self.render_file_group_label("Git Changes", cx))
                    .children(git_changes.into_iter().map(|row| self.render_file_row(row, cx)))
            })
            .when(hidden_git_changes > 0, |group| {
                group.child(
                    SidebarRow::new(
                        "nav-all-changes",
                        div(),
                        div()
                            .truncate()
                            .text_color(colors.text_placeholder)
                            .child(format!("All {git_change_count} Changes")),
                    )
                    .end(sidebar_icon(IconName::ChevronRight).text_color(colors.icon_muted))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(zed_actions::git_panel::ToggleFocus), cx)
                    }),
                )
            })
    }

    fn render_terminals(&self, workspace: &Entity<Workspace>, cx: &App) -> impl IntoElement {
        let terminals = workspace
            .read(cx)
            .panes()
            .iter()
            .flat_map(|pane| pane.read(cx).items())
            .filter(|item| item.tab_category(cx) == TabCategory::Terminal)
            .map(|item| item.boxed_clone())
            .collect::<Vec<_>>();
        let active_item_id = workspace.read(cx).active_item(cx).map(|item| item.item_id());
        let colors = cx.theme().colors();
        let weak_workspace = self.workspace.clone();
        let new_terminal = sidebar_icon_button("new-terminal", IconName::Plus)
            .tooltip("New Terminal")
            .dropdown_menu(move |menu, _, cx| {
                let Some(workspace) = weak_workspace.upgrade() else {
                    return menu;
                };
                workspace.update(cx, |workspace, cx| {
                    workspace.tab_creation_menu(TabCategory::Terminal, menu, cx)
                })
            })
            .into_any_element();

        v_flex()
            .w_full()
            .mt(rems_from_px(GROUP_GAP))
            .child(self.render_group_label(
                "terminals",
                "Terminals",
                terminals.len(),
                vec![new_terminal],
                cx,
            ))
            .children(terminals.into_iter().map(|item| {
                let status = item.tab_status(cx);
                let leading = match status.as_ref().map(|(_, color)| *color) {
                    Some(Color::Accent) => StatusDot::Running.into_any_element(),
                    Some(Color::Success) => sidebar_icon(IconName::Check)
                        .text_color(cx.theme().status().success)
                        .into_any_element(),
                    _ => sidebar_icon(IconName::Terminal)
                        .text_color(colors.icon_muted)
                        .into_any_element(),
                };
                let is_active = active_item_id == Some(item.item_id());
                let workspace = self.workspace.clone();
                SidebarRow::new(
                    SharedString::from(format!("nav-terminal-{}", item.item_id())),
                    leading,
                    div()
                        .truncate()
                        .text_color(if is_active {
                            colors.text
                        } else {
                            colors.text_muted
                        })
                        .child(item.tab_content_text(0, cx)),
                )
                .when_some(status, |row, (label, color)| {
                    row.end(div().text_sm().text_color(color.color(cx)).child(label))
                })
                .selected(is_active)
                .on_click(move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.activate_item(item.as_ref(), true, true, window, cx);
                        })
                        .log_err();
                })
            }))
    }

    fn render_footer(
        &self,
        workspace: &Entity<Workspace>,
        window: &Window,
        cx: &App,
    ) -> impl IntoElement {
        let change_count = workspace
            .read(cx)
            .project()
            .read(cx)
            .active_repository(cx)
            .map(|repository| repository.read(cx).status_summary().count)
            .unwrap_or_default();
        let label = match change_count {
            0 => "No Changes".to_string(),
            1 => "1 Change".to_string(),
            count => format!("{count} Changes"),
        };
        let shortcut = window
            .highest_precedence_binding_for_action(&zed_actions::git_panel::ToggleFocus)
            .and_then(|binding| {
                let keystroke = binding.keystrokes().first()?;
                Some(Kbd::format(keystroke.as_keystroke()))
            });
        let colors = cx.theme().colors();

        div()
            .w_full()
            .pt(rems_from_px(6_f32))
            .pb(rems_from_px(LIST_INSET))
            .border_t_1()
            .border_color(colors.border_variant)
            .child(
                SidebarRow::new(
                    "workspace-changes",
                    sidebar_icon(IconName::GitBranch).text_color(colors.icon_muted),
                    div()
                        .truncate()
                        .text_color(colors.text_muted)
                        .child(label),
                )
                .when_some(shortcut, |row, shortcut| {
                    row.end(div().text_xs().text_color(colors.text_muted).child(shortcut))
                })
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(zed_actions::git_panel::ToggleFocus), cx)
                }),
            )
    }
}

impl Render for WorkspaceNav {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.connect_thread_list(cx);
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        v_flex()
            .size_full()
            .pt(rems_from_px(LIST_INSET))
            .child(self.render_header(&workspace, cx))
            .child(
                v_flex()
                    .id("workspace-nav-groups")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pt(rems_from_px(4_f32))
                    .pb(rems_from_px(LIST_INSET))
                    .child(self.render_threads(&workspace, cx))
                    .child(self.render_files(&workspace, cx))
                    .child(self.render_terminals(&workspace, cx)),
            )
            .child(self.render_footer(&workspace, window, cx))
            .into_any_element()
    }
}
