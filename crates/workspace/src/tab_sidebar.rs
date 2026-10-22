use agent_settings::AgentSettings;
use gpui::{
    Anchor, App, ElementId, Entity, EntityId, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, TaskExt, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px, rems,
};
use gpui_kit::{
    assets::IconName as KitIconName,
    component::{
        Collapsible, Icon, Side, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        list::ListItem,
        menu::{DropdownMenu, PopupMenu},
        sidebar::{Sidebar, SidebarCollapsible, SidebarItem},
    },
};
use settings::Settings as _;
use std::rc::Rc;
use ui::Tooltip;
use util::ResultExt as _;

use crate::{
    ItemHandle, Pane, SaveIntent, Workspace, ghost_button_style,
    item::TabCategory,
    rail::{RailButton, rail_container},
};

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
        if self
            .panes()
            .iter()
            .all(|pane| pane.read(cx).items_len() == 0)
        {
            return self.render_empty_tab_rail(cx).into_any_element();
        }
        let workspace = self.weak_handle();
        let active_item_id = self.active_item(cx).map(|item| item.item_id());
        let mut agent_items = Vec::new();
        let mut terminal_items = Vec::new();
        let mut file_items = Vec::new();
        let mut action_items = Vec::new();

        for pane in self.panes() {
            for item in pane.read(cx).items() {
                let menu_item = TabSidebarItem {
                    item: Rc::from(item.boxed_clone()),
                    active: active_item_id == Some(item.item_id()),
                };
                match item.tab_category(cx) {
                    TabCategory::Agent => agent_items.push(menu_item),
                    TabCategory::Terminal => terminal_items.push(menu_item),
                    TabCategory::File => file_items.push(menu_item),
                    TabCategory::Action => action_items.push(menu_item),
                }
            }
        }

        let groups = [
            tab_group(TabCategory::Agent, agent_items, &workspace),
            tab_group(TabCategory::Terminal, terminal_items, &workspace),
            tab_group(TabCategory::File, file_items, &workspace),
            tab_group(TabCategory::Action, action_items, &workspace),
        ];
        let theme = theme::ActiveTheme::theme(cx);
        let background = theme.sidebar_backdrop();

        Sidebar::new("tab-sidebar")
            .side(Side::Right)
            .collapsible(SidebarCollapsible::None)
            // A right sidebar draws its own left border. The content block already has an edge,
            // so that border is a second line.
            .border_l_0()
            .w(px(240.))
            .h_full()
            .flex_none()
            .bg(background)
            .children(groups)
            .into_any_element()
    }

    fn render_empty_tab_rail(&self, cx: &App) -> impl IntoElement + use<> {
        let categories = [
            TabCategory::Agent,
            TabCategory::Terminal,
            TabCategory::File,
            TabCategory::Action,
        ];
        rail_container("tab-sidebar-rail", cx).children(categories.into_iter().map(|category| {
            let workspace = self.weak_handle();
            RailButton::new(category.label(), category_icon(category).path())
                .tooltip(Tooltip::text(category.label()))
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                    let Some(workspace) = workspace.upgrade() else {
                        return menu;
                    };
                    workspace.update(cx, |workspace, cx| {
                        workspace.tab_creation_menu(category, menu, cx)
                    })
                })
        }))
    }

    fn tab_creation_menu(&self, category: TabCategory, menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let menu = menu.action_context(self.active_pane.read(cx).focus_handle(cx));
        match category {
            TabCategory::Agent => {
                let has_project = self.project.read(cx).visible_worktrees(cx).next().is_some();
                menu.menu_with_disabled(
                    "New Agent Tab",
                    Box::new(zed_actions::agent::NewThread),
                    !has_project || !AgentSettings::get_global(cx).enabled(cx),
                )
                .when(!has_project, |menu| {
                    menu.separator()
                        .menu("Open Folder…", Box::new(crate::Open::default()))
                })
            }
            TabCategory::Terminal => {
                let mut menu = menu.menu_with_disabled(
                    "New Terminal",
                    Box::new(crate::NewTerminal::default()),
                    self.terminal_provider.is_none(),
                );
                if let Some(provider) = self.terminal_provider.as_ref() {
                    let actions = provider.quick_actions(cx);
                    if !actions.is_empty() {
                        menu = menu.separator();
                        for (label, action) in actions {
                            menu = menu.menu(label, action);
                        }
                    }
                    menu = menu.separator().menu(
                        "Manage quick actions…",
                        Box::new(crate::OpenTerminalQuickActions),
                    );
                }
                menu
            }
            TabCategory::File => menu
                .menu("New File", Box::new(crate::NewFile))
                .menu("Open File…", Box::new(crate::Open::default())),
            TabCategory::Action => {
                let mut menu = menu;
                for item in self.rail_items.iter().filter(|item| (item.is_visible)(cx)) {
                    menu = menu.menu(item.tooltip, item.action.boxed_clone());
                }
                menu
            }
        }
    }
}

