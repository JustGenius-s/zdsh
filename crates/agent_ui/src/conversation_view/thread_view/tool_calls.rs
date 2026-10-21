use super::composer::PillButton;
use super::transcript::{
    ActivityRow, AlertCard, ROW_GAP, STEP_GAP, TEXT_INSET, kit_icon, transcript_card,
};
use super::*;
use crate::entry_view_state::ActivityGroup;
use gpui::{AnyView, HighlightStyle, StyledText};
use gpui_kit::component::{
    Disableable as _, Icon as KitIcon, Size as KitSize, Theme as KitTheme,
    button::{Button as KitButton, ButtonVariant},
    shimmer::ShimmerText,
    skeleton::Skeleton,
    spinner::Spinner,
};
use markdown::parser::MarkdownEvent;
use std::ops::Range;
use std::time::Duration;
use theme_settings::ThemeSettings;

impl ThreadView {
    pub(super) fn render_any_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        tool_call: &ToolCall,
        focus_handle: &FocusHandle,
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let has_terminals = tool_call.terminals().next().is_some();

        // Give every tool-call subtree a unique element-id prefix derived from
        // the globally-unique tool call id and the layout. This single wrapper
        // is what keeps all the `entry_ix`-keyed element ids inside the card
        // collision-free, even when the same tool call is rendered in multiple
        // places at once (inline list + floating awaiting-permission row) or
        // when subagent entries are inlined into the parent view's element tree.
        let container_id = ElementId::Name(SharedString::from(format!(
            "tool-call-{}-{}",
            tool_call.id.0,
            layout.id_str()
        )));

        div().w_full().id(container_id).map(|this| {
            if tool_call.is_subagent() {
                this.child(
                    self.render_subagent_tool_call(
                        active_session_id,
                        entry_ix,
                        tool_call,
                        tool_call
                            .subagent_session_info
                            .as_ref()
                            .map(|i| i.session_id.clone()),
                        focus_handle,
                        window,
                        cx,
                    ),
                )
            } else if has_terminals {
                this.children(tool_call.terminals().map(|terminal| {
                    self.render_terminal_tool_call(
                        active_session_id,
                        entry_ix,
                        terminal,
                        tool_call,
                        focus_handle,
                        layout,
                        window,
                        cx,
                    )
                }))
            } else if Self::is_activity_tool_call(tool_call) {
                this.child(self.render_activity_tool_call(
                    active_session_id,
                    entry_ix,
                    tool_call,
                    focus_handle,
                    layout,
                    window,
                    cx,
                ))
            } else {
                this.child(self.render_tool_call(
                    active_session_id,
                    entry_ix,
                    tool_call,
                    focus_handle,
                    layout,
                    window,
                    cx,
                ))
            }
        })
    }

    /// Whether the tool call shows as one line in the transcript rather than as a card, which
    /// edits, commands, and anything awaiting confirmation get.
    fn is_activity_tool_call(tool_call: &ToolCall) -> bool {
        let is_edit = matches!(tool_call.kind(), acp_v2::ToolKind::Edit)
            || tool_call.diffs().next().is_some()
            || tool_call
                .content()
                .iter()
                .any(|content| matches!(content, ToolCallContent::DiffPatch { .. }));
        !is_edit
            && !matches!(tool_call.kind(), acp_v2::ToolKind::Execute)
            && !matches!(tool_call.status(), ToolCallStatus::WaitingForConfirmation)
    }

    fn render_activity_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        tool_call: &ToolCall,
        focus_handle: &FocusHandle,
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let is_read = matches!(tool_call.kind(), acp_v2::ToolKind::Read);
        let has_location =
            tool_call.locations.len() == 1 || (is_read && !tool_call.locations.is_empty());
        let failed_or_canceled = matches!(
            tool_call.status(),
            ToolCallStatus::Rejected | ToolCallStatus::Canceled | ToolCallStatus::Failed
        );
        let running = matches!(
            tool_call.status(),
            ToolCallStatus::Pending | ToolCallStatus::InProgress
        );
        let has_image_content = tool_call.content().iter().any(|c| c.image().is_some());
        let should_show_raw_input = !has_image_content;
        // A read opens its file instead of showing what it read, unless it failed, to show why.
        let shows_content = !is_read || failed_or_canceled;
        let has_content = shows_content
            && (!tool_call.content().is_empty()
                || (should_show_raw_input && tool_call.raw_input.is_some()));
        let is_open = has_content
            && self
                .entry_view_state
                .read(cx)
                .is_tool_call_content_visible(tool_call);

        let read_file_name = tool_call
            .locations
            .first()
            .filter(|_| is_read)
            .and_then(|location| location.path.file_name())
            .map(|file_name| file_name.to_string_lossy().into_owned());
        let row_id = ("tool-call-activity", entry_ix);
        let row = match read_file_name {
            Some(file_name) => ActivityRow::new(row_id, "Read").details(file_name),
            None => ActivityRow::from_label(row_id, &markdown_line(tool_call.label.read(cx)).0),
        }
        .running(running)
        .failed(failed_or_canceled)
        .map(|row| {
            if has_content {
                row.expanded(is_open).on_click(cx.listener({
                    let id = tool_call.id.clone();
                    move |this, _, window, cx| {
                        this.entry_view_state.update(cx, |state, _cx| {
                            state.toggle_tool_call_expansion(&id);
                        });
                        this.refresh_thread_search(window, cx);
                        cx.notify();
                    }
                }))
            } else if has_location {
                row.on_click(cx.listener(move |this, _, window, cx| {
                    this.open_tool_call_location(entry_ix, 0, window, cx);
                }))
            } else {
                row
            }
        })
        .when(has_location && has_content, |row| {
            row.end_slot(
                PillButton::new(("open-tool-call-location", entry_ix))
                    .size(KitSize::XSmall)
                    .icon(IconName::ArrowUpRight)
                    .tooltip(Tooltip::text("Go to File"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_tool_call_location(entry_ix, 0, window, cx);
                    })),
            )
        });

        let input_output_header = |label: SharedString| {
            Label::new(label)
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .buffer_font(cx)
        };

        let output =
            (is_open && !matches!(tool_call.status(), ToolCallStatus::Rejected)).then(|| {
                v_flex()
                    .pt_1()
                    .gap_1()
                    .when(should_show_raw_input, |this| {
                        this.w_full().child(
                            v_flex()
                                .gap_1()
                                .child(input_output_header("Raw Input:".into()))
                                .children(tool_call.raw_input_markdown.clone().map(|input| {
                                    div().id(("tool-call-raw-input-markdown", entry_ix)).child(
                                        self.render_markdown(
                                            input,
                                            MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                                            cx,
                                        ),
                                    )
                                }))
                                .child(input_output_header("Output:".into())),
                        )
                    })
                    .children(tool_call.content().iter().enumerate().map(
                        |(content_ix, content)| {
                            let output_id = SharedString::from(format!(
                                "tool-call-output-{entry_ix}-{content_ix}"
                            ));
                            div()
                                .id(output_id.clone())
                                .debug_selector(move || output_id.to_string())
                                .child(self.render_tool_call_content(
                                    active_session_id,
                                    entry_ix,
                                    content,
                                    content_ix,
                                    tool_call,
                                    false,
                                    focus_handle,
                                    window,
                                    cx,
                                ))
                        },
                    ))
                    .child(
                        PillButton::new(SharedString::from(format!(
                            "tool_output-collapse-{:?}",
                            tool_call.id
                        )))
                        .size(KitSize::XSmall)
                        .icon(IconName::ChevronUp)
                        .tooltip(Tooltip::text("Collapse"))
                        .on_click(cx.listener({
                            let tool_call_id = tool_call.id.clone();
                            move |this, _, window, cx| {
                                this.entry_view_state.update(cx, |state, _cx| {
                                    state.collapse_tool_call(&tool_call_id);
                                });
                                this.refresh_thread_search(window, cx);
                                cx.notify();
                            }
                        })),
                    )
            });

        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(row)
            .children(output)
    }

    fn tool_call_kind_icon(tool_call: &ToolCall) -> IconName {
        match tool_call.kind() {
            acp_v2::ToolKind::Read => IconName::ToolSearch,
            acp_v2::ToolKind::Edit => IconName::ToolPencil,
            acp_v2::ToolKind::Delete => IconName::ToolDeleteFile,
            acp_v2::ToolKind::Move => IconName::ArrowRightLeft,
            acp_v2::ToolKind::Search => IconName::ToolSearch,
            acp_v2::ToolKind::Execute => IconName::ToolTerminal,
            acp_v2::ToolKind::Think => IconName::ToolThink,
            acp_v2::ToolKind::Fetch => IconName::ToolWeb,
            acp_v2::ToolKind::SwitchMode => IconName::ArrowRightLeft,
            _ => IconName::ToolHammer,
        }
    }

    /// Renders an entry that's part of an activity group. The group's first member carries the
    /// group's line, and the rest show only while the group is expanded.
    pub(super) fn render_activity_group_member(
        &self,
        entry_ix: usize,
        total_entries: usize,
        entry: &AgentThreadEntry,
        group: &ActivityGroup,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let expanded = self
            .entry_view_state
            .read(cx)
            .is_activity_group_expanded(group);
        let is_first = entry_ix == group.entries.start;
        if !is_first && !expanded {
            return None;
        }

        let member = expanded
            .then(|| match entry {
                AgentThreadEntry::ToolCall(tool_call) => {
                    let session_id = self.thread.read(cx).session_id();
                    let focus_handle = self.focus_handle(cx);
                    Some(
                        self.render_any_tool_call(
                            session_id,
                            entry_ix,
                            tool_call,
                            &focus_handle,
                            ToolCallLayout::Grouped,
                            window,
                            cx,
                        )
                        .into_any_element(),
                    )
                }
                AgentThreadEntry::AssistantMessage(message) => self.render_assistant_message(
                    entry_ix,
                    total_entries,
                    message,
                    STEP_GAP,
                    window,
                    cx,
                ),
                _ => None,
            })
            .flatten();
        let header = is_first.then(|| self.render_activity_group_header(group, expanded, cx));

        Some(
            v_flex()
                .w_full()
                .children(header)
                .children(member)
                .into_any_element(),
        )
    }

    fn render_activity_group_header(
        &self,
        group: &ActivityGroup,
        expanded: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let thread = self.thread.read(cx);
        let entries = thread.entries();
        let tool_calls = group
            .tool_call_entries
            .iter()
            .filter_map(|&entry_ix| match entries.get(entry_ix) {
                Some(AgentThreadEntry::ToolCall(tool_call)) => Some(tool_call),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Until something else shows, the agent is still at work on what the group describes.
        let running =
            group.running || (group.ends_thread && thread.status() == ThreadStatus::Generating);
        let summary = ActivitySummary::new(&tool_calls, cx);
        let (action, details) = summary.label(running);
        let group_start = group.entries.start;

        div().px(TEXT_INSET).pt(ROW_GAP).child(
            ActivityRow::new(("activity-group", group_start), action)
                .details(details)
                .when(
                    summary.diff_stats.lines_added > 0 || summary.diff_stats.lines_removed > 0,
                    |row| row.diff_stats(summary.diff_stats),
                )
                .running(running)
                .expanded(expanded)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let group = ActivityGroup::containing(this.thread.read(cx), group_start, cx);
                    if let Some(group) = group {
                        this.toggle_activity_group(&group, window, cx);
                    }
                })),
        )
    }

    fn toggle_activity_group(
        &mut self,
        group: &ActivityGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.entry_view_state.update(cx, |state, _cx| {
            state.toggle_activity_group_expansion(group);
        });
        // Members other than the first change height without being rendered, so their cached
        // heights would otherwise go stale.
        self.list_state.remeasure_items(group.entries.clone());
        self.refresh_thread_search(window, cx);
        cx.notify();
    }
}

