use crate::{
    AgentPanel, ConversationView, OpenActiveThreadAsMarkdown, ToggleNewThreadMenu,
    ToggleOptionsMenu, thread_metadata_store::ThreadMetadataStore,
};
use acp_thread::ThreadStatus;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, WeakEntity};
use settings::Settings;
use theme_settings::ThemeSettings;
use ui::{ContextMenu, PopoverMenu, PopoverMenuHandle, Tooltip, prelude::*, utils::WithRemSize};
use util::ResultExt;
use workspace::item::{Item, ItemEvent, TabContentParams};
use zed_actions::{
    DecreaseBufferFontSize, IncreaseBufferFontSize, ResetBufferFontSize,
    agent::{LogoutAgent, ReauthenticateAgent},
};

/// A single conversation owned by a workspace tab. Moving the tab preserves its session.
pub(crate) struct ConversationItem {
    conversation: Entity<ConversationView>,
    panel: WeakEntity<AgentPanel>,
    focus_handle: FocusHandle,
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
        let mut subscriptions = vec![cx.observe(&conversation, |_, _, cx| {
            cx.emit(ItemEvent::UpdateTab);
            cx.notify();
        })];
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
        cx.on_release(move |this, cx| {
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
                            .child(
                                div()
                                    .relative()
                                    .h_full()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.conversation.clone())
                                    .children(drag_target),
                            ),
                    ),
            )
    }
}
