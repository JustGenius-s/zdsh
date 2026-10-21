use crate::{
    AgentPanel, ConversationView, OpenActiveThreadAsMarkdown,
    thread_metadata_store::ThreadMetadataStore,
};
use acp_thread::ThreadStatus;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, WeakEntity};
use settings::Settings;
use theme_settings::ThemeSettings;
use ui::{prelude::*, utils::WithRemSize};
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

    /// Shows `conversation` in this tab in place of the one it shows, as when an empty draft
    /// switches to another agent.
    pub(crate) fn replace_conversation(
        &mut self,
        conversation: Entity<ConversationView>,
        cx: &mut Context<Self>,
    ) {
        self._conversation_subscriptions = Self::subscribe_to_conversation(&conversation, cx);
        self.conversation = conversation;
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
