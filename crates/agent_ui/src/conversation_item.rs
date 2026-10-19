use crate::{
    AgentPanel, ConversationView, OpenActiveThreadAsMarkdown, ToggleNewThreadMenu,
    ToggleOptionsMenu, thread_metadata_store::ThreadMetadataStore,
};
use acp_thread::ThreadStatus;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, WeakEntity};
use settings::Settings;
use theme_settings::ThemeSettings;
use ui::{
    ContextMenu, Indicator, PopoverMenu, PopoverMenuHandle, Tooltip, prelude::*, utils::WithRemSize,
};
use util::ResultExt;
use workspace::{
    CloseWorkspaceSidebar, DraggedSidebar, FocusWorkspaceSidebar, MultiWorkspace,
    ToggleWorkspaceSidebar,
    item::{Item, ItemEvent, TabContentParams},
};
use zed_actions::{
    DecreaseBufferFontSize, IncreaseBufferFontSize, ResetBufferFontSize,
    agent::{LogoutAgent, ReauthenticateAgent},
};

/// A single conversation owned by a workspace tab. Moving the tab preserves its session.
pub(crate) struct ConversationItem {
    conversation: Entity<ConversationView>,
    panel: WeakEntity<AgentPanel>,
    focus_handle: FocusHandle,
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
    content_bounds: gpui::Bounds<Pixels>,
    new_thread_menu: PopoverMenuHandle<ContextMenu>,
    options_menu: PopoverMenuHandle<ContextMenu>,
    _subscriptions: Vec<Subscription>,
}

impl ConversationItem {
    pub(crate) fn new(
        conversation: Entity<ConversationView>,
        panel: WeakEntity<AgentPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let multi_workspace = window.root::<MultiWorkspace>().flatten();
        let mut subscriptions = vec![cx.observe(&conversation, |_, _, cx| {
            cx.emit(ItemEvent::UpdateTab);
            cx.notify();
        })];
        if let Some(multi_workspace) = &multi_workspace {
            subscriptions.push(cx.observe(multi_workspace, |_, _, cx| cx.notify()));
        }
        subscriptions.push(cx.subscribe(
            &conversation,
            |_, _, _: &crate::conversation_view::RootThreadUpdated, cx| {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            },
        ));
        if let Some(store) = ThreadMetadataStore::try_global(cx) {
            subscriptions.push(cx.observe(&store, |_, _, cx| {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }));
        }
        subscriptions.push(cx.on_focus_in(&focus_handle, window, |this, window, cx| {
            let conversation = this.conversation.clone();
            let panel = this.panel.clone();
            if this.focus_handle.is_focused(window) {
                conversation
                    .read(cx)
                    .activation_focus_handle(cx)
                    .focus(window, cx);
            }
            window.defer(cx, move |window, cx| {
                panel
                    .update(cx, |panel, cx| {
                        panel.select_conversation_item(conversation, window, cx)
                    })
                    .log_err();
            });
        }));
        // Pane moves also call Item::on_removed; release runs only after the last tab owner drops it.
        let item_id = cx.entity_id();
        cx.on_release(move |this, cx| {
            if let Some(multi_workspace) =
                this.multi_workspace.as_ref().and_then(WeakEntity::upgrade)
            {
                cx.defer(move |cx| {
                    multi_workspace.update(cx, |workspace, cx| {
                        workspace.release_sidebar_item(item_id, cx)
                    });
                });
            }
            let id = this.conversation.read(cx).thread_id;
            this.conversation
                .update(cx, |view, cx| view.cancel_all_generation(cx));
            if let Some(panel) = this.panel.upgrade() {
                panel.update(cx, |panel, cx| panel.release_conversation_item(id, cx));
            }
        })
        .detach();
        Self {
            conversation,
            panel,
            focus_handle,
            multi_workspace: multi_workspace.map(|workspace| workspace.downgrade()),
            content_bounds: gpui::Bounds::default(),
            new_thread_menu: PopoverMenuHandle::default(),
            options_menu: PopoverMenuHandle::default(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn conversation_id(&self) -> gpui::EntityId {
        self.conversation.entity_id()
    }

    pub(crate) fn toggle_options_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.options_menu.toggle(window, cx);
    }

    pub(crate) fn toggle_new_thread_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_thread_menu.toggle(window, cx);
    }

    fn history(&self, cx: &Context<Self>) -> Option<gpui::AnyView> {
        self.multi_workspace
            .as_ref()?
            .upgrade()?
            .read(cx)
            .sidebar_for_item(cx.entity_id())
    }

    pub(crate) fn show_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.multi_workspace.as_ref().and_then(WeakEntity::upgrade) {
            let item_id = cx.entity_id();
            workspace.update(cx, |workspace, cx| {
                workspace.show_sidebar_in_item(item_id, window, cx)
            });
        }
    }

