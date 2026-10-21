use super::composer::{PillButton, composer_surface};
use super::tool_calls::markdown_line;
use super::*;
use crate::entry_view_state::{ExploreGroup, is_hidden_canceled_tool_call};
use gpui::{AnyView, FontWeight, TextStyleRefinement, transparent_white};
use gpui_kit::component::{
    Colorize as _, Icon as KitIcon, Size as KitSize, Theme as KitTheme, alert::AlertVariant,
    shimmer::ShimmerText,
};
use markdown::HeadingLevelStyles;
use theme_settings::ThemeSettings;

impl ThreadView {
    pub(super) fn render_entries(&mut self, cx: &mut Context<Self>) -> List {
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
        let centered_container = move |content: AnyElement| {
            h_flex().w_full().justify_center().child(
                div()
                    .when_some(max_content_width, |this, max_w| this.max_w(max_w))
                    .w_full()
                    .child(content),
            )
        };

        list(
            self.list_state.clone(),
            cx.processor(move |this, index: usize, window, cx| {
                let entries = this.thread.read(cx).entries();
                if let Some(entry) = entries.get(index) {
                    let rendered = this.render_entry(index, entries.len(), entry, window, cx);
                    centered_container(rendered.into_any_element()).into_any_element()
                } else if this.generating_indicator_in_list {
                    let confirmation = this.thread.read(cx).is_waiting_for_confirmation()
                        || this.has_pending_request_elicitation(cx);
                    let rendered = this.render_generating(confirmation, cx);
                    centered_container(rendered.into_any_element()).into_any_element()
                } else {
                    Empty.into_any()
                }
            }),
        )
        .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
        .flex_grow_1()
    }

    /// The prompt of the turn at the top of the transcript, once the prompt itself has scrolled
    /// out of view.
    pub(super) fn render_sticky_prompt(&self, cx: &Context<Self>) -> Option<StickyPrompt> {
        let entries = self.thread.read(cx).entries();
        // The list knows its top item from the last layout, which is a frame behind at most.
        let top_entry_ix = self
            .list_state
            .logical_scroll_top()
            .item_ix
            .min(entries.len().checked_sub(1)?);
        let (prompt_entry_ix, message) = entries
            .get(..=top_entry_ix)?
            .iter()
            .enumerate()
            .rev()
            .find_map(|(entry_ix, entry)| match entry {
                AgentThreadEntry::UserMessage(message)
                    if !message.indented && !message.content.source_blocks().is_empty() =>
                {
                    Some((entry_ix, message))
                }
                _ => None,
            })?;
        if prompt_entry_ix == top_entry_ix {
            return None;
        }

        let text = message
            .content
            .markdowns()
            .map(|markdown| markdown_line(markdown.read(cx)).0)
            .filter(|line| !line.trim().is_empty())
            .join(" ");
        if text.is_empty() {
            return None;
        }

        Some(StickyPrompt::new(
            "sticky-prompt",
            text,
            cx.listener(move |this, _, _, cx| {
                this.list_state.scroll_to(ListOffset {
                    item_ix: prompt_entry_ix,
                    offset_in_item: px(0.),
                });
                cx.notify();
            }),
        ))
    }