/// What the tool calls of an activity group did, counted for the group's line.
#[derive(Default)]
struct ActivitySummary {
    tool_calls: usize,
    files_read: usize,
    searches: usize,
    fetches: usize,
    commands: usize,
    other_tools: usize,
    has_edits: bool,
    changed_files: Vec<String>,
    /// Edits and deletions whose file isn't known, counted as one file each.
    unknown_changed_files: usize,
    first_command: Option<String>,
    diff_stats: DiffStats,
}

impl ActivitySummary {
    fn new(tool_calls: &[&ToolCall], cx: &App) -> Self {
        let mut summary = Self::default();
        for tool_call in tool_calls {
            summary.tool_calls += 1;
            let is_command = tool_call.terminals().next().is_some()
                || matches!(tool_call.kind(), acp_v2::ToolKind::Execute);
            let has_diffs = tool_call.diffs().next().is_some()
                || tool_call
                    .content()
                    .iter()
                    .any(|content| matches!(content, ToolCallContent::DiffPatch { .. }));
            if is_command {
                summary.commands += 1;
                if summary.first_command.is_none() {
                    summary.first_command = Some(command_line(tool_call, cx));
                }
                continue;
            }
            match tool_call.kind() {
                acp_v2::ToolKind::Read => summary.files_read += 1,
                acp_v2::ToolKind::Search => summary.searches += 1,
                acp_v2::ToolKind::Fetch => summary.fetches += 1,
                acp_v2::ToolKind::Delete => summary.add_changed_files(tool_call, cx),
                acp_v2::ToolKind::Edit | acp_v2::ToolKind::Move => {
                    summary.has_edits = true;
                    summary.add_changed_files(tool_call, cx);
                }
                _ if has_diffs => {
                    summary.has_edits = true;
                    summary.add_changed_files(tool_call, cx);
                }
                _ => summary.other_tools += 1,
            }
            let stats = tool_call_diff_stats(tool_call, cx);
            summary.diff_stats.lines_added += stats.lines_added;
            summary.diff_stats.lines_removed += stats.lines_removed;
        }
        summary
    }

    fn add_changed_files(&mut self, tool_call: &ToolCall, cx: &App) {
        let mut paths = tool_call
            .locations
            .iter()
            .map(|location| location.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            paths.extend(
                tool_call
                    .diffs()
                    .filter_map(|diff| diff.read(cx).file_path(cx)),
            );
        }
        if paths.is_empty() {
            self.unknown_changed_files += 1;
        }
        for path in paths {
            if !self.changed_files.contains(&path) {
                self.changed_files.push(path);
            }
        }
    }

    /// The group's line, as what the agent did and the details: the files it changed, then what
    /// it explored, then the commands it ran.
    fn label(&self, running: bool) -> (&'static str, String) {
        if self.commands > 0 && self.commands == self.tool_calls {
            let details = match (self.commands, &self.first_command) {
                (1, Some(command)) => command.clone(),
                (commands, _) => count(commands, "command", "commands"),
            };
            return (if running { "Running" } else { "Ran" }, details);
        }

        let mut explored = [
            (self.files_read, "file", "files"),
            (self.searches, "search", "searches"),
            (self.fetches, "fetch", "fetches"),
            (self.other_tools, "tool", "tools"),
        ]
        .into_iter()
        .filter(|(amount, _, _)| *amount > 0)
        .map(|(amount, singular, plural)| count(amount, singular, plural))
        .collect::<Vec<_>>();

        let mut parts = Vec::new();
        let changed_files = self.changed_files.len() + self.unknown_changed_files;
        let action = if changed_files > 0 {
            let single_file_name = (changed_files == 1)
                .then(|| self.changed_files.first())
                .flatten()
                .and_then(|path| Path::new(path).file_name())
                .map(|name| name.to_string_lossy().into_owned());
            parts.push(single_file_name.unwrap_or_else(|| count(changed_files, "file", "files")));
            if let Some(first) = explored.first_mut() {
                *first = format!("explored {first}");
            }
            match (self.has_edits, running) {
                (true, true) => "Editing",
                (true, false) => "Edited",
                (false, true) => "Deleting",
                (false, false) => "Deleted",
            }
        } else if self.files_read + self.searches + self.fetches + self.commands == 0 {
            if running { "Running" } else { "Ran" }
        } else if running {
            "Exploring"
        } else {
            "Explored"
        };
        parts.extend(explored);
        if self.commands > 0 {
            parts.push(format!(
                "ran {}",
                count(self.commands, "command", "commands")
            ));
        }
        (action, parts.join(", "))
    }
}

fn count(amount: usize, singular: &str, plural: &str) -> String {
    format!("{amount} {}", if amount == 1 { singular } else { plural })
}