#[derive(Clone)]
struct TabSidebarItem {
    item: Rc<dyn ItemHandle>,
    active: bool,
}

#[derive(Clone)]
struct TabGroup {
    category: TabCategory,
    items: Vec<TabSidebarItem>,
    workspace: WeakEntity<Workspace>,
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
        let menu = div()
            .id(id)
            .flex()
            .flex_col()
            .gap_2()
            .pl(if self.collapsed { rems(0.) } else { rems(1.) })
            .children(self.items.into_iter().map(|item| {
                tab_menu_item(item.item.as_ref(), item.active, &self.workspace, window, cx)
            }));
        let category_color = theme::ActiveTheme::theme(cx).colors().text_accent;
        let workspace = self.workspace;
        div()
            .flex()
            .flex_col()
            .group("tab-category")
            .when(!self.collapsed, |group| {
                group.child(
                    Button::new(self.category.label())
                        .custom(ghost_button_style(cx))
                        .w_full()
                        .h_8()
                        .px_2()
                        .accessibility_label(format!("{} actions", self.category.label()))
                        .child(
                            h_flex()
                                .flex_1()
                                .gap_2()
                                .text_base()
                                .text_color(category_color)
                                .child(Icon::new(category_icon(self.category)).size(rems(1.)))
                                .child(self.category.label()),
                        )
                        .child(
                            div()
                                .invisible()
                                .group_hover("tab-category", |style| style.visible())
                                .child(Icon::new(KitIconName::Ellipsis).size(rems(1.))),
                        )
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                            let Some(workspace) = workspace.upgrade() else {
                                return menu;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.tab_creation_menu(self.category, menu, cx)
                            })
                        }),
                )
            })
            .child(menu)
            .into_any_element()
    }
}

fn tab_group(
    category: TabCategory,
    items: Vec<TabSidebarItem>,
    workspace: &WeakEntity<Workspace>,
) -> TabGroup {
    TabGroup {
        category,
        items,
        workspace: workspace.clone(),
        collapsed: false,
    }
}

fn tab_menu_item(
    item: &dyn ItemHandle,
    active: bool,
    workspace: &WeakEntity<Workspace>,
    window: &Window,
    cx: &App,
) -> ListItem {
    let item_id = item.item_id();
    let category = item.tab_category(cx);
    let title = item.tab_content_text(0, cx);
    let status = item.tab_status(cx);
    let icon = item
        .tab_icon(window, cx)
        .map(|icon| icon.into_any_element())
        .unwrap_or_else(|| {
            Icon::new(category_icon(category))
                .size(rems(1.))
                .into_any_element()
        });
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

    ListItem::new(SharedString::from(format!("tab-sidebar-{item_id}")))
        .accessibility_label(accessible_name)
        .selected(active)
        .child(
            h_flex()
                .gap_2()
                .min_w_0()
                .flex_1()
                .child(icon)
                .child(div().min_w_0().truncate().child(title)),
        )
        .on_click(move |_, window, cx| {
            workspace_for_activate
                .update(cx, |workspace, cx| {
                    workspace.activate_item(activate_item.as_ref(), true, true, window, cx);
                })
                .log_err();
        })
        .suffix(move |_window, cx| {
            let workspace_for_close = workspace_for_close.clone();
            let close_id = close_id.clone();
            h_flex()
                .items_center()
                .gap_1()
                .when_some(status.clone(), |row, (label, color)| {
                    row.child(
                        div()
                            .id("tab-status")
                            .text_xs()
                            .text_color(color.color(cx))
                            .tooltip(Tooltip::text(label.clone()))
                            .child(label),
                    )
                })
                .when_some(dirty_color, |row, color| {
                    row.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(color))
                })
                .child(
                    Button::new(close_id)
                        .custom(ghost_button_style(cx))
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