    pub(super) fn render_entry(
        &self,
        entry_ix: usize,
        total_entries: usize,
        entry: &AgentThreadEntry,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_indented = entry.is_indented();
        let is_first_indented = is_indented
            && self
                .thread
                .read(cx)
                .entries()
                .get(entry_ix.saturating_sub(1))
                .is_none_or(|entry| !entry.is_indented());

        let mut assistant_message_is_blank = false;

        let primary = match &entry {
            AgentThreadEntry::UserMessage(message) => {
                let Some(prompt) = self.render_user_message(
                    entry_ix,
                    message,
                    is_indented,
                    is_first_indented,
                    window,
                    cx,
                ) else {
                    return Empty.into_any_element();
                };
                prompt
            }
            AgentThreadEntry::AssistantMessage(message) => {
                match self.render_assistant_message(entry_ix, total_entries, message, window, cx) {
                    Some(response) => response,
                    None => {
                        assistant_message_is_blank = true;
                        Empty.into_any()
                    }
                }
            }
            AgentThreadEntry::ToolCall(tool_call) => {
                if is_hidden_canceled_tool_call(tool_call, cx) {
                    return Empty.into_any();
                }

                let session_id = self.thread.read(cx).session_id();
                let focus_handle = self.focus_handle(cx);
                let tool_call =
                    match ExploreGroup::containing(self.thread.read(cx).entries(), entry_ix, cx) {
                        Some(group) => self.render_explore_group_member(
                            session_id,
                            entry_ix,
                            tool_call,
                            group,
                            &focus_handle,
                            window,
                            cx,
                        ),
                        None => Some(self.render_any_tool_call(
                            session_id,
                            entry_ix,
                            tool_call,
                            &focus_handle,
                            ToolCallLayout::Standalone,
                            window,
                            cx,
                        )),
                    };
                let entry_focus_handle = self
                    .entry_view_state
                    .read(cx)
                    .entry(entry_ix)
                    .and_then(|entry| entry.focus_handle(cx));

                match (tool_call, entry_focus_handle) {
                    (Some(tool_call), Some(handle)) => tool_call.track_focus(&handle).into_any(),
                    (Some(tool_call), None) => tool_call.into_any(),
                    (None, _) => Empty.into_any(),
                }
            }
            AgentThreadEntry::Elicitation(elicitation_id) => {
                let thread = self.thread.read(cx);
                if let Some((_, elicitation)) = thread.elicitation(elicitation_id)
                    && should_render_elicitation(elicitation)
                {
                    let elicitation = self.render_elicitation(entry_ix, elicitation, window, cx);

                    if let Some(handle) = self
                        .entry_view_state
                        .read(cx)
                        .entry(entry_ix)
                        .and_then(|entry| entry.focus_handle(cx))
                    {
                        elicitation.track_focus(&handle).into_any()
                    } else {
                        elicitation.into_any()
                    }
                } else {
                    Empty.into_any()
                }
            }
            AgentThreadEntry::ContextCompaction(compaction) => {
                self.render_context_compaction(entry_ix, compaction, window, cx)
            }
        };

        let is_subagent_output = self.is_subagent()
            && matches!(entry, AgentThreadEntry::AssistantMessage(msg) if msg.is_subagent_output);

        let primary = if is_subagent_output {
            v_flex()
                .w_full()
                .child(
                    div().px(COLUMN_INSET).pt(ROW_GAP).child(TranscriptDivider::new(
                        ActivityRow::new("subagent_output", "Subagent Output").tooltip(Tooltip::text(
                            "Everything below this line was sent as output from this subagent to the main agent.",
                        )),
                    )),
                )
                .child(primary)
                .into_any_element()
        } else {
            primary
        };

        let thread = self.thread.clone();

        let primary = if is_indented {
            let line_top = if is_first_indented {
                rems_from_px(-12.0_f32)
            } else {
                rems_from_px(0.0_f32)
            };

            div()
                .relative()
                .w_full()
                .pl_5()
                .bg(cx.theme().colors().panel_background.opacity(0.2))
                .child(
                    div()
                        .absolute()
                        .left(rems_from_px(18.0_f32))
                        .top(line_top)
                        .bottom_0()
                        .w_px()
                        .bg(KitTheme::global(cx).border),
                )
                .child(primary)
                .into_any_element()
        } else {
            primary
        };

        let is_generating = matches!(thread.read(cx).status(), ThreadStatus::Generating);

        let is_turn_end = Self::entry_is_finalized_turn_end(thread.read(cx).entries(), entry_ix)
            .unwrap_or(!is_generating);

        let primary = if is_turn_end && !assistant_message_is_blank {
            let user_message_index = {
                let thread = thread.read(cx);
                thread
                    .entries()
                    .iter()
                    .take(entry_ix)
                    .rposition(|entry| thread.is_user_authored_scroll_target(entry))
            };

            v_flex()
                .w_full()
                .child(primary)
                .child(self.render_thread_controls(
                    &thread,
                    entry_ix,
                    Some(entry_ix),
                    entry_ix + 1 == total_entries,
                    user_message_index,
                    cx,
                ))
                .into_any_element()
        } else {
            primary
        };

        let is_assistant = matches!(entry, AgentThreadEntry::AssistantMessage(_));

        let comments_editor = self.thread_feedback.comments_editor.clone();

        let primary = if entry_ix + 1 == total_entries {
            let last_assistant_index = thread
                .read(cx)
                .entries()
                .iter()
                .rposition(|entry| matches!(entry, AgentThreadEntry::AssistantMessage(_)));

            v_flex()
                .w_full()
                .child(primary)
                .when(!is_assistant, |this| {
                    this.child(self.render_thread_controls(
                        &thread,
                        entry_ix,
                        last_assistant_index,
                        true,
                        None,
                        cx,
                    ))
                })
                .when_some(comments_editor, |this, editor| {
                    this.child(Self::render_feedback_feedback_editor(editor, cx))
                })
                .into_any_element()
        } else {
            primary
        };

        if let Some(editing_index) = self.editing_message
            && editing_index < entry_ix
        {
            let is_subagent = self.is_subagent();

            let backdrop = div()
                .id(("backdrop", entry_ix))
                .size_full()
                .absolute()
                .inset_0()
                .bg(cx.theme().colors().panel_background)
                .opacity(0.8)
                .block_mouse_except_scroll()
                .on_click(cx.listener(Self::cancel_editing));

            div()
                .relative()
                .child(primary)
                .when(!is_subagent, |this| this.child(backdrop))
                .into_any_element()
        } else {
            primary
        }
    }

    fn render_user_message(
        &self,
        entry_ix: usize,
        message: &acp_thread::UserMessage,
        is_indented: bool,
        is_first_indented: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if message.content.source_blocks().is_empty() {
            return None;
        }
        let editor = self
            .entry_view_state
            .read(cx)
            .entry(entry_ix)
            .and_then(|entry| entry.message_editor())
            .cloned()?;

        let editing = self.editing_message == Some(entry_ix);
        let editor_focus = editor.focus_handle(cx).is_focused(window);
        let has_checkpoint_button = message
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.show);

        let is_subagent = self.is_subagent();
        let can_restore_checkpoint = self
            .thread
            .read(cx)
            .can_rewind_to(message.client_id.as_ref(), cx);
        let source_is_representable = message
            .content
            .source_blocks()
            .iter()
            .all(acp_thread::content::can_convert_to_v1);
        let is_editable = can_restore_checkpoint && source_is_representable;
        let agent_name = if is_subagent {
            "subagents".into()
        } else {
            self.agent_id.clone()
        };

