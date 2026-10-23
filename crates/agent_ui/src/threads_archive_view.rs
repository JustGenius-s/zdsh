use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_connection_store::AgentConnectionStore;

use crate::thread_metadata_store::{
    ThreadId, ThreadMetadata, ThreadMetadataStore, worktree_info_from_thread_paths,
};
use crate::{Agent, ArchiveSelectedThread, DEFAULT_THREAD_TITLE, RemoveSelectedThread};

use agent::ThreadStore;
use agent_client_protocol::schema::v2 as acp;
use chrono::{DateTime, Local, NaiveDate, Utc};
use collections::HashMap;
use fs::Fs;
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    ListState, Render, SharedString, Subscription, Task, TaskExt, WeakEntity, Window, list,
    prelude::*, px, rems,
};
use gpui_kit::{
    base::{Scrollbar, ScrollbarAxis, ScrollbarMode, StyledExt as _},
    component::{
        Disableable as _, Icon as KitIcon, Selectable as _, Sizable as _,
        button::{Button as KitButton, ButtonVariants as _},
        h_flex,
        label::Label as KitLabel,
        list::ListItem as KitListItem,
        menu::{DropdownMenu as _, PopupMenuItem},
        separator::Separator,
        spinner::Spinner,
        tooltip::Tooltip as KitTooltip,
        v_flex,
    },
};
use itertools::Itertools as _;
use menu::{Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use picker::{
    Picker, PickerDelegate,
    highlighted_match_with_paths::{HighlightedMatch, HighlightedMatchWithPaths},
};
use project::{AgentId, AgentServerStore};
use settings::Settings as _;
use theme::ActiveTheme;
use ui::{HighlightedLabel, prelude::*};
use util::ResultExt;
use util::paths::PathExt;
use workspace::{
    ModalView, PathList, RecentWorkspace, SerializedWorkspaceLocation, Workspace, WorkspaceDb,
    WorkspaceId,
};

use zed_actions::editor::{MoveDown, MoveUp};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ThreadFilter {
    #[default]
    Unarchived,
    ArchivedOnly,
}

#[derive(Clone)]
enum ArchiveListItem {
    BucketSeparator(TimeBucket),
    Entry {
        thread: ThreadMetadata,
        highlight_positions: Vec<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeBucket {
    Today,
    Yesterday,
    LastSevenDays,
    LastThirtyDays,
    Older,
}

impl TimeBucket {
    fn from_dates(reference: NaiveDate, date: NaiveDate) -> Self {
        match reference.signed_duration_since(date).num_days() {
            days if days <= 0 => TimeBucket::Today,
            1 => TimeBucket::Yesterday,
            2..=6 => TimeBucket::LastSevenDays,
            7..=29 => TimeBucket::LastThirtyDays,
            _ => TimeBucket::Older,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            TimeBucket::Today => "Today",
            TimeBucket::Yesterday => "Yesterday",
            TimeBucket::LastSevenDays => "Last 7 Days",
            TimeBucket::LastThirtyDays => "Last 30 Days",
            TimeBucket::Older => "Older",
        }
    }
}

pub fn fuzzy_match_positions(query: &str, candidate: &str) -> Option<Vec<usize>> {
    let query_chars: Vec<char> = query.chars().collect();
    if query_chars.is_empty() {
        return Some(Vec::new());
    }

    let candidate_chars: Vec<(usize, char)> = candidate.char_indices().collect();
    let window_count = candidate_chars.len().checked_sub(query_chars.len() - 1)?;

    'outer: for window_start in 0..window_count {
        for (qi, &query_char) in query_chars.iter().enumerate() {
            let (_, cand_char) = candidate_chars[window_start + qi];
            if !cand_char.eq_ignore_ascii_case(&query_char) {
                continue 'outer;
            }
        }
        return Some(
            (0..query_chars.len())
                .map(|qi| candidate_chars[window_start + qi].0)
                .collect(),
        );
    }

    None
}

pub enum ThreadsArchiveViewEvent {
    Close,
    Activate { thread: ThreadMetadata },
    CancelRestore { thread_id: ThreadId },
    Import,
    Search,
}

impl EventEmitter<ThreadsArchiveViewEvent> for ThreadsArchiveView {}

pub struct ThreadsArchiveView {
    _history_subscription: Subscription,
    focus_handle: FocusHandle,
    list_state: ListState,
    items: Vec<ArchiveListItem>,
    selection: Option<usize>,
    preserve_selection_on_next_update: bool,
    _subscriptions: Vec<gpui::Subscription>,
    _refresh_history_task: Task<()>,
    workspace: WeakEntity<Workspace>,
    agent_connection_store: WeakEntity<AgentConnectionStore>,
    agent_server_store: WeakEntity<AgentServerStore>,
    restoring: HashSet<ThreadId>,
    archived_thread_ids: HashSet<ThreadId>,
    archived_branch_names: HashMap<ThreadId, HashMap<PathBuf, String>>,
    _load_branch_names_task: Task<()>,
    thread_filter: ThreadFilter,
}

impl ThreadsArchiveView {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        agent_connection_store: WeakEntity<AgentConnectionStore>,
        agent_server_store: WeakEntity<AgentServerStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        let thread_metadata_store_subscription = cx.observe(
            &ThreadMetadataStore::global(cx),
            |this: &mut Self, _, cx| {
                this.update_items(cx);
                this.reload_branch_names_if_threads_changed(cx);
            },
        );

        cx.on_focus_out(&focus_handle, window, |this: &mut Self, _, _window, cx| {
            this.selection = None;
            cx.notify();
        })
        .detach();

        let mut this = Self {
            _history_subscription: Subscription::new(|| {}),
            focus_handle,
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(1000.)),
            items: Vec::new(),
            selection: None,
            preserve_selection_on_next_update: false,
            _subscriptions: vec![thread_metadata_store_subscription],
            _refresh_history_task: Task::ready(()),
            workspace,
            agent_connection_store,
            agent_server_store,
            restoring: HashSet::default(),
            archived_thread_ids: HashSet::default(),
            archived_branch_names: HashMap::default(),
            _load_branch_names_task: Task::ready(()),
            thread_filter: ThreadFilter::Unarchived,
        };

        this.update_items(cx);
        this.reload_branch_names_if_threads_changed(cx);
        this
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn mark_restoring(&mut self, thread_id: &ThreadId, cx: &mut Context<Self>) {
        self.restoring.insert(*thread_id);
        cx.notify();
    }

    pub fn clear_restoring(&mut self, thread_id: &ThreadId, cx: &mut Context<Self>) {
        self.restoring.remove(thread_id);
        cx.notify();
    }

    fn update_items(&mut self, cx: &mut Context<Self>) {
        let store = ThreadMetadataStore::global(cx).read(cx);

        let thread_filter = self.thread_filter;
        let sessions = store
            .entries()
            .filter(|t| match thread_filter {
                ThreadFilter::Unarchived => !t.archived,
                ThreadFilter::ArchivedOnly => t.archived,
            })
            .sorted_by_cached_key(|t| t.created_at.unwrap_or(t.updated_at))
            .rev()
            .cloned()
            .collect::<Vec<_>>();

        let today = Local::now().naive_local().date();

        let mut items = Vec::with_capacity(sessions.len() + 5);
        let mut current_bucket: Option<TimeBucket> = None;

        for session in sessions {
            let entry_bucket = {
                let entry_date = session
                    .created_at
                    .unwrap_or(session.updated_at)
                    .with_timezone(&Local)
                    .naive_local()
                    .date();
                TimeBucket::from_dates(today, entry_date)
            };

            if Some(entry_bucket) != current_bucket {
                current_bucket = Some(entry_bucket);
                items.push(ArchiveListItem::BucketSeparator(entry_bucket));
            }

            items.push(ArchiveListItem::Entry {
                thread: session,
                highlight_positions: Vec::new(),
            });
        }

        let preserve = self.preserve_selection_on_next_update;
        self.preserve_selection_on_next_update = false;

        let saved_scroll = self.list_state.logical_scroll_top();

        self.list_state.reset(items.len());
        self.items = items;

        self.list_state.scroll_to(saved_scroll);

        if preserve {
            if let Some(ix) = self.selection {
                let next = self.find_next_selectable(ix).or_else(|| {
                    ix.checked_sub(1)
                        .and_then(|i| self.find_previous_selectable(i))
                });
                self.selection = next;
                if let Some(next) = next {
                    self.list_state.scroll_to_reveal_item(next);
                }
            }
        } else {
            self.selection = None;
        }

        cx.notify();
    }

    fn reload_branch_names_if_threads_changed(&mut self, cx: &mut Context<Self>) {
        let current_ids: HashSet<ThreadId> = self
            .items
            .iter()
            .filter_map(|item| match item {
                ArchiveListItem::Entry { thread, .. } => Some(thread.thread_id),
                _ => None,
            })
            .collect();

        if current_ids != self.archived_thread_ids {
            self.archived_thread_ids = current_ids;
            self.load_archived_branch_names(cx);
        }
    }

    fn load_archived_branch_names(&mut self, cx: &mut Context<Self>) {
        let task = ThreadMetadataStore::global(cx)
            .read(cx)
            .get_all_archived_branch_names(cx);
        self._load_branch_names_task = cx.spawn(async move |this, cx| {
            if let Some(branch_names) = task.await.log_err() {
                this.update(cx, |this, cx| {
                    this.archived_branch_names = branch_names;
                    cx.notify();
                })
                .log_err();
            }
        });
    }

    fn archive_thread(&mut self, thread_id: ThreadId, cx: &mut Context<Self>) {
        self.preserve_selection_on_next_update = true;
        ThreadMetadataStore::global(cx).update(cx, |store, cx| store.archive(thread_id, None, cx));
    }

    fn archive_selected_thread(
        &mut self,
        _: &ArchiveSelectedThread,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.selection else { return };
        let Some(ArchiveListItem::Entry { thread, .. }) = self.items.get(ix) else {
            return;
        };

        if thread.archived {
            return;
        }

        self.archive_thread(thread.thread_id, cx);
    }

    fn unarchive_thread(
        &mut self,
        thread: ThreadMetadata,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring.contains(&thread.thread_id) {
            return;
        }

        if thread.folder_paths().is_empty() {
            self.show_project_picker_for_thread(thread, window, cx);
            return;
        }

        self.mark_restoring(&thread.thread_id, cx);
        self.selection = None;
        cx.emit(ThreadsArchiveViewEvent::Activate { thread });
    }

    fn show_project_picker_for_thread(
        &mut self,
        thread: ThreadMetadata,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let archive_view = cx.weak_entity();
        let fs = workspace.read(cx).app_state().fs.clone();
        let current_workspace_id = workspace.read(cx).database_id();
        let sibling_workspace_ids: HashSet<WorkspaceId> = workspace
            .read(cx)
            .multi_workspace()
            .and_then(|mw| mw.upgrade())
            .map(|mw| {
                mw.read(cx)
                    .workspaces()
                    .filter_map(|ws| ws.read(cx).database_id())
                    .collect()
            })
            .unwrap_or_default();

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                ProjectPickerModal::new(
                    thread,
                    fs,
                    archive_view,
                    current_workspace_id,
                    sibling_workspace_ids,
                    window,
                    cx,
                )
            });
        });
    }

    fn is_selectable_item(&self, ix: usize) -> bool {
        matches!(self.items.get(ix), Some(ArchiveListItem::Entry { .. }))
    }

    fn find_next_selectable(&self, start: usize) -> Option<usize> {
        (start..self.items.len()).find(|&i| self.is_selectable_item(i))
    }

    fn find_previous_selectable(&self, start: usize) -> Option<usize> {
        (0..=start).rev().find(|&i| self.is_selectable_item(i))
    }

    fn editor_move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.select_next(&SelectNext, window, cx);
        if self.selection.is_some() {
            self.focus_handle.focus(window, cx);
        }
    }

    fn editor_move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.select_previous(&SelectPrevious, window, cx);
        if self.selection.is_some() {
            self.focus_handle.focus(window, cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.selection {
            Some(ix) => self.find_next_selectable(ix + 1),
            None => self.find_next_selectable(0),
        };
        if let Some(next) = next {
            self.selection = Some(next);
            self.list_state.scroll_to_reveal_item(next);
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        match self.selection {
            Some(ix) => {
                if let Some(prev) = (ix > 0)
                    .then(|| self.find_previous_selectable(ix - 1))
                    .flatten()
                {
                    self.selection = Some(prev);
                    self.list_state.scroll_to_reveal_item(prev);
                } else {
                    self.selection = None;
                }
                cx.notify();
            }
            None => {
                let last = self.items.len().saturating_sub(1);
                if let Some(prev) = self.find_previous_selectable(last) {
                    self.selection = Some(prev);
                    self.list_state.scroll_to_reveal_item(prev);
                    cx.notify();
                }
            }
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(first) = self.find_next_selectable(0) {
            self.selection = Some(first);
            self.list_state.scroll_to_reveal_item(first);
            cx.notify();
        }
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        let last = self.items.len().saturating_sub(1);
        if let Some(last) = self.find_previous_selectable(last) {
            self.selection = Some(last);
            self.list_state.scroll_to_reveal_item(last);
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selection else { return };
        let Some(ArchiveListItem::Entry { thread, .. }) = self.items.get(ix) else {
            return;
        };

        self.unarchive_thread(thread.clone(), window, cx);
    }

    fn render_list_entry(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.items.get(ix) else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors();

        match item {
            ArchiveListItem::BucketSeparator(bucket) => h_flex()
                .w_full()
                .px(rems(0.875))
                .pt(rems(if ix == 0 { 0.25 } else { 1. }))
                .pb(rems(0.375))
                .child(
                    KitLabel::new(bucket.label())
                        .text_sm()
                        .text_color(colors.text_muted),
                )
                .into_any_element(),
            ArchiveListItem::Entry {
                thread,
                highlight_positions,
            } => {
                let is_focused = self.selection == Some(ix);
                let is_restoring = self.restoring.contains(&thread.thread_id);
                let is_archived = thread.archived;
                let title = thread.display_title();
                let timestamp =
                    format_history_entry_timestamp(thread.created_at.unwrap_or(thread.updated_at));
                let group: SharedString = format!("history-entry-{:?}", thread.thread_id).into();

                let icon = if is_restoring {
                    Spinner::new()
                        .small()
                        .color(colors.text_muted)
                        .into_any_element()
                } else {
                    let external_icon = self
                        .agent_server_store
                        .upgrade()
                        .and_then(|store| store.read(cx).agent_icon(&thread.agent_id));
                    let icon_color = if is_archived {
                        colors.icon_muted
                    } else {
                        colors.icon
                    };
                    if let Some(path) = external_icon {
                        gpui::svg()
                            .external_path(path)
                            .size(rems(0.875))
                            .text_color(icon_color)
                            .into_any_element()
                    } else {
                        history_icon(
                            if thread.agent_id.as_ref() == agent::ZED_AGENT_ID.as_ref() {
                                IconName::ZedAgent
                            } else {
                                IconName::Sparkle
                            },
                        )
                        .text_color(icon_color)
                        .into_any_element()
                    }
                };

                let title_label = if highlight_positions.is_empty() {
                    KitLabel::new(title.clone())
                        .truncate()
                        .text_color(if is_focused {
                            colors.text
                        } else {
                            colors.text_muted
                        })
                        .into_any_element()
                } else {
                    HighlightedLabel::new(title.clone(), highlight_positions.clone())
                        .color(if is_focused {
                            Color::Default
                        } else {
                            Color::Muted
                        })
                        .truncate()
                        .into_any_element()
                };
                let project_paths = thread.folder_paths().paths_owned();
                let project_tooltip = if project_paths.is_empty() {
                    "No project".to_string()
                } else {
                    project_paths
                        .iter()
                        .map(|path| path.compact().to_string_lossy().into_owned())
                        .join("\n")
                };
                let branch_names: HashMap<PathBuf, SharedString> = self
                    .archived_branch_names
                    .get(&thread.thread_id)
                    .map(|names| {
                        names
                            .iter()
                            .map(|(path, name)| (path.clone(), name.clone().into()))
                            .collect()
                    })
                    .unwrap_or_default();
                let worktrees =
                    worktree_info_from_thread_paths(&thread.worktree_paths, &branch_names);
                let worktree_tooltip = worktrees
                    .iter()
                    .map(|worktree| {
                        if let Some(branch) = &worktree.branch_name {
                            format!("{} · {}", worktree.full_path, branch)
                        } else {
                            worktree.full_path.to_string()
                        }
                    })
                    .join("\n");

                let mut tooltip = format!("{title}\n{project_tooltip}");
                if !worktree_tooltip.is_empty() {
                    tooltip.push_str(&format!("\n{worktree_tooltip}"));
                }
                if is_archived {
                    tooltip.push_str("\nArchived");
                }

                let actions = if is_restoring {
                    history_icon_button("cancel-restore", IconName::Close)
                        .tooltip("Cancel Restore")
                        .on_click({
                            let thread_id = thread.thread_id;
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.clear_restoring(&thread_id, cx);
                                cx.emit(ThreadsArchiveViewEvent::CancelRestore { thread_id });
                            })
                        })
                        .into_any_element()
                } else if is_archived {
                    h_flex()
                        .gap_0p5()
                        .child(
                            history_icon_button("restore-thread", IconName::RotateCcw)
                                .tooltip("Restore Thread")
                                .on_click({
                                    let thread = thread.clone();
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.unarchive_thread(thread.clone(), window, cx);
                                    })
                                }),
                        )
                        .child(
                            history_icon_button("delete-thread", IconName::Trash)
                                .tooltip_with_action(
                                    "Delete Thread",
                                    &RemoveSelectedThread,
                                    Some("ThreadsArchiveView"),
                                )
                                .on_click({
                                    let agent = thread.agent_id.clone();
                                    let thread_id = thread.thread_id;
                                    let session_id = thread.session_id.clone();
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.preserve_selection_on_next_update = true;
                                        this.delete_thread(
                                            thread_id,
                                            session_id.clone(),
                                            agent.clone(),
                                            cx,
                                        );
                                    })
                                }),
                        )
                        .into_any_element()
                } else {
                    history_icon_button("archive-thread", IconName::Archive)
                        .tooltip_with_action(
                            "Archive Thread",
                            &ArchiveSelectedThread,
                            Some("ThreadsArchiveView"),
                        )
                        .on_click({
                            let thread_id = thread.thread_id;
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.archive_thread(thread_id, cx);
                            })
                        })
                        .into_any_element()
                };

                div()
                    .w_full()
                    .px_2()
                    .py(rems(0.0625))
                    .child(
                        KitListItem::new(SharedString::from(format!(
                            "archive-entry-{:?}",
                            thread.thread_id
                        )))
                        .accessibility_label(title)
                        .group(group.clone())
                        .w_full()
                        .min_w_0()
                        .min_h(rems(1.875))
                        .px(rems(0.375))
                        .py(rems(0.125))
                        .rounded(rems(0.5))
                        .selected(is_focused)
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1p5()
                                .child(
                                    div()
                                        .size_5()
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .flex()
                                        .child(icon),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(title_label),
                                )
                                .child(
                                    h_flex()
                                        .relative()
                                        .flex_none()
                                        .w(rems(3.5))
                                        .h_6()
                                        .justify_end()
                                        .child(
                                            div()
                                                .id("history-timestamp")
                                                .when(is_focused || is_restoring, |this| {
                                                    this.invisible()
                                                })
                                                .group_hover(group.clone(), |this| this.invisible())
                                                .child(
                                                    KitLabel::new(timestamp)
                                                        .text_sm()
                                                        .text_color(colors.text_placeholder),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .id("history-actions")
                                                .absolute()
                                                .inset_0()
                                                .justify_end()
                                                .when(!is_focused && !is_restoring, |this| {
                                                    this.invisible()
                                                        .group_hover(group, |this| this.visible())
                                                })
                                                .child(actions),
                                        ),
                                ),
                        )
                        .tooltip(move |window, cx| {
                            KitTooltip::new(tooltip.clone()).build(window, cx)
                        })
                        .when(!is_restoring, |this| {
                            this.on_click({
                                let thread = thread.clone();
                                cx.listener(move |this, _, window, cx| {
                                    if !thread.archived {
                                        telemetry::event!(
                                            "Archived Thread Opened",
                                            agent = thread.agent_id.as_ref(),
                                            side = crate::agent_sidebar_side(cx)
                                        );
                                    }
                                    this.unarchive_thread(thread.clone(), window, cx);
                                })
                            })
                        }),
                    )
                    .into_any_element()
            }
        }
    }

    fn remove_selected_thread(
        &mut self,
        _: &RemoveSelectedThread,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.selection else { return };
        let Some(ArchiveListItem::Entry { thread, .. }) = self.items.get(ix) else {
            return;
        };

        self.preserve_selection_on_next_update = true;
        self.delete_thread(
            thread.thread_id,
            thread.session_id.clone(),
            thread.agent_id.clone(),
            cx,
        );
    }

    fn delete_thread(
        &mut self,
        thread_id: ThreadId,
        session_id: Option<acp::SessionId>,
        agent: AgentId,
        cx: &mut Context<Self>,
    ) {
        ThreadMetadataStore::global(cx).update(cx, |store, cx| store.delete(thread_id, cx));

        let agent = Agent::from(agent);

        let Some(agent_connection_store) = self.agent_connection_store.upgrade() else {
            return;
        };
        let fs = <dyn Fs>::global(cx);

        let task = agent_connection_store.update(cx, |store, cx| {
            store
                .request_connection(agent.clone(), agent.server(fs, ThreadStore::global(cx)), cx)
                .read(cx)
                .wait_for_connection()
        });
        cx.spawn(async move |_this, cx| {
            crate::thread_worktree_archive::cleanup_thread_archived_worktrees(thread_id, cx).await;

            let state = task.await?;
            let task = cx.update(|cx| {
                if let Some(session_id) = &session_id {
                    if let Some(list) = state
                        .connection
                        .session_list(cx)
                        .filter(|list| list.supports_delete())
                    {
                        list.delete_session(session_id, cx)
                    } else {
                        Task::ready(Ok(()))
                    }
                } else {
                    Task::ready(Ok(()))
                }
            });
            task.await
        })
        .detach_and_log_err(cx);
    }

    pub fn show_archived(&mut self, cx: &mut Context<Self>) {
        self.set_filter(ThreadFilter::ArchivedOnly, cx);
    }

    fn set_filter(&mut self, filter: ThreadFilter, cx: &mut Context<Self>) {
        if self.thread_filter == filter {
            return;
        }
        self.thread_filter = filter;
        self.update_items(cx);
        self.reload_branch_names_if_threads_changed(cx);
        if !self.items.is_empty() {
            self.list_state.scroll_to_reveal_item(0);
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let archive_view = cx.weak_entity();
        let show_archived = self.thread_filter == ThreadFilter::ArchivedOnly;

        h_flex()
            .w_full()
            .flex_none()
            .h(rems(2.5))
            .px_3()
            .gap_1p5()
            .child(
                history_icon_button("back-to-threads", IconName::ArrowLeft)
                    .tooltip("Back to Threads")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ThreadsArchiveViewEvent::Close))),
            )
            .child(
                KitLabel::new("History")
                    .text_color(cx.theme().colors().text_muted)
                    .truncate()
                    .flex_1(),
            )
            .child(
                history_icon_button("search-threads", IconName::MagnifyingGlass)
                    .tooltip("Search Threads")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ThreadsArchiveViewEvent::Search))),
            )
            .child(
                history_icon_button("history-options", IconName::Filter)
                    .selected(show_archived)
                    .tooltip("History Options")
                    .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                        let archive_view_for_filter = archive_view.clone();
                        let archive_view_for_import = archive_view.clone();
                        menu.min_w(px(220.))
                            .max_w(px(420.))
                            .label("Filters")
                            .item(
                                PopupMenuItem::new("Archived")
                                    .checked(show_archived)
                                    .on_click(move |_, _, cx| {
                                        archive_view_for_filter
                                            .update(cx, |this, cx| {
                                                this.set_filter(
                                                    if show_archived {
                                                        ThreadFilter::Unarchived
                                                    } else {
                                                        ThreadFilter::ArchivedOnly
                                                    },
                                                    cx,
                                                );
                                            })
                                            .log_err();
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new("Import Threads").on_click(move |_, _, cx| {
                                    archive_view_for_import
                                        .update(cx, |_, cx| {
                                            cx.emit(ThreadsArchiveViewEvent::Import)
                                        })
                                        .log_err();
                                }),
                            )
                    }),
            )
    }
}

