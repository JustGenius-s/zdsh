use std::time::Duration;

use action_log::DiffStats;
use agent_ui::AgentPanel;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, Entity, Focusable as _, IntoElement,
    MouseButton, ParentElement, Render, SharedString, Styled, Subscription, WeakEntity, Window,
    div, ease_out_quint, prelude::*, px,
};
use gpui_icons::IconName;
use gpui_kit::component::{
    Selectable as _, h_flex,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    tooltip::Tooltip as KitTooltip,
    v_flex,
};
use project::{ProjectPath, git_store::GitStore};
use project_panel::ProjectPanel;
use theme::ActiveTheme;
use ui::{Color, rems_from_px};
use util::ResultExt as _;
use workspace::dock::Dock;
use workspace::{ItemHandle, MultiWorkspace, Workspace, WorkspaceNavigation, item::TabCategory};

use crate::Sidebar;
use crate::sidebar_row::{
    GROUP_GAP, LIST_INSET, ROW_MIN_HEIGHT, ROW_STACK_GAP, SidebarRow, StatusDot, file_icon, glyph,
    glyph_button,
};

/// Space before a group's title, lining it up with the icons of the rows below it.
const GROUP_LABEL_PADDING_START: f32 = 9.;
/// Space after a group's title and buttons: the rows' border and padding, so the buttons end
/// where the rows' content does.
const GROUP_LABEL_PADDING_END: f32 = 7.;
const HEADER_HEIGHT: f32 = ROW_MIN_HEIGHT;
const NAV_WIDTH: f32 = 272.;
const FILES_WIDTH: f32 = 264.;
const FILES_ENTER_DURATION: Duration = Duration::from_millis(240);
/// How many of the repository's changed files the sidebar lists before linking to all of them.
const GIT_CHANGES_SHOWN: usize = 3;
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
        let left_dock = workspace.left_dock().clone();
        let nav = cx.new(|cx| Self::new(&workspace_handle, &git_store, &left_dock, window, cx));
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(workspace, |_, _, cx| cx.notify()),
            cx.subscribe(git_store, |_, _, _, cx| cx.notify()),
            cx.observe_in(left_dock, window, Self::take_project_files_from_dock),
        ];
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
            _threads_subscription: None,
            _project_files_subscription: None,
            _subscriptions: subscriptions,
        }
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
                                    .truncate()
                                    .text_sm()
                                    .text_color(colors.text_muted)
                                    .child(name),
                            )
                            .child(div().flex_1())
                            .child(
                                glyph_button("pin-project-files", IconName::Pin)
                                    .tooltip(if pinned { "Unpin" } else { "Pin as a Column" })
                                    .selected(pinned)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_files_pinned(cx)),
                                    ),
                            )
                            .child(
                                glyph_button("close-project-files", IconName::X)
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
                            .truncate()
                            .text_color(colors.text_placeholder)
                            .child("Find a File…"),
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
        let colors = cx.theme().colors();
        let nav = cx.entity().downgrade();
        let customized = self.thread_order != ThreadOrder::Updated
            || !self.show_thread_times
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
                    .gap(rems_from_px(4_f32))
                    .pl(rems_from_px(GROUP_LABEL_PADDING_START))
                    .pr(rems_from_px(GROUP_LABEL_PADDING_END))
                    .text_color(colors.text_muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(key.display_name(&Default::default())),
                    )
                    .child(
                        glyph_button("customize-sidebar", IconName::ListFilter)
                            .tooltip("Customize Sidebar")
                            .selected(customized)
                            .dropdown_menu(move |menu, window, cx| {
                                Self::customize_menu(nav.clone(), menu, window, cx)
                            }),
                    )
                    .child(
                        glyph_button("workspace-search", IconName::MagnifyingGlass)
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
        let (thread_order, show_thread_times, status_filter) = {
            let this = this.read(cx);
            (
                this.thread_order,
                this.show_thread_times,
                this.status_filter.clone(),
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
            div().truncate().text_color(colors.text).child(title),
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
                .truncate()
                .text_color(cx.theme().colors().text_muted)
                .child(text),
        )
        .into_any_element()
    }

    fn render_threads(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let key = workspace.read(cx).project_group_key(cx);
        let mut threads = self
            .threads
            .as_ref()
            .map(|threads| threads.read(cx).project_threads(&key))
            .unwrap_or_default();
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

        let colors = cx.theme().colors();
        let warning = cx.theme().status().warning;
        let show_thread_times = self.show_thread_times;
        let rows = threads
            .into_iter()
            .map(|thread| {
                let entry_index = thread.entry_index;
                let end = if thread.status == StatusDot::WaitingForConfirmation {
                    Some(div().text_sm().text_color(warning).child("Approve"))
                } else {
                    thread
                        .updated
                        .filter(|_| show_thread_times)
                        .map(|updated| div().text_sm().text_color(colors.text_muted).child(updated))
                };
                let thread_list = self.threads.clone();
                SidebarRow::new(
                    SharedString::from(format!("nav-thread-{entry_index}")),
                    thread.status,
                    div().truncate().text_color(colors.text).child(thread.title),
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
            })
            .collect::<Vec<_>>();
        let new_thread = glyph_button("new-thread", IconName::Plus)
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
                let placeholder = if self.status_filter.is_empty() {
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
                                div().truncate().text_color(colors.text_muted).child("More"),
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

    /// The files this workspace's threads changed, then what git reports as changed, and how many
    /// files git reports in all.
    fn changed_files(
        &self,
        workspace: &Entity<Workspace>,
        cx: &App,
    ) -> (Vec<FileRow>, Vec<FileRow>, usize) {
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

        (thread_changes, git_changes, git_change_count)
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
                if open_files.iter().any(|row| row.path == path) {
                    continue;
                }
                open_files.push(FileRow {
                    path,
                    item: Some(item.boxed_clone()),
                    end: FileRowEnd::None,
                });
            }
        }
        open_files
    }

    /// `id_prefix` tells apart the rows of a file listed more than once, such as one that is
    /// both open and changed.
    fn render_file_row(&self, id_prefix: &str, row: FileRow, cx: &App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let name: SharedString = row
            .path
            .path
            .file_name()
            .unwrap_or_default()
            .to_string()
            .into();
        let full_path: SharedString = row.path.path.as_unix_str().to_string().into();
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
                "{id_prefix}-{}-{}",
                row.path.worktree_id.to_usize(),
                row.path.path.as_unix_str()
            )),
            file_icon(row.path.path.as_std_path(), cx).text_color(colors.icon_muted),
            div().truncate().text_color(colors.text).child(name),
        )
        .when_some(end, |row, end| row.end(end))
        .tooltip(move |window, cx| KitTooltip::new(full_path.clone()).build(window, cx))
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

    fn render_files(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let open_files = self.open_files(workspace, cx);
        let weak_workspace = self.workspace.clone();
        let buttons = vec![
            glyph_button("project-tree", IconName::ListTodoSubtask)
                .tooltip("Project Files")
                .selected(self.files != FilesState::Closed)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_project_files(window, cx)))
                .into_any_element(),
            glyph_button("open-file", IconName::Plus)
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
                        group.child(self.render_placeholder("nav-no-files", "No open files", cx))
                    })
                    .children(
                        open_files
                            .into_iter()
                            .map(|row| self.render_file_row("nav-file", row, cx)),
                    )
            })
            .into_any_element()
    }

    fn render_changes(&self, workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> AnyElement {
        let (thread_changes, git_changes, git_change_count) = self.changed_files(workspace, cx);
        let colors = cx.theme().colors();
        let hidden_git_changes = git_change_count.saturating_sub(git_changes.len());

        v_flex()
            .w_full()
            .mt(rems_from_px(GROUP_GAP))
            .child(self.render_section_label(
                Section::Changes,
                IconName::DiffSingle,
                "Changes",
                Vec::new(),
                cx,
            ))
            .when(!self.is_collapsed(Section::Changes), |group| {
                group
                    .when(
                        thread_changes.is_empty() && git_changes.is_empty(),
                        |group| {
                            group.child(self.render_placeholder("nav-no-changes", "No changes", cx))
                        },
                    )
                    .children(
                        thread_changes
                            .into_iter()
                            .map(|row| self.render_file_row("nav-thread-change", row, cx)),
                    )
                    .children(
                        git_changes
                            .into_iter()
                            .map(|row| self.render_file_row("nav-git-change", row, cx)),
                    )
                    .when(hidden_git_changes > 0, |group| {
                        group.child(
                            SidebarRow::new(
                                "nav-all-changes",
                                div(),
                                div()
                                    .truncate()
                                    .text_color(colors.text_muted)
                                    .child(format!("All {git_change_count} Changes")),
                            )
                            .end(glyph(IconName::ChevronRight).text_color(colors.icon_muted))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(zed_actions::git_panel::ToggleFocus),
                                    cx,
                                )
                            }),
                        )
                    })
            })
            .into_any_element()
    }

    fn render_terminals(
        &self,
        workspace: &Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminals = workspace
            .read(cx)
            .panes()
            .iter()
            .flat_map(|pane| pane.read(cx).items())
            .filter(|item| item.tab_category(cx) == TabCategory::Terminal)
            .map(|item| item.boxed_clone())
            .collect::<Vec<_>>();
        let active_item_id = workspace
            .read(cx)
            .active_item(cx)
            .map(|item| item.item_id());
        let rows = self.render_terminal_rows(terminals, active_item_id, cx);
        let weak_workspace = self.workspace.clone();
        let new_terminal = glyph_button("new-terminal", IconName::Plus)
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
                            "No terminals yet",
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
        cx: &App,
    ) -> Vec<AnyElement> {
        let colors = cx.theme().colors();
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
                    div().truncate().text_color(colors.text).child(title),
                )
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

    /// The repository's branch and changes, which open the Changes tab, and the workspace's
    /// tools, laid out like the account row and settings button at the foot of Cursor's sidebar.
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
        let change_count = repository
            .map(|repository| repository.read(cx).status_summary().count)
            .unwrap_or_default();
        let colors = cx.theme().colors();
        let weak_workspace = self.workspace.clone();

        h_flex()
            .w_full()
            .pt(rems_from_px(6_f32))
            .pb(rems_from_px(LIST_INSET))
            .pr(rems_from_px(LIST_INSET + GROUP_LABEL_PADDING_END))
            .border_t_1()
            .border_color(colors.border_variant)
            .child(
                div().flex_1().min_w_0().child(
                    SidebarRow::new(
                        "workspace-changes",
                        glyph(IconName::GitBranch).text_color(colors.text_muted),
                        div().truncate().text_color(colors.text_muted).child(branch),
                    )
                    .when(change_count > 0, |row| {
                        row.end(
                            div()
                                .text_sm()
                                .text_color(colors.text_muted)
                                .child(change_count.to_string()),
                        )
                    })
                    .tooltip(|window, cx| {
                        KitTooltip::new("Changes")
                            .action(&zed_actions::git_panel::ToggleFocus, None)
                            .build(window, cx)
                    })
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(zed_actions::git_panel::ToggleFocus), cx)
                    }),
                ),
            )
            .child(
                glyph_button("workspace-tools", IconName::Wrench)
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
        v_flex()
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
                    .child(self.render_threads(&workspace, cx))
                    .child(self.render_files(&workspace, cx))
                    .child(self.render_changes(&workspace, cx))
                    .child(self.render_terminals(&workspace, cx)),
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