/// The first line of the command a tool call runs.
fn command_line(tool_call: &ToolCall, cx: &App) -> String {
    let label = tool_call.label.read(cx);
    let command = if label.source().starts_with("```") {
        command_text(label)
    } else {
        markdown_line(label).0.to_string()
    };
    command
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The lines added and removed by a tool call's edits.
fn tool_call_diff_stats(tool_call: &ToolCall, cx: &App) -> DiffStats {
    tool_call
        .diffs()
        .filter_map(|diff| {
            let diff = diff.read(cx);
            let buffer_id = diff.buffer().read(cx).remote_id();
            diff.multibuffer().read(cx).diff_for(buffer_id)
        })
        .fold(DiffStats::default(), |mut total, buffer_diff| {
            let stats = DiffStats::single_file(buffer_diff.read(cx));
            total.lines_added += stats.lines_added;
            total.lines_removed += stats.lines_removed;
            total
        })
}

/// Markdown as one line of text, with the ranges it quotes as code. In a tool call's label,
/// those are usually the paths and patterns the tool acted on.
pub(super) fn markdown_line(label: &Markdown) -> (SharedString, Vec<Range<usize>>) {
    let parsed = label.parsed_markdown();
    let source = parsed.source();
    let mut text = String::new();
    let mut code_ranges = Vec::new();
    for (range, event) in parsed.events().iter() {
        let (fragment, is_code) = match event {
            MarkdownEvent::Text => (source.get(range.clone()).unwrap_or_default(), false),
            MarkdownEvent::SubstitutedText(substituted) => (substituted.as_str(), false),
            MarkdownEvent::Code => (source.get(range.clone()).unwrap_or_default(), true),
            MarkdownEvent::SubstitutedCode(substituted) => (substituted.as_str(), true),
            MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => (" ", false),
            _ => continue,
        };
        let start = text.len();
        text.push_str(fragment);
        if is_code && text.len() > start {
            code_ranges.push(start..text.len());
        }
    }

    // A label that hasn't been parsed yet, or isn't markdown, has no text events.
    if text.trim().is_empty() {
        return (label.source().clone(), Vec::new());
    }
    (text.into(), code_ranges)
}

/// How long a command runs before its card shows the elapsed time.
const ELAPSED_DISPLAY_THRESHOLD: Duration = Duration::from_secs(10);

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

pub(super) struct TerminalSandboxWarning {
    pub(super) title: SharedString,
    pub(super) detail: SharedString,
    pub(super) docs_url: SharedString,
}

impl ThreadView {
    fn is_first_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        tool_call_id: &acp_v2::ToolCallId,
        cx: &App,
    ) -> bool {
        self.conversation
            .read(cx)
            .pending_tool_call(active_session_id, cx)
            .map_or(false, |(pending_session_id, pending_tool_call_id, _)| {
                self.thread.read(cx).session_id() == &pending_session_id
                    && tool_call_id == &pending_tool_call_id
            })
    }

    /// Renders the card of an edit, a command that hasn't started a terminal yet, or any other
    /// tool call waiting for permission.
    fn render_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        tool_call: &ToolCall,
        focus_handle: &FocusHandle,
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let has_location = tool_call.locations.len() == 1;
        let failed_or_canceled = matches!(
            tool_call.status(),
            ToolCallStatus::Rejected | ToolCallStatus::Canceled | ToolCallStatus::Failed
        );
        let running = matches!(
            tool_call.status(),
            ToolCallStatus::Pending | ToolCallStatus::InProgress
        );
        let needs_confirmation =
            matches!(tool_call.status(), ToolCallStatus::WaitingForConfirmation);
        let is_terminal_tool = matches!(tool_call.kind(), acp_v2::ToolKind::Execute);
        let is_edit = matches!(tool_call.kind(), acp_v2::ToolKind::Edit)
            || tool_call.diffs().next().is_some()
            || tool_call
                .content()
                .iter()
                .any(|content| matches!(content, ToolCallContent::DiffPatch { .. }));
        let is_cancelled_edit = is_edit && matches!(tool_call.status(), ToolCallStatus::Canceled);
        let (has_revealed_diff, tool_call_output_focus, tool_call_output_focus_handle) = tool_call
            .diffs()
            .next()
            .and_then(|diff| {
                let editor = self
                    .entry_view_state
                    .read(cx)
                    .entry(entry_ix)
                    .and_then(|entry| entry.editor_for_diff(diff))?;
                let has_revealed_diff = diff.read(cx).has_revealed_range(cx);
                let has_focus = editor.read(cx).is_focused(window);
                let focus_handle = editor.focus_handle(cx);
                Some((has_revealed_diff, has_focus, focus_handle))
            })
            .unwrap_or_else(|| (false, false, focus_handle.clone()));

        let has_image_content = tool_call.content().iter().any(|c| c.image().is_some());
        let should_show_raw_input = !is_terminal_tool && !is_edit && !has_image_content;
        let has_content = !tool_call.content().is_empty()
            || (should_show_raw_input && tool_call.raw_input.is_some());
        let is_collapsible = has_content && !needs_confirmation;
        let is_open = self
            .entry_view_state
            .read(cx)
            .is_tool_call_content_visible(tool_call);

        let header = self
            .render_tool_card_header(
                entry_ix,
                tool_call,
                is_terminal_tool,
                is_edit,
                is_cancelled_edit,
                has_revealed_diff,
                running,
                failed_or_canceled,
                cx,
            )
            .when(has_location && !is_terminal_tool, |header| {
                header.hover_action(
                    PillButton::new(("open-tool-call-location", entry_ix))
                        .size(KitSize::XSmall)
                        .icon(IconName::ArrowUpRight)
                        .tooltip(Tooltip::text("Go to File"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_tool_call_location(entry_ix, 0, window, cx);
                        })),
                )
            })
            .when(is_terminal_tool, |header| {
                header.hover_action(copy_command_button(
                    ("copy-command", entry_ix),
                    command_text(tool_call.label.read(cx)),
                ))
            })
            .when(tool_call_output_focus, |header| {
                header.action(
                    PillButton::new("open-file-button")
                        .size(KitSize::XSmall)
                        .label("Open File")
                        .child(
                            KeyBinding::for_action_in(
                                &OpenExcerpts,
                                &tool_call_output_focus_handle,
                                cx,
                            )
                            .size(rems_from_px(12_f32)),
                        )
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenExcerpts), cx)
                        }),
                )
            })
            .when(is_collapsible, |header| {
                header.expanded(is_open).on_click(cx.listener({
                    let id = tool_call.id.clone();
                    move |this, _, window, cx| {
                        this.entry_view_state.update(cx, |state, _cx| {
                            state.toggle_tool_call_expansion(&id);
                        });
                        this.refresh_thread_search(window, cx);
                        cx.notify();
                    }
                }))
            });

        let content =
            |cx: &Context<Self>| {
                v_flex()
                    .w_full()
                    .children(tool_call.content().iter().enumerate().map(
                        |(content_ix, content)| {
                            let output_id = SharedString::from(format!(
                                "tool-call-output-{entry_ix}-{content_ix}"
                            ));
                            div()
                                .id(output_id.clone())
                                .debug_selector(move || output_id.to_string())
                                .child(self.render_tool_call_content(
                                    active_session_id,
                                    entry_ix,
                                    content,
                                    content_ix,
                                    tool_call,
                                    true,
                                    focus_handle,
                                    window,
                                    cx,
                                ))
                        },
                    ))
            };

        let mut sections = Vec::new();
        if is_terminal_tool {
            sections.push(
                self.render_command_block(tool_call.label.clone(), window, cx)
                    .into_any_element(),
            );
        }
        match tool_call.status() {
            ToolCallStatus::WaitingForConfirmation => {
                let details = content(cx)
                    .when_some(
                        tool_call.sandbox_authorization_details.as_ref(),
                        |this, details| {
                            this.child(self.render_sandbox_authorization_details(
                                entry_ix,
                                &tool_call.id,
                                details,
                                window,
                                cx,
                            ))
                        },
                    )
                    .when_some(
                        tool_call.sandbox_fallback_authorization_details.as_ref(),
                        |this, details| {
                            this.child(
                                self.render_sandbox_fallback_authorization_details(details, cx),
                            )
                        },
                    )
                    .when(should_show_raw_input, |this| {
                        this.child(
                            self.render_raw_input_disclosure(entry_ix, tool_call, window, cx),
                        )
                    });
                sections.push(details.into_any_element());
            }
            ToolCallStatus::Pending | ToolCallStatus::InProgress
                if is_edit
                    && tool_call.content().is_empty()
                    && self.as_native_connection(cx).is_some() =>
            {
                sections.push(self.render_diff_loading(cx));
            }
            ToolCallStatus::Rejected => {}
            ToolCallStatus::Pending
            | ToolCallStatus::InProgress
            | ToolCallStatus::Completed
            | ToolCallStatus::Failed
            | ToolCallStatus::Canceled => {
                if is_open && has_content {
                    sections.push(content(cx).into_any_element());
                }
            }
        }

        let permission_buttons = tool_call
            .authorization_id()
            .and_then(|id| self.thread.read(cx).permission_request(id))
            .map(|request| {
                self.render_permission_buttons(
                    self.thread.read(cx).session_id().clone(),
                    self.is_first_tool_call(active_session_id, &tool_call.id, cx),
                    request,
                    entry_ix,
                    focus_handle,
                    self.sandbox_confusables_block_allow(tool_call, cx),
                    cx,
                )
            });

        let card = ToolCard::new(("tool-card", entry_ix), header)
            .sections(sections)
            .dashed(failed_or_canceled)
            .bare(matches!(
                layout,
                ToolCallLayout::Embedded | ToolCallLayout::Floating
            ));

        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .map(|this| {
                if layout == ToolCallLayout::Floating {
                    // The buttons stay in view while a long request scrolls.
                    this.child(
                        div()
                            .id(("floating-tool-call-body", entry_ix))
                            .max_h_40()
                            .overflow_y_scroll()
                            .child(card),
                    )
                    .children(permission_buttons.map(|buttons| {
                        div()
                            .w_full()
                            .p_1p5()
                            .border_t_1()
                            .border_color(KitTheme::global(cx).border)
                            .child(buttons)
                    }))
                } else {
                    this.child(
                        card.when_some(permission_buttons, |card, buttons| card.footer(buttons)),
                    )
                }
            })
    }

    fn render_tool_card_header(
        &self,
        entry_ix: usize,
        tool_call: &ToolCall,
        is_terminal_tool: bool,
        is_edit: bool,
        is_cancelled_edit: bool,
        has_revealed_diff: bool,
        running: bool,
        failed_or_canceled: bool,
        cx: &Context<Self>,
    ) -> ToolCardHeader {
        let theme = KitTheme::global(cx);
        let has_location = tool_call.locations.len() == 1;
        let is_file = matches!(tool_call.kind(), acp_v2::ToolKind::Edit) && has_location;
        let interrupted_edit = is_file && is_cancelled_edit && has_revealed_diff;

        let icon_path: SharedString = tool_call
            .locations
            .first()
            .filter(|_| is_file)
            .and_then(|location| FileIcons::get_icon(&location.path, cx))
            .unwrap_or_else(|| Self::tool_call_kind_icon(tool_call).path().into());
        let icon = div()
            .id(("tool-call-icon", entry_ix))
            .relative()
            .child(KitIcon::empty().path(icon_path).with_size(KitSize::Small))
            .when(interrupted_edit, |this| {
                this.child(
                    div().absolute().top(px(-3.)).left(px(-3.)).child(
                        kit_icon(IconName::Warning)
                            .with_size(KitSize::XSmall)
                            .text_color(theme.warning),
                    ),
                )
            })
            .when_some(
                Self::tool_call_icon_tooltip(tool_call.tool_name.as_ref(), interrupted_edit),
                |this, tooltip| this.tooltip(Tooltip::text(tooltip)),
            );

        let title = if is_terminal_tool {
            let command = command_text(tool_call.label.read(cx));
            command_title(&command, cx).into_any_element()
        } else {
            let (label, highlights) = markdown_line(tool_call.label.read(cx));
            if running {
                ShimmerText::new(label).into_any_element()
            } else {
                highlighted_label(label, highlights, cx).into_any_element()
            }
        };

        let diff_stats = is_edit.then(|| tool_call_diff_stats(tool_call, cx));

        let diff_for_discard = if has_revealed_diff && is_cancelled_edit {
            tool_call.diffs().next().cloned()
        } else {
            None
        };

        ToolCardHeader::new(
            SharedString::from(format!("inner-tool-call-header-{entry_ix}")),
            icon,
            title,
        )
        .when_some(
            diff_stats.filter(|stats| stats.lines_added > 0 || stats.lines_removed > 0),
            |header, stats| header.meta(diff_stat(stats, cx)),
        )
        .when(failed_or_canceled, |header| {
            if is_cancelled_edit && !has_revealed_diff {
                header.meta(
                    div()
                        .id(("interrupted-edit", entry_ix))
                        .tooltip(Tooltip::text("Interrupted Edit"))
                        .child(kit_icon(IconName::XCircle).with_size(KitSize::Small)),
                )
            } else if is_cancelled_edit {
                header
            } else {
                header.meta(
                    kit_icon(IconName::Close)
                        .with_size(KitSize::Small)
                        .text_color(theme.danger),
                )
            }
        })
        .when_some(diff_for_discard, |header, diff| {
            let tool_call_id = tool_call.id.clone();
            if self.discarded_partial_edits.contains(&tool_call_id) {
                return header;
            }
            header.action(
                PillButton::new(("discard-partial-edit", entry_ix))
                    .size(KitSize::XSmall)
                    .icon(IconName::Undo)
                    .tooltip(move |_, cx| {
                        Tooltip::with_meta(
                            "Discard Interrupted Edit",
                            None,
                            "You can discard this interrupted partial edit and restore the original file content.",
                            cx,
                        )
                    })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        let diff_data = diff.read(cx);
                        let base_text = diff_data.base_text().clone();
                        let buffer = diff_data.buffer().clone();
                        buffer.update(cx, |buffer, cx| {
                            buffer.set_text(base_text.as_ref(), cx);
                        });
                        this.discarded_partial_edits.insert(tool_call_id.clone());
                        cx.notify();
                    })),
            )
        })
    }

    fn render_raw_input_disclosure(
        &self,
        entry_ix: usize,
        tool_call: &ToolCall,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let is_raw_input_expanded = self.expanded_tool_call_raw_inputs.contains(&tool_call.id);
        let label = if is_raw_input_expanded {
            "Raw Input:"
        } else {
            "View Raw Input"
        };

        v_flex()
            .p_2()
            .gap_1()
            .border_t_1()
            .border_color(KitTheme::global(cx).border)
            .child(
                ActivityRow::new(("raw-input-disclosure", entry_ix), label)
                    .expanded(is_raw_input_expanded)
                    .on_click(cx.listener({
                        let id = tool_call.id.clone();
                        move |this: &mut Self, _, _, cx| {
                            if !this.expanded_tool_call_raw_inputs.remove(&id) {
                                this.expanded_tool_call_raw_inputs.insert(id.clone());
                            }
                            cx.notify();
                        }
                    })),
            )
            .when(is_raw_input_expanded, |this| {
                this.children(tool_call.raw_input_markdown.clone().map(|input| {
                    self.render_markdown(
                        input,
                        MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                        cx,
                    )
                }))
            })
    }

    /// The full text of a command, for approving it or reading one that doesn't fit on a line.
    fn render_command_block(
        &self,
        command: Entity<Markdown>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let mut style =
            MarkdownStyle::themed(MarkdownFont::Agent, window, cx).with_agent_buffer_font(cx);
        style.container_style.text.font_size = Some(rems_from_px(12_f32).into());
        style.container_style.text.line_height = Some(rems_from_px(17_f32).into());
        style.height_is_multiple_of_line_height = true;
        // Soft-wrap the command instead of horizontally scrolling it: the card is
        // narrow, and in scroll mode a long command wraps anyway but its wrapped
        // lines don't pick up the code block's left padding. Wrap mode lays the
        // text out as a normal block inside the padded content box, so every
        // line (wrapped or not) is padded consistently.
        style.code_block_overflow_x_scroll = false;

        // The card's header copies the command without the fences the label's
        // markdown keeps it in, so the code block's own copy button would repeat it.
        let markdown_element = self
            .render_markdown(command, style, cx)
            .code_block_renderer(CodeBlockRenderer::Default {
                copy_button_visibility: CopyButtonVisibility::Hidden,
                wrap_button_visibility: markdown::WrapButtonVisibility::Hidden,
                border: false,
            });

        v_flex().px_1p5().py_1().child(markdown_element)
    }

    pub(super) fn render_terminal_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        terminal: &Entity<acp_thread::Terminal>,
        tool_call: &ToolCall,
        focus_handle: &FocusHandle,
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let terminal_data = terminal.read(cx);
        let working_dir = terminal_data.working_dir();
        let started_at = terminal_data.started_at();

        let tool_failed = matches!(
            tool_call.status(),
            ToolCallStatus::Rejected | ToolCallStatus::Canceled | ToolCallStatus::Failed
        );

        let permission_request = tool_call
            .authorization_id()
            .and_then(|id| self.thread.read(cx).permission_request(id));
        let needs_confirmation = permission_request.is_some();

        let output = terminal_data.output();
        let command_finished = output.is_some()
            && !matches!(
                tool_call.status(),
                ToolCallStatus::InProgress | ToolCallStatus::Pending
            );
        let truncated_output =
            output.is_some_and(|output| output.original_content_len > output.content.len());
        let output_line_count = output.map(|output| output.content_line_count).unwrap_or(0);

        let command_failed = command_finished
            && output.is_some_and(|output| {
                output
                    .exit_status
                    .exit_code
                    .is_some_and(|exit_code| exit_code != 0)
                    || output.exit_status.signal.is_some()
            });
        let exit_code = output.and_then(|output| output.exit_status.exit_code);

        let time_elapsed = if let Some(output) = output {
            output.ended_at.duration_since(started_at)
        } else {
            started_at.elapsed()
        };
        let running = !command_finished && !needs_confirmation;

        let terminal_id = terminal.entity_id().to_string();
        let child_id = |name: &str| format!("terminal-tool-{name}-{terminal_id}");
        let header_group = SharedString::from(format!(
            "terminal-tool-header-group-{}",
            terminal.entity_id()
        ));

        let working_dir = working_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "current directory".to_string());
        let command = command_text(tool_call.label.read(cx));
        let description = tool_call
            .raw_input
            .as_ref()
            .and_then(|input| input.get("description"))
            .and_then(|description| description.as_str())
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string);

        let is_expanded = self
            .entry_view_state
            .read(cx)
            .is_tool_call_expanded(&tool_call.id);

        let truncated_tooltip = truncated_output.then(|| {
            if let Some(output) = output {
                if output_line_count + 10 > terminal::MAX_SCROLL_HISTORY_LINES {
                    format!(
                        "Output exceeded terminal max lines and was \
                         truncated, the model received the first {}.",
                        format_file_size(output.content.len() as u64, true)
                    )
                } else {
                    format!(
                        "Output is {} long, and to avoid unexpected token usage, \
                         only {} was sent back to the agent.",
                        format_file_size(output.original_content_len as u64, true),
                        format_file_size(output.content.len() as u64, true)
                    )
                }
            } else {
                "Output was truncated".to_string()
            }
        });

        let theme = KitTheme::global(cx);
        let icon = if running {
            Spinner::new()
                .icon(kit_icon(IconName::LoadCircle))
                .with_size(KitSize::Small)
                .into_any_element()
        } else {
            kit_icon(IconName::ToolTerminal)
                .with_size(KitSize::Small)
                .into_any_element()
        };

        let header = ToolCardHeader::new(
            SharedString::from(child_id("header")),
            icon,
            div()
                .id(SharedString::from(child_id("command")))
                .tooltip(Tooltip::text(format!("Working directory: {working_dir}")))
                .map(|this| match &description {
                    Some(description) => {
                        let verb = if needs_confirmation {
                            "Run"
                        } else if running {
                            "Running"
                        } else {
                            "Ran"
                        };
                        this.child(description_title(verb, description, cx))
                    }
                    None => this.child(command_title(&command, cx)),
                }),
        )
        .when(time_elapsed > ELAPSED_DISPLAY_THRESHOLD, |header| {
            header.meta(duration_alt_display(time_elapsed))
        })
        .when_some(truncated_tooltip, |header, tooltip| {
            header.meta(
                div()
                    .id(SharedString::from(child_id("truncated")))
                    .tooltip(Tooltip::text(tooltip))
                    .child(kit_icon(IconName::Info).with_size(KitSize::Small)),
            )
        })
        .when(tool_failed || command_failed, |header| {
            header.meta(
                div()
                    .id(SharedString::from(child_id("failed")))
                    .debug_selector(move || format!("terminal-tool-failed-{exit_code:?}"))
                    .when_some(exit_code, |this, code| {
                        this.tooltip(Tooltip::text(format!("Exited with code {code}")))
                    })
                    .child(
                        kit_icon(IconName::Close)
                            .with_size(KitSize::Small)
                            .text_color(theme.danger),
                    ),
            )
        })
        .when_some(tool_call.sandbox_not_applied.as_ref(), |header, reason| {
            let TerminalSandboxWarning {
                title,
                detail,
                docs_url,
            } = self.sandbox_not_applied_warning(reason, cx);
            header.meta(
                PillButton::new(SharedString::from(child_id("sandbox-not-applied")))
                    .size(KitSize::XSmall)
                    .icon(IconName::LockOff)
                    .tooltip(move |_window, cx| {
                        Tooltip::with_meta(
                            title.clone(),
                            None,
                            format!("{detail} Click to learn more about sandboxing."),
                            cx,
                        )
                    })
                    .on_click(move |_, _, cx| cx.open_url(&docs_url)),
            )
        })
        .hover_action(copy_command_button(
            SharedString::from(format!("{header_group}-copy-command")),
            command.clone(),
        ))
        .when(running && terminal_data.is_process_backed(), |header| {
            header.action(
                PillButton::new(SharedString::from(child_id("stop")))
                    .size(KitSize::XSmall)
                    .icon(IconName::Stop)
                    .debug_selector("ICON-Stop")
                    .tooltip(move |_window, cx| {
                        Tooltip::with_meta(
                            "Stop This Command",
                            None,
                            "Also possible by placing your cursor inside the \
                             terminal and using regular terminal bindings.",
                            cx,
                        )
                    })
                    .on_click(cx.listener({
                        let terminal = terminal.clone();
                        move |this, _event, _window, cx| {
                            terminal.update(cx, |terminal, cx| {
                                terminal.stop_by_user(cx);
                            });
                            if AgentSettings::get_global(cx).cancel_generation_on_terminal_stop {
                                this.cancel_generation(cx);
                            }
                        }
                    })),
            )
        })
        .expanded(is_expanded)
        .on_click(cx.listener({
            let id = tool_call.id.clone();
            move |this, _event, window, cx| {
                this.entry_view_state.update(cx, |state, _cx| {
                    state.toggle_tool_call_expansion(&id);
                });
                this.refresh_thread_search(window, cx);
                cx.notify();
            }
        }));

        let terminal_view = self
            .entry_view_state
            .read(cx)
            .entry(entry_ix)
            .and_then(|entry| entry.terminal(terminal));

        // A header with a description leaves the command to the body. Otherwise the header shows the
        // command's first line, and the whole command shows when it's longer. Either way the whole
        // command shows while it's waiting to be approved.
        let show_full_command = needs_confirmation
            || (is_expanded && (description.is_some() || command.contains('\n')));
        let mut sections = Vec::new();
        if show_full_command {
            sections.push(
                self.render_command_block(tool_call.label.clone(), window, cx)
                    .into_any_element(),
            );
        }
        if is_expanded && let Some(terminal_view) = terminal_view {
            let element = if terminal_view
                .read(cx)
                .content_mode(window, cx)
                .is_scrollable()
            {
                div()
                    .h(rems(18. + TERMINAL_CONTENT_TOP_PADDING))
                    .child(terminal_view)
                    .into_any_element()
            } else {
                terminal_view.into_any_element()
            };
            sections.push(
                div()
                    .text_ui_sm(cx)
                    .h_full()
                    .on_action(cx.listener(|_this, _: &NewTerminal, window, cx| {
                        window.dispatch_action(NewThread.boxed_clone(), cx);
                        cx.stop_propagation();
                    }))
                    .child(element)
                    .into_any_element(),
            );
        }

        let permission_buttons = permission_request.map(|request| {
            let is_first = self.is_first_tool_call(active_session_id, &tool_call.id, cx);
            let allow_disabled = self.sandbox_confusables_block_allow(tool_call, cx);
            self.render_permission_buttons(
                self.thread.read(cx).session_id().clone(),
                is_first,
                request,
                entry_ix,
                focus_handle,
                allow_disabled,
                cx,
            )
        });

        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(
                ToolCard::new(SharedString::from(child_id("card")), header)
                    .sections(sections)
                    .when_some(permission_buttons, |card, buttons| card.footer(buttons))
                    .dashed(tool_failed || command_failed)
                    .bare(layout.row_gap().is_none()),
            )
            .into_any()
    }

    fn render_permission_buttons(
        &self,
        session_id: acp_v2::SessionId,
        is_first: bool,
        request: &PermissionRequest,
        entry_ix: usize,
        focus_handle: &FocusHandle,
        // When true, the "allow" choices are disabled (e.g. an unacknowledged
        // surprising-Unicode warning is showing). "Deny"/"Retry" stay enabled.
        allow_disabled: bool,
        cx: &Context<Self>,
    ) -> Div {
        let (Some(options), Some(tool_call_id)) =
            (request.legacy_options(), request.legacy_tool_call_id())
        else {
            return div();
        };
        match options {
            PermissionOptions::Flat(options) => self.render_permission_buttons_flat(
                session_id,
                is_first,
                options,
                entry_ix,
                request.id,
                focus_handle,
                allow_disabled,
                cx,
            ),
            PermissionOptions::Dropdown(choices) => self.render_permission_buttons_with_dropdown(
                is_first,
                choices,
                None,
                entry_ix,
                session_id,
                request.id,
                tool_call_id.clone(),
                focus_handle,
                allow_disabled,
                cx,
            ),
            PermissionOptions::DropdownWithPatterns {
                choices,
                patterns,
                tool_name,
            } => self.render_permission_buttons_with_dropdown(
                is_first,
                choices,
                Some((patterns, tool_name)),
                entry_ix,
                session_id,
                request.id,
                tool_call_id.clone(),
                focus_handle,
                allow_disabled,
                cx,
            ),
        }
    }

    fn render_permission_buttons_with_dropdown(
        &self,
        is_first: bool,
        choices: &[PermissionOptionChoice],
        patterns: Option<(&[PermissionPattern], &str)>,
        entry_ix: usize,
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
        tool_call_id: acp_v2::ToolCallId,
        focus_handle: &FocusHandle,
        allow_disabled: bool,
        cx: &Context<Self>,
    ) -> Div {
        let selection = self
            .conversation
            .read(cx)
            .permission_selections
            .get(&request_id);

        let selected_index = selection
            .and_then(|s| s.choice_index())
            .unwrap_or_else(|| choices.len().saturating_sub(1));

        let dropdown_label: SharedString =
            if matches!(selection, Some(PermissionSelection::SelectedPatterns(_))) {
                "Always for selected commands".into()
            } else {
                choices
                    .get(selected_index)
                    .or(choices.last())
                    .map(|choice| choice.label())
                    .unwrap_or_else(|| "Only this time".into())
            };
        let permission_buttons_selector = {
            let session_id = session_id.clone();
            let dropdown_label = dropdown_label.clone();
            move || format!("PERMISSION_BUTTONS-{session_id}-{dropdown_label}")
        };

        let dropdown = if let Some((pattern_list, tool_name)) = patterns {
            self.render_permission_granularity_dropdown_with_patterns(
                choices,
                pattern_list,
                tool_name,
                dropdown_label,
                entry_ix,
                session_id.clone(),
                request_id,
                is_first,
                cx,
            )
        } else {
            self.render_permission_granularity_dropdown(
                choices,
                dropdown_label,
                entry_ix,
                session_id.clone(),
                request_id,
                tool_call_id,
                selected_index,
                is_first,
                cx,
            )
        };

        let key_binding = |action: &dyn Action| {
            KeyBinding::for_action_in(action, focus_handle, cx).size(rems_from_px(12_f32))
        };

        h_flex()
            .w_full()
            .debug_selector(permission_buttons_selector)
            .gap_2()
            .justify_between()
            .child(dropdown)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        PillButton::new(("deny-btn", entry_ix))
                            .size(KitSize::Small)
                            .label("Deny")
                            .children(is_first.then(|| key_binding(&RejectOnce)))
                            .on_click(cx.listener({
                                let session_id = session_id.clone();
                                move |this, _, window, cx| {
                                    this.authorize_with_granularity(
                                        session_id.clone(),
                                        request_id,
                                        false,
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        PillButton::new(("allow-btn", entry_ix))
                            .size(KitSize::Small)
                            .variant(ButtonVariant::Primary)
                            .label("Allow")
                            .disabled(allow_disabled)
                            .children(
                                (is_first && !allow_disabled).then(|| key_binding(&AllowOnce)),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.authorize_with_granularity(
                                    session_id.clone(),
                                    request_id,
                                    true,
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
    }

    fn permission_dropdown_trigger(
        &self,
        current_label: SharedString,
        entry_ix: usize,
        is_first: bool,
        cx: &Context<Self>,
    ) -> PillButton {
        PillButton::new(("granularity-trigger", entry_ix))
            .size(KitSize::Small)
            .label(current_label)
            .dropdown_caret(true)
            .children(is_first.then(|| {
                KeyBinding::for_action_in(
                    &crate::OpenPermissionDropdown as &dyn Action,
                    &self.focus_handle(cx),
                    cx,
                )
                .size(rems_from_px(12_f32))
            }))
    }

    fn render_permission_granularity_dropdown(
        &self,
        choices: &[PermissionOptionChoice],
        current_label: SharedString,
        entry_ix: usize,
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
        tool_call_id: acp_v2::ToolCallId,
        selected_index: usize,
        is_first: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let menu_options: Vec<(usize, SharedString)> = choices
            .iter()
            .enumerate()
            .map(|(i, choice)| (i, choice.label()))
            .collect();

        let permission_dropdown_handle = self.permission_dropdown_handle.clone();

        PopoverMenu::new(("permission-granularity", entry_ix))
            .with_handle(permission_dropdown_handle)
            .trigger(self.permission_dropdown_trigger(current_label, entry_ix, is_first, cx))
            .menu(move |window, cx| {
                let tool_call_id = tool_call_id.clone();
                let session_id = session_id.clone();
                let options = menu_options.clone();

                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    for (index, display_name) in options.iter() {
                        let display_name = display_name.clone();
                        let index = *index;
                        let tool_call_id_for_entry = tool_call_id.clone();
                        let session_id = session_id.clone();
                        let is_selected = index == selected_index;
                        menu = menu.toggleable_entry(
                            display_name,
                            is_selected,
                            IconPosition::End,
                            None,
                            move |window, cx| {
                                window.dispatch_action(
                                    SelectPermissionGranularity {
                                        tool_call_id: tool_call_id_for_entry.0.to_string(),
                                        request_id: Some(request_id),
                                        session_id: Some(session_id.0.to_string()),
                                        index,
                                    }
                                    .boxed_clone(),
                                    cx,
                                );
                            },
                        );
                    }

                    menu
                }))
            })
            .into_any_element()
    }

    fn render_permission_granularity_dropdown_with_patterns(
        &self,
        choices: &[PermissionOptionChoice],
        patterns: &[PermissionPattern],
        _tool_name: &str,
        current_label: SharedString,
        entry_ix: usize,
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
        is_first: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let default_choice_index = choices.len().saturating_sub(1);
        let menu_options: Vec<(usize, SharedString)> = choices
            .iter()
            .enumerate()
            .map(|(i, choice)| (i, choice.label()))
            .collect();

        let pattern_options: Vec<(usize, SharedString)> = patterns
            .iter()
            .enumerate()
            .map(|(i, cp)| {
                (
                    i,
                    SharedString::from(format!("Always for `{}` commands", cp.display_name)),
                )
            })
            .collect();

        let permission_dropdown_handle = self.permission_dropdown_handle.clone();
        let conversation = self.conversation.downgrade();

        PopoverMenu::new(("permission-granularity", entry_ix))
            .with_handle(permission_dropdown_handle.clone())
            .anchor(gpui::Anchor::TopRight)
            .attach(gpui::Anchor::BottomRight)
            .trigger(self.permission_dropdown_trigger(current_label, entry_ix, is_first, cx))
            .menu(move |window, cx| {
                let session_id = session_id.clone();
                let options = menu_options.clone();
                let patterns = pattern_options.clone();
                let conversation = conversation.clone();
                let dropdown_handle = permission_dropdown_handle.clone();

                Some(ContextMenu::build_persistent(
                    window,
                    cx,
                    move |menu, _window, cx| {
                        let mut menu = menu;

                        let selection = conversation.upgrade().and_then(|conversation| {
                            conversation
                                .read(cx)
                                .permission_selections
                                .get(&request_id)
                                .cloned()
                        });

                        let is_pattern_mode =
                            matches!(selection, Some(PermissionSelection::SelectedPatterns(_)));

                        // Granularity choices: "Always for terminal", "Only this time"
                        for (index, display_name) in options.iter() {
                            let display_name = display_name.clone();
                            let index = *index;
                            let session_id = session_id.clone();
                            let is_selected = !is_pattern_mode
                                && selection
                                    .as_ref()
                                    .and_then(|s| s.choice_index())
                                    .map_or(index == default_choice_index, |ci| ci == index);

                            let conversation = conversation.clone();
                            menu = menu.toggleable_entry(
                                display_name,
                                is_selected,
                                IconPosition::End,
                                None,
                                move |_window, cx| {
                                    conversation
                                        .update(cx, |conversation, cx| {
                                            conversation.set_permission_choice(
                                                &session_id,
                                                request_id,
                                                index,
                                                cx,
                                            );
                                        })
                                        .log_err();
                                },
                            );
                        }

                        menu = menu.separator().header("Select Options…");

                        for (pattern_index, label) in patterns.iter() {
                            let label = label.clone();
                            let pattern_index = *pattern_index;
                            let session_id = session_id.clone();
                            let is_checked = selection
                                .as_ref()
                                .is_some_and(|s| s.is_pattern_checked(pattern_index));

                            let conversation = conversation.clone();
                            menu = menu.toggleable_entry(
                                label,
                                is_checked,
                                IconPosition::End,
                                None,
                                move |_window, cx| {
                                    conversation
                                        .update(cx, |conversation, cx| {
                                            conversation.toggle_permission_pattern(
                                                &session_id,
                                                request_id,
                                                pattern_index,
                                                cx,
                                            );
                                        })
                                        .log_err();
                                },
                            );
                        }

                        let any_patterns_checked = selection
                            .as_ref()
                            .is_some_and(|s| s.has_any_checked_patterns());
                        let dropdown_handle = dropdown_handle.clone();
                        menu = menu.custom_row(move |_window, _cx| {
                            div()
                                .py_1()
                                .w_full()
                                .child(
                                    KitButton::new("apply-patterns")
                                        .outline()
                                        .small()
                                        .w_full()
                                        .label("Apply")
                                        .disabled(!any_patterns_checked)
                                        .on_click({
                                            let dropdown_handle = dropdown_handle.clone();
                                            move |_event, _window, cx| {
                                                dropdown_handle.hide(cx);
                                            }
                                        }),
                                )
                                .into_any_element()
                        });

                        menu
                    },
                ))
            })
            .into_any_element()
    }

    fn render_permission_buttons_flat(
        &self,
        session_id: acp_v2::SessionId,
        is_first: bool,
        options: &[acp_v2::PermissionOption],
        entry_ix: usize,
        request_id: PermissionRequestId,
        focus_handle: &FocusHandle,
        allow_disabled: bool,
        cx: &Context<Self>,
    ) -> Div {
        let is_retry = |option: &acp_v2::PermissionOption| {
            option.option_id.0.as_ref() == acp_thread::SANDBOX_FALLBACK_RETRY_OPTION_ID
        };
        // Declining comes first and allowing once last, so the usual choice sits at the end
        // of the row.
        let rank = |option: &acp_v2::PermissionOption| {
            if is_retry(option) {
                return 1;
            }
            match option.kind {
                acp_v2::PermissionOptionKind::AllowAlways => 2,
                acp_v2::PermissionOptionKind::AllowOnce => 3,
                _ => 0,
            }
        };
        let mut options = options.iter().collect::<Vec<_>>();
        options.sort_by_key(|option| rank(option));

        let mut seen_kinds: ArrayVec<acp_v2::PermissionOptionKind, 3, u8> = ArrayVec::new();

        h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .justify_end()
            .children(options.into_iter().map(move |option| {
                let option_id = SharedString::from(option.option_id.0.clone());
                // The sandbox-fallback prompt offers a "Retry" option that re-attempts
                // creating the sandbox; it isn't an allow/deny choice, so it gets no
                // keybinding.
                let action = if is_retry(option) {
                    None
                } else {
                    match &option.kind {
                        acp_v2::PermissionOptionKind::AllowOnce => Some(&AllowOnce as &dyn Action),
                        acp_v2::PermissionOptionKind::AllowAlways => {
                            if option.option_id.0.as_ref()
                                == acp_thread::SandboxPermission::AllowThread.as_id()
                            {
                                None
                            } else {
                                Some(&AllowAlways as &dyn Action)
                            }
                        }
                        acp_v2::PermissionOptionKind::RejectOnce => {
                            Some(&RejectOnce as &dyn Action)
                        }
                        acp_v2::PermissionOptionKind::RejectAlways | _ => None,
                    }
                };

                // An "allow" choice is disabled while a surprising-Unicode
                // warning is unacknowledged; "deny"/"retry" stay enabled.
                let is_allow = matches!(
                    option.kind,
                    acp_v2::PermissionOptionKind::AllowOnce
                        | acp_v2::PermissionOptionKind::AllowAlways
                ) && !is_retry(option);
                let disabled = allow_disabled && is_allow;

                let key_binding = action
                    .filter(|_| {
                        is_first
                            && !disabled
                            && !seen_kinds.contains(&option.kind)
                            && seen_kinds.push(option.kind.clone()).is_ok()
                    })
                    .map(|action| {
                        KeyBinding::for_action_in(action, focus_handle, cx)
                            .size(rems_from_px(12_f32))
                    });

                PillButton::new((option_id, entry_ix))
                    .size(KitSize::Small)
                    .label(option.name.clone())
                    .map(|button| match option.kind {
                        acp_v2::PermissionOptionKind::AllowOnce if !is_retry(option) => {
                            button.variant(ButtonVariant::Primary)
                        }
                        acp_v2::PermissionOptionKind::AllowAlways => button.outline(),
                        _ => button,
                    })
                    .disabled(disabled)
                    .children(key_binding)
                    .on_click(cx.listener({
                        let option_id = option.option_id.clone();
                        let option_kind = option.kind.clone();
                        let session_id = session_id.clone();
                        move |this, _, window, cx| {
                            this.authorize_permission_request(
                                session_id.clone(),
                                request_id,
                                SelectedPermissionOutcome::new(
                                    option_id.clone(),
                                    option_kind.clone(),
                                ),
                                window,
                                cx,
                            );
                        }
                    }))
            }))
    }

    fn render_diff_loading(&self, _cx: &Context<Self>) -> AnyElement {
        v_flex()
            .p_3()
            .gap_1()
            .child(Skeleton::new().h_1().w_3_4())
            .child(Skeleton::new().h_1().w_1_4())
            .child(Skeleton::new().h_1().w_2_4())
            .child(Skeleton::new().h_1().w_3_5())
            .child(Skeleton::new().h_1().w_2_5())
            .into_any_element()
    }

    pub(super) fn tool_call_icon_tooltip(
        tool_name: Option<&SharedString>,
        interrupted_edit: bool,
    ) -> Option<SharedString> {
        let tool_name = tool_name.filter(|name| !name.trim().is_empty());
        match (tool_name, interrupted_edit) {
            (Some(name), true) => Some(format!("Interrupted Edit\nTool: {name}").into()),
            (Some(name), false) => Some(format!("Tool: {name}").into()),
            (None, true) => Some("Interrupted Edit".into()),
            (None, false) => None,
        }
    }

    pub(super) fn render_diff_editor(
        &self,
        entry_ix: usize,
        diff: &Entity<acp_thread::Diff>,
        tool_call: &ToolCall,
        cx: &Context<Self>,
    ) -> AnyElement {
        let tool_progress = matches!(
            tool_call.status(),
            ToolCallStatus::InProgress | ToolCallStatus::Pending
        );

        if let Some(entry) = self.entry_view_state.read(cx).entry(entry_ix)
            && let Some(editor) = entry.editor_for_diff(diff)
            && diff.read(cx).has_revealed_range(cx)
        {
            editor.into_any_element()
        } else if tool_progress && self.as_native_connection(cx).is_some() {
            self.render_diff_loading(cx)
        } else {
            Empty.into_any()
        }
    }

    fn render_subagent_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        tool_call: &ToolCall,
        subagent_session_id: Option<acp_v2::SessionId>,
        focus_handle: &FocusHandle,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let subagent_thread_view = subagent_session_id.and_then(|session_id| {
            self.server_view
                .upgrade()
                .and_then(|server_view| server_view.read(cx).as_connected())
                .and_then(|connected| connected.threads.get(&session_id))
        });

        let content = self.render_subagent_card(
            active_session_id,
            entry_ix,
            subagent_thread_view,
            tool_call,
            focus_handle,
            window,
            cx,
        );

        v_flex().px(TEXT_INSET).pt(ROW_GAP).child(content)
    }

    fn render_subagent_card(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        thread_view: Option<&Entity<ThreadView>>,
        tool_call: &ToolCall,
        focus_handle: &FocusHandle,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let thread = thread_view
            .as_ref()
            .map(|view| view.read(cx).thread.clone());
        let subagent_session_id = thread
            .as_ref()
            .map(|thread| thread.read(cx).session_id().clone());
        let action_log = thread.as_ref().map(|thread| thread.read(cx).action_log());
        let changed_buffers = action_log
            .map(|log| log.read(cx).changed_buffers(cx).collect::<Vec<_>>())
            .unwrap_or_default();

        let is_pending_tool_call = thread_view
            .as_ref()
            .and_then(|tv| {
                let sid = tv.read(cx).thread.read(cx).session_id();
                self.conversation.read(cx).pending_tool_call(sid, cx)
            })
            .is_some();

        let is_expanded = self
            .entry_view_state
            .read(cx)
            .is_tool_call_expanded(&tool_call.id);
        let files_changed = changed_buffers.len();
        let diff_stats = DiffStats::all_files(changed_buffers, cx);

        let is_running = matches!(
            tool_call.status(),
            ToolCallStatus::Pending
                | ToolCallStatus::InProgress
                | ToolCallStatus::WaitingForConfirmation
        );

        let is_failed = matches!(
            tool_call.status(),
            ToolCallStatus::Failed | ToolCallStatus::Rejected
        );

        let is_cancelled = matches!(tool_call.status(), ToolCallStatus::Canceled)
            || tool_call.content().iter().any(|c| match c {
                ToolCallContent::ContentBlock { block, .. } => {
                    block.text_content(cx) == Some("User canceled")
                }
                _ => false,
            });

        let model_name = thread_view
            .and_then(|view| view.read(cx).as_native_thread(cx))
            .and_then(|thread| {
                let thread = thread.read(cx);
                let model = thread.model()?;
                Some(model.name().0)
            });
        let thread_title = thread
            .as_ref()
            .and_then(|t| t.read(cx).title())
            .filter(|t| !t.is_empty());
        let tool_call_label = tool_call.label.read(cx).source().to_string();
        let has_tool_call_label = !tool_call_label.is_empty();

        let has_title = thread_title.is_some() || has_tool_call_label;
        let has_no_title_or_canceled = !has_title || is_failed || is_cancelled;

        let title: SharedString = if let Some(thread_title) = thread_title {
            thread_title
        } else if !tool_call_label.is_empty() {
            tool_call_label.into()
        } else if is_cancelled {
            "Subagent Canceled".into()
        } else if is_failed {
            "Subagent Failed".into()
        } else {
            "Spawning Agent…".into()
        };

        let theme = KitTheme::global(cx);
        let status_icon = format!("status-icon-{}", entry_ix);
        let icon = if is_running {
            Spinner::new()
                .icon(kit_icon(IconName::LoadCircle))
                .with_size(KitSize::Small)
                .into_any_element()
        } else if is_cancelled {
            div()
                .id(status_icon)
                .tooltip(Tooltip::text("Subagent Cancelled"))
                .child(
                    kit_icon(IconName::Circle)
                        .with_size(KitSize::Small)
                        .text_color(theme.muted_foreground.opacity(0.5)),
                )
                .into_any_element()
        } else if is_failed {
            div()
                .id(status_icon)
                .tooltip(Tooltip::text("Subagent Failed"))
                .child(
                    kit_icon(IconName::Close)
                        .with_size(KitSize::Small)
                        .text_color(theme.danger),
                )
                .into_any_element()
        } else {
            kit_icon(IconName::Check)
                .with_size(KitSize::Small)
                .text_color(theme.success)
                .into_any_element()
        };

        let has_expandable_content = thread
            .as_ref()
            .map_or(false, |thread| !thread.read(cx).entries().is_empty());

        let tooltip_meta_description = if is_expanded {
            "Click to Collapse"
        } else {
            "Click to Preview"
        };

        let error_message = self.subagent_error_message(&tool_call.status(), tool_call, cx);

        let header = ToolCardHeader::new(
            SharedString::from(format!("subagent-header-{}", entry_ix)),
            icon,
            div().text_color(theme.foreground).child(title.clone()),
        )
        .when_some(model_name, |header, model_name| {
            header.meta(format!("· {model_name}"))
        })
        .when(files_changed > 0, |header| {
            header
                .meta(format!(
                    "— {} {} changed",
                    files_changed,
                    if files_changed == 1 { "file" } else { "files" }
                ))
                .meta(diff_stat(diff_stats, cx))
        })
        .when(
            !has_no_title_or_canceled && !is_pending_tool_call,
            |header| {
                header.tooltip(move |_, cx| {
                    Tooltip::with_meta(title.to_string(), None, tooltip_meta_description, cx)
                })
            },
        )
        .when(has_expandable_content && !is_pending_tool_call, |header| {
            header.expanded(is_expanded).on_click(cx.listener({
                let tool_call_id = tool_call.id.clone();
                move |this, _, window, cx| {
                    let expanded = this.entry_view_state.update(cx, |state, _cx| {
                        state.toggle_tool_call_expansion(&tool_call_id);
                        state.is_tool_call_expanded(&tool_call_id)
                    });
                    this.refresh_thread_search(window, cx);
                    telemetry::event!("Subagent Toggled", expanded);
                    cx.notify();
                }
            }))
        })
        .when_some(
            thread_view.map(|view| view.read(cx).thread.read(cx).session_id().clone()),
            |header, nav_session_id| {
                header.hover_action(
                    PillButton::new(("subagent-full-screen", entry_ix))
                        .size(KitSize::XSmall)
                        .icon(IconName::Maximize)
                        .tooltip(Tooltip::text("Make Subagent Full Screen"))
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            telemetry::event!("Subagent Maximized");
                            this.server_view
                                .update(cx, |this, cx| {
                                    this.navigate_to_thread(nav_session_id.clone(), window, cx);
                                })
                                .log_err();
                        })),
                )
            },
        )
        .when(is_running && subagent_session_id.is_some(), |header| {
            header.action(
                PillButton::new(SharedString::from(format!("stop-subagent-{}", entry_ix)))
                    .size(KitSize::XSmall)
                    .icon(IconName::Stop)
                    .debug_selector("ICON-Stop")
                    .tooltip(Tooltip::text("Stop Subagent"))
                    .when_some(thread.clone(), |button, thread| {
                        button.on_click(cx.listener(move |this, _event, _window, cx| {
                            telemetry::event!("Subagent Stopped");
                            this.cancel_thread(&thread, cx).detach();
                        }))
                    }),
            )
        });

        let mut sections = Vec::new();
        if let Some(thread_view) = thread_view {
            let thread = &thread_view.read(cx).thread;
            let pending_tool_call = self
                .conversation
                .read(cx)
                .pending_tool_call(thread.read(cx).session_id(), cx);

            if is_running && let Some((_, subagent_tool_call_id, _)) = pending_tool_call {
                if let Some((entry_ix, tool_call)) =
                    thread.read(cx).tool_call(&subagent_tool_call_id)
                {
                    sections.push(
                        thread_view
                            .read(cx)
                            .render_any_tool_call(
                                active_session_id,
                                entry_ix,
                                tool_call,
                                focus_handle,
                                ToolCallLayout::Embedded,
                                window,
                                cx,
                            )
                            .into_any_element(),
                    );
                }
            } else if is_expanded {
                sections.push(
                    self.render_subagent_expanded_content(thread_view, tool_call, window, cx)
                        .into_any_element(),
                );
                if let Some(message) = error_message {
                    sections.push(
                        div()
                            .p_1p5()
                            .child(AlertCard::new(
                                ("subagent-error", entry_ix),
                                AlertVariant::Error,
                                message,
                            ))
                            .into_any_element(),
                    );
                }
            }
        }

        ToolCard::new(("subagent-card", entry_ix), header)
            .sections(sections)
            .dashed(has_no_title_or_canceled)
            .into_any_element()
    }

    fn render_subagent_expanded_content(
        &self,
        thread_view: &Entity<ThreadView>,
        tool_call: &ToolCall,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        const MAX_PREVIEW_ENTRIES: usize = 8;

        let subagent_view = thread_view.read(cx);
        let session_id = subagent_view.thread.read(cx).session_id().clone();

        let editor_bg = cx.theme().colors().editor_background;
        let overlay = {
            div()
                .absolute()
                .inset_0()
                .size_full()
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(editor_bg.opacity(0.5), 0.),
                    linear_color_stop(editor_bg.opacity(0.), 0.1),
                ))
                .block_mouse_except_scroll()
        };

        let entries = subagent_view.thread.read(cx).entries();
        let total_entries = entries.len();
        let mut entry_range = if let Some(info) = tool_call.subagent_session_info.as_ref() {
            info.message_start_index
                ..info
                    .message_end_index
                    .map(|i| (i + 1).min(total_entries))
                    .unwrap_or(total_entries)
        } else {
            0..total_entries
        };
        let first_entry_ix = entry_range.start;
        entry_range.start = entry_range
            .end
            .saturating_sub(MAX_PREVIEW_ENTRIES)
            .max(first_entry_ix);
        // A collapsed activity group shows its line at its first entry, so the preview starts
        // there rather than among the entries the line stands for.
        if let Some(group) =
            ActivityGroup::containing(subagent_view.thread.read(cx), entry_range.start, cx)
        {
            entry_range.start = group.entries.start.max(first_entry_ix);
        }
        let start_ix = entry_range.start;

        let scroll_handle = self
            .subagent_scroll_handles
            .borrow_mut()
            .entry(subagent_view.session_id.clone())
            .or_default()
            .clone();

        scroll_handle.scroll_to_bottom();

        let rendered_entries: Vec<AnyElement> = entries
            .get(entry_range)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let actual_ix = start_ix + i;
                subagent_view.render_entry(actual_ix, total_entries, entry, window, cx)
            })
            .collect();

        v_flex()
            .relative()
            .w_full()
            .overflow_hidden()
            .child(
                div()
                    .pb_1()
                    .min_h_0()
                    // Include the tool call id so the same subagent session
                    // rendered in multiple parent cards gets distinct element
                    // ids for its inlined entries (avoids duplicate a11y ids).
                    .id(format!(
                        "subagent-entries-{}-{}",
                        session_id, tool_call.id.0
                    ))
                    .track_scroll(&scroll_handle)
                    .children(rendered_entries),
            )
            .h_56()
            .child(overlay)
    }

    fn subagent_error_message(
        &self,
        status: &ToolCallStatus,
        tool_call: &ToolCall,
        cx: &App,
    ) -> Option<SharedString> {
        if matches!(status, ToolCallStatus::Failed) {
            tool_call.content().iter().find_map(|content| {
                if let ToolCallContent::ContentBlock { block, .. } = content {
                    if let Some(source) = block.text_content(cx).filter(|source| !source.is_empty())
                    {
                        if source == "User canceled" {
                            return None;
                        } else {
                            return Some(SharedString::from(source));
                        }
                    }
                }
                None
            })
        } else {
            None
        }
    }
}