    fn close_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history(cx).is_some() {
            if let Some(workspace) = self.multi_workspace.as_ref().and_then(WeakEntity::upgrade) {
                workspace.update(cx, |workspace, cx| workspace.close_sidebar(window, cx));
            }
            self.conversation
                .read(cx)
                .activation_focus_handle(cx)
                .focus(window, cx);
        }
    }

    pub(crate) fn toggle_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history(cx).is_some() {
            self.close_history(window, cx);
        } else {
            self.show_history(window, cx);
        }
    }

    pub(crate) fn focus_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.multi_workspace.as_ref().and_then(WeakEntity::upgrade)
            && self.history(cx).is_some()
        {
            workspace.update(cx, |workspace, cx| workspace.focus_sidebar(window, cx));
        } else {
            self.show_history(window, cx);
        }
    }

    fn render_history(
        &self,
        history: gpui::AnyView,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("thread-history")
            .relative()
            .h_full()
            .w(width)
            .flex_none()
            .border_l_1()
            .border_color(cx.theme().colors().border)
            .occlude()
            .child(history)
            .child(gpui::deferred(
                div()
                    .id("thread-history-resize")
                    .absolute()
                    .left(px(-3.))
                    .top_0()
                    .h_full()
                    .w(px(6.))
                    .cursor_col_resize()
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_drag(DraggedSidebar, |dragged, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| dragged.clone())
                    })
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                            if let Some(workspace) =
                                this.multi_workspace.as_ref().and_then(WeakEntity::upgrade)
                            {
                                workspace.update(cx, |workspace, cx| {
                                    if event.click_count == 2
                                        && let Some(sidebar) = workspace.sidebar()
                                    {
                                        sidebar.set_width(None, cx);
                                    }
                                    workspace.serialize(cx);
                                    cx.notify();
                                });
                            }
                        }),
                    ),
            ))
            .into_any_element()
    }

    fn with_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut AgentPanel, &mut Window, &mut Context<AgentPanel>),
    ) {
        self.panel
            .update(cx, |panel, cx| {
                panel.select_conversation_item(self.conversation.clone(), window, cx);
                action(panel, window, cx);
            })
            .log_err();
    }

    fn status(&self, cx: &App) -> Option<(&'static str, Color)> {
        let view = self.conversation.read(cx);
        if view.is_loading() {
            return Some(("Connecting", Color::Muted));
        }
        if view
            .as_connected()
            .is_some_and(|connected| connected.has_thread_error(cx))
        {
            return Some(("Error", Color::Error));
        }
        if let Some(thread) = view.root_thread(cx) {
            let thread = thread.read(cx);
            if thread.is_waiting_for_confirmation() {
                return Some(("Approval", Color::Warning));
            }
            if thread.status() == ThreadStatus::Generating {
                return Some(("Working", Color::Accent));
            }
        }
        None
    }
}

impl EventEmitter<ItemEvent> for ConversationItem {}
impl Focusable for ConversationItem {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for ConversationItem {
    type Event = ItemEvent;
    fn tab_content_text(&self, _: usize, cx: &App) -> SharedString {
        let view = self.conversation.read(cx);
        view.title_override(cx).unwrap_or_else(|| view.title(cx))
    }
    fn tab_content(&self, params: TabContentParams, _: &Window, cx: &App) -> AnyElement {
        h_flex()
            .gap_1()
            .child(
                Icon::new(IconName::Sparkle)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(self.tab_content_text(0, cx))
                    .single_line()
                    .color(params.text_color()),
            )
            .when_some(self.status(cx), |this, (status, color)| {
                this.child(Label::new(status).size(LabelSize::Small).color(color))
            })
            .into_any_element()
    }
    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(
            format!(
                "{}\nClosing this tab stops active work.",
                self.tab_content_text(0, cx)
            )
            .into(),
        )
    }
    fn to_item_events(event: &ItemEvent, emit: &mut dyn FnMut(ItemEvent)) {
        emit(*event);
    }
    fn show_toolbar(&self) -> bool {
        false
    }
}

