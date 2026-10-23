mod application_menu;
mod onboarding_banner;
mod title_bar_settings;
mod update_version;

use crate::application_menu::{ApplicationMenu, show_menus};
use arrayvec::ArrayVec;
pub use platform_title_bar::{
    self, DraggedWindowTab, MergeAllWindows, MoveTabToNewWindow, PlatformTitleBar,
    ShowNextWindowTab, ShowPreviousWindowTab,
};

#[cfg(not(target_os = "macos"))]
use crate::application_menu::{
    ActivateDirection, ActivateMenuLeft, ActivateMenuRight, OpenApplicationMenu,
};

use auto_update::AutoUpdateStatus;
use client::{Client, UserStore};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Entity, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription,
    WeakEntity, Window, actions, div, pulsating_between,
};
use gpui_icons::IconName as MenuIcon;
use gpui_kit::component::{
    Sizable as _,
    button::{Button as KitButton, ButtonVariants as _},
    menu::DropdownMenu as _,
};
use onboarding_banner::OnboardingBanner;
use project::{Project, trusted_worktrees::TrustedWorktrees};
use remote::RemoteConnectionOptions;
use settings::Settings as _;

use std::sync::Arc;
use std::time::Duration;
use theme::ActiveTheme;
use title_bar_settings::TitleBarSettings;
use ui::{
    ButtonLike, IconWithIndicator, Indicator, PopoverMenu, TintColor, Tooltip, prelude::*,
    utils::platform_title_bar_height,
};
use update_version::UpdateVersion;
use util::ResultExt;
use workspace::{
    AccessibleMode, MultiWorkspace, ToggleWorktreeSecurity, Workspace,
    notifications::NotifyResultExt, theme_menu,
};

use zed_actions::OpenRemote;

pub use onboarding_banner::restore_banner;

actions!(
    collab,
    [
        /// Toggles the user menu dropdown.
        ToggleUserMenu,
        /// Toggles the project menu dropdown.
        ToggleProjectMenu,
        /// Switches to a different git branch.
        SwitchBranch,
        /// A debug action to simulate an update being available to test the update banner UI.
        SimulateUpdateAvailable
    ]
);

pub fn init(cx: &mut App) {
    platform_title_bar::PlatformTitleBar::init(cx);

    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let multi_workspace = workspace.multi_workspace().cloned();
        let item = cx.new(|cx| TitleBar::new("title-bar", workspace, multi_workspace, window, cx));
        workspace.set_titlebar_item(item.into(), window, cx);

        workspace.register_action(|workspace, _: &SimulateUpdateAvailable, _window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    titlebar.toggle_update_simulation(cx);
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, action: &OpenApplicationMenu, window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    if let Some(ref menu) = titlebar.application_menu {
                        menu.update(cx, |menu, cx| menu.open_menu(action, window, cx));
                    }
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, _: &ActivateMenuRight, window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    if let Some(ref menu) = titlebar.application_menu {
                        menu.update(cx, |menu, cx| {
                            menu.navigate_menus_in_direction(ActivateDirection::Right, window, cx)
                        });
                    }
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, _: &ActivateMenuLeft, window, cx| {
            if let Some(titlebar) = workspace
                .titlebar_item()
                .and_then(|item| item.downcast::<TitleBar>().ok())
            {
                titlebar.update(cx, |titlebar, cx| {
                    if let Some(ref menu) = titlebar.application_menu {
                        menu.update(cx, |menu, cx| {
                            menu.navigate_menus_in_direction(ActivateDirection::Left, window, cx)
                        });
                    }
                });
            }
        });
    })
    .detach();
}