fn history_icon(icon: IconName) -> KitIcon {
    KitIcon::empty().path(icon.path()).size(rems(0.875))
}

fn history_icon_button(id: impl Into<ElementId>, icon: IconName) -> KitButton {
    KitButton::new(id)
        .ghost()
        .small()
        .size_6()
        .p_0()
        .child(history_icon(icon))
}

fn history_scrollbar_mode(cx: &App) -> Option<ScrollbarMode> {
    use ui::scrollbars::{ScrollbarAutoHide, ShowScrollbar};

    match editor::EditorSettings::get_global(cx).scrollbar.show {
        ShowScrollbar::Never => None,
        ShowScrollbar::Always => Some(ScrollbarMode::Always),
        ShowScrollbar::Auto => Some(ScrollbarMode::Scrolling),
        ShowScrollbar::System => Some(
            if cx
                .try_global::<ScrollbarAutoHide>()
                .is_some_and(ScrollbarAutoHide::should_hide)
            {
                ScrollbarMode::Scrolling
            } else {
                ScrollbarMode::Always
            },
        ),
    }
}

pub fn format_history_entry_timestamp(entry_time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(entry_time);

    let minutes = duration.num_minutes();
    let hours = duration.num_hours();
    let days = duration.num_days();
    let weeks = days / 7;
    let months = days / 30;

    if minutes < 60 {
        format!("{}m", minutes.max(1))
    } else if hours < 24 {
        format!("{}h", hours.max(1))
    } else if days < 7 {
        format!("{}d", days.max(1))
    } else if weeks < 4 {
        format!("{}w", weeks.max(1))
    } else {
        format!("{}mo", months.max(1))
    }
}