        let actions = if editor_focus {
            Some(if is_editable {
                let is_loading_contents = self.is_loading_contents;
                h_flex()
                    .gap_0p5()
                    .child(
                        PillButton::new("cancel")
                            .size(KitSize::XSmall)
                            .icon(IconName::Close)
                            .disabled(is_loading_contents)
                            .tooltip(Tooltip::text("Cancel Editing"))
                            .on_click(cx.listener(Self::cancel_editing)),
                    )
                    .child(if is_loading_contents {
                        div()
                            .id("loading-edited-message-content")
                            .tooltip(Tooltip::text("Loading Added Context…"))
                            .child(loading_contents_spinner(IconSize::XSmall))
                            .into_any_element()
                    } else {
                        PillButton::new("regenerate")
                            .size(KitSize::XSmall)
                            .icon(IconName::Return)
                            .tooltip(Tooltip::text(
                                "Editing will restart the thread from this point.",
                            ))
                            .on_click(cx.listener({
                                let editor = editor.clone();
                                move |this, _, window, cx| {
                                    this.regenerate(entry_ix, editor.clone(), window, cx);
                                }
                            }))
                            .into_any_element()
                    })
                    .into_any_element()
            } else {
                PillButton::new("non_editable")
                    .size(KitSize::XSmall)
                    .icon(IconName::PencilUnavailable)
                    .tooltip(Tooltip::element(move |_, _| {
                        v_flex()
                            .gap_1()
                            .child(Label::new("Unavailable Editing"))
                            .child(
                                div().max_w_64().child(
                                    Label::new(if source_is_representable {
                                        format!(
                                            "Editing previous messages is not available for {} yet.",
                                            agent_name
                                        )
                                    } else {
                                        "This message contains unsupported content and cannot be edited or resent.".to_string()
                                    })
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                                ),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            })
        } else {
            (can_restore_checkpoint && has_checkpoint_button)
                .then(|| message.client_id.clone())
                .flatten()
                .map(|client_id| {
                    PillButton::new("restore-checkpoint")
                        .size(KitSize::XSmall)
                        .icon(IconName::Undo)
                        .label("Restore Checkpoint")
                        .tooltip(Tooltip::text(
                            "Restores all files in the project to the content they had at this point in the conversation.",
                        ))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.restore_checkpoint(&client_id, cx);
                        }))
                        .into_any_element()
                })
        };

        Some(
            v_flex()
                .id(("user_message", entry_ix))
                .map(|this| {
                    if is_first_indented {
                        this.pt_0p5()
                    } else {
                        this.pt(ROW_GAP)
                    }
                })
                .px(COLUMN_INSET)
                .w_full()
                .child(
                    PromptCard::new(("user-prompt", entry_ix), editor.into_any_element())
                        .editable(is_editable)
                        .editing(editing)
                        .focused(editor_focus)
                        .subagent(is_subagent)
                        .compact(is_indented)
                        .when_some(actions, |card, actions| {
                            card.actions(actions, !editor_focus)
                        }),
                )
                .into_any(),
        )
    }

    fn render_assistant_message(
        &self,
        entry_ix: usize,
        total_entries: usize,
        message: &AssistantMessage,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let is_last = entry_ix + 1 == total_entries;
        let is_generating = self.thread.read(cx).status() == ThreadStatus::Generating;
        let last_chunk_ix = message.chunks.len().saturating_sub(1);

        let chunks = message
            .chunks
            .iter()
            .enumerate()
            .filter_map(|(chunk_ix, chunk)| match chunk {
                AssistantMessageChunk::Message { block, .. } => {
                    block.visible_content(cx).then(|| {
                        div()
                            .id(("assistant-message-chunk", chunk_ix))
                            .child(self.render_message_content(
                                entry_ix, chunk_ix, block, false, window, cx,
                            ))
                            .into_any_element()
                    })
                }
                AssistantMessageChunk::Thought { block, .. } => {
                    block.visible_content(cx).then(|| {
                        let is_streaming = is_generating && is_last && chunk_ix == last_chunk_ix;
                        self.render_thinking_block(
                            entry_ix,
                            chunk_ix,
                            block,
                            is_streaming,
                            window,
                            cx,
                        )
                    })
                }
            })
            .collect::<Vec<_>>();

        if chunks.is_empty() {
            return None;
        }

        Some(
            v_flex()
                .px(TEXT_INSET)
                .pt(ROW_GAP)
                .when(is_last, |this| this.pb_4())
                .w_full()
                .gap(ROW_GAP)
                .text_ui(cx)
                .children(chunks)
                .when_some(
                    self.entry_view_state
                        .read(cx)
                        .entry(entry_ix)
                        .and_then(|entry| entry.focus_handle(cx)),
                    |this, handle| this.track_focus(&handle),
                )
                .into_any(),
        )
    }

    fn entry_is_finalized_turn_end(entries: &[AgentThreadEntry], entry_ix: usize) -> Option<bool> {
        if !matches!(
            entries.get(entry_ix),
            Some(AgentThreadEntry::AssistantMessage(_))
        ) {
            return Some(false);
        }

        for entry in &entries[entry_ix + 1..] {
            match entry {
                AgentThreadEntry::UserMessage(_) => return Some(true),
                AgentThreadEntry::AssistantMessage(_) | AgentThreadEntry::ToolCall(_) => {
                    return Some(false);
                }
                _ => {}
            }
        }

        None
    }