/// The command a terminal tool call runs, without the code fences its label's markdown keeps it in.
fn command_text(command: &Markdown) -> String {
    let source = command.source();
    source
        .strip_prefix("```\n")
        .and_then(|source| source.strip_suffix("\n```"))
        .unwrap_or(source)
        .to_string()
}

fn copy_command_button(id: impl Into<ElementId>, command: String) -> PillButton {
    PillButton::new(id)
        .size(KitSize::XSmall)
        .icon(IconName::Copy)
        .tooltip(Tooltip::text("Copy Command"))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
        })
}

fn command_title(command: &str, cx: &App) -> impl IntoElement {
    div()
        .font_family(ThemeSettings::get_global(cx).buffer_font.family.clone())
        .text_xs()
        .text_color(KitTheme::global(cx).foreground)
        .child(command.lines().next().unwrap_or_default().to_string())
}

fn description_title(verb: &'static str, description: &str, cx: &App) -> impl IntoElement {
    h_flex()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .flex_none()
                .text_color(KitTheme::global(cx).foreground)
                .child(verb),
        )
        .child(div().min_w_0().truncate().child(description.to_string()))
}

fn highlighted_label(
    label: SharedString,
    highlights: Vec<Range<usize>>,
    cx: &App,
) -> impl IntoElement {
    let highlight = HighlightStyle {
        color: Some(KitTheme::global(cx).foreground),
        ..Default::default()
    };
    StyledText::new(label).with_highlights(highlights.into_iter().map(|range| (range, highlight)))
}

