use gpui::AsKeystroke as _;
use gpui::{
    App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_kit::component::{h_flex, kbd::Kbd, menu::DropdownMenu as _, v_flex};
use icons::IconName;
use project::git_store::GitStore;
use theme::ActiveTheme;
use ui::rems_from_px;
use workspace::{MultiWorkspace, Workspace, WorkspaceNavigation};

use crate::sidebar_row::{LIST_INSET, SidebarRow, sidebar_icon, sidebar_icon_button};

/// Space before a group's title, lining it up with the icons of the rows below it.
const GROUP_LABEL_PADDING_START: f32 = 9.;
const GROUP_LABEL_PADDING_END: f32 = 4.;
const HEADER_HEIGHT: f32 = 30.;

/// The sidebar at a workspace's left edge, showing that workspace's threads, files and
/// terminals.
pub struct WorkspaceNav {
    workspace: WeakEntity<Workspace>,
    _subscriptions: Vec<Subscription>,
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
            _subscriptions: subscriptions,
        }
    }

    fn needs_attention(&self, _cx: &App) -> bool {
        false
    }

    fn multi_workspace(&self, cx: &App) -> Option<Entity<MultiWorkspace>> {
        self.workspace
            .upgrade()?
            .read(cx)
            .multi_workspace()?
            .upgrade()
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
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        v_flex()
            .size_full()
            .pt(rems_from_px(LIST_INSET))
            .child(self.render_header(&workspace, cx))
            .child(div().flex_1().min_h_0())
            .child(self.render_footer(&workspace, window, cx))
            .into_any_element()
    }
}