impl Focusable for ThreadsArchiveView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ThreadsArchiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.items.is_empty();
        let content = if is_empty {
            let (message, description) = match self.thread_filter {
                ThreadFilter::Unarchived => ("No active threads", "Your threads will appear here."),
                ThreadFilter::ArchivedOnly => (
                    "No archived threads",
                    "Archived threads can be restored here.",
                ),
            };

            v_flex()
                .flex_1()
                .justify_center()
                .items_center()
                .p_4()
                .gap_3()
                .child(
                    history_icon(IconName::Archive)
                        .size(rems(1.75))
                        .text_color(cx.theme().colors().text_muted),
                )
                .child(KitLabel::new(message).font_semibold().text_center())
                .child(
                    KitLabel::new(description)
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().colors().text_muted),
                )
                .into_any_element()
        } else {
            v_flex()
                .relative()
                .min_h_0()
                .flex_1()
                .overflow_hidden()
                .child(
                    list(
                        self.list_state.clone(),
                        cx.processor(Self::render_list_entry),
                    )
                    .flex_1()
                    .size_full(),
                )
                .when_some(history_scrollbar_mode(cx), |this, mode| {
                    this.child(
                        div().absolute().inset_0().child(
                            Scrollbar::new(&self.list_state)
                                .id("history-scrollbar")
                                .axis(ScrollbarAxis::Vertical)
                                .mode(mode)
                                .viewport_from_layout(),
                        ),
                    )
                })
                .into_any_element()
        };

        v_flex()
            .key_context("ThreadsArchiveView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::editor_move_down))
            .on_action(cx.listener(Self::editor_move_up))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::remove_selected_thread))
            .on_action(cx.listener(Self::archive_selected_thread))
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(self.render_header(cx))
            .child(content)
    }
}