    fn render_thread_controls(
        &self,
        thread: &Entity<AcpThread>,
        entry_ix: usize,
        copy_response_index: Option<usize>,
        is_thread_bottom: bool,
        user_message_index: Option<usize>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let is_generating = matches!(thread.read(cx).status(), ThreadStatus::Generating);
        let needs_confirmation = thread.read(cx).is_waiting_for_confirmation()
            || self.has_pending_request_elicitation(cx);

        if is_thread_bottom && (is_generating || needs_confirmation) {
            return Empty.into_any_element();
        }

        let copy_response_button = copy_response_index.map(|response_index| {
            PillButton::new(("copy_agent_response", entry_ix))
                .size(KitSize::XSmall)
                .icon(IconName::Copy)
                .tooltip(Tooltip::text("Copy This Agent Response"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let entries = this.thread.read(cx).entries();
                    if let Some(text) = Self::get_agent_message_content(entries, response_index, cx)
                    {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }))
        });

        let scroll_to_recent_user_prompt =
            PillButton::new(("scroll_to_recent_user_prompt", entry_ix))
                .size(KitSize::XSmall)
                .icon(IconName::UserArrowUp)
                .tooltip(Tooltip::text("Scroll to User Message"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scroll_to_user_message_index(user_message_index, cx);
                }));

