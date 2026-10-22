use gpui::{
    App, ElementId, Entity, EntityId, Focusable as _, IntoElement, ParentElement, SharedString,
    Styled, TaskExt, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit::{
    assets::IconName as KitIconName,
    component::{
        Collapsible, Side, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        sidebar::{
            Sidebar, SidebarCollapsible, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem,
        },
    },
};
use util::ResultExt as _;

use crate::{ItemHandle, Pane, SaveIntent, Workspace, item::TabCategory};

impl Workspace {
    /// Tab list fixed to the right of the content block.
    ///
    /// It is window chrome, like the rail: docks and zoom stay inside the content block, so this
    /// list remains visible while those change.
    pub(crate) fn render_tab_sidebar(
        &self,
        _window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let workspace = self.weak_handle();
        let active_item_id = self.active_item(cx).map(|item| item.item_id());
        let mut agent_items = Vec::new();
        let mut terminal_items = Vec::new();
        let mut file_items = Vec::new();
        let mut action_items = Vec::new();

        for pane in self.panes() {
            for item in pane.read(cx).items() {
                let menu_item = tab_menu_item(
                    item.as_ref(),
                    active_item_id == Some(item.item_id()),
                    &workspace,
                    cx,
                );
                match item.tab_category(cx) {
                    TabCategory::Agent => agent_items.push(menu_item),
                    TabCategory::Terminal => terminal_items.push(menu_item),
                    TabCategory::File => file_items.push(menu_item),
                    TabCategory::Action => action_items.push(menu_item),
                }
            }
        }

        let groups = [
            tab_group(TabCategory::Agent, agent_items, &workspace, false),
            tab_group(
                TabCategory::Terminal,
                terminal_items,
                &workspace,
                self.terminal_provider.is_some(),
            ),
            tab_group(TabCategory::File, file_items, &workspace, false),
            tab_group(TabCategory::Action, action_items, &workspace, false),
        ];

        Sidebar::new("tab-sidebar")
            .side(Side::Right)
            .collapsible(SidebarCollapsible::None)
            // A right sidebar draws its own left border. The content block already has an edge,
            // so that border is a second line.
            .border_l_0()
            .w(px(240.))
            .h_full()
            .flex_none()
            .children(groups.into_iter().flatten())
    }
}

#[derive(Clone)]
struct TabGroup {
    category: TabCategory,
    items: Vec<SidebarMenuItem>,
    workspace: WeakEntity<Workspace>,
    quick_actions: bool,
    collapsed: bool,
}

impl Collapsible for TabGroup {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for TabGroup {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id = id.into();
        let menu = SidebarMenu::new().children(self.items);
        if !self.quick_actions {
            return SidebarGroup::new(self.category.label())
                .child(menu)
                .collapsed(self.collapsed)
                .render(id, window, cx)
                .into_any_element();
        }
        let workspace = self.workspace;
        div()
            .flex()
            .flex_col()
            .child(
                h_flex()
                    .h_8()
                    .px_2()
                    .justify_between()
                    .child(div().text_xs().opacity(0.7).child(self.category.label()))
                    .child(
                        Button::new("terminal-quick-actions")
                            .ghost()
                            .xsmall()
                            .icon(KitIconName::Command)
                            .tooltip("Terminal quick actions")
                            .accessibility_label("Terminal quick actions")
                            .dropdown_menu(move |mut menu, _, cx| {
                                let actions = workspace
                                    .update(cx, |workspace, cx| {
                                        workspace
                                            .terminal_provider
                                            .as_ref()
                                            .map(|provider| provider.quick_actions(cx))
                                            .unwrap_or_default()
                                    })
                                    .log_err()
                                    .unwrap_or_default();
                                let has_actions = !actions.is_empty();
                                for (label, action) in actions {
                                    let workspace = workspace.clone();
                                    menu = menu.item(PopupMenuItem::new(label).on_click(
                                        move |_, window, cx| {
                                            workspace
                                                .update(cx, |workspace, cx| {
                                                    workspace.focus_handle(cx).focus(window, cx);
                                                    window
                                                        .dispatch_action(action.boxed_clone(), cx);
                                                })
                                                .log_err();
                                        },
                                    ));
                                }
                                let workspace = workspace.clone();
                                menu.when(has_actions, |menu| menu.separator()).item(
                                    PopupMenuItem::new("Manage quick actions…").on_click(
                                        move |_, window, cx| {
                                            workspace
                                                .update(cx, |workspace, cx| {
                                                    workspace.focus_handle(cx).focus(window, cx);
                                                    window.dispatch_action(
                                                        Box::new(crate::OpenTerminalQuickActions),
                                                        cx,
                                                    );
                                                })
                                                .log_err();
                                        },
                                    ),
                                )
                            }),
                    ),
            )
            .child(menu.render(id, window, cx))
            .into_any_element()
    }
}

fn tab_group(
    category: TabCategory,
    items: Vec<SidebarMenuItem>,
    workspace: &WeakEntity<Workspace>,
    quick_actions: bool,
) -> Option<TabGroup> {
    if items.is_empty() && !quick_actions {
        return None;
    }
    Some(TabGroup {
        category,
        items,
        workspace: workspace.clone(),
        quick_actions,
        collapsed: false,
    })
}

fn tab_menu_item(
    item: &dyn ItemHandle,
    active: bool,
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> SidebarMenuItem {
    let item_id = item.item_id();
    let category = item.tab_category(cx);
    let title = item.tab_content_text(0, cx);
    let accessible_name = item.tab_tooltip_text(cx).unwrap_or_else(|| title.clone());
    let dirty_color = if item.has_conflict(cx) {
        Some(theme::ActiveTheme::theme(cx).status().warning)
    } else if item.is_dirty(cx) {
        Some(theme::ActiveTheme::theme(cx).colors().text_accent)
    } else {
        None
    };
    let workspace_for_activate = workspace.clone();
    let workspace_for_close = workspace.clone();
    let activate_item = item.boxed_clone();
    let close_id = SharedString::from(format!("tab-sidebar-close-{item_id}"));

    SidebarMenuItem::new(title)
        .accessibility_label(accessible_name)
        .icon(category_icon(category))
        .active(active)
        .on_click(move |_, window, cx| {
            workspace_for_activate
                .update(cx, |workspace, cx| {
                    workspace.activate_item(activate_item.as_ref(), true, true, window, cx);
                })
                .log_err();
        })
        .suffix(move |_window, _cx| {
            let workspace_for_close = workspace_for_close.clone();
            let close_id = close_id.clone();
            h_flex()
                .items_center()
                .gap_1()
                .when_some(dirty_color, |row, color| {
                    row.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(color))
                })
                .child(
                    Button::new(close_id)
                        .ghost()
                        .xsmall()
                        .icon(KitIconName::Close)
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            close_tab(&workspace_for_close, item_id, window, cx);
                        }),
                )
        })
}

fn category_icon(category: TabCategory) -> KitIconName {
    match category {
        TabCategory::Agent => KitIconName::Sparkle,
        TabCategory::Terminal => KitIconName::Terminal,
        TabCategory::File => KitIconName::File,
        TabCategory::Action => KitIconName::Command,
    }
}

fn close_tab(
    workspace: &WeakEntity<Workspace>,
    item_id: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            let Some(pane) = pane_for_item(workspace, item_id, cx) else {
                return;
            };
            pane.update(cx, |pane, cx| {
                pane.close_item_by_id(item_id, SaveIntent::Close, window, cx)
            })
            .detach_and_log_err(cx);
        })
        .log_err();
}

fn pane_for_item(workspace: &Workspace, item_id: EntityId, cx: &App) -> Option<Entity<Pane>> {
    if let Some(pane) = workspace.pane_for_item_id(item_id) {
        return Some(pane);
    }
    workspace.panes().iter().find_map(|pane| {
        pane.read(cx)
            .items()
            .any(|item| item.item_id() == item_id)
            .then(|| pane.clone())
    })
}
