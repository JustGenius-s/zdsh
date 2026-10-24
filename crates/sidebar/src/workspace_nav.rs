use std::{
    collections::{HashMap, HashSet},
    mem,
    time::Duration,
};

use action_log::DiffStats;
use agent_ui::{AgentPanel, ThreadId};
use db::kvp::KeyValueStore;
use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Context, DismissEvent, Entity, EntityId,
    Focusable as _, Hsla, IntoElement, MouseButton, ParentElement, Render, SharedString, Styled,
    Subscription, Task, WeakEntity, Window, div, ease_out_quint, prelude::*, px, rgb,
};
use gpui_icons::IconName;
use gpui_kit::base::Popover;
use gpui_kit::component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    tooltip::Tooltip as KitTooltip,
    v_flex,
};
use project::{ProjectPath, git_store::GitStore};
use project_panel::ProjectPanel;
use theme::{ActiveTheme, Appearance, UiTextSize, UiTextTone, UiTypography};
use ui::{Color, rems_from_px};
use util::ResultExt as _;
use workspace::dock::Dock;
use workspace::notifications::NotifyTaskExt as _;
use workspace::{ItemHandle, MultiWorkspace, Workspace, WorkspaceNavigation, item::TabCategory};

use crate::sidebar_row::{
    GROUP_GAP, LIST_INSET, ROW_MIN_HEIGHT, ROW_STACK_GAP, SidebarRow, StatusDot, fading_label,
    file_icon, glyph, glyph_button,
};
use crate::{ProjectThread, Sidebar};

/// Space before a group's title, lining it up with the icons of the rows below it.
const GROUP_LABEL_PADDING_START: f32 = 9.;
/// Space after a group's title and buttons: the rows' border and padding, so the buttons end
/// where the rows' content does.
const GROUP_LABEL_PADDING_END: f32 = 7.;
const HEADER_HEIGHT: f32 = ROW_MIN_HEIGHT;
const NAV_WIDTH: f32 = 272.;
const FILES_WIDTH: f32 = 264.;
const FILES_ENTER_DURATION: Duration = Duration::from_millis(240);
/// How many threads the sidebar lists before a More row, as many as Cursor's sidebar does.
const THREADS_SHOWN: usize = 8;
/// The statuses threads can be filtered by, in the order the Status ordering lists them.
const THREAD_STATUSES: [(StatusDot, &str); 6] = [
    (StatusDot::WaitingForConfirmation, "Needs Attention"),
    (StatusDot::Error, "Error"),
    (StatusDot::Unread, "Unread"),
    (StatusDot::Running, "Working"),
    (StatusDot::Draft, "Draft"),
    (StatusDot::Read, "Done"),
];

/// The sidebar at a workspace's left edge, showing that workspace's threads, files, changes and
/// terminals.
pub struct WorkspaceNav {
    workspace: WeakEntity<Workspace>,
    /// The window's thread list, which already tracks every project's threads and their status.
    threads: Option<Entity<Sidebar>>,
    needed_attention: bool,
    /// The project's file tree, borrowed from the left dock and shown at the content block's
    /// right edge.
    project_files: Option<Entity<ProjectPanel>>,
    files: FilesState,
    /// Counts openings of the file tree, so each one replays its entrance.
    files_openings: usize,
    collapsed_sections: Vec<Section>,
    thread_order: ThreadOrder,
    show_thread_times: bool,
    /// The statuses whose threads are listed, or every status when empty.
    status_filter: Vec<StatusDot>,
    show_all_threads: bool,
    show_archived: bool,
    changes_tree: bool,
    collapsed_change_folders: HashSet<String>,
    pinned_threads: Vec<ThreadId>,
    pin_storage_key: Option<String>,
    pin_save_task: Task<()>,
    renaming_thread: Option<ThreadId>,
    rename_input: Entity<InputState>,
    pane_observations: HashMap<EntityId, (Subscription, Subscription)>,
    _threads_subscription: Option<Subscription>,
    _project_files_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilesState {
    Closed,
    /// Floats over the center and closes once a file opens.
    Floating,
    /// Takes a column of its own and stays open.
    Pinned,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Pinned,
    Archived,
    Threads,
    Files,
    Changes,
    Terminals,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThreadOrder {
    Updated,
    /// Threads that need the user first, then by recency within each status.
    Status,
}

struct FileRow {
    path: ProjectPath,
    action: FileRowAction,
    diff_stats: Option<DiffStats>,
    status: Option<(&'static str, Color)>,
    depth: usize,
}

struct ChangeColors {
    added: Hsla,
    deleted: Hsla,
    modified: Hsla,
    untracked: Hsla,
}

fn change_colors(cx: &App) -> ChangeColors {
    let (added, deleted, modified, untracked) = match cx.theme().appearance() {
        Appearance::Dark => (0x70b489, 0xfc6b83, 0xf1b467, 0x88c0d0),
        Appearance::Light => (0x007041, 0xbe1744, 0xa46700, 0x176c74),
    };
    ChangeColors {
        added: rgb(added).into(),
        deleted: rgb(deleted).into(),
        modified: rgb(modified).into(),
        untracked: rgb(untracked).into(),
    }
}

enum FileRowAction {
    OpenFile(Box<dyn ItemHandle>),
    OpenDiff(Box<dyn Fn(&mut Window, &mut App)>),
}

impl WorkspaceNav {
    /// Creates the sidebar for `workspace` and puts it at the workspace's left edge.
    pub fn install(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let workspace_handle = cx.entity();
        // The workspace is mid-update here, so the sidebar can't read it yet.
        let git_store = workspace.project().read(cx).git_store().clone();
        let left_dock = workspace.left_dock().clone();
        let group = workspace.project_group_key(cx);
        let pin_storage_key = serde_json::to_string(&(group.path_list().paths(), group.host()))
            .log_err()
            .map(|key| format!("workspace-nav-pinned-threads:{key}"));
        let nav = cx.new(|cx| {
            Self::new(
                &workspace_handle,
                &git_store,
                &left_dock,
                pin_storage_key,
                window,
                cx,
            )
        });
        let project_files = cx.new(|_| ProjectFilesSide {
            nav: nav.downgrade(),
        });
        workspace.set_navigation(
            WorkspaceNavigation {
                view: nav.clone().into(),
                trailing_view: project_files.into(),
                needs_attention: Box::new(move |cx| nav.read(cx).needs_attention(cx)),
            },
            cx,
        );
    }

    fn new(
        workspace: &Entity<Workspace>,
        git_store: &Entity<GitStore>,
        left_dock: &Entity<Dock>,
        pin_storage_key: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pinned_threads = pin_storage_key
            .as_ref()
            .and_then(|key| KeyValueStore::global(cx).read_kvp(key).log_err().flatten())
            .and_then(|value| serde_json::from_str(&value).log_err())
            .unwrap_or_default();
        let rename_input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.observe(workspace, |_, _, cx| cx.notify()),
            cx.subscribe(git_store, |_, _, _, cx| cx.notify()),
            cx.observe_in(left_dock, window, Self::take_project_files_from_dock),
            cx.subscribe_in(&rename_input, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.finish_thread_rename(window, cx);
                }
            }),
        ];
        cx.on_release(|this, _| mem::replace(&mut this.pin_save_task, Task::ready(())).detach())
            .detach();
        Self {
            workspace: workspace.downgrade(),
            threads: None,
            needed_attention: false,
            project_files: None,
            files: FilesState::Closed,
            files_openings: 0,
            collapsed_sections: Vec::new(),
            thread_order: ThreadOrder::Updated,
            show_thread_times: true,
            status_filter: Vec::new(),
            show_all_threads: false,
            show_archived: false,
            changes_tree: false,
            collapsed_change_folders: HashSet::new(),
            pinned_threads,
            pin_storage_key,
            pin_save_task: Task::ready(()),
            renaming_thread: None,
            rename_input,
            pane_observations: HashMap::new(),
            _threads_subscription: None,
            _project_files_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    fn connect_panes(&mut self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        let panes = workspace.read(cx).panes().to_vec();
        self.pane_observations
            .retain(|id, _| panes.iter().any(|pane| pane.entity_id() == *id));
        for pane in panes {
            self.pane_observations
                .entry(pane.entity_id())
                .or_insert_with(|| {
                    let observation = cx.observe(&pane, |_, _, cx| cx.notify());
                    let subscription = cx.subscribe(&pane, |this, pane, event, cx| {
                        if !matches!(
                            event,
                            workspace::pane::Event::ActivateItem { local: true, .. }
                        ) {
                            return;
                        }
                        let thread_id = this.workspace.upgrade().and_then(|workspace| {
                            let item = pane.read(cx).active_item()?;
                            workspace
                                .read(cx)
                                .panel::<AgentPanel>(cx)?
                                .read(cx)
                                .thread_id_for_item(item.as_ref())
                        });
                        if let Some(thread_id) = thread_id
                            && let Some(threads) = &this.threads
                        {
                            threads.update(cx, |threads, cx| {
                                threads.set_project_thread_read(thread_id, true, cx)
                            });
                        }
                    });
                    (observation, subscription)
                });
        }
    }

    fn toggle_thread_pin(&mut self, thread_id: ThreadId, cx: &mut Context<Self>) {
        if self.pinned_threads.contains(&thread_id) {
            self.pinned_threads.retain(|id| *id != thread_id);
        } else {
            self.pinned_threads.push(thread_id);
        }
        if let Some(key) = self.pin_storage_key.clone()
            && let Some(value) = serde_json::to_string(&self.pinned_threads).log_err()
        {
            let previous = mem::replace(&mut self.pin_save_task, Task::ready(()));
            let database = KeyValueStore::global(cx);
            let workspace = self.workspace.clone();
            self.pin_save_task = cx.spawn(async move |_, cx| {
                previous.await;
                if let Err(error) = database.write_kvp(key, value).await {
                    log::error!("Saving pinned threads: {error:#}");
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .show_error(format!("Could not save pinned threads: {error}"), cx)
                        })
                        .log_err();
                }
            });
        }
        cx.notify();
    }