        let scroll_to_top = is_thread_bottom.then(|| {
            PillButton::new(("scroll_to_top", entry_ix))
                .size(KitSize::XSmall)
                .icon(IconName::ArrowUp)
                .tooltip(Tooltip::text("Scroll to Top"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scroll_to_top(cx);
                }))
        });

        let show_stats = is_thread_bottom && AgentSettings::get_global(cx).show_turn_stats;

        let last_turn_clock = show_stats
            .then(|| {
                self.turn_fields
                    .last_turn_duration
                    .filter(|&duration| duration > STOPWATCH_THRESHOLD)
                    .map(duration_alt_display)
            })
            .flatten();

        let last_turn_tokens = last_turn_clock
            .is_some()
            .then(|| {
                self.turn_fields
                    .last_turn_tokens
                    .filter(|&tokens| tokens > TOKEN_THRESHOLD)
                    .map(|tokens| format!("{} tokens", crate::humanize_token_count(tokens)))
            })
            .flatten();

        let feedback_buttons = is_thread_bottom
            .then(|| {
                (self.is_subagent() && self.is_thread_feedback_enabled(cx)).then(|| {
                    let feedback = self.thread_feedback.feedback;
                    let tooltip_meta =
                        "Rating the thread sends all of your current conversation to the Zed team.";

                    h_flex()
                        .gap_0p5()
                        .child(
                            PillButton::new("feedback-thumbs-up")
                                .size(KitSize::XSmall)
                                .icon(IconName::ThumbsUp)
                                .toggle_state(feedback == Some(ThreadFeedback::Positive))
                                .tooltip(move |window, cx| match feedback {
                                    Some(ThreadFeedback::Positive) => {
                                        Tooltip::text("Thanks for your feedback!")(window, cx)
                                    }
                                    _ => Tooltip::with_meta(
                                        "Helpful Response",
                                        None,
                                        tooltip_meta,
                                        cx,
                                    ),
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.handle_feedback_click(ThreadFeedback::Positive, window, cx);
                                })),
                        )
                        .child(
                            PillButton::new("feedback-thumbs-down")
                                .size(KitSize::XSmall)
                                .icon(IconName::ThumbsDown)
                                .toggle_state(feedback == Some(ThreadFeedback::Negative))
                                .tooltip(move |window, cx| match feedback {
                                    Some(ThreadFeedback::Negative) => Tooltip::text(
                                        "We appreciate your feedback and will use it to improve in the future.",
                                    )(
                                        window, cx
                                    ),
                                    _ => Tooltip::with_meta(
                                        "Not Helpful Response",
                                        None,
                                        tooltip_meta,
                                        cx,
                                    ),
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.handle_feedback_click(ThreadFeedback::Negative, window, cx);
                                })),
                        )
                })
            })
            .flatten();

        TurnFooter::new(last_turn_tokens.into_iter().chain(last_turn_clock))
            .children(feedback_buttons)
            .children(copy_response_button)
            .child(scroll_to_recent_user_prompt)
            .children(scroll_to_top)
            .into_any_element()
    }

    pub(super) fn render_generating(&self, confirmation: bool, cx: &App) -> impl IntoElement {
        let show_stats = AgentSettings::get_global(cx).show_turn_stats;
        let elapsed_label = show_stats
            .then(|| {
                self.turn_fields.turn_started_at.and_then(|started_at| {
                    let elapsed = started_at.elapsed();
                    (elapsed > STOPWATCH_THRESHOLD).then(|| duration_alt_display(elapsed))
                })
            })
            .flatten();

        let is_blocked_on_terminal_command =
            !confirmation && self.is_blocked_on_terminal_command(cx);
        let is_waiting = confirmation || self.thread.read(cx).has_in_progress_tool_calls();

        let turn_tokens_label = elapsed_label
            .is_some()
            .then(|| {
                self.turn_fields
                    .turn_tokens
                    .filter(|&tokens| tokens > TOKEN_THRESHOLD)
                    .map(|tokens| crate::humanize_token_count(tokens))
            })
            .flatten();

        let arrow_icon = if is_waiting {
            IconName::ArrowUp
        } else {
            IconName::ArrowDown
        };

        // A running terminal shows its own progress, so a second indicator would only repeat it.
        let status_label = if confirmation {
            Some("Awaiting Confirmation…")
        } else if is_blocked_on_terminal_command {
            None
        } else {
            Some("Generating…")
        };

        let shades = text_shades(cx);
        h_flex()
            .id("generating-spinner")
            .pt(ROW_GAP)
            .pb_2()
            .px(TEXT_INSET)
            .gap_1()
            .text_size(ThemeSettings::get_global(cx).agent_ui_font_size(cx))
            .line_height(relative(LINE_HEIGHT))
            .text_color(shades.tertiary)
            .children(status_label.map(|label| {
                div()
                    .text_color(shades.secondary)
                    .child(ShimmerText::new(label))
            }))
            .children(elapsed_label)
            .when_some(turn_tokens_label, |this, tokens| {
                this.child(
                    h_flex()
                        .gap_0p5()
                        .child(kit_icon(arrow_icon).with_size(KitSize::XSmall))
                        .child(format!("{} tokens", tokens)),
                )
            })
            .into_any_element()
    }

    fn render_message_content(
        &self,
        entry_ix: usize,
        chunk_ix: usize,
        content: &acp_thread::MessageContent,
        muted: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        v_flex().w_full().gap(ROW_GAP).children(
            content
                .blocks()
                .enumerate()
                .filter(|(_, block)| block.visible_content(cx))
                .map(|(block_ix, block)| {
                    let content = match block.markdown() {
                        Some(markdown) => {
                            let mut style = transcript_markdown_style(window, cx);
                            if muted {
                                style.base_text_style.color = text_shades(cx).secondary;
                            }
                            self.render_markdown(markdown.clone(), style, cx)
                                .into_any_element()
                        }
                        None => self.render_output_content_block(
                            entry_ix, block_ix, block, None, false, window, cx,
                        ),
                    };
                    div()
                        .id(("message-content-block", block_ix))
                        .debug_selector(move || {
                            format!("message-content-{entry_ix}-{chunk_ix}-{block_ix}")
                        })
                        .child(self.render_message_context_menu(
                            entry_ix,
                            block.markdown().cloned(),
                            content,
                            cx,
                        ))
                }),
        )
    }

    fn render_thinking_block(
        &self,
        entry_ix: usize,
        chunk_ix: usize,
        chunk: &acp_thread::MessageContent,
        is_streaming: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let header_id = SharedString::from(format!("thinking-block-header-{}", entry_ix));
        let key = (entry_ix, chunk_ix);

        let entry_view_state = self.entry_view_state.read(cx);
        let (is_open, is_constrained) = entry_view_state.thinking_block_state(key, cx);
        let should_auto_scroll = entry_view_state.is_auto_expanded_thinking_block(key);
        let scroll_handle = entry_view_state
            .entry(entry_ix)
            .and_then(|entry| entry.scroll_handle_for_assistant_message_chunk(chunk_ix));

        if should_auto_scroll {
            if let Some(ref handle) = scroll_handle {
                handle.scroll_to_bottom();
            }
        }

        let panel_bg = cx.theme().colors().panel_background;
        let header = ActivityRow::new(header_id, if is_streaming { "Thinking" } else { "Thought" })
            .running(is_streaming)
            .expanded(is_open)
            .on_click(cx.listener(move |this, _event: &ClickEvent, window, cx| {
                this.toggle_thinking_block_expansion(key, window, cx);
            }));

        v_flex()
            .id(("thinking-block", chunk_ix))
            .gap_1()
            .child(self.render_message_context_menu(entry_ix, None, header.into_any_element(), cx))
            .when(is_open, |this| {
                this.child(
                    div()
                        .when(is_constrained, |this| this.relative())
                        .child(
                            div()
                                .id(("thinking-content", chunk_ix))
                                .opacity(0.5)
                                .when(is_constrained, |this| this.max_h_64())
                                .when_some(scroll_handle, |this, scroll_handle| {
                                    this.track_scroll(&scroll_handle)
                                })
                                .overflow_hidden()
                                .child(self.render_message_content(
                                    entry_ix, chunk_ix, chunk, true, window, cx,
                                )),
                        )
                        .when(is_constrained, |this| {
                            this.child(div().absolute().inset_0().size_full().bg(linear_gradient(
                                180.,
                                linear_color_stop(panel_bg.opacity(0.8), 0.),
                                linear_color_stop(panel_bg.opacity(0.), 0.1),
                            )))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_context_compaction(
        &self,
        entry_ix: usize,
        compaction: &acp_thread::ContextCompaction,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_compacting = compaction.is_in_progress();
        let summary = &compaction.summary;
        let error = compaction.error.clone();
        let has_details = summary.blocks().len() > 0 || error.is_some();
        let is_expanded = self
            .entry_view_state
            .read(cx)
            .is_compaction_expanded(entry_ix);
        let details = (is_expanded && has_details).then_some((summary, error));

        let header_label = match &compaction.status {
            acp_thread::ContextCompactionStatus::InProgress => "Compacting Context…",
            acp_thread::ContextCompactionStatus::Completed => "Context Compacted",
            acp_thread::ContextCompactionStatus::Failed => "Compaction Failed",
            acp_thread::ContextCompactionStatus::Canceled => "Compaction Canceled",
            acp_thread::ContextCompactionStatus::Other(_) => "Context Compaction",
        };
        let header = ActivityRow::new(
            SharedString::from(format!("context-compaction-{entry_ix}")),
            header_label,
        )
        .running(is_compacting)
        .when(has_details, |header| {
            header.expanded(is_expanded).on_click(cx.listener(
                move |this, _event: &ClickEvent, window, cx| {
                    this.toggle_compaction_expansion(entry_ix, window, cx);
                },
            ))
        });

        let theme = KitTheme::global(cx);
        let border = theme.border;
        let hover = theme.list_hover;

        v_flex()
            .px(COLUMN_INSET)
            .pt(ROW_GAP)
            .w_full()
            .gap_1p5()
            .child(TranscriptDivider::new(header))
            .when_some(details, |this, (summary, error)| {
                this.child(
                    transcript_card(cx)
                        .when(summary.blocks().len() > 0, |this| {
                            this.child(
                                v_flex()
                                    .id(("compaction-summary", entry_ix))
                                    .p_2()
                                    .gap_2()
                                    .text_ui(cx)
                                    .children(summary.blocks().enumerate().map(
                                        |(content_ix, content)| {
                                            self.render_output_content_block(
                                                entry_ix, content_ix, content, None, true, window,
                                                cx,
                                            )
                                        },
                                    )),
                            )
                        })
                        .when_some(error, |this, error| {
                            let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
                            style.base_text_style.color = Color::Error.color(cx);
                            this.child(
                                div()
                                    .id(("compaction-error", entry_ix))
                                    .p_2()
                                    .child(self.render_markdown(error, style, cx)),
                            )
                        })
                        .child(
                            h_flex()
                                .id(("compaction-summary-collapse", entry_ix))
                                .h_6()
                                .justify_center()
                                .border_t_1()
                                .border_color(border)
                                .cursor_pointer()
                                .hover(|style| style.bg(hover))
                                .child(kit_icon(IconName::ChevronUp).with_size(KitSize::Small))
                                .on_click(cx.listener(
                                    move |this, _event: &ClickEvent, window, cx| {
                                        this.entry_view_state.update(cx, |state, _cx| {
                                            state.collapse_compaction(entry_ix);
                                        });
                                        this.refresh_thread_search(window, cx);
                                        cx.notify();
                                    },
                                )),
                        ),
                )
            })
            .into_any()
    }
}

/// Where cards, prompts and the composer sit, inset from the panel's edges.
pub(super) const COLUMN_INSET: Pixels = px(16.);
/// Where text sits: past a prompt card's border and padding, so text inside and outside of cards
/// starts at the same place.
pub(super) const TEXT_INSET: Pixels = px(29.);
/// The space above each row of the transcript, which is the same for every kind of row.
pub(super) const ROW_GAP: Pixels = px(14.);
/// The space between the line of a group of tool calls and each of its tool calls.
pub(super) const STEP_GAP: Pixels = px(6.);
const LINE_HEIGHT: f32 = 1.6;

/// The shades of text in the transcript: the response itself, what the agent did, and the details
/// of what it did.
pub(super) struct TextShades {
    pub(super) primary: Hsla,
    pub(super) secondary: Hsla,
    pub(super) tertiary: Hsla,
}

pub(super) fn text_shades(cx: &App) -> TextShades {
    let foreground = KitTheme::global(cx).foreground;
    TextShades {
        primary: foreground.opacity(0.85),
        secondary: foreground.opacity(0.62),
        tertiary: foreground.opacity(0.42),
    }
}

/// How the agent's responses read: text one and a half lines apart, lists as tight as the lines
/// in them, and quiet code and links.
pub(super) fn transcript_markdown_style(window: &Window, cx: &App) -> MarkdownStyle {
    let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
    let font_size = ThemeSettings::get_global(cx).agent_ui_font_size(cx);
    let shades = text_shades(cx);

    style.base_text_style.color = shades.primary;
    style.base_text_style.line_height = relative(LINE_HEIGHT);
    style.paragraph_line_height = relative(LINE_HEIGHT);
    style.paragraph_spacing = px(16.);
    // The last item's own spacing comes on top, for 18px below a list.
    style.list_spacing = px(10.);
    style.list_item_spacing = px(8.).into();
    style.list_indent = px(16.).into();
    style.list_paragraph_spacing = Some(px(4.));
    style.inline_code.font_size = Some((font_size * 0.9).into());
    style.inline_code.color = Some(shades.primary);
    style.link.underline = None;
    style.link.background_color = None;
    style.link.font_weight = Some(FontWeight::MEDIUM);

    let heading = |scale: f32, color: Option<Hsla>| {
        Some(TextStyleRefinement {
            font_size: Some((font_size * scale).into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            color,
            ..Default::default()
        })
    };
    style.heading_level_styles = Some(HeadingLevelStyles {
        h1: heading(1.466, None),
        h2: heading(1.333, None),
        h3: heading(1.133, None),
        h4: heading(1., None),
        h5: heading(0.866, Some(shades.secondary)),
        h6: heading(0.8, Some(shades.secondary)),
    });
    style
}

pub(super) fn kit_icon(icon: IconName) -> KitIcon {
    KitIcon::empty().path(icon.path())
}

/// The surface of cards in the transcript. It matches the editor's background, since cards hold
/// diffs and terminals that editors draw.
pub(super) fn transcript_card(cx: &App) -> Div {
    v_flex()
        .w_full()
        .rounded(px(10.))
        .border_1()
        .border_color(KitTheme::global(cx).border)
        .bg(cx.theme().colors().editor_background)
        .overflow_hidden()
}

/// A prompt the user sent, drawn on the composer's surface so it reads as what was typed there.
#[derive(IntoElement)]
pub(super) struct PromptCard {
    id: ElementId,
    content: AnyElement,
    editable: bool,
    editing: bool,
    focused: bool,
    subagent: bool,
    compact: bool,
    actions: Option<AnyElement>,
    actions_on_hover: bool,
}

impl PromptCard {
    pub(super) fn new(id: impl Into<ElementId>, content: AnyElement) -> Self {
        Self {
            id: id.into(),
            content,
            editable: false,
            editing: false,
            focused: false,
            subagent: false,
            compact: false,
            actions: None,
            actions_on_hover: false,
        }
    }

    pub(super) fn editable(mut self, editable: bool) -> Self {
        self.editable = editable;
        self
    }

    pub(super) fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub(super) fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub(super) fn subagent(mut self, subagent: bool) -> Self {
        self.subagent = subagent;
        self
    }

    pub(super) fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Floats `actions` over the card's top edge, only while the card is hovered when
    /// `on_hover` is set.
    pub(super) fn actions(mut self, actions: AnyElement, on_hover: bool) -> Self {
        self.actions = Some(actions);
        self.actions_on_hover = on_hover;
        self
    }
}

impl RenderOnce for PromptCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (background, border) = composer_surface(cx);
        let focus_border = KitTheme::global(cx).ring;
        let group = SharedString::from(format!("prompt-card-{}", self.id));

        div()
            .id(self.id)
            .group(group.clone())
            .relative()
            .w_full()
            .child(
                div()
                    .w_full()
                    .px_3()
                    .map(|this| {
                        if self.compact {
                            this.py_1p5()
                        } else {
                            this.py_2()
                        }
                    })
                    .rounded(px(12.))
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .map(|this| {
                        if !self.editable {
                            if self.subagent {
                                this.border_dashed()
                            } else {
                                this
                            }
                        } else if self.editing && self.focused {
                            this.border_color(focus_border)
                        } else if self.editing {
                            this.border_dashed()
                        } else {
                            this.hover(|style| style.border_color(focus_border.opacity(0.8)))
                        }
                    })
                    .text_xs()
                    .child(self.content),
            )
            .when_some(self.actions, |this, actions| {
                this.child(
                    h_flex()
                        .absolute()
                        .top_neg_3p5()
                        .right_2()
                        .p_0p5()
                        .rounded_full()
                        .border_1()
                        .border_color(border)
                        .bg(background)
                        .when(self.actions_on_hover, |this| this.visible_on_hover(group))
                        .child(actions),
                )
            })
    }
}

/// A line in the transcript for something the agent did: what it did, such as "Read", and the
/// details, such as which file.
#[derive(IntoElement)]
pub(super) struct ActivityRow {
    id: ElementId,
    action: SharedString,
    details: Option<SharedString>,
    running: bool,
    failed: bool,
    expanded: Option<bool>,
    end_slot: Option<AnyElement>,
    tooltip: Option<Box<dyn Fn(&mut Window, &mut App) -> AnyView>>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl ActivityRow {
    pub(super) fn new(id: impl Into<ElementId>, action: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            action: action.into(),
            details: None,
            running: false,
            failed: false,
            expanded: None,
            end_slot: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// Splits `label` into its first word, as what the agent did, and the rest, as the details.
    pub(super) fn from_label(id: impl Into<ElementId>, label: &str) -> Self {
        let label = label.trim();
        match label.split_once(char::is_whitespace) {
            Some((action, details)) => {
                Self::new(id, action.to_string()).details(details.trim_start().to_string())
            }
            None => Self::new(id, label.to_string()),
        }
    }

    pub(super) fn details(mut self, details: impl Into<SharedString>) -> Self {
        let details = details.into();
        self.details = (!details.is_empty()).then_some(details);
        self
    }

    pub(super) fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    pub(super) fn failed(mut self, failed: bool) -> Self {
        self.failed = failed;
        self
    }

    /// Shows a disclosure chevron, which points down while `expanded`.
    pub(super) fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    /// Adds an element after the label that shows while the row is hovered.
    pub(super) fn end_slot(mut self, end_slot: impl IntoElement) -> Self {
        self.end_slot = Some(end_slot.into_any_element());
        self
    }

    pub(super) fn tooltip(
        mut self,
        tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.tooltip = Some(Box::new(tooltip));
        self
    }

    pub(super) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ActivityRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let shades = text_shades(cx);
        let danger = KitTheme::global(cx).danger;
        let font_size = ThemeSettings::get_global(cx).agent_ui_font_size(cx);
        let group = SharedString::from(format!("activity-row-{}", self.id));
        let clickable = self.on_click.is_some();
        let action = if self.running {
            ShimmerText::new(self.action).into_any_element()
        } else {
            self.action.into_any_element()
        };

        h_flex()
            .id(self.id)
            .group(group.clone())
            .min_w_0()
            .gap_1()
            .text_size(font_size)
            .line_height(relative(LINE_HEIGHT))
            .when(clickable, |this| this.cursor_pointer())
            .child(
                div()
                    .flex_none()
                    .text_color(shades.secondary)
                    .when(clickable, |this| {
                        this.group_hover(group.clone(), |style| style.text_color(shades.primary))
                    })
                    .child(action),
            )
            .when_some(self.details, |this, details| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(shades.tertiary)
                        .when(clickable, |this| {
                            this.group_hover(group.clone(), |style| {
                                style.text_color(shades.secondary)
                            })
                        })
                        .child(details),
                )
            })
            .when_some(self.expanded, |this, expanded| {
                let chevron = if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                this.child(
                    div()
                        .flex_none()
                        .ml(px(3.))
                        .text_color(shades.tertiary)
                        .when(!expanded, |this| this.visible_on_hover(group.clone()))
                        .child(kit_icon(chevron).with_size(KitSize::XSmall)),
                )
            })
            .when(self.failed, |this| {
                this.child(
                    kit_icon(IconName::Close)
                        .with_size(KitSize::XSmall)
                        .text_color(danger)
                        .flex_none(),
                )
            })
            .when_some(self.end_slot, |this, end_slot| {
                this.child(div().flex_none().visible_on_hover(group).child(end_slot))
            })
            .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}

/// A rule across the transcript with a label in the middle, for events that belong to the
/// conversation rather than to one message.
#[derive(IntoElement)]
pub(super) struct TranscriptDivider {
    label: AnyElement,
}

impl TranscriptDivider {
    pub(super) fn new(label: impl IntoElement) -> Self {
        Self {
            label: label.into_any_element(),
        }
    }
}

impl RenderOnce for TranscriptDivider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let line = KitTheme::global(cx).border;
        h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_1().h_px().bg(line))
            .child(div().flex_none().child(self.label))
            .child(div().flex_1().h_px().bg(line))
    }
}

/// The quiet row under a finished turn, with its stats and actions such as copying the response.
#[derive(IntoElement)]
pub(super) struct TurnFooter {
    stats: Vec<String>,
    children: Vec<AnyElement>,
}

impl TurnFooter {
    pub(super) fn new(stats: impl IntoIterator<Item = String>) -> Self {
        Self {
            stats: stats.into_iter().collect(),
            children: Vec::new(),
        }
    }
}

impl ParentElement for TurnFooter {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for TurnFooter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .pt_2()
            // Lines the icons, rather than the buttons around them, up with the text.
            .pl(TEXT_INSET - px(4.))
            .pr(TEXT_INSET)
            .gap_0p5()
            .text_xs()
            .text_color(text_shades(cx).tertiary)
            .opacity(0.5)
            .hover(|style| style.opacity(1.))
            .children(self.children)
            .when(!self.stats.is_empty(), |this| {
                this.child(div().px_1().child(self.stats.join(" · ")))
            })
    }
}