struct ProjectPickerModal {
    picker: Entity<Picker<ProjectPickerDelegate>>,
    _subscription: Subscription,
}

impl ProjectPickerModal {
    fn new(
        thread: ThreadMetadata,
        fs: Arc<dyn Fs>,
        archive_view: WeakEntity<ThreadsArchiveView>,
        current_workspace_id: Option<WorkspaceId>,
        sibling_workspace_ids: HashSet<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = ProjectPickerDelegate {
            thread,
            archive_view,
            workspaces: Vec::new(),
            filtered_entries: Vec::new(),
            selected_index: 0,
            current_workspace_id,
            sibling_workspace_ids,
        };

        let picker = cx.new(|cx| {
            Picker::list(delegate, window, cx)
                .list_measure_all()
                .embedded()
        });

        let _subscription =
            cx.subscribe(&picker, |_this: &mut Self, _, _event: &DismissEvent, cx| {
                cx.emit(DismissEvent);
            });

        let db = WorkspaceDb::global(cx);
        cx.spawn_in(window, async move |this, cx| {
            let workspaces = db
                .recent_project_workspaces(fs.as_ref())
                .await
                .log_err()
                .unwrap_or_default();
            this.update_in(cx, move |this, window, cx| {
                this.picker.update(cx, move |picker, cx| {
                    picker.delegate.workspaces = workspaces;
                    picker.update_matches(picker.query(cx), window, cx)
                })
            })
            .ok();
        })
        .detach();

        picker.focus_handle(cx).focus(window, cx);

        Self {
            picker,
            _subscription,
        }
    }
}