impl Render for ConversationItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let history = self.history(cx);
        let history_open = history.is_some();
        let has_notifications = self
            .multi_workspace
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|workspace| workspace.read(cx).sidebar_has_notifications(cx));
        let overlay = self.content_bounds.size.width < px(720.);
        let history_width = self
            .multi_workspace
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|workspace| {
                workspace
                    .read(cx)
                    .sidebar()
                    .map(|sidebar| sidebar.width(cx))
            })
            .unwrap_or(px(280.))
            .min(if overlay {
                self.content_bounds.size.width
            } else {
                self.content_bounds.size.width - px(360.)
            })
            .max(px(0.));
        let history = history.map(|history| self.render_history(history, history_width, cx));
        let panel = self.panel.clone();
        let options_panel = panel.clone();
        let focus_handle = self.focus_handle.clone();
        let new_menu_focus = focus_handle.clone();
        let conversation = self.conversation.clone();
        let drag_target = panel
            .update(cx, |panel, cx| {
                panel.render_drag_target(Some(conversation.clone()), cx)
            })
            .log_err();
        let sandbox_status = conversation
            .read(cx)
            .root_thread_view()
            .and_then(|view| view.update(cx, |view, cx| view.render_sandbox_status(cx)));
        let agent = conversation.read(cx).agent_key().label();
        let theme = ThemeSettings::get_global(cx);
        WithRemSize::new(theme.agent_ui_font_size(cx))
            .size_full()
            .font_family(theme.agent_ui_font_family().clone())
            .child(
                v_flex()
                    .key_context("AgentPanel")
                    .track_focus(&self.focus_handle)
                    .relative()
                    .size_full()
                    .min_h_0()
                    .bg(cx.theme().colors().toolbar_background)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.with_panel(window, cx, |_, _, _| {});
                            cx.propagate();
                        }),
                    )
                    .on_action(
                        cx.listener(|this, action: &IncreaseBufferFontSize, window, cx| {
                            this.with_panel(window, cx, |panel, window, cx| {
                                panel.increase_font_size(action, window, cx)
                            })
                        }),
                    )
                    .on_action(
                        cx.listener(|this, action: &DecreaseBufferFontSize, window, cx| {
                            this.with_panel(window, cx, |panel, window, cx| {
                                panel.decrease_font_size(action, window, cx)
                            })
                        }),
                    )
                    .on_action(
                        cx.listener(|this, action: &ResetBufferFontSize, window, cx| {
                            this.with_panel(window, cx, |panel, window, cx| {
                                panel.reset_font_size(action, window, cx)
                            })
                        }),
                    )
                    .on_action(cx.listener(
                        |this, action: &OpenActiveThreadAsMarkdown, window, cx| {
                            this.with_panel(window, cx, |panel, window, cx| {
                                panel.open_active_thread_as_markdown(action, window, cx)
                            })
                        },
                    ))
                    .on_action(cx.listener(|this, _: &ReauthenticateAgent, window, cx| {
                        this.conversation
                            .update(cx, |view, cx| view.reauthenticate(window, cx))
                    }))
                    .on_action(cx.listener(|this, _: &LogoutAgent, window, cx| {
                        this.conversation
                            .update(cx, |view, cx| view.logout(window, cx))
                    }))
                    .on_action(cx.listener(|this, _: &ToggleNewThreadMenu, window, cx| {
                        cx.stop_propagation();
                        this.new_thread_menu.toggle(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ToggleOptionsMenu, window, cx| {
                        cx.stop_propagation();
                        this.options_menu.toggle(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ToggleWorkspaceSidebar, window, cx| {
                        cx.stop_propagation();
                        this.toggle_history(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &FocusWorkspaceSidebar, window, cx| {
                        cx.stop_propagation();
                        this.focus_history(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &CloseWorkspaceSidebar, window, cx| {
                        cx.stop_propagation();
                        this.close_history(window, cx);
                    }))
                    .child(
                        WithRemSize::new(theme.ui_font_size(cx))
                            .h_flex()
                            .font_family(theme.ui_font.family.clone())
                            .flex_none()
                            .justify_between()
                            .gap(DynamicSpacing::Base08.rems(cx))
                            .px(DynamicSpacing::Base08.rems(cx))
                            .py(DynamicSpacing::Base06.rems(cx))
                            .bg(cx.theme().colors().toolbar_background)
                            .border_b_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(
                                h_flex()
                                    .h_8()
                                    .gap_2()
                                    .child(Label::new(agent).color(Color::Muted))
                                    .children(sandbox_status),
                            )
                            .child(
                                h_flex()
                                    .h_8()
                                    .gap_1()
                                    .child(
                                        IconButton::new("thread-history-toggle", IconName::Chat)
                                            .toggle_state(history_open)
                                            .when(has_notifications, |button| {
                                                button.indicator(
                                                    Indicator::dot().color(Color::Accent),
                                                )
                                            })
                                            .aria_label("Thread history")
                                            .tooltip(|_, cx| {
                                                Tooltip::for_action(
                                                    "Thread History",
                                                    &ToggleWorkspaceSidebar,
                                                    cx,
                                                )
                                            })
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.toggle_history(window, cx)
                                            })),
                                    )
                                    .child(
                                        PopoverMenu::new("new-agent-tab")
                                            .trigger(
                                                IconButton::new("new-agent-tab", IconName::Plus)
                                                    .tooltip(Tooltip::text("New Agent Chat")),
                                            )
                                            .anchor(gpui::Anchor::TopRight)
                                            .with_handle(self.new_thread_menu.clone())
                                            .menu(move |window, cx| {
                                                let builder = panel
                                                    .update(cx, |panel, cx| {
                                                        panel.new_thread_menu_builder(
                                                            new_menu_focus.clone(),
                                                            cx,
                                                        )
                                                    })
                                                    .log_err()?;
                                                builder(window, cx)
                                            }),
                                    )
                                    .child(
                                        PopoverMenu::new("agent-tab-options")
                                            .trigger(
                                                IconButton::new(
                                                    "agent-tab-options",
                                                    IconName::Ellipsis,
                                                )
                                                .tooltip(Tooltip::text("Chat Options")),
                                            )
                                            .anchor(gpui::Anchor::TopRight)
                                            .with_handle(self.options_menu.clone())
                                            .menu(move |window, cx| {
                                                let builder = options_panel
                                                    .update(cx, |panel, cx| {
                                                        panel.options_menu_builder(
                                                            Some(conversation.clone()),
                                                            focus_handle.clone(),
                                                            cx,
                                                        )
                                                    })
                                                    .log_err()?;
                                                builder(window, cx)
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .child({
                                let this = cx.weak_entity();
                                gpui::canvas(
                                    move |bounds, _, cx| {
                                        this.update(cx, |this, cx| {
                                            if this.content_bounds != bounds {
                                                this.content_bounds = bounds;
                                                cx.notify();
                                            }
                                        })
                                        .log_err();
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full()
                            })
                            .on_drag_move(cx.listener(
                                |this, event: &gpui::DragMoveEvent<DraggedSidebar>, _, cx| {
                                    if this.history(cx).is_none() {
                                        return;
                                    }
                                    if let Some(workspace) =
                                        this.multi_workspace.as_ref().and_then(WeakEntity::upgrade)
                                    {
                                        let width =
                                            this.content_bounds.right() - event.event.position.x;
                                        workspace.update(cx, |workspace, cx| {
                                            if let Some(sidebar) = workspace.sidebar() {
                                                sidebar.set_width(Some(width), cx);
                                            }
                                            cx.notify();
                                        });
                                    }
                                },
                            ))
                            .map(|body| {
                                let conversation = div()
                                    .relative()
                                    .h_full()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.conversation.clone())
                                    .children(drag_target);
                                if overlay {
                                    body.child(conversation)
                                        .when_some(history, |body, history| {
                                            body.child(gpui::deferred(
                                                div()
                                                    .absolute()
                                                    .size_full()
                                                    .inset_0()
                                                    .child(
                                                        div()
                                                            .absolute()
                                                            .size_full()
                                                            .occlude()
                                                            .bg(cx
                                                                .theme()
                                                                .colors()
                                                                .background
                                                                .opacity(0.5))
                                                            .on_mouse_down(
                                                                gpui::MouseButton::Left,
                                                                cx.listener(
                                                                    |this, _, window, cx| {
                                                                        cx.stop_propagation();
                                                                        this.close_history(
                                                                            window, cx,
                                                                        );
                                                                    },
                                                                ),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .absolute()
                                                            .top_0()
                                                            .bottom_0()
                                                            .right_0()
                                                            .child(history),
                                                    ),
                                            ))
                                        })
                                } else {
                                    body.child(conversation).children(history)
                                }
                            }),
                    ),
            )
    }
}