/// A notice about the thread, such as an error that stopped it or a warning about what comes
/// next. It looks like GPUI Kit's `Alert`, which has no room for the actions these notices offer.
#[derive(IntoElement)]
pub(super) struct AlertCard {
    id: ElementId,
    variant: AlertVariant,
    title: SharedString,
    description: Option<AnyElement>,
    actions: Vec<AnyElement>,
    on_dismiss: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl AlertCard {
    pub(super) fn new(
        id: impl Into<ElementId>,
        variant: AlertVariant,
        title: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            variant,
            title: title.into(),
            description: None,
            actions: Vec::new(),
            on_dismiss: None,
        }
    }

    pub(super) fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    pub(super) fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub(super) fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for AlertCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = KitTheme::global(cx);
        let (icon, accent) = match self.variant {
            AlertVariant::Default => (IconName::Info, theme.foreground),
            AlertVariant::Info => (IconName::Info, theme.info),
            AlertVariant::Success => (IconName::Check, theme.success),
            AlertVariant::Warning => (IconName::Warning, theme.warning),
            AlertVariant::Error => (IconName::XCircle, theme.danger),
        };
        // Tinted the way GPUI Kit's `Alert` tints its surface and border.
        let (background, border) = match self.variant {
            AlertVariant::Default => (theme.background, theme.border),
            _ => (
                accent.mix_oklab(transparent_white(), 0.04),
                accent.mix_oklab(transparent_white(), 0.3),
            ),
        };