impl EventEmitter<DismissEvent> for ProjectPickerModal {}

impl Focusable for ProjectPickerModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl ModalView for ProjectPickerModal {}

impl Render for ProjectPickerModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ProjectPickerModal")
            .elevation_3(cx)
            .on_action(cx.listener(|this, _: &workspace::Open, window, cx| {
                this.picker.update(cx, |picker, cx| {
                    picker.delegate.open_local_folder(window, cx)
                })
            }))
            .child(self.picker.clone())
    }
}

enum ProjectPickerEntry {
    Header(SharedString),
    Workspace(StringMatch),
}

struct ProjectPickerDelegate {
    thread: ThreadMetadata,
    archive_view: WeakEntity<ThreadsArchiveView>,
    current_workspace_id: Option<WorkspaceId>,
    sibling_workspace_ids: HashSet<WorkspaceId>,
    workspaces: Vec<RecentWorkspace>,
    filtered_entries: Vec<ProjectPickerEntry>,
    selected_index: usize,
}

impl ProjectPickerDelegate {
    fn update_working_directories_and_unarchive(
        &mut self,
        paths: PathList,
        cx: &mut Context<Picker<Self>>,
    ) {
        self.thread.worktree_paths =
            super::thread_metadata_store::WorktreePaths::from_folder_paths(&paths);
        ThreadMetadataStore::global(cx).update(cx, |store, cx| {
            store.update_working_directories(self.thread.thread_id, paths, cx);
        });

        self.archive_view
            .update(cx, |view, cx| {
                view.selection = None;
                cx.emit(ThreadsArchiveViewEvent::Activate {
                    thread: self.thread.clone(),
                });
            })
            .log_err();
    }