pub(super) fn diff_stat(stats: DiffStats, cx: &App) -> impl IntoElement {
    let theme = KitTheme::global(cx);
    h_flex()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_color(theme.success)
                .child(format!("+{}", stats.lines_added)),
        )
        .child(
            div()
                .text_color(theme.danger)
                .child(format!("−{}", stats.lines_removed)),
        )
}

/// The top row of a [`ToolCard`]: what the tool did and to what, its status, and actions.
#[derive(IntoElement)]
pub(super) struct ToolCardHeader {
    id: ElementId,
    icon: AnyElement,
    title: AnyElement,
    meta: Vec<AnyElement>,
    hover_actions: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    expanded: Option<bool>,
    tooltip: Option<Box<dyn Fn(&mut Window, &mut App) -> AnyView>>,
    on_click: Option<ClickHandler>,
}

impl ToolCardHeader {
    pub(super) fn new(
        id: impl Into<ElementId>,
        icon: impl IntoElement,
        title: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.into_any_element(),
            title: title.into_any_element(),
            meta: Vec::new(),
            hover_actions: Vec::new(),
            actions: Vec::new(),
            expanded: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// Adds status beside the title, such as a diff stat or an exit code.
    pub(super) fn meta(mut self, meta: impl IntoElement) -> Self {
        self.meta.push(meta.into_any_element());
        self
    }

    /// Adds an action at the end of the row that shows while the row is hovered.
    pub(super) fn hover_action(mut self, action: impl IntoElement) -> Self {
        self.hover_actions.push(action.into_any_element());
        self
    }

    /// Adds an action at the end of the row that always shows.
    pub(super) fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// Shows a disclosure chevron, which points down while `expanded`.
    pub(super) fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
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

impl RenderOnce for ToolCardHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = KitTheme::global(cx);
        let hover_background = theme.list_hover;
        let group = SharedString::from(format!("tool-card-header-{}", self.id));

        h_flex()
            .id(self.id)
            .group(group.clone())
            .w_full()
            .h_8()
            .px_2()
            .gap_1p5()
            .text_size(rems_from_px(13_f32))
            .text_color(theme.muted_foreground)
            .when(self.on_click.is_some(), |this| {
                this.cursor_pointer()
                    .hover(|style| style.bg(hover_background))
            })
            .child(div().flex_none().child(self.icon))
            .child(div().min_w_0().truncate().child(self.title))
            .children(
                self.meta
                    .into_iter()
                    .map(|meta| div().flex_none().child(meta)),
            )
            .child(div().flex_1())
            .when(!self.hover_actions.is_empty(), |this| {
                this.child(
                    h_flex()
                        .id("hover-actions")
                        .flex_none()
                        .gap_0p5()
                        .visible_on_hover(group.clone())
                        .children(self.hover_actions),
                )
            })
            .when(!self.actions.is_empty(), |this| {
                this.child(h_flex().flex_none().gap_0p5().children(self.actions))
            })
            .when_some(self.expanded, |this, expanded| {
                let chevron = if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                this.child(
                    div()
                        .id("chevron")
                        .flex_none()
                        .when(!expanded, |this| this.visible_on_hover(group))
                        .child(kit_icon(chevron).with_size(KitSize::XSmall)),
                )
            })
            .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}

/// A card in the transcript for a tool call that needs more than a line, such as an edit's diff,
/// a command's output, or a request for permission.
#[derive(IntoElement)]
pub(super) struct ToolCard {
    id: ElementId,
    header: AnyElement,
    sections: Vec<AnyElement>,
    footer: Option<AnyElement>,
    dashed: bool,
    bare: bool,
}

impl ToolCard {
    pub(super) fn new(id: impl Into<ElementId>, header: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            header: header.into_any_element(),
            sections: Vec::new(),
            footer: None,
            dashed: false,
            bare: false,
        }
    }