pub struct TitleBar {
    platform_titlebar: Entity<PlatformTitleBar>,
    project: Entity<Project>,
    user_store: Entity<UserStore>,
    client: Arc<Client>,
    workspace: WeakEntity<Workspace>,
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
    application_menu: Option<Entity<ApplicationMenu>>,
    _subscriptions: Vec<Subscription>,
    banner: Option<Entity<OnboardingBanner>>,
    update_version: Entity<UpdateVersion>,
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.multi_workspace.is_none() {
            if let Some(mw) = self
                .workspace
                .upgrade()
                .and_then(|ws| ws.read(cx).multi_workspace().cloned())
            {
                if let Some(multi_workspace) = mw.upgrade() {
                    self._subscriptions
                        .push(cx.observe(&multi_workspace, |_, _, cx| cx.notify()));
                }
                self.multi_workspace = Some(mw.clone());
                self.platform_titlebar.update(cx, |titlebar, _cx| {
                    titlebar.set_multi_workspace(mw);
                });
            }
        }

        let title_bar_settings = *TitleBarSettings::get_global(cx);

        let show_menus = show_menus(cx);

        let mut children = <ArrayVec<_, 5>>::new();

        children.push(
            h_flex()
                .h_full()
                .gap_0p5()
                .map(|title_bar| {
                    let mut render_project_items = title_bar_settings.show_project_items;
                    title_bar
                        .when_some(
                            self.application_menu.clone().filter(|_| !show_menus),
                            |title_bar, menu| {
                                // Hide the project items to make room when the menu bar is
                                // expanded -- except in accessible mode, where the menu bar is
                                // always expanded but those controls must still remain reachable.
                                render_project_items &= !menu
                                    .update(cx, |menu, cx| menu.all_menus_shown(cx))
                                    || cx.accessible_mode();
                                title_bar.child(menu)
                            },
                        )
                        .children(self.render_restricted_mode(cx))
                        .when(render_project_items, |title_bar| {
                            title_bar.children(self.render_project_host(cx))
                        })
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .into_any_element(),
        );

        children.extend(self.render_project_pills(cx));

        if title_bar_settings.show_onboarding_banner {
            if let Some(banner) = &self.banner {
                children.push(banner.clone().into_any_element())
            }
        }

        let accounts_enabled = self.client.accounts_enabled();
        let status = self.client.status();
        let status = &*status.borrow();
        let user = self.user_store.read(cx).current_user();
        let is_signing_in = user.is_none()
            && matches!(
                status,
                client::Status::Authenticating
                    | client::Status::Authenticated
                    | client::Status::Connecting
            );
        let is_signed_out_or_auth_error = user.is_none()
            && matches!(
                status,
                client::Status::SignedOut | client::Status::AuthenticationError
            );

        children.push(
            h_flex()
                .flex_none()
                .pr_1()
                .gap_1()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(accounts_enabled, |this| {
                    this.children(self.render_connection_status(status, cx))
                })
                .child(self.update_version.clone())
                .child(self.render_settings_menu(cx))
                .when(
                    accounts_enabled
                        && user.is_none()
                        && is_signed_out_or_auth_error
                        && TitleBarSettings::get_global(cx).show_sign_in,
                    |this| this.child(self.render_sign_in_button(cx)),
                )
                .when(accounts_enabled && is_signing_in, |this| {
                    this.child(
                        Label::new("Signing in…")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .with_animation(
                                "signing-in",
                                Animation::new(Duration::from_secs(2))
                                    .repeat()
                                    .with_easing(pulsating_between(0.4, 0.8)),
                                |label, delta| label.alpha(delta),
                            ),
                    )
                })
                .into_any_element(),
        );

        if show_menus {
            self.platform_titlebar.update(cx, |this, _| {
                this.set_children(
                    self.application_menu
                        .clone()
                        .map(|menu| menu.into_any_element()),
                );
            });

            let height = platform_title_bar_height(window);
            let title_bar_color = self.platform_titlebar.update(cx, |platform_titlebar, cx| {
                platform_titlebar.title_bar_color(window, cx)
            });

            v_flex()
                .w_full()
                .child(self.platform_titlebar.clone().into_any_element())
                .child(
                    h_flex()
                        .bg(title_bar_color)
                        .h(height)
                        .pl_2()
                        .justify_between()
                        .w_full()
                        .children(children),
                )
                .into_any_element()
        } else {
            self.platform_titlebar.update(cx, |this, _| {
                this.set_children(children);
            });
            self.platform_titlebar.clone().into_any_element()
        }
    }
}

/// The project pills fill the macOS title bar's 44px with room to spare, like the design's.
const PILL_HEIGHT: f32 = 28.;
const PILL_TRACK_PADDING: f32 = 3.;

impl TitleBar {
    /// One pill per project open in the window, switching the window between them.
    fn render_project_pills(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let multi_workspace = self.multi_workspace.as_ref()?.upgrade()?;
        let multi_workspace = multi_workspace.read(cx);
        let groups = multi_workspace.project_groups(cx);
        let active_key =
            multi_workspace.project_group_key_for_workspace(multi_workspace.workspace(), cx);
        let active_index = groups.iter().position(|group| group.key == active_key);
        let accents = cx.theme().accents().clone();
        let attention_color = cx.theme().status().warning;
        let colors = cx.theme().colors();
        let weak_multi_workspace = self.multi_workspace.clone()?;
        // The kit's segmented tabs mark the selected tab with the window background, which the
        // title bar shares, so the pills draw their own selection.
        let pills = groups
            .into_iter()
            .enumerate()
            .map(|(index, group)| {
                let is_active = active_index == Some(index);
                let needs_attention = !is_active
                    && group
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.read(cx).needs_attention(cx));
                let key = group.key.clone();
                let close_key = group.key.clone();
                let close_multi_workspace = weak_multi_workspace.clone();
                let weak_multi_workspace = weak_multi_workspace.clone();
                let pill_group = SharedString::from(format!("project-pill-{index}"));
                h_flex()
                    .id(("project-pill", index))
                    .group(pill_group.clone())
                    .h(px(PILL_HEIGHT))
                    .pl(px(12.))
                    .pr(px(6.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .text_color(if is_active {
                        colors.text
                    } else {
                        colors.text_muted
                    })
                    .when(is_active, |pill| {
                        pill.bg(colors.element_selected)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .shadow_xs()
                    })
                    .when(!is_active, |pill| {
                        pill.cursor_pointer().hover(|style| {
                            style.bg(colors.ghost_element_hover).text_color(colors.text)
                        })
                    })
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.))
                            .rounded(px(2.))
                            .bg(accents.color_for_index(index as u32)),
                    )
                    .child(
                        div()
                            .max_w(px(180.))
                            .truncate()
                            .child(group.key.display_name(&Default::default())),
                    )
                    .when(needs_attention, |pill| {
                        pill.child(
                            div()
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(attention_color),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .invisible()
                            .group_hover(pill_group, |style| style.visible())
                            .child(
                                KitButton::new(("close-project", index))
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui_kit::assets::IconName::Close)
                                    .tooltip("Close Project")
                                    .on_click(move |_, window, cx| {
                                        // Otherwise the pill under the button switches to the
                                        // project being closed.
                                        cx.stop_propagation();
                                        close_multi_workspace
                                            .update(cx, |multi_workspace, cx| {
                                                multi_workspace
                                                    .remove_project_group(&close_key, window, cx)
                                                    .detach_and_log_err(cx);
                                            })
                                            .log_err();
                                    }),
                            ),
                    )
                    .on_click(move |_, window, cx| {
                        weak_multi_workspace
                            .update(cx, |multi_workspace, cx| {
                                if let Some(sidebar) = multi_workspace.sidebar() {
                                    sidebar.activate_project_group(key.clone(), window, cx);
                                }
                            })
                            .log_err();
                    })
            })
            .collect::<Vec<_>>();
        Some(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .justify_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(!pills.is_empty(), |row| {
                    row.child(
                        h_flex()
                            .p(px(PILL_TRACK_PADDING))
                            .gap(px(2.))
                            .rounded(px(8.))
                            .bg(colors.element_background)
                            .border_1()
                            .border_color(colors.border_variant)
                            .children(pills),
                    )
                })
                .child(
                    KitButton::new("new-project")
                        .ghost()
                        .icon(gpui_kit::assets::IconName::Plus)
                        .label("New")
                        .dropdown_menu(|menu, window, _| {
                            let menu = workspace::style_menu(menu, window);
                            menu.menu_with_icon(
                                "Open Folder…",
                                workspace::menu_icon(MenuIcon::FolderOpen),
                                Box::new(workspace::Open::default()),
                            )
                            .menu_with_icon(
                                "Clone Repository…",
                                workspace::menu_icon(MenuIcon::CloudArrowDown),
                                Box::new(git::Clone),
                            )
                            .menu_with_icon(
                                "Connect to SSH Host…",
                                workspace::menu_icon(MenuIcon::Server),
                                Box::new(zed_actions::OpenRemote::default()),
                            )
                            .menu_with_icon(
                                "Open in Dev Container…",
                                workspace::menu_icon(MenuIcon::Package),
                                Box::new(zed_actions::OpenDevContainer),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_settings_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fs = self.project.read(cx).fs().clone();
        KitButton::new("title-bar-settings")
            .ghost()
            .small()
            .icon(gpui_kit::assets::IconName::Settings)
            .tooltip("Settings and Themes")
            .dropdown_menu(move |menu, window, cx| {
                let menu = workspace::style_menu(menu, window);
                let menu = menu
                    .menu_with_icon(
                        "Settings",
                        workspace::menu_icon(MenuIcon::Settings),
                        Box::new(zed_actions::OpenSettings),
                    )
                    .menu_with_icon(
                        "Extensions",
                        workspace::menu_icon(MenuIcon::Extensions),
                        Box::new(zed_actions::Extensions::default()),
                    )
                    .menu_with_icon(
                        "Keymap",
                        workspace::menu_icon(MenuIcon::Keyboard),
                        Box::new(zed_actions::OpenKeymap),
                    )
                    .separator();
                theme_menu(menu, fs.clone(), cx)
            })
    }

    pub fn new(
        id: impl Into<ElementId>,
        workspace: &Workspace,
        multi_workspace: Option<WeakEntity<MultiWorkspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = workspace.project().clone();
        let user_store = workspace.app_state().user_store.clone();
        let client = workspace.app_state().client.clone();

        let platform_style = PlatformStyle::platform();
        let application_menu = match platform_style {
            PlatformStyle::Mac => {
                if option_env!("ZED_USE_CROSS_PLATFORM_MENU").is_some() {
                    Some(cx.new(|cx| ApplicationMenu::new(window, cx)))
                } else {
                    None
                }
            }
            PlatformStyle::Linux | PlatformStyle::Windows => {
                Some(cx.new(|cx| ApplicationMenu::new(window, cx)))
            }
        };

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.observe(&workspace.weak_handle().upgrade().unwrap(), |_, _, cx| {
                cx.notify()
            }),
        );

        subscriptions.push(cx.observe(&user_store, |_a, _, cx| cx.notify()));
        if let Some(workspace_entity) = workspace.weak_handle().upgrade() {
            subscriptions.push(cx.subscribe(
                &workspace_entity,
                |_, _, event: &workspace::Event, cx| {
                    if matches!(event, workspace::Event::WorktreeCreationChanged) {
                        cx.notify();
                    }
                },
            ));
        }
        if let Some(trusted_worktrees) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(cx.subscribe(&trusted_worktrees, |_, _, _, cx| {
                cx.notify();
            }));
        }

        let update_version = cx.new(|cx| UpdateVersion::new(cx));
        let platform_titlebar = cx.new(|cx| {
            let mut titlebar = PlatformTitleBar::new(id, cx);
            if let Some(mw) = multi_workspace.clone() {
                titlebar = titlebar.with_multi_workspace(mw);
            }
            titlebar
        });

        let banner = None;

        Self {
            platform_titlebar,
            application_menu,
            workspace: workspace.weak_handle(),
            multi_workspace,
            project,
            user_store,
            client,
            _subscriptions: subscriptions,
            banner,
            update_version,
        }
    }

    fn toggle_update_simulation(&mut self, cx: &mut Context<Self>) {
        self.update_version
            .update(cx, |banner, cx| banner.update_simulation(cx));
        cx.notify();
    }

    fn render_remote_project_connection(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let workspace = self.workspace.clone();

        let options = self.project.read(cx).remote_connection_options(cx)?;
        let host: SharedString = options.display_name().into();

        let (nickname, tooltip_title, icon) = match options {
            RemoteConnectionOptions::Ssh(options) => (
                options.nickname.map(|nick| nick.into()),
                "Remote Project",
                IconName::Server,
            ),
            RemoteConnectionOptions::Wsl(_) => (None, "Remote Project", IconName::Linux),
            RemoteConnectionOptions::Docker(_dev_container_connection) => {
                (None, "Dev Container", IconName::Box)
            }
            #[cfg(any(test, feature = "test-support"))]
            RemoteConnectionOptions::Mock(_) => (None, "Mock Remote Project", IconName::Server),
        };

        let nickname = nickname.unwrap_or_else(|| host.clone());

        let (indicator_color, meta) = match self.project.read(cx).remote_connection_state(cx)? {
            remote::ConnectionState::Connecting => (Color::Info, format!("Connecting to: {host}")),
            remote::ConnectionState::Connected => (Color::Success, format!("Connected to: {host}")),
            remote::ConnectionState::HeartbeatMissed => (
                Color::Warning,
                format!("Connection attempt to {host} missed. Retrying..."),
            ),
            remote::ConnectionState::Reconnecting => (
                Color::Warning,
                format!("Lost connection to {host}. Reconnecting..."),
            ),
            remote::ConnectionState::Disconnected => {
                (Color::Error, format!("Disconnected from {host}"))
            }
        };

        let icon_color = match self.project.read(cx).remote_connection_state(cx)? {
            remote::ConnectionState::Connecting => Color::Info,
            remote::ConnectionState::Connected => Color::Default,
            remote::ConnectionState::HeartbeatMissed => Color::Warning,
            remote::ConnectionState::Reconnecting => Color::Warning,
            remote::ConnectionState::Disconnected => Color::Error,
        };

        let meta = SharedString::from(meta);

        Some(
            PopoverMenu::new("remote-project-menu")
                .menu(move |window, cx| {
                    let workspace_entity = workspace.upgrade()?;
                    let fs = workspace_entity.read(cx).project().read(cx).fs().clone();
                    Some(recent_projects::RemoteServerProjects::popover(
                        fs,
                        workspace.clone(),
                        None,
                        window,
                        cx,
                    ))
                })
                .trigger_with_tooltip(
                    ButtonLike::new("remote_project")
                        .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                        .child(
                            h_flex()
                                .gap_2()
                                .max_w_32()
                                .child(
                                    IconWithIndicator::new(
                                        Icon::new(icon).size(IconSize::Small).color(icon_color),
                                        Some(Indicator::dot().color(indicator_color)),
                                    )
                                    .indicator_border_color(Some(
                                        cx.theme().colors().title_bar_background,
                                    ))
                                    .into_any_element(),
                                )
                                .child(Label::new(nickname).size(LabelSize::Small).truncate()),
                        ),
                    move |_window, cx| {
                        Tooltip::with_meta(
                            tooltip_title,
                            Some(&OpenRemote::default()),
                            meta.clone(),
                            cx,
                        )
                    },
                )
                .anchor(gpui::Anchor::TopLeft)
                .into_any_element(),
        )
    }

    pub fn render_restricted_mode(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let has_restricted_worktrees =
            TrustedWorktrees::has_restricted_worktrees(&self.project.read(cx).worktree_store(), cx);
        if !has_restricted_worktrees {
            return None;
        }

        let button = Button::new("restricted_mode_trigger", "Restricted Mode")
            .style(ButtonStyle::Tinted(TintColor::Warning))
            .label_size(LabelSize::Small)
            .color(Color::Warning)
            .start_icon(
                Icon::new(IconName::Warning)
                    .size(IconSize::Small)
                    .color(Color::Warning),
            )
            .tooltip(|_, cx| {
                Tooltip::with_meta(
                    "You're in Restricted Mode",
                    Some(&ToggleWorktreeSecurity),
                    "Mark this project as trusted and unlock all features",
                    cx,
                )
            })
            .on_click({
                cx.listener(move |this, _, window, cx| {
                    this.workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_worktree_trust_security_modal(true, window, cx)
                        })
                        .log_err();
                })
            });

        if ui::utils::MACOS_SDK_26_OR_LATER {
            // Make up for Tahoe's traffic light buttons having less spacing around them
            Some(div().child(button).ml_0p5().into_any_element())
        } else {
            Some(button.into_any_element())
        }
    }