    fn is_current_workspace(&self, workspace_id: WorkspaceId) -> bool {
        self.current_workspace_id == Some(workspace_id)
    }

    fn is_sibling_workspace(&self, workspace_id: WorkspaceId) -> bool {
        self.sibling_workspace_ids.contains(&workspace_id)
            && !self.is_current_workspace(workspace_id)
    }

    fn selected_match(&self) -> Option<&StringMatch> {
        match self.filtered_entries.get(self.selected_index)? {
            ProjectPickerEntry::Workspace(hit) => Some(hit),
            ProjectPickerEntry::Header(_) => None,
        }
    }

    fn open_local_folder(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let paths_receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths_receiver.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }

            let work_dirs = PathList::new(&paths);

            this.update(cx, |this, cx| {
                this.delegate
                    .update_working_directories_and_unarchive(work_dirs, cx);
                cx.emit(DismissEvent);
            })
            .log_err();
        })
        .detach();
    }
}

impl EventEmitter<DismissEvent> for ProjectPickerDelegate {}

impl PickerDelegate for ProjectPickerDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "thread archive project picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        format!(
            "Associate the \"{}\" thread with...",
            self.thread
                .title
                .as_ref()
                .map(|t| t.as_ref())
                .unwrap_or(DEFAULT_THREAD_TITLE)
        )
        .into()
    }

    fn match_count(&self) -> usize {
        self.filtered_entries.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn can_select(&self, ix: usize, _window: &mut Window, _cx: &mut Context<Picker<Self>>) -> bool {
        matches!(
            self.filtered_entries.get(ix),
            Some(ProjectPickerEntry::Workspace(_))
        )
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim_start();
        let smart_case = query.chars().any(|c| c.is_uppercase());
        let is_empty_query = query.is_empty();

        let sibling_candidates: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, workspace)| self.is_sibling_workspace(workspace.workspace_id))
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

        let mut sibling_matches = gpui::block_on(fuzzy::match_strings(
            &sibling_candidates,
            query,
            smart_case,
            true,
            100,
            &Default::default(),
            cx.background_executor().clone(),
        ));

        sibling_matches.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.candidate_id.cmp(&b.candidate_id))
        });

        let recent_candidates: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, workspace)| {
                !self.is_current_workspace(workspace.workspace_id)
                    && !self.is_sibling_workspace(workspace.workspace_id)
            })
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

        let mut recent_matches = gpui::block_on(fuzzy::match_strings(
            &recent_candidates,
            query,
            smart_case,
            true,
            100,
            &Default::default(),
            cx.background_executor().clone(),
        ));

        recent_matches.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.candidate_id.cmp(&b.candidate_id))
        });

        let mut entries = Vec::new();

        let has_siblings_to_show = if is_empty_query {
            !sibling_candidates.is_empty()
        } else {
            !sibling_matches.is_empty()
        };

        if has_siblings_to_show {
            entries.push(ProjectPickerEntry::Header("This Window".into()));

            if is_empty_query {
                for (id, workspace) in self.workspaces.iter().enumerate() {
                    if self.is_sibling_workspace(workspace.workspace_id) {
                        entries.push(ProjectPickerEntry::Workspace(StringMatch {
                            candidate_id: id,
                            score: 0.0,
                            positions: Vec::new(),
                            string: String::new(),
                        }));
                    }
                }
            } else {
                for m in sibling_matches {
                    entries.push(ProjectPickerEntry::Workspace(m));
                }
            }
        }

        let has_recent_to_show = if is_empty_query {
            !recent_candidates.is_empty()
        } else {
            !recent_matches.is_empty()
        };

        if has_recent_to_show {
            entries.push(ProjectPickerEntry::Header("Recent Projects".into()));

            if is_empty_query {
                for (id, workspace) in self.workspaces.iter().enumerate() {
                    if !self.is_current_workspace(workspace.workspace_id)
                        && !self.is_sibling_workspace(workspace.workspace_id)
                    {
                        entries.push(ProjectPickerEntry::Workspace(StringMatch {
                            candidate_id: id,
                            score: 0.0,
                            positions: Vec::new(),
                            string: String::new(),
                        }));
                    }
                }
            } else {
                for m in recent_matches {
                    entries.push(ProjectPickerEntry::Workspace(m));
                }
            }
        }

        self.filtered_entries = entries;

        self.selected_index = self
            .filtered_entries
            .iter()
            .position(|e| matches!(e, ProjectPickerEntry::Workspace(_)))
            .unwrap_or(0);

        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let candidate_id = match self.filtered_entries.get(self.selected_index) {
            Some(ProjectPickerEntry::Workspace(hit)) => hit.candidate_id,
            _ => return,
        };
        let Some(workspace) = self.workspaces.get(candidate_id) else {
            return;
        };

        self.update_working_directories_and_unarchive(workspace.paths.clone(), cx);
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        let text = if self.workspaces.is_empty() {
            "No recent projects found"
        } else {
            "No matches"
        };
        Some(text.into())
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        match self.filtered_entries.get(ix)? {
            ProjectPickerEntry::Header(title) => Some(
                v_flex()
                    .w_full()
                    .gap_1()
                    .when(ix > 0, |this| this.mt_1().child(Separator::horizontal()))
                    .child(
                        KitLabel::new(title.clone())
                            .text_xs()
                            .font_semibold()
                            .px_3()
                            .py_1()
                            .text_color(cx.theme().colors().text_muted),
                    )
                    .into_any_element(),
            ),
            ProjectPickerEntry::Workspace(hit) => {
                let workspace = self.workspaces.get(hit.candidate_id)?;
                let location = &workspace.location;

                let ordered_paths: Vec<_> = workspace
                    .identity_paths
                    .ordered_paths()
                    .map(|p| p.compact().to_string_lossy().to_string())
                    .collect();

                let tooltip_path: SharedString = ordered_paths.join("\n").into();

                let mut path_start_offset = 0;
                let match_labels: Vec<_> = workspace
                    .identity_paths
                    .ordered_paths()
                    .map(|p| p.compact())
                    .map(|path| {
                        let path_string = path.to_string_lossy();
                        let path_text = path_string.to_string();
                        let path_byte_len = path_text.len();

                        let path_positions: Vec<usize> = hit
                            .positions
                            .iter()
                            .copied()
                            .skip_while(|pos| *pos < path_start_offset)
                            .take_while(|pos| *pos < path_start_offset + path_byte_len)
                            .map(|pos| pos - path_start_offset)
                            .collect();

                        let file_name_match = path.file_name().map(|file_name| {
                            let file_name_text = file_name.to_string_lossy().into_owned();
                            let file_name_start = path_byte_len - file_name_text.len();
                            let highlight_positions: Vec<usize> = path_positions
                                .iter()
                                .copied()
                                .skip_while(|pos| *pos < file_name_start)
                                .take_while(|pos| *pos < file_name_start + file_name_text.len())
                                .map(|pos| pos - file_name_start)
                                .collect();
                            HighlightedMatch {
                                text: file_name_text,
                                highlight_positions,
                                color: Color::Default,
                            }
                        });

                        path_start_offset += path_byte_len;
                        file_name_match
                    })
                    .collect();

                let highlighted_match = HighlightedMatchWithPaths {
                    prefix: match location {
                        SerializedWorkspaceLocation::Remote(options) => {
                            Some(SharedString::from(options.display_name()))
                        }
                        _ => None,
                    },
                    match_label: HighlightedMatch::join(match_labels.into_iter().flatten(), ", "),
                    paths: Vec::new(),
                    active: false,
                };

                Some(
                    KitListItem::new(ix)
                        .selected(selected)
                        .px_3()
                        .py_2()
                        .child(
                            h_flex()
                                .gap_3()
                                .flex_grow_1()
                                .child(highlighted_match.render(window, cx)),
                        )
                        .tooltip(move |window, cx| {
                            KitTooltip::new(tooltip_path.clone()).build(window, cx)
                        })
                        .into_any_element(),
                )
            }
        }
    }

    fn render_footer(&self, _: &mut Window, cx: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let has_selection = self.selected_match().is_some();

        Some(
            h_flex()
                .flex_1()
                .p_1p5()
                .gap_1()
                .justify_end()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    KitButton::new("open_local_folder")
                        .small()
                        .label("Choose Local Folder")
                        .tooltip_with_action(
                            "Choose Local Folder",
                            &workspace::Open::default(),
                            None,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.delegate.open_local_folder(window, cx);
                        })),
                )
                .child(
                    KitButton::new("select_project")
                        .primary()
                        .small()
                        .label("Select")
                        .disabled(!has_selection)
                        .tooltip_with_action("Select Project", &menu::Confirm, None)
                        .on_click(cx.listener(move |picker, _, window, cx| {
                            picker.delegate.confirm(false, window, cx);
                        })),
                )
                .into_any(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match_positions_returns_byte_indices() {
        // "🔥abc" — the fire emoji is 4 bytes, so 'a' starts at byte 4, 'b' at 5, 'c' at 6.
        let text = "🔥abc";
        let positions = fuzzy_match_positions("ab", text).expect("should match");
        assert_eq!(positions, vec![4, 5]);

        // Verify positions are valid char boundaries (this is the assertion that
        // panicked before the fix).
        for &pos in &positions {
            assert!(
                text.is_char_boundary(pos),
                "position {pos} is not a valid UTF-8 boundary in {text:?}"
            );
        }
    }

    #[test]
    fn test_fuzzy_match_positions_ascii_still_works() {
        let positions = fuzzy_match_positions("he", "hello").expect("should match");
        assert_eq!(positions, vec![0, 1]);
    }

    #[test]
    fn test_fuzzy_match_positions_case_insensitive() {
        let positions = fuzzy_match_positions("HE", "hello").expect("should match");
        assert_eq!(positions, vec![0, 1]);
    }

    #[test]
    fn test_fuzzy_match_positions_no_match() {
        assert!(fuzzy_match_positions("xyz", "hello").is_none());
    }

    #[test]
    fn test_fuzzy_match_positions_multi_byte_interior() {
        // "café" — 'é' is 2 bytes (0xC3 0xA9), so 'f' starts at byte 4, 'é' at byte 5.
        let text = "café";
        let positions = fuzzy_match_positions("fé", text).expect("should match");
        // 'c'=0, 'a'=1, 'f'=2, 'é'=3..4 — wait, let's verify:
        // Actually: c=1 byte, a=1 byte, f=1 byte, é=2 bytes
        // So byte positions: c=0, a=1, f=2, é=3
        assert_eq!(positions, vec![2, 3]);
        for &pos in &positions {
            assert!(
                text.is_char_boundary(pos),
                "position {pos} is not a valid UTF-8 boundary in {text:?}"
            );
        }
    }
}
