use crate::{
    AgentPanel, ConversationView, OpenActiveThreadAsMarkdown,
    thread_metadata_store::ThreadMetadataStore,
};
use acp_thread::ThreadStatus;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, WeakEntity, Window,
};
use settings::Settings;
use theme_settings::ThemeSettings;
use ui::{prelude::*, utils::WithRemSize};
use util::ResultExt;
use workspace::item::{Item, ItemEvent, TabCategory, TabContentParams};
use zed_actions::{
    DecreaseBufferFontSize, IncreaseBufferFontSize, ResetBufferFontSize,
    agent::{LogoutAgent, ReauthenticateAgent},
};

/// A single conversation owned by a workspace tab. Moving the tab preserves its session.
pub(crate) struct ConversationItem {
    conversation: Entity<ConversationView>,
    panel: WeakEntity<AgentPanel>,
    focus_handle: FocusHandle,
    overlay_expanded: Option<bool>,
    _conversation_subscriptions: [Subscription; 2],
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
        let mut subscriptions = Vec::new();
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
            _conversation_subscriptions: Self::subscribe_to_conversation(&conversation, cx),
            conversation,
            panel,
            focus_handle,
            overlay_expanded: None,
            _subscriptions: subscriptions,
        }
    }

    fn subscribe_to_conversation(
        conversation: &Entity<ConversationView>,
        cx: &mut Context<Self>,
    ) -> [Subscription; 2] {
        [
            cx.observe(conversation, |_, _, cx| {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }),
            cx.subscribe(
                conversation,
                |_, _, _: &crate::conversation_view::RootThreadUpdated, cx| {
                    cx.emit(ItemEvent::UpdateTab);
                    cx.notify();
                },
            ),
        ]
    }

    pub(crate) fn conversation_id(&self) -> gpui::EntityId {
        self.conversation.entity_id()
    }

    pub(crate) fn overlay_thread(
        &self,
        cx: &App,
    ) -> Option<Entity<crate::conversation_view::ThreadView>> {
        self.conversation.read(cx).root_thread_view()
    }

    pub(crate) fn overlay_needs_attention(&self, cx: &App) -> bool {
        let view = self.conversation.read(cx);
        view.has_pending_request_elicitations(cx)
            || view.as_connected().is_some_and(|connected| {
                connected.has_thread_error(cx)
                    || connected.threads.values().any(|thread| {
                        thread
                            .read(cx)
                            .thread
                            .read(cx)
                            .is_waiting_for_confirmation()
                    })
            })
    }

    pub(crate) fn overlay_composer_focus_handle(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<FocusHandle> {
        if self.focus_handle.contains_focused(window, cx)
            && let Some(thread) = self.conversation.read(cx).active_thread()
        {
            Some(thread.read(cx).message_editor.focus_handle(cx))
        } else {
            None
        }
    }

    pub(crate) fn set_overlay_presentation(
        &mut self,
        expanded: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        if self.overlay_expanded == expanded {
            return;
        }
        self.overlay_expanded = expanded;
        self.conversation
            .update(cx, |view, cx| view.set_overlay_presentation(expanded, cx));
        cx.notify();
    }

    /// Shows `conversation` in this tab in place of the one it shows, as when an empty draft
    /// switches to another agent.
    pub(crate) fn replace_conversation(
        &mut self,
        conversation: Entity<ConversationView>,
        cx: &mut Context<Self>,
    ) {
        self._conversation_subscriptions = Self::subscribe_to_conversation(&conversation, cx);
        self.conversation = conversation;
        self.conversation.update(cx, |view, cx| {
            view.set_overlay_presentation(self.overlay_expanded, cx)
        });
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
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

    fn tab_category(&self, _: &App) -> TabCategory {
        TabCategory::Agent
    }
    fn tab_status(&self, cx: &App) -> Option<(SharedString, Color)> {
        self.status(cx).map(|(label, color)| (label.into(), color))
    }
    fn tab_content(&self, params: TabContentParams, _: &Window, cx: &App) -> AnyElement {
        h_flex()
            .min_w_0()
            .gap_1()
            .child(
                Icon::new(IconName::Sparkle)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(self.tab_content_text(0, cx))
                    .truncate()
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

    fn handle_drop(
        &self,
        _active_pane: &workspace::Pane,
        dropped: &dyn std::any::Any,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.conversation.update(cx, |conversation, cx| {
            conversation.handle_path_drop(dropped, window, cx)
        })
    }
}

impl Render for ConversationItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let conversation = self.conversation.clone();
        let drag_target = self
            .panel
            .update(cx, |panel, cx| {
                panel.render_drag_target(Some(conversation), cx)
            })
            .log_err();
        let theme = ThemeSettings::get_global(cx);
        let floating = self.overlay_expanded.is_some();
        WithRemSize::new(theme.agent_ui_font_size(cx))
            .w_full()
            .when(!floating, |this| this.h_full())
            .font_family(theme.agent_ui_font_family().clone())
            .child(
                v_flex()
                    .key_context(if floating {
                        "AgentPanel ConversationOverlay"
                    } else {
                        "AgentPanel"
                    })
                    .track_focus(&self.focus_handle)
                    .relative()
                    .w_full()
                    .when(!floating, |this| this.h_full())
                    .min_h_0()
                    .when(!floating, |this| {
                        this.bg(cx.theme().colors().toolbar_background)
                    })
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
                    .child(
                        h_flex()
                            .relative()
                            .w_full()
                            .when(!floating, |this| this.flex_1().overflow_hidden())
                            .min_h_0()
                            .min_w_0()
                            .child(
                                div()
                                    .relative()
                                    .when(!floating, |this| this.h_full())
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.conversation.clone())
                                    .children(drag_target),
                            ),
                    ),
            )
    }
}
