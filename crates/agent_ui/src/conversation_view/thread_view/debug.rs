use super::*;
use acp_thread::MessageIdentity;
use agent::debug_session::{
    DebugLogSnapshot, DebugSession, reproduction_steps, strip_reproduction_steps,
};
use gpui_kit::base::{h_flex, v_flex};
use theme_settings::ThemeSettings;

const PROCEED_MESSAGE: &str = "Issue reproduced, please proceed";
const FIXED_MESSAGE: &str = "The issue has been fixed. Please clean up the instrumentation.";

#[derive(Clone, PartialEq, Eq)]
enum UserIdentity {
    Client(ClientUserMessageId),
    Protocol(MessageIdentity),
    Legacy(usize, SharedString),
}

impl UserIdentity {
    fn new(message: &acp_thread::UserMessage, index: usize, cx: &App) -> Self {
        if let Some(id) = message.client_id.clone() {
            Self::Client(id)
        } else if matches!(message.identity, MessageIdentity::Legacy(None)) {
            Self::Legacy(index, message.content.to_markdown(cx).into())
        } else {
            Self::Protocol(message.identity.clone())
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ReproductionSource {
    user: UserIdentity,
    assistant: MessageIdentity,
    fallback: Option<(usize, usize)>,
}

#[derive(Clone, PartialEq, Eq)]
struct Reproduction {
    source: ReproductionSource,
    content: SharedString,
    is_open: bool,
    retry_fixed: Option<bool>,
}

struct RenderedMarkdown {
    markdown: Entity<Markdown>,
    _subscription: Subscription,
}

#[derive(Default)]
pub(super) struct DebugUiState {
    pub(super) active: bool,
    reproduction: Option<Reproduction>,
    ready_source: Option<ReproductionSource>,
    response_failed: bool,
    expanded: bool,
    steps: Option<RenderedMarkdown>,
    transcript: HashMap<gpui::EntityId, RenderedMarkdown>,
    last_stop: Option<(UserIdentity, acp_v2::StopReason)>,
    logs_open: bool,
    logs: DebugLogSnapshot,
    logs_loading: bool,
    logs_error: Option<SharedString>,
    clear_error: Option<SharedString>,
    logs_path: Option<PathBuf>,
    logs_session: Option<Arc<DebugSession>>,
    logs_task: Option<Task<()>>,
    logs_generation: usize,
    logs_scroll: ScrollHandle,
    raw_log: Option<usize>,
    clear_task: Option<Task<()>>,
}

fn normalized_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn follow_up_kind(message: &acp_thread::UserMessage, cx: &App) -> Option<bool> {
    let text = normalized_text(&message.content.to_markdown(cx));
    if text == PROCEED_MESSAGE {
        Some(false)
    } else if text == FIXED_MESSAGE {
        Some(true)
    } else {
        None
    }
}

fn turn_reproduction(
    entries: &[AgentThreadEntry],
    start: usize,
    end: usize,
    cx: &App,
) -> Option<Reproduction> {
    let user = entries.get(start)?.user_message()?;
    for (entry_index, entry) in entries.get(start + 1..end)?.iter().enumerate().rev() {
        let AgentThreadEntry::AssistantMessage(message) = entry else {
            continue;
        };
        if message.is_subagent_output {
            continue;
        }
        for (chunk_index, chunk) in message.chunks.iter().enumerate().rev() {
            let AssistantMessageChunk::Message {
                identity, block, ..
            } = chunk
            else {
                continue;
            };
            let text = block.to_markdown(cx);
            let Some(steps) = reproduction_steps(&text) else {
                continue;
            };
            return Some(Reproduction {
                source: ReproductionSource {
                    user: UserIdentity::new(user, start, cx),
                    assistant: identity.clone(),
                    fallback: matches!(identity, MessageIdentity::Legacy(None))
                        .then_some((entry_index, chunk_index)),
                },
                content: steps.content.to_owned().into(),
                is_open: steps.is_open,
                retry_fixed: None,
            });
        }
    }
    None
}

impl ThreadView {
    fn debug_active(&self, cx: &App) -> bool {
        !self.is_subagent()
            && self.as_native_thread(cx).is_some_and(|thread| {
                thread.read(cx).profile().as_str() == agent_settings::builtin_profiles::DEBUG
            })
    }

    fn debug_reproduction(&self, cx: &App) -> Option<Reproduction> {
        if !self.debug_active(cx) || self.thread_error.is_some() {
            return None;
        }
        let thread = self.thread.read(cx);
        let entries = thread.entries();
        let user_index = entries
            .iter()
            .rposition(|entry| entry.user_message().is_some())?;
        let user = entries.get(user_index)?.user_message()?;
        let user_identity = UserIdentity::new(user, user_index, cx);
        let outcome = user.client_id.as_ref().and_then(|id| {
            self.as_native_thread(cx)
                .and_then(|thread| thread.read(cx).debug_turn_outcome(id))
        });
        let stop_reason = match thread.foreground_state() {
            acp_v2::StateUpdate::Idle(state) => state.stop_reason.as_ref().or_else(|| {
                self.debug_ui
                    .last_stop
                    .as_ref()
                    .and_then(|(identity, reason)| (identity == &user_identity).then_some(reason))
            }),
            _ => None,
        };
        if matches!(stop_reason, Some(acp_v2::StopReason::Error(_)))
            || matches!(outcome, Some(agent::DebugTurnOutcome::Failed))
        {
            return None;
        }
        if let Some(mut reproduction) = turn_reproduction(entries, user_index, entries.len(), cx) {
            if thread.status() == ThreadStatus::Idle {
                if reproduction.content.trim().is_empty() {
                    return None;
                }
                reproduction.is_open = false;
            }
            return Some(reproduction);
        }
        if thread.status() != ThreadStatus::Idle {
            return None;
        }
        let fixed = follow_up_kind(user, cx)?;
        if matches!(outcome, Some(agent::DebugTurnOutcome::Completed)) {
            return None;
        }
        let cancelled = matches!(stop_reason, Some(acp_v2::StopReason::Cancelled))
            || matches!(
                outcome,
                Some(agent::DebugTurnOutcome::Cancelled | agent::DebugTurnOutcome::InProgress)
            )
            || thread
                .latest_submission_id()
                .and_then(|id| thread.submission(id))
                .is_some_and(|submission| matches!(submission.state, SubmissionState::Cancelled));
        // A restored thread may not retain its stop reason. Only an untouched follow-up is
        // safe to recover without it; a completed response must never be treated as a retry.
        let untouched = stop_reason.is_none()
            && entries.get(user_index + 1..).is_some_and(|entries| entries.iter().all(|entry| {
                matches!(entry, AgentThreadEntry::AssistantMessage(message) if message.chunks.iter().all(|chunk| {
                    let block = match chunk {
                        AssistantMessageChunk::Message { block, .. }
                        | AssistantMessageChunk::Thought { block, .. } => block,
                    };
                    block.to_markdown(cx).trim().is_empty()
                }))
            }));
        if !cancelled && !untouched {
            return None;
        }
        let previous_user = entries
            .get(..user_index)?
            .iter()
            .rposition(|entry| entry.user_message().is_some())?;
        let mut reproduction = turn_reproduction(entries, previous_user, user_index, cx)?;
        if reproduction.content.trim().is_empty() {
            return None;
        }
        reproduction.is_open = false;
        reproduction.retry_fixed = Some(fixed);
        Some(reproduction)
    }

    pub(super) fn waiting_for_debug_reproduction(&self, cx: &App) -> bool {
        self.debug_reproduction(cx)
            .is_some_and(|reproduction| !reproduction.content.trim().is_empty())
    }

    pub(super) fn record_debug_stop(&mut self, reason: Option<&acp_v2::StopReason>, cx: &App) {
        let entries = self.thread.read(cx).entries();
        self.debug_ui.last_stop = reason.and_then(|reason| {
            let (index, message) = entries
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, entry)| entry.user_message().map(|message| (index, message)))?;
            Some((UserIdentity::new(message, index, cx), reason.clone()))
        });
    }

    fn debug_ready(&self, cx: &App) -> bool {
        self.debug_ui
            .reproduction
            .as_ref()
            .is_some_and(|reproduction| {
                !reproduction.is_open && !reproduction.content.trim().is_empty()
            })
            && !self.debug_ui.response_failed
            && self.thread.read(cx).status() == ThreadStatus::Idle
            && self.thread_error.is_none()
            && !self.is_loading_contents
    }

    fn new_debug_markdown(&self, source: SharedString, cx: &mut Context<Self>) -> RenderedMarkdown {
        let languages = self.thread.read(cx).project().read(cx).languages().clone();
        let markdown = cx.new(|cx| {
            Markdown::new_with_options(
                source,
                Some(languages),
                None,
                markdown::MarkdownOptions {
                    render_mermaid_diagrams: true,
                    render_metadata_blocks: true,
                    ..Default::default()
                },
                cx,
            )
        });
        let subscription = cx.observe(&markdown, |_, _, cx| cx.notify());
        RenderedMarkdown {
            markdown,
            _subscription: subscription,
        }
    }

    fn sync_debug_transcript(&mut self, cx: &mut Context<Self>) {
        let sources = self
            .thread
            .read(cx)
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                AgentThreadEntry::AssistantMessage(message) => Some(message),
                _ => None,
            })
            .flat_map(|message| message.chunks.iter())
            .filter_map(|chunk| match chunk {
                AssistantMessageChunk::Message { block, .. } => Some(block),
                AssistantMessageChunk::Thought { .. } => None,
            })
            .flat_map(|block| block.markdowns())
            .filter_map(|markdown| {
                let source = markdown.read(cx).source();
                let stripped = strip_reproduction_steps(source);
                (&*stripped != source.as_str()).then(|| {
                    (
                        markdown.entity_id(),
                        SharedString::from(stripped.into_owned()),
                    )
                })
            })
            .collect::<Vec<_>>();
        let active = sources.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
        self.debug_ui.transcript.retain(|id, _| active.contains(id));
        for (id, source) in sources {
            if let Some(rendered) = self.debug_ui.transcript.get(&id) {
                if rendered.markdown.read(cx).source() != &source {
                    rendered
                        .markdown
                        .update(cx, |markdown, cx| markdown.replace(source, cx));
                }
            } else {
                let rendered = self.new_debug_markdown(source, cx);
                self.debug_ui.transcript.insert(id, rendered);
            }
        }
    }

    pub(super) fn debug_display_markdown(&self, markdown: &Entity<Markdown>) -> Entity<Markdown> {
        self.debug_ui
            .transcript
            .get(&markdown.entity_id())
            .map_or_else(|| markdown.clone(), |rendered| rendered.markdown.clone())
    }

    pub(super) fn sync_debug_state(&mut self, cx: &mut Context<Self>) {
        self.sync_debug_transcript(cx);
        let was_waiting = self
            .debug_ui
            .reproduction
            .as_ref()
            .is_some_and(|reproduction| !reproduction.content.trim().is_empty());
        let response_had_failed = self.debug_ui.response_failed;
        let was_active = self.debug_ui.active;
        self.debug_ui.active = self.debug_active(cx);
        if self.debug_ui.active
            && !was_active
            && let Some(thread) = self.as_native_thread(cx)
            && thread.read(cx).debug_session().is_none()
            && thread.read(cx).debug_error().is_none()
        {
            thread.update(cx, |thread, cx| {
                drop(thread.prepare_debug_session(cx));
            });
        }
        self.debug_ui.response_failed = self.thread_error.is_some()
            || matches!(self.thread.read(cx).foreground_state(), acp_v2::StateUpdate::Idle(state) if matches!(state.stop_reason, Some(acp_v2::StopReason::Error(_))))
            || self
                .thread
                .read(cx)
                .entries()
                .iter()
                .rev()
                .find_map(|entry| entry.user_message())
                .and_then(|message| message.client_id.as_ref())
                .and_then(|id| {
                    self.as_native_thread(cx)
                        .and_then(|thread| thread.read(cx).debug_turn_outcome(id))
                })
                .is_some_and(|outcome| outcome == agent::DebugTurnOutcome::Failed);
        let reproduction = self.debug_reproduction(cx);
        if let Some(reproduction) = &reproduction {
            if let Some(steps) = &self.debug_ui.steps {
                if steps.markdown.read(cx).source() != &reproduction.content {
                    steps.markdown.update(cx, |markdown, cx| {
                        markdown.replace(reproduction.content.clone(), cx)
                    });
                }
            } else {
                self.debug_ui.steps =
                    Some(self.new_debug_markdown(reproduction.content.clone(), cx));
            }
        } else {
            self.debug_ui.steps = None;
        }
        self.debug_ui.reproduction = reproduction;
        let ready_source = self
            .debug_ready(cx)
            .then(|| {
                self.debug_ui
                    .reproduction
                    .as_ref()
                    .map(|reproduction| reproduction.source.clone())
            })
            .flatten();
        if ready_source.is_some() && ready_source != self.debug_ui.ready_source {
            self.debug_ui.expanded = true;
            self.debug_ui.logs_open = false;
        }
        self.debug_ui.ready_source = ready_source;
        if !self.debug_ui.active || (self.debug_ui.response_failed && !response_had_failed) {
            self.debug_ui.expanded = false;
            self.debug_ui.logs_open = false;
        }
        if was_waiting != self.waiting_for_debug_reproduction(cx) {
            self.sync_queue_flag_to_native_thread(cx);
        }
        self.sync_debug_logs(cx);
    }

    fn stop_debug_logs(&mut self) {
        self.debug_ui.logs_generation = self.debug_ui.logs_generation.wrapping_add(1);
        self.debug_ui.logs_task.take();
        self.debug_ui.logs_session = None;
        self.debug_ui.logs_loading = false;
    }

    fn sync_debug_logs(&mut self, cx: &mut Context<Self>) {
        if self.debug_ui.clear_task.is_some() {
            return;
        }
        let session = self
            .debug_ui
            .active
            .then(|| self.as_native_thread(cx))
            .flatten()
            .and_then(|thread| thread.read(cx).debug_session().cloned());
        let should_poll = self.debug_ui.expanded || self.debug_ui.logs_open;
        let Some(session) = session.filter(|_| should_poll) else {
            self.stop_debug_logs();
            return;
        };
        if self
            .debug_ui
            .logs_session
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &session))
            && self.debug_ui.logs_task.is_some()
        {
            return;
        }
        self.stop_debug_logs();
        let log_path = session.log_path().to_path_buf();
        if self.debug_ui.logs_path.as_ref() != Some(&log_path) {
            self.debug_ui.logs_path = Some(log_path);
            self.debug_ui.logs = DebugLogSnapshot::default();
            self.debug_ui.raw_log = None;
            self.debug_ui.clear_error = None;
        }
        self.debug_ui.logs_session = Some(session.clone());
        self.debug_ui.logs_loading = true;
        self.debug_ui.logs_error = None;
        let generation = self.debug_ui.logs_generation;
        self.debug_ui.logs_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let snapshot = cx
                    .background_spawn({
                        let session = session.clone();
                        async move { session.logs() }
                    })
                    .await;
                let keep_polling = this.update(cx, |this, cx| {
                    if this.debug_ui.logs_generation != generation {
                        return false;
                    }
                    let was_loading = this.debug_ui.logs_loading;
                    this.debug_ui.logs_loading = false;
                    match snapshot {
                        Ok(snapshot) => {
                            let changed = was_loading
                                || this.debug_ui.logs != snapshot
                                || this.debug_ui.logs_error.is_some();
                            if snapshot.total_entries != this.debug_ui.logs.total_entries {
                                this.debug_ui.logs_scroll.scroll_to_bottom();
                            }
                            this.debug_ui.logs = snapshot;
                            this.debug_ui.logs_error = None;
                            if changed {
                                cx.notify();
                            }
                        }
                        Err(error) => {
                            let error = SharedString::from(format!("{error:#}"));
                            if was_loading || this.debug_ui.logs_error.as_ref() != Some(&error) {
                                this.debug_ui.logs_error = Some(error);
                                cx.notify();
                            }
                        }
                    }
                    true
                });
                match keep_polling {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        log::debug!("Debug log view closed: {error}");
                        break;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
            }
        }));
    }

    pub(super) fn dismiss_debug_panel(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.debug_ui.expanded && !self.debug_ui.logs_open {
            return false;
        }
        self.debug_ui.expanded = false;
        self.debug_ui.logs_open = false;
        self.stop_debug_logs();
        cx.notify();
        true
    }

    pub(super) fn render_debug_pill(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.debug_ui.active {
            return None;
        }
        let pending = self.debug_ready(cx);
        Some(
            PillButton::new("debug-reproduction-pill")
                .size(KitSize::Small)
                .label("Debug")
                .toggle_state(self.debug_ui.expanded)
                .when(pending, |button| {
                    button.child(
                        div()
                            .size(px(5.))
                            .rounded_full()
                            .bg(KitTheme::global(cx).danger),
                    )
                })
                .tooltip(Tooltip::text(if pending {
                    "Reproduction steps are waiting for you"
                } else {
                    "Debug reproduction and logs"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.debug_ui.expanded = !this.debug_ui.expanded;
                    this.debug_ui.logs_open = false;
                    this.sync_debug_logs(cx);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    pub(super) fn render_debug_panel(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.debug_ui.active || !self.debug_ui.expanded {
            return None;
        }
        if self.debug_ui.logs_open {
            return Some(self.render_debug_logs(window, cx));
        }
        let native_thread = self.as_native_thread(cx)?;
        let thread = native_thread.read(cx);
        let session = thread.debug_session();
        let ready = self.debug_ready(cx);
        let collector_error = thread
            .debug_error()
            .map(ToString::to_string)
            .or_else(|| session.and_then(|session| session.collector_error()));
        let collector_running = session.is_some_and(|session| session.is_running());
        Some(composer::composer_card(cx)
            .child(h_flex().flex_none().justify_between().px_3().py_1p5()
                .text_color(KitTheme::global(cx).muted_foreground)
                .child("Reproduction Steps")
                .child(PillButton::new("close-debug-reproduction").size(KitSize::XSmall).icon(IconName::Close)
                    .tooltip(Tooltip::text("Close reproduction steps"))
                    .on_click(cx.listener(|this, _, _, cx| { this.dismiss_debug_panel(cx); }))))
            .child(v_flex().id("debug-reproduction-steps").min_h_0().max_h(px(180.)).overflow_y_scroll().px_3().py_1().gap_2()
                .when_some(self.debug_ui.steps.as_ref(), |body, steps| body.child(
                    self.render_markdown(steps.markdown.clone(), transcript::transcript_markdown_style(window, cx), cx)))
                .when(self.debug_ui.steps.is_none(), |body| body.child(div().text_color(KitTheme::global(cx).muted_foreground)
                    .child(if self.debug_ui.response_failed {
                        "The debug request stopped with an error. Review the logs before retrying."
                    } else {
                        "Reproduction steps will appear after the agent instruments the issue."
                    })))
                .when_some(collector_error, |body, error| body.child(div().text_color(KitTheme::global(cx).danger).child(error)))
                .when(session.is_some_and(|session| session.endpoint_changed()), |body| body.child(div().text_color(KitTheme::global(cx).muted_foreground)
                    .child("The log collector address changed. Continue so the agent can refresh its instrumentation."))))
            .when(ready || !collector_running || self.debug_ui.response_failed, |card| card.child(h_flex().flex_none().gap_2().px_3().py_1p5()
                .border_t_1().border_color(KitTheme::global(cx).border)
                .child(PillButton::new("debug-open-log-viewer").size(KitSize::Small)
                    .label(format!("Logs {}", self.debug_ui.logs.total_entries))
                    .on_click(cx.listener(|this, _, _, cx| { this.debug_ui.logs_open = true; this.sync_debug_logs(cx); cx.notify(); })))
                .child(div().flex_1())
                .when(!collector_running, |footer| footer.child(PillButton::new("retry-debug-collector")
                    .size(KitSize::Small).label("Retry Collector")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(thread) = this.as_native_thread(cx) {
                            thread.update(cx, |thread, cx| { drop(thread.restart_debug_session(cx)); });
                        }
                    }))))
                .when(ready, |footer| footer
                    .child(PillButton::new("debug-mark-fixed").size(KitSize::Small).label("Mark Fixed")
                        .on_click(cx.listener(|this, _, window, cx| this.send_debug_follow_up(true, window, cx))))
                    .child(PillButton::new("debug-proceed").size(KitSize::Small).variant(ButtonVariant::Danger).label("Proceed")
                        .disabled(!collector_running)
                        .on_click(cx.listener(|this, _, window, cx| this.send_debug_follow_up(false, window, cx)))))))
            .into_any_element())
    }

    fn render_debug_logs(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let session = self
            .as_native_thread(cx)
            .and_then(|thread| thread.read(cx).debug_session().cloned());
        let log_path = session
            .as_ref()
            .map(|session| session.log_path().to_path_buf());
        let error = self
            .debug_ui
            .clear_error
            .clone()
            .or_else(|| self.debug_ui.logs_error.clone())
            .or_else(|| self.debug_ui.logs.collector_error.clone().map(Into::into));
        let theme = KitTheme::global(cx);
        let clearing = self.debug_ui.clear_task.is_some();
        let entries = self.debug_ui.logs.entries.iter().map(|entry| {
            let line = entry.line_number;
            let timestamp = entry
                .timestamp
                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                .map(|timestamp| {
                    timestamp
                        .with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                })
                .unwrap_or_else(|| "—".to_string());
            let raw = self.debug_ui.raw_log == Some(line);
            v_flex()
                .id(("debug-log-entry", line))
                .gap_1()
                .py_1()
                .min_w_0()
                .child(
                    h_flex()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(timestamp),
                        )
                        .child(div().min_w_0().flex_1().child(entry.message.clone()))
                        .child(
                            PillButton::new(("debug-log-raw", line))
                                .size(KitSize::XSmall)
                                .label("Raw")
                                .toggle_state(raw)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.debug_ui.raw_log = (!raw).then_some(line);
                                    cx.notify();
                                })),
                        ),
                )
                .when(raw, |row| {
                    row.child(
                        v_flex()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .bg(theme.background)
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("Line {line}"))
                                    .child(
                                        KitClipboard::new(("copy-debug-log", line))
                                            .value(entry.raw.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(
                                        ThemeSettings::get_global(cx).agent_ui_font_size(cx) * 0.9,
                                    )
                                    .child(entry.raw.clone()),
                            ),
                    )
                })
        });
        composer::composer_card(cx)
            .max_h((window.viewport_size().height * 0.5).min(px(420.)))
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child("Debug Logs")
                    .when_some(log_path.as_ref(), |header, path| {
                        header.child(
                            div()
                                .min_w_0()
                                .text_ellipsis()
                                .text_color(theme.muted_foreground)
                                .child(
                                    path.file_name()
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        PillButton::new("close-debug-logs")
                            .size(KitSize::XSmall)
                            .icon(IconName::Close)
                            .tooltip(Tooltip::text("Close debug logs"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.debug_ui.logs_open = false;
                                this.sync_debug_logs(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("debug-log-lines")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.debug_ui.logs_scroll)
                    .px_3()
                    .pb_2()
                    .when_some(error, |body, error| {
                        body.child(div().py_2().text_color(theme.danger).child(error))
                    })
                    .when(self.debug_ui.logs.entries.is_empty(), |body| {
                        body.child(div().py_3().text_color(theme.muted_foreground).child(
                            if self.debug_ui.logs_loading {
                                "Loading log entries…"
                            } else {
                                "Waiting for log entries…"
                            },
                        ))
                    })
                    .children(entries),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} entries", self.debug_ui.logs.total_entries)),
                    )
                    .child(div().flex_1())
                    .when_some(log_path, |footer, log_path| {
                        footer.child(
                            PillButton::new("debug-open-log-file")
                                .size(KitSize::Small)
                                .label("Open File")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.workspace
                                        .update(cx, |workspace, cx| {
                                            workspace
                                                .open_abs_path(
                                                    log_path.clone(),
                                                    OpenOptions {
                                                        focus: Some(true),
                                                        ..Default::default()
                                                    },
                                                    window,
                                                    cx,
                                                )
                                                .detach_and_log_err(cx);
                                        })
                                        .log_err();
                                })),
                        )
                    })
                    .child(
                        PillButton::new("debug-clear-logs")
                            .size(KitSize::Small)
                            .label(if clearing {
                                "Clearing…"
                            } else {
                                "Clear Logs"
                            })
                            .disabled(clearing || session.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.clear_debug_logs(cx))),
                    ),
            )
            .into_any_element()
    }

    fn clear_debug_logs(&mut self, cx: &mut Context<Self>) {
        if self.debug_ui.clear_task.is_some() {
            return;
        }
        let Some(session) = self
            .as_native_thread(cx)
            .and_then(|thread| thread.read(cx).debug_session().cloned())
        else {
            return;
        };
        self.stop_debug_logs();
        self.debug_ui.clear_error = None;
        let clearing = cx.background_spawn({
            let session = session.clone();
            async move { session.clear() }
        });
        self.debug_ui.clear_task = Some(cx.spawn(async move |this, cx| {
            let result = clearing.await;
            this.update(cx, |this, cx| {
                this.debug_ui.clear_task = None;
                let current_session = this
                    .as_native_thread(cx)
                    .and_then(|thread| thread.read(cx).debug_session().cloned());
                if current_session
                    .as_ref()
                    .is_none_or(|current| !Arc::ptr_eq(current, &session))
                {
                    this.sync_debug_logs(cx);
                    cx.notify();
                    return;
                }
                match result {
                    Ok(()) => {
                        this.debug_ui.logs = DebugLogSnapshot::default();
                        this.debug_ui.raw_log = None;
                        this.debug_ui.logs_error = None;
                    }
                    Err(error) => this.debug_ui.clear_error = Some(format!("{error:#}").into()),
                }
                this.sync_debug_logs(cx);
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn send_debug_follow_up(&mut self, fixed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reproduction) = self.debug_reproduction(cx) else {
            return;
        };
        if !self.debug_ready(cx) {
            return;
        }
        if reproduction.retry_fixed == Some(fixed) {
            if !self.thread.read(cx).can_retry(cx) {
                return;
            }
            self.dismiss_debug_panel(cx);
            self.message_queue.resume();
            self.retry_generation(cx);
            return;
        }
        let text = if fixed {
            FIXED_MESSAGE
        } else {
            PROCEED_MESSAGE
        };
        self.dismiss_debug_panel(cx);
        self.message_queue.resume();
        self.send_content(
            Task::ready(Ok(Some((
                vec![acp_v2::ContentBlock::Text(acp_v2::TextContent::new(text))],
                Vec::new(),
            )))),
            false,
            window,
            cx,
        );
    }
}