    pub fn render_project_host(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.project.read(cx).is_via_remote_server() {
            return self.render_remote_project_connection(cx);
        }

        if self.project.read(cx).is_disconnected(cx) {
            return Some(
                Button::new("disconnected", "Disconnected")
                    .disabled(true)
                    .color(Color::Disabled)
                    .label_size(LabelSize::Small)
                    .into_any_element(),
            );
        }

        let host = self.project.read(cx).host()?;
        let host_user = self.user_store.read(cx).get_cached_user(host.user_id)?;
        let participant_index = self
            .user_store
            .read(cx)
            .participant_indices()
            .get(&host_user.legacy_id)?;

        Some(
            Button::new("project_owner_trigger", host_user.username.clone())
                .color(Color::Player(participant_index.0))
                .label_size(LabelSize::Small)
                .tab_index(0isize)
                .tooltip(move |_, cx| {
                    let tooltip_title = format!(
                        "{} is sharing this project. Click to follow.",
                        host_user.username
                    );

                    Tooltip::with_meta(tooltip_title, None, "Click to Follow", cx)
                })
                .on_click({
                    let host_peer_id = host.peer_id;
                    cx.listener(move |this, _, window, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| {
                                workspace.follow(host_peer_id, window, cx);
                            })
                            .log_err();
                    })
                })
                .into_any_element(),
        )
    }

    fn render_connection_status(
        &self,
        status: &client::Status,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match status {
            client::Status::ConnectionError
            | client::Status::ConnectionLost
            | client::Status::Reauthenticating
            | client::Status::Reconnecting
            | client::Status::ReconnectionError { .. } => Some(
                div()
                    .id("disconnected")
                    .child(Icon::new(IconName::Disconnected).size(IconSize::Small))
                    .tooltip(Tooltip::text("Disconnected"))
                    .into_any_element(),
            ),
            client::Status::UpgradeRequired => {
                let auto_updater = auto_update::AutoUpdater::get(cx);
                let label = match auto_updater.map(|auto_update| auto_update.read(cx).status()) {
                    Some(AutoUpdateStatus::Updated { .. }) => "Please restart Zed to Collaborate",
                    Some(AutoUpdateStatus::Installing { .. })
                    | Some(AutoUpdateStatus::Downloading { .. })
                    | Some(AutoUpdateStatus::Checking) => "Updating...",
                    Some(AutoUpdateStatus::Idle)
                    | Some(AutoUpdateStatus::Errored { .. })
                    | None => "Please update Zed to Collaborate",
                };

                Some(
                    Button::new("connection-status", label)
                        .label_size(LabelSize::Small)
                        .on_click(|_, window, cx| {
                            if let Some(auto_updater) = auto_update::AutoUpdater::get(cx)
                                && auto_updater.read(cx).status().is_updated()
                            {
                                workspace::reload(cx);
                                return;
                            }
                            auto_update::check(&Default::default(), window, cx);
                        })
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }

    pub fn render_sign_in_button(&mut self, _: &mut Context<Self>) -> Button {
        let client = self.client.clone();
        let workspace = self.workspace.clone();
        Button::new("sign_in", "Sign In")
            .label_size(LabelSize::Small)
            .tab_index(0isize)
            .on_click(move |_, window, cx| {
                let client = client.clone();
                let workspace = workspace.clone();
                window
                    .spawn(cx, async move |mut cx| {
                        client
                            .sign_in_with_optional_connect(true, cx)
                            .await
                            .notify_workspace_async_err(workspace, &mut cx);
                    })
                    .detach();
            })
    }
}