    fn pinned_tab_items(&self, cx: &App) -> Vec<Box<dyn ItemHandle>> {
        let mut items = Vec::new();
        if let Some(workspace) = self.workspace.upgrade() {
            for pane in workspace.read(cx).panes() {
                let pane = pane.read(cx);
                items.extend(
                    pane.items()
                        .take(pane.pinned_count())
                        .map(|item| item.boxed_clone()),
                );
            }
        }
        items
    }

    fn item_is_pinned(&self, item_id: EntityId, cx: &App) -> bool {
        self.pinned_tab_items(cx)
            .iter()
            .any(|item| item.item_id() == item_id)
    }

    fn toggle_item_pin(&mut self, item_id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).pane_for_item_id(item_id));
        if let Some(pane) = pane {
            pane.update(cx, |pane, cx| pane.toggle_item_pin(item_id, window, cx));
        }
        cx.notify();
    }

    fn tab_pin_button(&self, item_id: EntityId, cx: &Context<Self>) -> Button {
        let pinned = self.item_is_pinned(item_id, cx);
        glyph_button(
            "pin-item",
            if pinned {
                IconName::PinSlash
            } else {
                IconName::Pin
            },
            cx,
        )
        .tooltip(if pinned { "Unpin" } else { "Pin" })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.toggle_item_pin(item_id, window, cx);
        }))
    }

    fn thread_pin_button(&self, thread_id: ThreadId, cx: &Context<Self>) -> Button {
        let pinned = self.pinned_threads.contains(&thread_id);
        glyph_button(
            "pin-thread",
            if pinned {
                IconName::PinSlash
            } else {
                IconName::Pin
            },
            cx,
        )
        .tooltip(if pinned { "Unpin" } else { "Pin" })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_thread_pin(thread_id, cx);
        }))
    }

    fn start_thread_rename(
        &mut self,
        thread_id: ThreadId,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_thread_rename(window, cx);
        self.renaming_thread = Some(thread_id);
        self.rename_input.update(cx, |input, cx| {
            input.set_value(title, window, cx);
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn finish_thread_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.renaming_thread.take() else {
            return;
        };
        let title = self.rename_input.read(cx).value().trim().to_string();
        if !title.is_empty()
            && let Some(threads) = &self.threads
        {
            threads.update(cx, |threads, cx| {
                threads.apply_thread_rename(thread_id, title.into(), window, cx)
            });
        }
        cx.notify();
    }

    /// The file tree lives in the left dock, which actions such as revealing a file open. The
    /// sidebar shows the tree itself instead, so it closes the dock and opens the tree at the
    /// content block's right edge.
    fn take_project_files_from_dock(
        &mut self,
        left_dock: Entity<Dock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock = left_dock.read(cx);
        let Some(project_files) = dock.panel::<ProjectPanel>() else {
            return;
        };
        let shows_project_files = dock.is_open()
            && dock
                .visible_panel()
                .is_some_and(|panel| panel.panel_id() == project_files.entity_id());
        if !shows_project_files {
            return;
        }
        // The dock is mid-update while it notifies, so close it once that is done.
        cx.defer_in(window, move |this, window, cx| {
            left_dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
            this.open_project_files(window, cx);
        });
    }

    fn connect_project_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ProjectPanel>> {
        if let Some(project_files) = &self.project_files {
            return Some(project_files.clone());
        }
        let project_files = self
            .workspace
            .upgrade()?
            .read(cx)
            .panel::<ProjectPanel>(cx)?;
        self._project_files_subscription = Some(cx.subscribe_in(
            &project_files,
            window,
            |this, _, event: &project_panel::Event, window, cx| match event {
                project_panel::Event::OpenedEntry { .. }
                | project_panel::Event::SplitEntry { .. } => {
                    if this.files == FilesState::Floating {
                        this.close_project_files(window, cx);
                    }
                }
                project_panel::Event::Focus => {}
            },
        ));
        self.project_files = Some(project_files.clone());
        Some(project_files)
    }

    fn open_project_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_files) = self.connect_project_files(window, cx) else {
            return;
        };
        if self.files == FilesState::Closed {
            self.files = FilesState::Floating;
            self.files_openings += 1;
        }
        window.focus(&project_files.focus_handle(cx), cx);
        cx.notify();
    }

    fn close_project_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files == FilesState::Closed {
            return;
        }
        self.files = FilesState::Closed;
        if let Some(workspace) = self.workspace.upgrade() {
            let pane_focus = workspace.read(cx).active_pane().focus_handle(cx);
            window.focus(&pane_focus, cx);
        }
        cx.notify();
    }

    fn toggle_project_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files == FilesState::Closed {
            self.open_project_files(window, cx);
        } else {
            self.close_project_files(window, cx);
        }
    }

    fn toggle_files_pinned(&mut self, cx: &mut Context<Self>) {
        self.files = match self.files {
            FilesState::Pinned => FilesState::Floating,
            FilesState::Floating | FilesState::Closed => FilesState::Pinned,
        };
        cx.notify();
    }

    fn render_project_files(
        &self,
        project_files: Entity<ProjectPanel>,
        workspace: &Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let pinned = self.files == FilesState::Pinned;
        let name = workspace
            .read(cx)
            .project_group_key(cx)
            .display_name(&Default::default());
        v_flex()
            .id("workspace-project-files")
            .w(rems_from_px(FILES_WIDTH))
            .h_full()
            .flex_none()
            .pt(rems_from_px(LIST_INSET))
            .bg(colors.panel_background)
            .border_l_1()
            .border_color(colors.border_variant)
            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                this.close_project_files(window, cx)
            }))
            .child(
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
                            .child(div().flex_none().text_color(colors.text).child("Files"))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(colors.text_muted)
                                    .child(fading_label(name)),
                            )
                            .child(
                                glyph_button("pin-project-files", IconName::Pin, cx)
                                    .tooltip(if pinned { "Unpin" } else { "Pin as a Column" })
                                    .selected(pinned)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_files_pinned(cx)),
                                    ),
                            )
                            .child(
                                glyph_button("close-project-files", IconName::X, cx)
                                    .tooltip("Close")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_project_files(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div().w_full().pb(rems_from_px(6_f32)).child(
                    SidebarRow::new(
                        "filter-project-files",
                        glyph(IconName::MagnifyingGlass).text_color(colors.icon_muted),
                        div()
                            .w_full()
                            .text_color(colors.text_placeholder)
                            .child(fading_label("Find a File…")),
                    )
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(zed_actions::unified_search::Toggle {
                                tab: Some(zed_actions::unified_search::SearchTab::Files),
                            }),
                            cx,
                        )
                    }),
                ),
            )
            .child(div().flex_1().min_h_0().child(project_files))
            .into_any_element()
    }

    /// Nothing while the tree is closed, a column of its own while pinned, and otherwise the
    /// tree floating over the center from the content block's right edge.
    fn render_project_files_side(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        let project_files = (self.files != FilesState::Closed)
            .then(|| self.connect_project_files(window, cx))
            .flatten()
            .map(|project_files| self.render_project_files(project_files, &workspace, cx));
        match project_files {
            Some(project_files) if self.files == FilesState::Floating => div()
                .flex_none()
                .h_full()
                .child(
                    // Drawn after the center and before the content block's rounded edge, so it
                    // floats over the center and keeps the block's corners.
                    div()
                        .id("floating-project-files")
                        .occlude()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .shadow_lg()
                        .child(project_files)
                        .with_animation(
                            ("project-files-enter", self.files_openings),
                            Animation::new(FILES_ENTER_DURATION).with_easing(ease_out_quint()),
                            |files, delta| files.opacity(delta).mr(px(-16. * (1. - delta))),
                        ),
                )
                .into_any_element(),
            Some(project_files) => project_files,
            None => div().into_any_element(),
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

    fn render_header(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let key = workspace.read(cx).project_group_key(cx);
        let nav = cx.entity().downgrade();
        let customized = self.thread_order != ThreadOrder::Updated
            || !self.show_thread_times
            || self.show_archived
            || !self.status_filter.is_empty();

        // Like Cursor's collection label: the base size in a muted color, its start lined up
        // with the section icons below, and its buttons over the rows' action column.
        h_flex()
            .w_full()
            .min_h(rems_from_px(HEADER_HEIGHT))
            .px(rems_from_px(LIST_INSET))
            .pb(rems_from_px(ROW_STACK_GAP))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rems_from_px(8_f32))
                    .pl(rems_from_px(GROUP_LABEL_PADDING_START))
                    .pr(rems_from_px(GROUP_LABEL_PADDING_END))
                    .ui_text(UiTextSize::Body, UiTextTone::Muted, cx)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(fading_label(key.display_name(&Default::default()))),
                    )
                    .child(
                        glyph_button("customize-sidebar", IconName::ListFilter, cx)
                            .tooltip("Customize Sidebar")
                            .selected(customized)
                            .dropdown_menu(move |menu, window, cx| {
                                Self::customize_menu(nav.clone(), menu, window, cx)
                            }),
                    )
                    .child(
                        glyph_button("workspace-search", IconName::MagnifyingGlass, cx)
                            .tooltip("Search")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(zed_actions::unified_search::Toggle::default()),
                                    cx,
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The menu behind the header's filter button, laid out like Cursor's Customize Sidebar menu.
    fn customize_menu(
        nav: WeakEntity<Self>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(this) = nav.upgrade() else {
            return menu;
        };
        let (thread_order, show_thread_times, status_filter, show_archived) = {
            let this = this.read(cx);
            (
                this.thread_order,
                this.show_thread_times,
                this.status_filter.clone(),
                this.show_archived,
            )
        };
        workspace::style_menu(menu, window)
            .submenu("Ordering", window, cx, {
                let nav = nav.clone();
                move |menu, window, _| {
                    let menu = workspace::style_submenu(menu, window);
                    [
                        (ThreadOrder::Updated, "Updated"),
                        (ThreadOrder::Status, "Status"),
                    ]
                    .into_iter()
                    .fold(menu, |menu, (order, label)| {
                        let nav = nav.clone();
                        menu.item(
                            PopupMenuItem::new(label)
                                .checked(thread_order == order)
                                .on_click(move |_, _, cx| {
                                    nav.update(cx, |this, cx| {
                                        this.thread_order = order;
                                        cx.notify();
                                    })
                                    .log_err();
                                }),
                        )
                    })
                }
            })
            .submenu("Show", window, cx, {
                let nav = nav.clone();
                move |menu, window, _| {
                    let menu = workspace::style_submenu(menu, window);
                    let nav = nav.clone();
                    let archived_nav = nav.clone();
                    menu.item(
                        PopupMenuItem::new("Updated")
                            .checked(show_thread_times)
                            .on_click(move |_, _, cx| {
                                nav.update(cx, |this, cx| {
                                    this.show_thread_times = !this.show_thread_times;
                                    cx.notify();
                                })
                                .log_err();
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Archived")
                            .checked(show_archived)
                            .on_click(move |_, _, cx| {
                                archived_nav
                                    .update(cx, |this, cx| {
                                        this.show_archived = !this.show_archived;
                                        cx.notify();
                                    })
                                    .log_err();
                            }),
                    )
                }
            })
            .separator()
            .label("Filters")
            .submenu("Status", window, cx, {
                let nav = nav.clone();
                move |menu, window, cx| {
                    let menu = workspace::style_submenu(menu, window);
                    THREAD_STATUSES
                        .into_iter()
                        .fold(menu, |menu, (status, label)| {
                            let nav = nav.clone();
                            menu.item(
                                PopupMenuItem::new(label)
                                    .icon(status.menu_icon(cx))
                                    .checked(status_filter.contains(&status))
                                    .on_click(move |_, _, cx| {
                                        nav.update(cx, |this, cx| {
                                            this.toggle_status_filter(status, cx)
                                        })
                                        .log_err();
                                    }),
                            )
                        })
                }
            })
            .separator()
            .item(
                PopupMenuItem::new("Mark All as Read").on_click(move |_, _, cx| {
                    nav.update(cx, |this, cx| this.mark_threads_read(cx))
                        .log_err();
                }),
            )
    }

    fn toggle_status_filter(&mut self, status: StatusDot, cx: &mut Context<Self>) {
        if let Some(index) = self.status_filter.iter().position(|shown| *shown == status) {
            self.status_filter.remove(index);
        } else {
            self.status_filter.push(status);
        }
        cx.notify();
    }

    fn mark_threads_read(&mut self, cx: &mut Context<Self>) {
        let (Some(threads), Some(workspace)) = (self.threads.clone(), self.workspace.upgrade())
        else {
            return;
        };
        let key = workspace.read(cx).project_group_key(cx);
        threads.update(cx, |threads, cx| {
            threads.mark_project_threads_read(&key, cx)
        });
    }

    fn is_collapsed(&self, section: Section) -> bool {
        self.collapsed_sections.contains(&section)
    }

    fn toggle_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if let Some(index) = self
            .collapsed_sections
            .iter()
            .position(|collapsed| *collapsed == section)
        {
            self.collapsed_sections.remove(index);
        } else {
            self.collapsed_sections.push(section);
        }
        cx.notify();
    }

    /// A section's icon and title in the rows' icon and title columns, like a Cursor group
    /// label. Clicking it folds the section; its buttons show while it is hovered.
    fn render_section_label(
        &self,
        section: Section,
        icon: IconName,
        title: &'static str,
        buttons: Vec<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let id = match section {
            Section::Pinned => "pinned",
            Section::Archived => "archived",
            Section::Threads => "threads",
            Section::Files => "files",
            Section::Changes => "changes",
            Section::Terminals => "terminals",
        };
        let group = SharedString::from(format!("workspace-nav-{id}"));
        let chevron = if self.is_collapsed(section) {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        SidebarRow::new(
            SharedString::from(format!("nav-section-{id}")),
            glyph(icon).text_color(colors.text_muted),
            div()
                .ui_text(UiTextSize::Body, UiTextTone::Muted, cx)
                .w_full()
                .child(fading_label(title)),
        )
        .leading_on_hover(glyph(chevron).text_color(colors.text_muted))
        .group(group.clone())
        .end(
            h_flex()
                .invisible()
                .group_hover(group, |style| style.visible())
                // A button's click shouldn't also fold the section.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(buttons),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
        .into_any_element()
    }

    /// What an expanded section shows when it has no rows, like Cursor's "No agents yet": muted
    /// text in the rows' title column.
    fn render_placeholder(&self, id: &'static str, text: &'static str, cx: &App) -> AnyElement {
        SidebarRow::new(
            id,
            div(),
            div()
                .w_full()
                .ui_text(UiTextSize::Body, UiTextTone::Muted, cx)
                .child(fading_label(text)),
        )
        .into_any_element()
    }

    fn render_thread_row(
        &self,
        thread: ProjectThread,
        active_thread_id: Option<ThreadId>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let thread_id = thread.thread_id;
        let selected = active_thread_id == Some(thread_id);
        let group: SharedString = format!("thread-row-{}", thread_id.to_key_string()).into();
        let nav = cx.weak_entity();
        let menu_thread = thread.clone();
        let title = thread.title.clone();
        let renaming = self.renaming_thread == Some(thread_id);
        let title_element = if renaming {
            div()
                .w_full()
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.renaming_thread = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Input::new(&self.rename_input)
                        .small()
                        .appearance(false)
                        .bordered(false)
                        .ui_text(UiTextSize::Body, UiTextTone::Primary, cx)
                        .w_full(),
                )
                .into_any_element()
        } else {
            fading_label(title.clone()).into_any_element()
        };
        let leading = if thread.archived {
            glyph(IconName::Archive)
                .text_color(cx.theme().colors().icon_muted)
                .into_any_element()
        } else {
            div()
                .when(thread.status == StatusDot::Unread, |status| {
                    status
                        .opacity(if selected { 1. } else { 0.88 })
                        .group_hover(group.clone(), |style| style.opacity(1.))
                })
                .child(thread.status)
                .into_any_element()
        };
        let end = if thread.status == StatusDot::WaitingForConfirmation {
            Some(
                div()
                    .text_color(StatusDot::WaitingForConfirmation.color(cx))
                    .child("Approve"),
            )
        } else {
            thread
                .updated
                .filter(|_| self.show_thread_times)
                .map(|updated| {
                    div()
                        .ui_text(UiTextSize::Body, UiTextTone::Muted, cx)
                        .child(updated)
                })
        };
        let actions = h_flex()
            .gap(rems_from_px(2_f32))
            .when(
                !thread.archived || self.pinned_threads.contains(&thread_id),
                |actions| actions.child(self.thread_pin_button(thread_id, cx)),
            )
            .when(thread.can_archive, |actions| {
                actions.child(
                    glyph_button("archive-thread", IconName::Archive, cx)
                        .tooltip("Archive Thread")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if this.pinned_threads.contains(&thread_id) {
                                this.toggle_thread_pin(thread_id, cx);
                            }
                            if let Some(threads) = &this.threads {
                                threads.update(cx, |threads, cx| {
                                    threads.archive_project_thread(thread_id, window, cx)
                                });
                            }
                        })),
                )
            });
        SidebarRow::new(
            SharedString::from(format!("nav-thread-{}", thread_id.to_key_string())),
            leading,
            title_element,
        )
        .group(group)
        .when_some(end, |row, end| row.end(end))
        .actions_on_hover(
            actions,
            usize::from(!thread.archived || self.pinned_threads.contains(&thread_id))
                + usize::from(thread.can_archive),
        )
        .selected(selected)
        .tooltip(move |window, cx| KitTooltip::new(title.clone()).build(window, cx))
        .context_menu(move |menu, window, cx| {
            Self::thread_menu(nav.clone(), menu_thread.clone(), menu, window, cx)
        })
        .when(!renaming, |row| {
            row.on_click(cx.listener(move |this, _, window, cx| {
                if let Some(threads) = &this.threads {
                    threads.update(cx, |threads, cx| {
                        threads.activate_project_thread_id(thread_id, window, cx)
                    });
                }
            }))
        })
        .into_any_element()
    }

    fn thread_menu(
        nav: WeakEntity<Self>,
        thread: ProjectThread,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(this) = nav.upgrade() else {
            return menu;
        };
        let thread_id = thread.thread_id;
        let pinned = this.read(cx).pinned_threads.contains(&thread_id);
        let pin_nav = nav.clone();
        let rename_nav = nav.clone();
        let read_nav = nav.clone();
        let fork_nav = nav.clone();
        let copy_nav = nav.clone();
        let title = thread.title.clone();
        let archive_label = if thread.archived {
            "Restore"
        } else {
            "Archive"
        };
        workspace::style_menu(menu, window)
            .item(
                PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" })
                    .icon(workspace::menu_icon(if pinned {
                        IconName::PinSlash
                    } else {
                        IconName::Pin
                    }))
                    .disabled(thread.archived && !pinned)
                    .on_click(move |_, _, cx| {
                        pin_nav
                            .update(cx, |this, cx| this.toggle_thread_pin(thread_id, cx))
                            .log_err();
                    }),
            )
            .item(
                PopupMenuItem::new("Rename")
                    .icon(workspace::menu_icon(IconName::Pencil))
                    .on_click(move |_, window, cx| {
                        let nav = rename_nav.clone();
                        let title = title.clone();
                        window.defer(cx, move |window, cx| {
                            nav.update(cx, |this, cx| {
                                this.start_thread_rename(thread_id, title, window, cx)
                            })
                            .log_err();
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(if thread.is_unread {
                    "Mark as Read"
                } else {
                    "Mark as Unread"
                })
                .icon(workspace::menu_icon(if thread.is_unread {
                    IconName::BellSlash
                } else {
                    IconName::BellDot
                }))
                .disabled(thread.archived)
                .on_click(move |_, _, cx| {
                    read_nav
                        .update(cx, |this, cx| {
                            if let Some(threads) = &this.threads {
                                threads.update(cx, |threads, cx| {
                                    threads.set_project_thread_read(thread_id, thread.is_unread, cx)
                                });
                            }
                        })
                        .log_err();
                }),
            )
            .separator()
            .item(
                PopupMenuItem::new("Fork")
                    .icon(workspace::menu_icon(IconName::Fork))
                    .disabled(!thread.can_fork)
                    .on_click(move |_, window, cx| {
                        fork_nav
                            .update(cx, |this, cx| {
                                if let Some(workspace) = this.workspace.upgrade() {
                                    let panel = workspace.read(cx).panel::<AgentPanel>(cx);
                                    if let Some(panel) = panel {
                                        panel.update(cx, |panel, cx| {
                                            panel.fork_thread(thread_id, window, cx)
                                        });
                                    }
                                }
                            })
                            .log_err();
                    }),
            )
            .separator()
            .submenu_with_icon(
                Some(workspace::menu_icon(IconName::Copy)),
                "Copy",
                window,
                cx,
                move |menu, window, _| {
                    let title = thread.title.clone();
                    let copy_nav = copy_nav.clone();
                    workspace::style_submenu(menu, window)
                        .item(
                            PopupMenuItem::new("Copy Title")
                                .icon(workspace::menu_icon(IconName::TextT))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        title.to_string(),
                                    ))
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Copy Thread ID")
                                .icon(workspace::menu_icon(IconName::Copy))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        thread_id.to_key_string(),
                                    ))
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Copy Conversation")
                                .icon(workspace::menu_icon(IconName::FileText))
                                .disabled(!thread.can_copy)
                                .on_click(move |_, window, cx| {
                                    copy_nav
                                        .update(cx, |this, cx| {
                                            if let (Some(threads), Some(workspace)) =
                                                (&this.threads, this.workspace.upgrade())
                                            {
                                                threads.update(cx, |threads, cx| {
                                                    threads.copy_project_thread(
                                                        thread_id, &workspace, window, cx,
                                                    )
                                                });
                                            }
                                        })
                                        .log_err();
                                }),
                        )
                },
            )
            .separator()
            .item(
                PopupMenuItem::new(archive_label)
                    .icon(workspace::menu_icon(IconName::Archive))
                    .disabled(!thread.archived && !thread.can_archive)
                    .on_click(move |_, window, cx| {
                        nav.update(cx, |this, cx| {
                            if !thread.archived && this.pinned_threads.contains(&thread_id) {
                                this.toggle_thread_pin(thread_id, cx);
                            }
                            if let Some(threads) = &this.threads {
                                threads.update(cx, |threads, cx| {
                                    if thread.archived {
                                        threads.activate_project_thread_id(thread_id, window, cx);
                                    } else {
                                        threads.archive_project_thread(thread_id, window, cx);
                                    }
                                });
                            }
                        })
                        .log_err();
                    }),
            )
    }

    fn render_pinned(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let key = workspace.read(cx).project_group_key(cx);
        let active_item = workspace.read(cx).active_item(cx);
        let active_thread_id = active_item.as_ref().and_then(|item| {
            workspace
                .read(cx)
                .panel::<AgentPanel>(cx)?
                .read(cx)
                .thread_id_for_item(item.as_ref())
        });
        let active_path = active_item
            .as_ref()
            .filter(|item| item.tab_category(cx) == TabCategory::File)
            .and_then(|item| item.project_path(cx));
        let active_item_id = active_item.map(|item| item.item_id());
        let mut rows = Vec::new();
        if let Some(threads) = &self.threads {
            let threads = threads.read(cx).project_threads(&key, false, cx);
            for thread_id in &self.pinned_threads {
                if let Some(thread) = threads.iter().find(|thread| thread.thread_id == *thread_id) {
                    rows.push(self.render_thread_row(thread.clone(), active_thread_id, cx));
                }
            }
        }
        for row in self.open_files(workspace, cx) {
            if matches!(&row.action, FileRowAction::OpenFile(item) if self.item_is_pinned(item.item_id(), cx))
            {
                rows.push(
                    self.render_file_row("nav-file", row, active_path.as_ref(), cx)
                        .into_any_element(),
                );
            }
        }
        let terminals = self
            .pinned_tab_items(cx)
            .into_iter()
            .filter(|item| item.tab_category(cx) == TabCategory::Terminal)
            .collect();
        rows.extend(self.render_terminal_rows(terminals, active_item_id, cx));
        if rows.is_empty() {
            return div().into_any_element();
        }
        v_flex()
            .w_full()
            .mb(rems_from_px(GROUP_GAP))
            .child(self.render_section_label(
                Section::Pinned,
                IconName::Pin,
                "Pinned",
                Vec::new(),
                cx,
            ))
            .when(!self.is_collapsed(Section::Pinned), |group| {
                group.children(rows)
            })
            .into_any_element()
    }

    fn render_archived(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        if !self.show_archived {
            return div().into_any_element();
        }
        let key = workspace.read(cx).project_group_key(cx);
        let threads = self
            .threads
            .as_ref()
            .map(|threads| threads.read(cx).project_threads(&key, true, cx))
            .unwrap_or_default();
        let rows = threads
            .into_iter()
            .filter(|thread| thread.archived)
            .map(|thread| self.render_thread_row(thread, None, cx))
            .collect::<Vec<_>>();
        v_flex()
            .w_full()
            .mt(rems_from_px(GROUP_GAP))
            .child(self.render_section_label(
                Section::Archived,
                IconName::Archive,
                "Archived",
                Vec::new(),
                cx,
            ))
            .when(!self.is_collapsed(Section::Archived), |group| {
                group
                    .when(rows.is_empty(), |group| {
                        group.child(self.render_placeholder(
                            "nav-no-archived",
                            "No archived threads",
                            cx,
                        ))
                    })
                    .children(rows)
            })
            .into_any_element()
    }

    fn render_threads(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let key = workspace.read(cx).project_group_key(cx);
        let active_thread_id = workspace.read(cx).active_item(cx).and_then(|item| {
            workspace
                .read(cx)
                .panel::<AgentPanel>(cx)?
                .read(cx)
                .thread_id_for_item(item.as_ref())
        });
        let mut threads = self
            .threads
            .as_ref()
            .map(|threads| threads.read(cx).project_threads(&key, false, cx))
            .unwrap_or_default();
        let has_pinned_threads = threads
            .iter()
            .any(|thread| self.pinned_threads.contains(&thread.thread_id));
        threads.retain(|thread| !self.pinned_threads.contains(&thread.thread_id));
        if !self.status_filter.is_empty() {
            threads.retain(|thread| self.status_filter.contains(&thread.status));
        }
        if self.thread_order == ThreadOrder::Status {
            // The list comes most recent first, and the stable sort keeps that within a status.
            threads.sort_by_key(|thread| {
                THREAD_STATUSES
                    .iter()
                    .position(|(status, _)| *status == thread.status)
            });
        }
        let hidden_threads = if self.show_all_threads {
            0
        } else {
            threads.len().saturating_sub(THREADS_SHOWN)
        };
        threads.truncate(threads.len() - hidden_threads);

        let rows = threads
            .into_iter()
            .map(|thread| self.render_thread_row(thread, active_thread_id, cx))
            .collect::<Vec<_>>();
        let new_thread = glyph_button("new-thread", IconName::Plus, cx)
            .tooltip("New Thread")
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(zed_actions::agent::NewThread), cx)
            })
            .into_any_element();

        v_flex()
            .w_full()
            .child(self.render_section_label(
                Section::Threads,
                IconName::Agent,
                "Threads",
                vec![new_thread],
                cx,
            ))
            .when(!self.is_collapsed(Section::Threads), |section| {
                let placeholder = if self.status_filter.is_empty() && has_pinned_threads {
                    "No unpinned threads"
                } else if self.status_filter.is_empty() {
                    "No threads yet"
                } else {
                    "No threads match the filters"
                };
                section
                    .when(rows.is_empty(), |section| {
                        section.child(self.render_placeholder("nav-no-threads", placeholder, cx))
                    })
                    .children(rows)
                    .when(hidden_threads > 0, |section| {
                        section.child(
                            SidebarRow::new(
                                "nav-more-threads",
                                div(),
                                div()
                                    .ui_text(UiTextSize::Body, UiTextTone::Muted, cx)
                                    .w_full()
                                    .child(fading_label("More")),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_all_threads = true;
                                cx.notify();
                            })),
                        )
                    })
            })
            .into_any_element()
    }

    fn changed_files(&self, workspace: &Entity<Workspace>, cx: &App) -> Vec<FileRow> {
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
                            diff_stats: Some(existing),
                            ..
                        }) => {
                            existing.lines_added += stats.lines_added;
                            existing.lines_removed += stats.lines_removed;
                        }
                        Some(_) => {}
                        None => {
                            let old_buffer = diff.read(cx).base_text_buffer().clone();
                            let workspace = self.workspace.clone();
                            thread_changes.push(FileRow {
                                path,
                                action: FileRowAction::OpenDiff(Box::new(move |window, cx| {
                                    git_ui_core::file_diff_view::FileDiffView::open_or_focus_buffers(
                                        old_buffer.clone(), buffer.clone(), workspace.clone(), window, cx,
                                    ).detach_and_notify_err(workspace.clone(), window, cx);
                                })),
                                diff_stats: Some(stats),
                                status: None,
                                depth: 0,
                            });
                        }
                    }
                }
            }
        }

        let mut git_changes = Vec::new();
        for repository in workspace.project().read(cx).repositories(cx).values() {
            let repository = repository.read(cx);
            let status = cx.theme().status();
            let colors = change_colors(cx);
            for entry in repository.cached_status() {
                let Some(path) = repository.repo_path_to_project_path(&entry.repo_path, cx) else {
                    continue;
                };
                let (letter, color) = if entry.status.is_conflicted() {
                    ("!", Color::Custom(status.conflict))
                } else if entry.status.is_deleted() {
                    ("D", Color::Custom(colors.deleted))
                } else if entry.status.is_untracked() {
                    ("A", Color::Custom(colors.untracked))
                } else if entry.status.is_created() {
                    ("A", Color::Custom(colors.added))
                } else {
                    ("M", Color::Custom(colors.modified))
                };
                let Some(absolute_path) = workspace.project().read(cx).absolute_path(&path, cx)
                else {
                    continue;
                };
                git_changes.push(FileRow {
                    path,
                    action: FileRowAction::OpenDiff(Box::new(move |window, cx| {
                        window.dispatch_action(
                            Box::new(git::OpenFileDiff {
                                path: Some(absolute_path.clone()),
                            }),
                            cx,
                        );
                    })),
                    diff_stats: entry.diff_stat.map(|stats| DiffStats {
                        lines_added: stats.added,
                        lines_removed: stats.deleted,
                    }),
                    status: Some((letter, color)),
                    depth: 0,
                });
            }
        }

        for row in git_changes {
            if let Some(existing) = thread_changes
                .iter_mut()
                .find(|existing| existing.path == row.path)
            {
                *existing = row;
            } else {
                thread_changes.push(row);
            }
        }
        thread_changes.sort_by(|left, right| {
            left.path
                .worktree_id
                .to_usize()
                .cmp(&right.path.worktree_id.to_usize())
                .then_with(|| {
                    left.path
                        .path
                        .as_unix_str()
                        .cmp(right.path.path.as_unix_str())
                })
        });
        thread_changes
    }

    fn open_files(&self, workspace: &Entity<Workspace>, cx: &App) -> Vec<FileRow> {
        let mut open_files: Vec<FileRow> = Vec::new();
        for pane in workspace.read(cx).panes() {
            for item in pane.read(cx).items() {
                if item.tab_category(cx) != TabCategory::File {
                    continue;
                }
                let Some(path) = item.project_path(cx) else {
                    continue;
                };
                if let Some(row) = open_files.iter_mut().find(|row| row.path == path) {
                    if self.item_is_pinned(item.item_id(), cx) {
                        row.action = FileRowAction::OpenFile(item.boxed_clone());
                    }
                    continue;
                }
                open_files.push(FileRow {
                    path,
                    action: FileRowAction::OpenFile(item.boxed_clone()),
                    diff_stats: None,
                    status: None,
                    depth: 0,
                });
            }
        }
        open_files
    }

    /// `id_prefix` tells apart the rows of a file listed more than once, such as one that is
    /// both open and changed.
    fn render_file_row(
        &self,
        id_prefix: &str,
        row: FileRow,
        active_path: Option<&ProjectPath>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = change_colors(cx);
        let item_id = match &row.action {
            FileRowAction::OpenFile(item) => Some(item.item_id()),
            _ => None,
        };
        let is_diff = matches!(&row.action, FileRowAction::OpenDiff(_));
        let selected = active_path == Some(&row.path);
        let name: SharedString = row
            .path
            .path
            .file_name()
            .unwrap_or_default()
            .to_string()
            .into();
        let full_path: SharedString = row.path.path.as_unix_str().to_string().into();
        let end = (row.diff_stats.is_some() || row.status.is_some()).then(|| {
            h_flex()
                .gap(rems_from_px(4_f32))
                .ui_text(UiTextSize::Caption, UiTextTone::Muted, cx)
                .when_some(row.diff_stats, |end, stats| {
                    end.child(
                        div()
                            .text_color(colors.added)
                            .child(format!("+{}", stats.lines_added)),
                    )
                    .child(
                        div()
                            .text_color(colors.deleted)
                            .child(format!("−{}", stats.lines_removed)),
                    )
                })
                .when_some(row.status, |end, (letter, color)| {
                    end.child(div().text_color(color.color(cx)).child(letter))
                })
        });
        let path = row.path.clone();
        let source_workspace = self.workspace.clone();
        let action = row.action;
        let workspace = self.workspace.clone();
        SidebarRow::new(
            SharedString::from(format!(
                "{id_prefix}-{}-{}",
                row.path.worktree_id.to_usize(),
                row.path.path.as_unix_str()
            )),
            file_icon(row.path.path.as_std_path(), cx),
            fading_label(name),
        )
        .indent(row.depth)
        .end_padding(if is_diff { 8. } else { 0. })
        .selected(selected)
        .when_some(end, |row, end| row.end(end))
        .when(is_diff, |row| {
            row.actions_on_hover(
                glyph_button("open-source-file", IconName::FileArrowRightUp, cx)
                    .tooltip("Open Source File")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        source_workspace
                            .update(cx, |workspace, cx| {
                                workspace
                                    .open_path(path.clone(), None, true, window, cx)
                                    .detach_and_notify_err(workspace.weak_handle(), window, cx);
                            })
                            .log_err();
                    }),
                1,
            )
        })
        .when_some(item_id, |row, item_id| {
            row.actions_on_hover(self.tab_pin_button(item_id, cx), 1)
        })
        .tooltip(move |window, cx| KitTooltip::new(full_path.clone()).build(window, cx))
        .on_click(move |_, window, cx| match &action {
            FileRowAction::OpenFile(item) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.activate_item(item.as_ref(), true, true, window, cx);
                    })
                    .log_err();
            }
            FileRowAction::OpenDiff(open_diff) => open_diff(window, cx),
        })
    }

    fn render_files(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let all_files = self.open_files(workspace, cx);
        let has_open_files = !all_files.is_empty();
        let open_files = all_files
            .into_iter()
            .filter(|row| match &row.action {
                FileRowAction::OpenFile(item) => !self.item_is_pinned(item.item_id(), cx),
                _ => true,
            })
            .collect::<Vec<_>>();
        let active_path = workspace
            .read(cx)
            .active_item(cx)
            .filter(|item| item.tab_category(cx) == TabCategory::File)
            .and_then(|item| item.project_path(cx));
        let weak_workspace = self.workspace.clone();
        let buttons = vec![
            glyph_button("project-tree", IconName::ListTodoSubtask, cx)
                .tooltip("Project Files")
                .selected(self.files != FilesState::Closed)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_project_files(window, cx)))
                .into_any_element(),
            glyph_button("open-file", IconName::Plus, cx)
                .tooltip("Open File")
                .dropdown_menu(move |menu, window, cx| {
                    let menu = workspace::style_menu(menu, window);
                    let Some(workspace) = weak_workspace.upgrade() else {
                        return menu;
                    };
                    workspace.update(cx, |workspace, cx| {
                        workspace.tab_creation_menu(TabCategory::File, menu, cx)
                    })
                })
                .into_any_element(),
        ];

        v_flex()
            .w_full()
            .mt(rems_from_px(GROUP_GAP))
            .child(self.render_section_label(
                Section::Files,
                if self.is_collapsed(Section::Files) {
                    IconName::Folder
                } else {
                    IconName::FolderOpen
                },
                "Files",
                buttons,
                cx,
            ))
            .when(!self.is_collapsed(Section::Files), |group| {
                group
                    .when(open_files.is_empty(), |group| {
                        group.child(self.render_placeholder(
                            "nav-no-files",
                            if has_open_files {
                                "All files are pinned"
                            } else {
                                "No open files"
                            },
                            cx,
                        ))
                    })
                    .children(
                        open_files.into_iter().map(|row| {
                            self.render_file_row("nav-file", row, active_path.as_ref(), cx)
                        }),
                    )
            })
            .into_any_element()
    }

    fn render_change_folder(
        &self,
        key: String,
        title: SharedString,
        depth: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let collapsed = self.collapsed_change_folders.contains(&key);
        SidebarRow::new(
            SharedString::from(format!("nav-change-folder-{key}")),
            glyph(if collapsed {
                IconName::Folder
            } else {
                IconName::FolderOpen
            })
            .text_color(cx.theme().colors().icon_muted),
            fading_label(title),
        )
        .indent(depth)
        .leading_on_hover(
            glyph(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .text_color(cx.theme().colors().icon_muted),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if !this.collapsed_change_folders.remove(&key) {
                this.collapsed_change_folders.insert(key.clone());
            }
            cx.notify();
        }))
        .into_any_element()
    }

    fn render_change_rows(
        &self,
        changes: Vec<FileRow>,
        workspace: &Entity<Workspace>,
        active_path: Option<&ProjectPath>,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        if !self.changes_tree {
            return changes
                .into_iter()
                .map(|row| {
                    self.render_file_row("nav-change", row, active_path, cx)
                        .into_any_element()
                })
                .collect();
        }
        let multiple_roots = changes
            .iter()
            .map(|row| row.path.worktree_id)
            .collect::<HashSet<_>>()
            .len()
            > 1;
        let mut seen_folders = HashSet::new();
        let mut rows = Vec::new();
        for mut row in changes {
            let worktree_id = row.path.worktree_id;
            let mut folders = Vec::new();
            if multiple_roots {
                let name = workspace
                    .read(cx)
                    .project()
                    .read(cx)
                    .worktree_for_id(worktree_id, cx)
                    .map(|worktree| worktree.read(cx).root_name().to_string())
                    .unwrap_or_else(|| format!("Worktree {}", worktree_id.to_usize()));
                folders.push((
                    format!("root:{}", worktree_id.to_usize()),
                    SharedString::from(name),
                ));
            }
            if let Some((parent, _)) = row.path.path.as_unix_str().rsplit_once('/') {
                let mut prefix = String::new();
                for component in parent.split('/') {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(component);
                    folders.push((
                        format!("{}/{prefix}", worktree_id.to_usize()),
                        SharedString::from(component.to_string()),
                    ));
                }
            }
            let mut hidden = false;
            for (depth, (key, name)) in folders.iter().enumerate() {
                if seen_folders.insert(key.clone()) {
                    rows.push(self.render_change_folder(key.clone(), name.clone(), depth + 1, cx));
                }
                if self.collapsed_change_folders.contains(key) {
                    hidden = true;
                    break;
                }
            }
            if !hidden {
                row.depth = folders.len() + 1;
                rows.push(
                    self.render_file_row("nav-change", row, active_path, cx)
                        .into_any_element(),
                );
            }
        }
        rows
    }

    fn render_changes(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let changes = self.changed_files(workspace, cx);
        let active_path = workspace
            .read(cx)
            .active_item(cx)
            .and_then(|item| item.project_path(cx));
        let empty = changes.is_empty();
        let rows = self.render_change_rows(changes, workspace, active_path.as_ref(), cx);
        let buttons = vec![
            glyph_button(
                "changes-view-mode",
                if self.changes_tree {
                    IconName::ListBullets
                } else {
                    IconName::ListTodoSubtask
                },
                cx,
            )
            .tooltip(if self.changes_tree {
                "Show as List"
            } else {
                "Show as Tree"
            })
            .selected(self.changes_tree)
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.changes_tree = !this.changes_tree;
                cx.notify();
            }))
            .into_any_element(),
            glyph_button("open-changes", IconName::LinkExternal, cx)
                .tooltip("Open Changes")
                .on_click(|_, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(Box::new(zed_actions::git_panel::ToggleFocus), cx);
                })
                .into_any_element(),
        ];
        v_flex()
            .w_full()
            .mt(rems_from_px(GROUP_GAP))
            .child(self.render_section_label(
                Section::Changes,
                IconName::DiffMultiple,
                "Changes",
                buttons,
                cx,
            ))
            .when(!self.is_collapsed(Section::Changes), |group| {
                group
                    .when(empty, |group| {
                        group.child(self.render_placeholder("nav-no-changes", "No changes", cx))
                    })
                    .children(rows)
            })
            .into_any_element()
    }

    fn render_terminals(
        &self,
        workspace: &Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let all_terminals = workspace
            .read(cx)
            .panes()
            .iter()
            .flat_map(|pane| pane.read(cx).items())
            .filter(|item| item.tab_category(cx) == TabCategory::Terminal)
            .map(|item| item.boxed_clone())
            .collect::<Vec<_>>();
        let has_terminals = !all_terminals.is_empty();
        let terminals = all_terminals
            .into_iter()
            .filter(|item| !self.item_is_pinned(item.item_id(), cx))
            .collect();
        let active_item_id = workspace
            .read(cx)
            .active_item(cx)
            .map(|item| item.item_id());
        let rows = self.render_terminal_rows(terminals, active_item_id, cx);
        let weak_workspace = self.workspace.clone();
        let new_terminal = glyph_button("new-terminal", IconName::Plus, cx)
            .tooltip("New Terminal")
            .dropdown_menu(move |menu, window, cx| {
                let menu = workspace::style_menu(menu, window);
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
            .child(self.render_section_label(
                Section::Terminals,
                IconName::TerminalRectangle,
                "Terminals",
                vec![new_terminal],
                cx,
            ))
            .when(!self.is_collapsed(Section::Terminals), |section| {
                section
                    .when(rows.is_empty(), |section| {
                        section.child(self.render_placeholder(
                            "nav-no-terminals",
                            if has_terminals {
                                "All terminals are pinned"
                            } else {
                                "No terminals yet"
                            },
                            cx,
                        ))
                    })
                    .children(rows)
            })
            .into_any_element()
    }

    fn render_terminal_rows(
        &self,
        terminals: Vec<Box<dyn ItemHandle>>,
        active_item_id: Option<gpui::EntityId>,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        terminals
            .into_iter()
            .map(|item| {
                let status = item.tab_status(cx);
                let dot = match status.as_ref().map(|(_, color)| *color) {
                    Some(Color::Accent) => StatusDot::Running,
                    Some(Color::Error) => StatusDot::Error,
                    _ => StatusDot::Read,
                };
                let full_title = item.tab_content_text(0, cx);
                // Terminal titles read "folder — command"; the row keeps the folder, like a
                // thread title, and the tooltip has the rest.
                let title: SharedString = full_title
                    .split(" — ")
                    .next()
                    .unwrap_or_default()
                    .to_string()
                    .into();
                let tooltip: SharedString = match status {
                    Some((label, _)) => format!("{full_title}\n{label}").into(),
                    None => full_title,
                };
                let is_active = active_item_id == Some(item.item_id());
                let workspace = self.workspace.clone();
                SidebarRow::new(
                    SharedString::from(format!("nav-terminal-{}", item.item_id())),
                    dot,
                    fading_label(title),
                )
                .actions_on_hover(self.tab_pin_button(item.item_id(), cx), 1)
                .tooltip(move |window, cx| KitTooltip::new(tooltip.clone()).build(window, cx))
                .selected(is_active)
                .on_click(move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.activate_item(item.as_ref(), true, true, window, cx);
                        })
                        .log_err();
                })
                .into_any_element()
            })
            .collect()
    }

    fn render_footer(&self, workspace: &Entity<Workspace>, cx: &App) -> impl IntoElement {
        let repository = workspace.read(cx).project().read(cx).active_repository(cx);
        let branch: SharedString = repository
            .as_ref()
            .and_then(|repository| {
                repository
                    .read(cx)
                    .branch
                    .as_ref()
                    .map(|branch| branch.name().to_string())
            })
            .unwrap_or_else(|| "No Branch".to_string())
            .into();
        let colors = cx.theme().colors();
        let weak_workspace = self.workspace.clone();
        let branch_workspace = self.workspace.clone();

        h_flex()
            .w_full()
            .pt(rems_from_px(6_f32))
            .pb(rems_from_px(LIST_INSET))
            .pl(rems_from_px(LIST_INSET))
            .pr(rems_from_px(LIST_INSET + GROUP_LABEL_PADDING_END))
            .child(
                div().flex_1().min_w_0().child(
                    Popover::new("workspace-branches")
                        .anchor(Anchor::BottomLeft)
                        .w_full()
                        .trigger(
                            Button::new("workspace-branch")
                                .custom(workspace::ghost_button_style(cx))
                                .small()
                                .w_full()
                                .h(rems_from_px(ROW_MIN_HEIGHT))
                                .px(rems_from_px(6_f32))
                                .rounded_lg()
                                .ui_text(UiTextSize::Body, UiTextTone::Muted, cx)
                                .accessibility_label(format!("Switch Branch: {branch}"))
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .gap(rems_from_px(6_f32))
                                        .child(
                                            div()
                                                .size(rems_from_px(20_f32))
                                                .flex_none()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(
                                                    glyph(IconName::GitBranch)
                                                        .text_color(colors.text_muted),
                                                ),
                                        )
                                        .child(
                                            div().flex_1().min_w_0().child(fading_label(branch)),
                                        ),
                                )
                                .tooltip("Switch Branch"),
                        )
                        .content(move |_, window, cx| {
                            let popover = cx.weak_entity();
                            let workspace = branch_workspace.clone();
                            let repository = repository.clone();
                            let picker_state =
                                window.use_keyed_state("branch-picker", cx, move |window, cx| {
                                    let picker = git_ui_core::build_branch_picker(
                                        workspace, repository, window, cx,
                                    );
                                    let subscription = picker.as_ref().map(|picker| {
                                        picker.focus_handle(cx).focus(window, cx);
                                        cx.subscribe_in(
                                            picker,
                                            window,
                                            move |_, _, _: &DismissEvent, window, cx| {
                                                popover
                                                    .update(cx, |popover, cx| {
                                                        popover.dismiss(window, cx)
                                                    })
                                                    .log_err();
                                            },
                                        )
                                    });
                                    (picker, subscription)
                                });
                            div().children(picker_state.read(cx).0.clone())
                        }),
                ),
            )
            .child(
                glyph_button("workspace-tools", IconName::Pulse, cx)
                    .tooltip("Workspace Tools")
                    .dropdown_menu(move |menu, window, cx| {
                        let menu = workspace::style_menu(menu, window);
                        let Some(workspace) = weak_workspace.upgrade() else {
                            return menu;
                        };
                        workspace.read(cx).tools_menu(menu, cx)
                    }),
            )
    }
}

impl Render for WorkspaceNav {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.connect_thread_list(cx);
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        self.connect_panes(&workspace, cx);
        v_flex()
            .ui_text(UiTextSize::Body, UiTextTone::Secondary, cx)
            .w(rems_from_px(NAV_WIDTH))
            .h_full()
            .flex_none()
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
                    .child(self.render_pinned(&workspace, cx))
                    .child(self.render_threads(&workspace, cx))
                    .child(self.render_files(&workspace, cx))
                    .child(self.render_changes(&workspace, cx))
                    .child(self.render_terminals(&workspace, cx))
                    .child(self.render_archived(&workspace, cx)),
            )
            .child(self.render_footer(&workspace, cx))
            .into_any_element()
    }
}

/// The project's file tree at the content block's right edge. The sidebar opens, pins and closes it,
/// so this view draws what the sidebar's state says.
struct ProjectFilesSide {
    nav: WeakEntity<WorkspaceNav>,
}

impl Render for ProjectFilesSide {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.nav
            .update(cx, |nav, cx| nav.render_project_files_side(window, cx))
            .log_err()
            .unwrap_or_else(|| div().into_any_element())
    }
}