    /// Adds sections below the header, each set off by a rule.
    pub(super) fn sections(mut self, sections: impl IntoIterator<Item = AnyElement>) -> Self {
        self.sections.extend(sections);
        self
    }

    /// Sets the row at the bottom of the card, for the buttons that answer a permission request.
    pub(super) fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// Draws the card's rules dashed, for a tool call that failed or was canceled.
    pub(super) fn dashed(mut self, dashed: bool) -> Self {
        self.dashed = dashed;
        self
    }

    /// Leaves out the card's border and background, for a card shown inside another surface.
    pub(super) fn bare(mut self, bare: bool) -> Self {
        self.bare = bare;
        self
    }
}

impl RenderOnce for ToolCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border = KitTheme::global(cx).border;
        let dashed = self.dashed;
        let section = move |content: AnyElement| {
            div()
                .w_full()
                .border_t_1()
                .border_color(border)
                .when(dashed, |this| this.border_dashed())
                .child(content)
        };

        let card = if self.bare {
            v_flex().w_full()
        } else {
            transcript_card(cx).when(dashed, |this| this.border_dashed())
        };
        card.id(self.id)
            .child(self.header)
            .children(self.sections.into_iter().map(section))
            .children(
                self.footer
                    .map(|footer| section(div().w_full().p_1p5().child(footer).into_any_element())),
            )
    }
}