        h_flex()
            .id(self.id)
            .w_full()
            .min_w_0()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(background)
            .text_size(rems_from_px(13_f32))
            .child(
                h_flex()
                    .h_5()
                    .flex_none()
                    .child(kit_icon(icon).with_size(KitSize::Small).text_color(accent)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .min_h_5()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(accent)
                                    .child(self.title),
                            )
                            .when_some(self.on_dismiss, |this, on_dismiss| {
                                this.child(
                                    PillButton::new("dismiss")
                                        .size(KitSize::XSmall)
                                        .icon(IconName::Close)
                                        .tooltip(Tooltip::text("Dismiss"))
                                        .on_click(on_dismiss),
                                )
                            }),
                    )
                    .when_some(self.description, |this, description| {
                        this.child(
                            div()
                                .id("description")
                                .max_h_32()
                                .overflow_y_scroll()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    })
                    .when(!self.actions.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .pt_1()
                                .gap_1()
                                .flex_wrap()
                                .justify_end()
                                .children(self.actions),
                        )
                    }),
            )
    }
}

/// The prompt of the turn being read, pinned over the top of the transcript once the prompt
/// itself has scrolled out of view. Clicking it scrolls back to the prompt.
#[derive(IntoElement)]
pub(super) struct StickyPrompt {
    id: ElementId,
    text: SharedString,
    on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
}

impl StickyPrompt {
    pub(super) fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            on_click: Box::new(on_click),
        }
    }
}

impl RenderOnce for StickyPrompt {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (background, border) = composer_surface(cx);
        let theme = KitTheme::global(cx);
        let focus_border = theme.ring;
        let on_click = self.on_click;

        h_flex()
            .id(self.id)
            .w_full()
            .min_w_0()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded(px(12.))
            .border_1()
            .border_color(border)
            .bg(background)
            .cursor_pointer()
            .hover(|style| style.border_color(focus_border.opacity(0.8)))
            // The transcript under it keeps scrolling, but mustn't also take the click.
            .block_mouse_except_scroll()
            .text_xs()
            .text_color(theme.foreground)
            .child(div().flex_1().min_w_0().truncate().child(self.text))
            .child(
                kit_icon(IconName::ArrowUp)
                    .with_size(KitSize::XSmall)
                    .text_color(theme.muted_foreground)
                    .flex_none(),
            )
            .tooltip(Tooltip::text("Scroll to Prompt"))
            .on_click(move |event, window, cx| on_click(event, window, cx))
    }
}
