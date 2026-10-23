use super::composer::PillButton;
use super::transcript::{
    ActivityRow, AlertCard, ROW_GAP, STEP_GAP, TEXT_INSET, kit_icon, transcript_card,
};
use super::*;
use crate::entry_view_state::ActivityGroup;
use gpui::{HighlightStyle, StyledText};
use gpui_kit::component::{
    Disableable as _, Icon as KitIcon, Selectable as _, Size as KitSize, Theme as KitTheme,
    button::ButtonVariant, hover_card::HoverCard, shimmer::ShimmerText, skeleton::Skeleton,
    spinner::Spinner,
};
use markdown::parser::MarkdownEvent;
use std::ops::Range;
use std::time::Duration;
use terminal::terminal_settings::TerminalSettings;
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
            if tool_call.tool_name.as_deref() == Some("ask_user")
                && self.as_native_thread(cx).is_some()
            {
                this.child(self.render_question_answers(tool_call, cx))
            } else if tool_call.is_subagent() && tool_call.authorization_id().is_none() {
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
                        layout,
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
            } else if Self::is_edit_row_tool_call(tool_call) {
                this.child(self.render_edit_tool_call(
                    active_session_id,
                    entry_ix,
                    tool_call,
                    focus_handle,
                    layout,
                    window,
                    cx,
                ))
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

    fn is_activity_tool_call(tool_call: &ToolCall) -> bool {
        !Self::is_edit_tool_call(tool_call)
            && !matches!(tool_call.status(), ToolCallStatus::WaitingForConfirmation)
    }

    fn is_edit_tool_call(tool_call: &ToolCall) -> bool {
        matches!(tool_call.kind(), acp_v2::ToolKind::Edit)
            || tool_call.diffs().next().is_some()
            || tool_call
                .content()
                .iter()
                .any(|content| matches!(content, ToolCallContent::DiffPatch { .. }))
    }

    /// Whether an edit shows as one line with its diff behind a disclosure. An edit waiting for
    /// permission keeps its card for the buttons, and a canceled one for discarding what it wrote.
    fn is_edit_row_tool_call(tool_call: &ToolCall) -> bool {
        Self::is_edit_tool_call(tool_call)
            && !matches!(
                tool_call.status(),
                ToolCallStatus::WaitingForConfirmation | ToolCallStatus::Canceled
            )
    }

    fn render_edit_tool_call(
        &self,
        active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        tool_call: &ToolCall,
        focus_handle: &FocusHandle,
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let running = matches!(
            tool_call.status(),
            ToolCallStatus::Pending | ToolCallStatus::InProgress
        );
        let failed = matches!(
            tool_call.status(),
            ToolCallStatus::Rejected | ToolCallStatus::Failed
        );
        let is_delete = matches!(tool_call.kind(), acp_v2::ToolKind::Delete);
        let action = match (is_delete, running) {
            (true, true) => "Deleting",
            (true, false) => "Deleted",
            (false, true) => "Editing",
            (false, false) => "Edited",
        };

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
        let has_location = tool_call.locations.len() == 1;
        let shows_loading =
            running && tool_call.content().is_empty() && self.as_native_connection(cx).is_some();
        let has_content = !tool_call.content().is_empty() || shows_loading;
        let is_open = has_content
            && self
                .entry_view_state
                .read(cx)
                .is_tool_call_content_visible(tool_call);

        let row_id = ("tool-call-edit", entry_ix);
        let row = match paths.as_slice() {
            [] => match native_tool_row_label(tool_call, running) {
                Some((action, details)) => {
                    ActivityRow::new(row_id, action).details(details.unwrap_or_default())
                }
                None => ActivityRow::from_label(row_id, &markdown_line(tool_call.label.read(cx)).0),
            },
            [path] => {
                let file_name = Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                let path = path.clone();
                ActivityRow::new(row_id, action)
                    .details(file_name)
                    .tooltip(Tooltip::text(path))
            }
            paths => ActivityRow::new(row_id, action).details(count(paths.len(), "file", "files")),
        };
        let diff_stats = tool_call_diff_stats(tool_call, cx);
        let row = row
            .when(
                diff_stats.lines_added > 0 || diff_stats.lines_removed > 0,
                |row| row.diff_stats(diff_stats),
            )
            .running(running)
            .failed(failed)
            .map(|row| {
                if has_content {
                    row.expanded(is_open).on_click(cx.listener({
                        let id = tool_call.id.clone();
                        move |this, _, window, cx| {
                            this.hold_scroll_position_for_toggle();
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

        let diff = is_open.then(|| {
            let content = if shows_loading {
                self.render_diff_loading(cx)
            } else {
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
                    .into_any_element()
            };
            div().pt_1().child(
                transcript_card(cx)
                    .when(failed, |this| this.border_dashed())
                    .child(scrollable_output(
                        ("tool-call-edit-output", entry_ix),
                        content,
                    )),
            )
        });

        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(row)
            .children(diff)
    }

    fn render_activity_tool_call(
        &self,
        _active_session_id: &acp_v2::SessionId,
        entry_ix: usize,
        tool_call: &ToolCall,
        _focus_handle: &FocusHandle,
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let is_read = matches!(tool_call.kind(), acp_v2::ToolKind::Read);
        let is_file_read = tool_call.tool_name.as_deref() == Some("read_file")
            || (tool_call.tool_name.is_none() && is_read && !tool_call.locations.is_empty());
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
        let should_show_raw_input = !has_image_content && tool_call.raw_input_markdown.is_some();
        // Only successful text-file reads navigate directly; directories, diagnostics and
        // images also use the Read kind, but their results need to remain inspectable.
        let shows_content = !is_file_read || has_image_content || failed_or_canceled;
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
            .filter(|_| is_file_read)
            .and_then(|location| location.path.file_name())
            .map(|file_name| file_name.to_string_lossy().into_owned());
        let row_id = ("tool-call-activity", entry_ix);
        let row = if matches!(tool_call.kind(), acp_v2::ToolKind::Execute) {
            ActivityRow::new(row_id, command_verb(false, running))
                .details(
                    command_description(tool_call).unwrap_or_else(|| command_line(tool_call, cx)),
                )
                .icon(IconName::Terminal)
        } else {
            match (native_tool_row_label(tool_call, running), read_file_name) {
                (Some((action, details)), _) => {
                    ActivityRow::new(row_id, action).details(details.unwrap_or_default())
                }
                (None, Some(file_name)) => {
                    ActivityRow::new(row_id, if running { "Reading" } else { "Read" })
                        .details(file_name)
                }
                (None, None) => {
                    ActivityRow::from_label(row_id, &markdown_line(tool_call.label.read(cx)).0)
                }
            }
        }
        .running(running)
        .failed(failed_or_canceled)
        .when_some(tool_result_summary(tool_call), |row, summary| {
            row.meta(summary)
        })
        .map(|row| {
            if has_content {
                row.expanded(is_open).on_click(cx.listener({
                    let id = tool_call.id.clone();
                    move |this, _, window, cx| {
                        this.hold_scroll_position_for_toggle();
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

        let row = if !running && !is_open && has_content && supports_result_preview(tool_call) {
            let owner = cx.weak_entity();
            let tool_call_id = tool_call.id.clone();
            HoverCard::new(("tool-result-preview", entry_ix))
                .anchor(gpui::Anchor::TopLeft)
                .w(px(360.))
                .p_1()
                .trigger(row)
                .content(move |_, window, cx| {
                    owner
                        .update(cx, |this, cx| {
                            let Some((entry_ix, tool_call)) =
                                this.thread.read(cx).tool_call(&tool_call_id)
                            else {
                                return Empty.into_any_element();
                            };
                            this.render_tool_result_content(entry_ix, tool_call, true, window, cx)
                                .into_any_element()
                        })
                        .log_err()
                        .unwrap_or_else(|| Empty.into_any_element())
                })
                .into_any_element()
        } else {
            row.into_any_element()
        };

        let output = is_open.then(|| {
            v_flex()
                .w_full()
                .mt_1()
                .rounded_md()
                .border_1()
                .border_color(KitTheme::global(cx).border.opacity(0.6))
                .overflow_hidden()
                .child(self.render_tool_result_content(entry_ix, tool_call, false, window, cx))
                .when(should_show_raw_input, |this| {
                    this.child(self.render_raw_input_disclosure(entry_ix, tool_call, window, cx))
                })
        });

        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(row)
            .children(output)
    }

    fn render_tool_result_content(
        &self,
        entry_ix: usize,
        tool_call: &ToolCall,
        preview: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let has_links = tool_call.content().iter().any(|content| {
            matches!(content, ToolCallContent::ContentBlock { block, .. } if block.as_view().resource_link().is_some())
        });
        div()
            .id((if preview { "tool-hover-results" } else { "tool-results" }, entry_ix))
            .w_full()
            .max_h(px(if preview { 280. } else { 240. }))
            .overflow_y_scroll()
            .text_size(ThemeSettings::get_global(cx).agent_ui_font_size(cx))
            .children(tool_call.content().iter().enumerate().filter_map(|(content_ix, content)| {
                if preview && has_links && !matches!(content, ToolCallContent::ContentBlock { block, .. } if block.as_view().resource_link().is_some()) {
                    return None;
                }
                Some(div().id(("result", content_ix)).child(self.render_tool_call_content(
                    &self.session_id,
                    entry_ix,
                    content,
                    content_ix,
                    tool_call,
                    true,
                    &self.focus_handle,
                    window,
                    cx,
                )))
            }))
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
        self.hold_scroll_position_for_toggle();
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

/// The start of a Zed Agent tool call's row, as what it's doing or did, and the part of its input
/// that says what it acted on. The tools' titles are imperative ("Search files for regex …"), which
/// reads wrong once the work is done, so the row doesn't use them. Tools not listed here, and the
/// tools of other agents, fall back to their titles.
fn native_tool_row_label(
    tool_call: &ToolCall,
    running: bool,
) -> Option<(&'static str, Option<String>)> {
    let input = tool_call.raw_input.as_ref();
    let field = |key: &str| {
        input
            .and_then(|input| input.get(key))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let symbol_name = || {
        input
            .and_then(|input| input.get("symbol"))
            .and_then(|symbol| symbol.get("symbol_name"))
            .and_then(|name| name.as_str())
            .map(str::to_string)
    };
    let symbol = |preposition: &str| symbol_name().map(|name| format!("{preposition} {name}"));
    let source_to_destination = || {
        Some(format!(
            "{} to {}",
            field("source_path")?,
            field("destination_path")?
        ))
    };

    let ((running_action, done_action), details) = match tool_call.tool_name.as_deref()? {
        "read_file" => (
            ("Reading", "Read"),
            field("path").map(|path| {
                let name = Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(path);
                let start = input
                    .and_then(|input| input.get("start_line"))
                    .and_then(|line| line.as_u64());
                let end = input
                    .and_then(|input| input.get("end_line"))
                    .and_then(|line| line.as_u64());
                match (start, end) {
                    (Some(start), Some(end)) if start != end => format!("{name} L{start}–{end}"),
                    (Some(start), _) => format!("{name} L{start}"),
                    (None, Some(end)) => format!("{name} L1–{end}"),
                    _ => name,
                }
            }),
        ),
        "list_directory" => (("Listing", "Listed"), field("path")),
        "grep" => (("Searching", "Searched"), field("regex")),
        "find_path" => (("Searching", "Searched"), field("glob")),
        "fetch" => (("Fetching", "Fetched"), field("url")),
        "search_web" => (("Searching the web", "Searched the web"), field("query")),
        "diagnostics" => (
            ("Checking diagnostics", "Checked diagnostics"),
            field("path").map(|path| format!("for {path}")),
        ),
        "go_to_definition" => (("Finding definition", "Found definition"), symbol("of")),
        "find_references" => (("Finding references", "Found references"), symbol("to")),
        "get_code_actions" => (("Getting code actions", "Got code actions"), symbol("for")),
        "rename_symbol" => (
            ("Renaming", "Renamed"),
            symbol_name()
                .zip(field("new_name"))
                .map(|(name, new_name)| format!("{name} to {new_name}")),
        ),
        "copy_path" => (("Copying", "Copied"), source_to_destination()),
        "move_path" => (("Moving", "Moved"), source_to_destination()),
        "delete_path" => (("Deleting", "Deleted"), field("path")),
        "create_directory" => (("Creating directory", "Created directory"), field("path")),
        "skill" => (("Using skill", "Used skill"), field("name")),
        "list_agents_and_models" => (
            ("Listing agents and models", "Listed agents and models"),
            None,
        ),
        "create_thread" => (("Creating thread", "Created thread"), field("title")),
        _ => return None,
    };
    Some((if running { running_action } else { done_action }, details))
}

fn supports_result_preview(tool_call: &ToolCall) -> bool {
    matches!(
        tool_call.kind(),
        acp_v2::ToolKind::Search | acp_v2::ToolKind::Fetch
    ) || matches!(
        tool_call.tool_name.as_deref(),
        Some("list_directory" | "diagnostics")
    )
}

fn tool_result_summary(tool_call: &ToolCall) -> Option<String> {
    if tool_call.status() != ToolCallStatus::Completed {
        return None;
    }
    let links = tool_call.content().iter().filter(|content| {
        matches!(content, ToolCallContent::ContentBlock { block, .. } if block.as_view().resource_link().is_some())
    }).count();
    if links > 0 {
        let noun = if tool_call.tool_name.as_deref() == Some("find_path") {
            "file"
        } else {
            "result"
        };
        return Some(format!(
            "{links} {noun}{}",
            if links == 1 { "" } else { "s" }
        ));
    }
    let output = tool_call.raw_output.as_ref()?.as_str()?;
    match tool_call.tool_name.as_deref()? {
        "grep" if output == "No matches found" => Some("No matches".into()),
        "diagnostics"
            if output.contains("No errors or warnings found")
                || output.contains("File doesn't have errors or warnings") =>
        {
            Some(
                if output.starts_with("Failed to refresh") {
                    "No issues · may be stale"
                } else {
                    "No errors or warnings"
                }
                .into(),
            )
        }
        _ => None,
    }
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

/// The tallest a tool call's output gets before it scrolls, so long output such as a big diff
/// doesn't stretch its card down the transcript. About as tall as a terminal's
/// [`TERMINAL_MAX_INLINE_LINES`].
const TOOL_OUTPUT_MAX_HEIGHT: Rems = Rems(15.);

fn scrollable_output(id: impl Into<ElementId>, output: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .max_h(TOOL_OUTPUT_MAX_HEIGHT)
        .overflow_y_scroll()
        .child(output)
}

// The existing permission shortcut opens a PopoverMenu handle, whose trigger traits predate Kit.
#[derive(IntoElement)]
struct PermissionMenuTrigger(KitButton);

impl Toggleable for PermissionMenuTrigger {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.0 = self.0.selected(selected);
        self
    }
}

impl Clickable for PermissionMenuTrigger {
    fn on_click(mut self, listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.0 = self.0.on_click(listener);
        self
    }

    fn cursor_style(mut self, cursor: CursorStyle) -> Self {
        self.0 = self.0.cursor(cursor);
        self
    }
}

impl RenderOnce for PermissionMenuTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.0
    }
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

pub(super) struct TerminalSandboxWarning {
    pub(super) title: SharedString,
    pub(super) detail: SharedString,
    pub(super) docs_url: SharedString,
}

impl ThreadView {
    fn render_question_answers(&self, tool_call: &ToolCall, cx: &Context<Self>) -> AnyElement {
        let input = tool_call.raw_input.as_ref().and_then(|input| {
            serde_json::from_value::<agent::AskUserToolInput>(input.clone()).ok()
        });
        let output = tool_call.raw_output.as_ref().and_then(|output| {
            serde_json::from_value::<agent::AskUserToolOutput>(output.clone()).ok()
        });
        let theme = KitTheme::global(cx);
        let Some(input) = input else {
            return Empty.into_any_element();
        };
        let form = agent::AskUserQuestionnaire::from_input(&input);
        let answers = match output {
            Some(agent::AskUserToolOutput::QuestionsAnswered { answers }) => answers,
            Some(agent::AskUserToolOutput::Answered { selected }) => vec![agent::AskUserAnswer {
                question_id: "question".into(),
                selected_option_ids: Vec::new(),
                freeform_text: Some(selected),
            }],
            Some(agent::AskUserToolOutput::Error { error }) => {
                return gpui_kit::base::h_flex()
                    .px(TEXT_INSET)
                    .py_2()
                    .gap_2()
                    .text_color(theme.muted_foreground)
                    .child(kit_icon(IconName::Info).small())
                    .child(error)
                    .into_any_element();
            }
            None => {
                return gpui_kit::base::h_flex()
                    .px(TEXT_INSET)
                    .py_2()
                    .gap_2()
                    .text_color(theme.muted_foreground)
                    .child(kit_icon(IconName::Info).small())
                    .child(
                        if matches!(
                            tool_call.status(),
                            ToolCallStatus::Pending | ToolCallStatus::InProgress
                        ) {
                            "Waiting for answers"
                        } else {
                            "Questions"
                        },
                    )
                    .into_any_element();
            }
        };
        gpui_kit::base::v_flex()
            .px(TEXT_INSET)
            .py_2()
            .gap_3()
            .children(form.questions.iter().map(|question| {
                let answer = answers
                    .iter()
                    .find(|answer| answer.question_id == question.id);
                let text = answer
                    .map(|answer| {
                        question
                            .options
                            .iter()
                            .filter(|option| answer.selected_option_ids.contains(&option.id))
                            .map(|option| option.label.clone())
                            .chain(answer.freeform_text.clone())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    })
                    .unwrap_or_default();
                gpui_kit::base::v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(question.prompt.clone()),
                    )
                    .child(
                        gpui_kit::base::h_flex()
                            .gap_2()
                            .child(kit_icon(IconName::Check).small())
                            .child(text),
                    )
            }))
            .into_any_element()
    }

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
        let is_edit = Self::is_edit_tool_call(tool_call);
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
            .compact(is_terminal_tool && needs_confirmation)
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
                        this.hold_scroll_position_for_toggle();
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
                                is_terminal_tool,
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
                // The floating permission row already scrolls as a whole.
                if layout == ToolCallLayout::Floating {
                    sections.push(details.into_any_element());
                } else {
                    sections.push(
                        scrollable_output(("tool-call-card-details", entry_ix), details)
                            .into_any_element(),
                    );
                }
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
                    sections.push(
                        scrollable_output(("tool-call-card-output", entry_ix), content(cx))
                            .into_any_element(),
                    );
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
            .command_approval(is_terminal_tool && needs_confirmation)
            .sections(sections)
            .dashed(failed_or_canceled)
            .bare(matches!(
                layout,
                ToolCallLayout::Embedded | ToolCallLayout::Floating
            ));

        let reason = needs_confirmation
            .then(|| tool_call.sandbox_authorization_details.as_ref())
            .flatten()
            .filter(|details| !details.reason.is_empty() || details.auto_review.is_some())
            .map(|details| {
                gpui_kit::base::v_flex()
                    .px_2()
                    .pt_1p5()
                    .pb_1()
                    .gap_1()
                    .text_size(rems(0.75))
                    .line_height(relative(1.42))
                    .text_color(KitTheme::global(cx).muted_foreground)
                    .when(!details.reason.is_empty(), |strip| {
                        strip.child(format!("Agent request: {}", details.reason))
                    })
                    .when_some(details.auto_review.clone(), |strip, review| {
                        strip.child(format!("Approval required: {review}"))
                    })
            });
        let card_with_reason = v_flex()
            .w_full()
            .rounded(rems(0.5))
            .when(reason.is_some(), |this| {
                this.bg(KitTheme::global(cx).secondary.opacity(0.3))
            })
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
                            .when(!is_terminal_tool, |footer| footer.p_1p5())
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
            .children(reason);
        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(card_with_reason)
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
            match command_description(tool_call) {
                Some(description) => {
                    let needs_confirmation =
                        matches!(tool_call.status(), ToolCallStatus::WaitingForConfirmation);
                    if needs_confirmation {
                        div().truncate().child(description).into_any_element()
                    } else {
                        description_title(command_verb(false, running), &description, cx)
                            .into_any_element()
                    }
                }
                None => {
                    let command = command_text(tool_call.label.read(cx));
                    command_title(&command, cx).into_any_element()
                }
            }
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
            "Hide parameters"
        } else {
            "Show parameters"
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
                            this.hold_scroll_position_for_toggle();
                            if !this.expanded_tool_call_raw_inputs.remove(&id) {
                                this.expanded_tool_call_raw_inputs.insert(id.clone());
                            }
                            cx.notify();
                        }
                    })),
            )
            .when(is_raw_input_expanded, |this| {
                this.children(tool_call.raw_input_markdown.clone().map(|input| {
                    scrollable_output(
                        ("tool-parameters", entry_ix),
                        self.render_markdown(
                            input,
                            MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                            cx,
                        ),
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
        let font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
        style.height_is_multiple_of_line_height = false;
        style.paragraph_spacing = px(0.);
        style.base_text_style.font_size = (font_size * 0.8125).into();
        style.paragraph_line_height = (font_size * 1.125).into();
        style.inline_code.font_size = Some((font_size * 0.8125).into());
        style.inline_code.line_height = Some(relative(18. / 13.));
        style.inline_code.background_color = None;
        style.code_block.text.font_size = Some((font_size * 0.8125).into());
        style.code_block.text.line_height = Some(relative(18. / 13.));
        style.code_block.padding = gpui::EdgesRefinement::default();
        style.code_block.margin = gpui::EdgesRefinement::default();
        style.code_block.background = None;
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

        gpui_kit::base::h_flex()
            .items_start()
            .gap_1()
            .px_2p5()
            .py_1p5()
            .child(
                div()
                    .text_size(font_size * 0.8125)
                    .line_height(relative(18. / 13.))
                    .font_family(KitTheme::global(cx).mono_font_family.clone())
                    .text_color(KitTheme::global(cx).muted_foreground)
                    .child("$"),
            )
            .child(div().flex_1().min_w_0().child(markdown_element))
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
        let background = terminal_data.background_task_id().is_some();
        let running = !needs_confirmation
            && (matches!(
                tool_call.status(),
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            ) || (background && output.is_none()));

        let terminal_id = terminal.entity_id().to_string();
        let child_id = |name: &str| format!("terminal-tool-{name}-{terminal_id}");

        let working_dir = working_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "current directory".to_string());
        let command = command_text(tool_call.label.read(cx));
        let description = command_description(tool_call);

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
        let font_size = ThemeSettings::get_global(cx).agent_ui_font_size(cx);
        let failed = tool_failed || command_failed;
        let row_id = SharedString::from(child_id("row"));
        let verb = if matches!(tool_call.status(), ToolCallStatus::Canceled) {
            "Stopped"
        } else if matches!(tool_call.status(), ToolCallStatus::Rejected) {
            "Skipped"
        } else if failed {
            "Failed"
        } else if background && running {
            "Running in background"
        } else {
            command_verb(needs_confirmation, running)
        };
        let row = match &description {
            Some(description) => ActivityRow::new(row_id, verb).details(description.clone()),
            None => ActivityRow::new(row_id, verb)
                .details(command.lines().next().unwrap_or_default().to_string()),
        }
        .icon(IconName::Terminal)
        .running(running)
        .tooltip(Tooltip::text(format!(
            "{command}\nWorking directory: {working_dir}"
        )))
        .when(
            (running || output.is_some()) && time_elapsed > ELAPSED_DISPLAY_THRESHOLD,
            |row| row.meta(duration_alt_display(time_elapsed)),
        )
        .when_some(truncated_tooltip, |row, tooltip| {
            row.meta(
                div()
                    .id(SharedString::from(child_id("truncated")))
                    .tooltip(Tooltip::text(tooltip))
                    .child(kit_icon(IconName::Info).with_size(KitSize::XSmall)),
            )
        })
        .when(failed, |row| {
            row.meta(
                div()
                    .id(SharedString::from(child_id("failed")))
                    .debug_selector(move || format!("terminal-tool-failed-{exit_code:?}"))
                    .when_some(exit_code, |this, code| {
                        this.tooltip(Tooltip::text(format!("Exited with code {code}")))
                    })
                    .child(
                        kit_icon(IconName::Close)
                            .with_size(KitSize::XSmall)
                            .text_color(theme.danger),
                    ),
            )
        })
        .when_some(tool_call.auto_review_allowed.clone(), |row, reason| {
            row.meta(
                div()
                    .id(SharedString::from(child_id("auto-review-allowed")))
                    .tooltip(move |_window, cx| {
                        Tooltip::with_meta("Allowed by Auto Review", None, reason.clone(), cx)
                    })
                    .child(kit_icon(IconName::Sparkle).with_size(KitSize::XSmall)),
            )
        })
        .when_some(tool_call.sandbox_not_applied.as_ref(), |row, reason| {
            let TerminalSandboxWarning {
                title,
                detail,
                docs_url,
            } = self.sandbox_not_applied_warning(reason, cx);
            row.meta(
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
        .when(running && terminal_data.can_run_in_background(), |row| {
            row.meta(
                PillButton::new(SharedString::from(child_id("background")))
                    .size(KitSize::XSmall)
                    .label("Run in background")
                    .on_click({
                        let terminal = terminal.clone();
                        move |_, _, cx| {
                            cx.stop_propagation();
                            terminal.update(cx, |terminal, cx| terminal.request_background(cx));
                        }
                    }),
            )
        })
        .when(running && terminal_data.is_process_backed(), |row| {
            row.meta(
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
                            if !background
                                && AgentSettings::get_global(cx).cancel_generation_on_terminal_stop
                            {
                                this.cancel_generation(cx);
                            }
                        }
                    })),
            )
        })
        .end_slot(copy_command_button(
            SharedString::from(child_id("copy-command")),
            command.clone(),
        ))
        .expanded(is_expanded)
        .on_click(cx.listener({
            let id = tool_call.id.clone();
            move |this, _event, window, cx| {
                this.hold_scroll_position_for_toggle();
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
            .and_then(|entry| entry.terminal(terminal))
            .filter(|_| is_expanded);
        let terminal_output = terminal_view.map(|terminal_view| {
            let element = if terminal_view
                .read(cx)
                .content_mode(window, cx)
                .is_scrollable()
            {
                // Embedded terminals inherit the surrounding text size. Use the same size
                // here so switching to a scrollable viewport doesn't clip a line or jump.
                let line_height = font_size * TerminalSettings::get_global(cx).line_height.value();
                div()
                    .h(line_height * TERMINAL_MAX_INLINE_LINES as f32
                        + rems(TERMINAL_CONTENT_TOP_PADDING).to_pixels(window.rem_size()))
                    .child(terminal_view)
                    .into_any_element()
            } else {
                terminal_view.into_any_element()
            };
            div()
                .w_full()
                .on_action(cx.listener(|_this, _: &NewTerminal, window, cx| {
                    window.dispatch_action(NewThread.boxed_clone(), cx);
                    cx.stop_propagation();
                }))
                .child(element)
        });

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

        // Like a terminal, the card shows the command after a prompt and then its output, in the
        // terminal's font. The terminal leaves a cell's width before its output, so the prompt
        // starts with a space to line up with it.
        let shows_card = is_expanded || needs_confirmation;
        let card = shows_card.then(|| {
            let terminal_settings = TerminalSettings::get_global(cx);
            let font_family = terminal_settings.font_family.as_ref().map_or_else(
                || ThemeSettings::get_global(cx).buffer_font.family.clone(),
                |family| family.0.clone().into(),
            );
            let has_output = terminal_output.is_some();
            let command_line = h_flex()
                .w_full()
                .items_start()
                .pt(rems(TERMINAL_CONTENT_TOP_PADDING))
                .when(!has_output, |this| {
                    this.pb(rems(TERMINAL_CONTENT_TOP_PADDING))
                })
                .pr_2()
                .font_family(font_family)
                .line_height(relative(terminal_settings.line_height.value()))
                .bg(cx.theme().colors().terminal_background)
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(" $ "),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_color(theme.foreground)
                        .child(command.clone()),
                )
                .child(copy_command_button(
                    SharedString::from(child_id("copy-expanded-command")),
                    command.clone(),
                ));
            let body = v_flex()
                .w_full()
                .text_size(font_size)
                .font_weight(gpui::FontWeight::NORMAL)
                .child(command_line)
                .children(terminal_output)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .text_color(theme.muted_foreground)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(working_dir.clone()),
                        )
                        .when(command_finished, |this| {
                            this.child(duration_alt_display(time_elapsed))
                        })
                        .when_some(exit_code.filter(|code| *code != 0), |this, code| {
                            this.child(div().text_color(theme.danger).child(format!("Exit {code}")))
                        }),
                );
            let footer = permission_buttons.map(|buttons| {
                div()
                    .w_full()
                    .p_1p5()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(failed, |this| this.border_dashed())
                    .child(buttons)
            });
            // Inside another surface, such as the floating permission row, the card leaves out its
            // own border.
            if layout.row_gap().is_none() {
                v_flex().w_full().child(body).children(footer)
            } else {
                transcript_card(cx)
                    .border_color(if failed {
                        theme.danger.opacity(0.5)
                    } else {
                        theme.border.opacity(0.6)
                    })
                    .child(body)
                    .children(footer)
            }
        });

        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(row)
            .children(card.map(|card| div().w_full().pt_1().child(card)))
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
        if self
            .thread
            .read(cx)
            .tool_call(tool_call_id)
            .is_some_and(|(_, call)| matches!(call.kind(), acp_v2::ToolKind::Execute))
            && options
                .first_option_of_kind(acp_v2::PermissionOptionKind::AllowOnce)
                .is_some()
            && options
                .first_option_of_kind(acp_v2::PermissionOptionKind::RejectOnce)
                .is_some()
        {
            return self.render_command_permission_buttons(
                session_id,
                is_first,
                request.id,
                options,
                entry_ix,
                focus_handle,
                allow_disabled,
                cx,
            );
        }
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

    fn render_command_permission_buttons(
        &self,
        session_id: acp_v2::SessionId,
        is_first: bool,
        request_id: PermissionRequestId,
        options: &PermissionOptions,
        entry_ix: usize,
        focus_handle: &FocusHandle,
        allow_disabled: bool,
        cx: &Context<Self>,
    ) -> Div {
        let font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
        let available_width = self
            .composer_bounds
            .get()
            .map(|bounds| bounds.size.width)
            .unwrap_or(font_size * 50.);
        let show_shortcuts = is_first && available_width >= font_size * 35.;
        let button = |id, label: &'static str| {
            KitButton::new(id)
                .ghost()
                .small()
                .h(font_size * 1.25)
                .rounded(font_size * 0.25)
                .px_1p5()
                .accessibility_label(label)
                .when(!label.is_empty(), |button| {
                    button.child(div().text_size(font_size * 0.8125).child(label))
                })
        };
        let patterns = match options {
            PermissionOptions::DropdownWithPatterns { patterns, .. } => patterns.clone(),
            _ => Vec::new(),
        };
        let permanent_choices = match options {
            PermissionOptions::Flat(options) => options
                .iter()
                .filter(|option| option.kind == acp_v2::PermissionOptionKind::AllowAlways)
                .map(|option| {
                    (
                        SharedString::from(option.name.clone()),
                        SelectedPermissionOutcome::new(
                            option.option_id.clone(),
                            option.kind.clone(),
                        ),
                    )
                })
                .collect::<Vec<_>>(),
            PermissionOptions::Dropdown(choices)
            | PermissionOptions::DropdownWithPatterns { choices, .. } => choices
                .iter()
                .filter(|choice| choice.allow.kind == acp_v2::PermissionOptionKind::AllowAlways)
                .map(|choice| (choice.label(), choice.build_outcome(true)))
                .collect(),
        };
        let allow_once = options
            .first_option_of_kind(acp_v2::PermissionOptionKind::AllowOnce)
            .cloned();
        let reject_once = options
            .first_option_of_kind(acp_v2::PermissionOptionKind::RejectOnce)
            .cloned();
        let permanent_outcome = if patterns.is_empty() {
            permanent_choices
                .first()
                .filter(|_| permanent_choices.len() == 1)
                .map(|(_, outcome)| outcome.clone())
        } else {
            match self
                .conversation
                .read(cx)
                .permission_selections
                .get(&request_id)
            {
                Some(PermissionSelection::SelectedPatterns(selected)) if !selected.is_empty() => {
                    options.build_outcome_for_checked_patterns(selected, true)
                }
                _ => None,
            }
        };
        let permanent_tooltip = if patterns.is_empty() {
            permanent_choices
                .iter()
                .map(|(label, _)| label.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        } else if let Some(PermissionSelection::SelectedPatterns(selected)) = self
            .conversation
            .read(cx)
            .permission_selections
            .get(&request_id)
        {
            selected
                .iter()
                .filter_map(|index| patterns.get(*index))
                .map(|pattern| pattern.display_name.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            "Choose commands to always allow".to_owned()
        };
        let conversation = self.conversation.downgrade();
        let view = cx.weak_entity();
        let menu_session = session_id.clone();
        let popup = (!permanent_choices.is_empty() && available_width >= font_size * 25.).then(|| {
            PopoverMenu::new(("command-always-run", entry_ix))
                .with_handle(self.permission_dropdown_handle.clone())
                .trigger(PermissionMenuTrigger(button(("always-run-options", entry_ix), "")
                    .secondary()
                    .w(font_size * 1.25)
                    .px_0()
                    .border_l_1()
                    .border_color(KitTheme::global(cx).border)
                    .rounded_tl(px(0.))
                    .rounded_bl(px(0.))
                    .dropdown_caret(true)
                    .accessibility_label("Choose commands to always allow")
                    .tooltip("Choose commands to always allow")
                    .disabled(allow_disabled)))
                .menu(move |window, cx| {
                    Some(PopupMenu::build(window, cx, |mut menu, _, cx| {
                        menu = menu.max_h(font_size * 20.);
                        if patterns.is_empty() {
                            for (label, outcome) in &permanent_choices {
                                let outcome = outcome.clone();
                                let view = view.clone();
                                let session = menu_session.clone();
                                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.authorize_permission_request(session.clone(), request_id, outcome.clone(), window, cx);
                                    }).log_err();
                                }));
                            }
                            return menu;
                        }
                        menu = menu.label("Always run matching commands");
                        for (index, pattern) in patterns.iter().enumerate() {
                            let conversation = conversation.clone();
                            let keyboard_conversation = conversation.clone();
                            let session = menu_session.clone();
                            let keyboard_session = session.clone();
                            let label = pattern.display_name.clone();
                            let nested = label.split_whitespace().count() > 1;
                            let popup = cx.weak_entity();
                            menu = menu.item(PopupMenuItem::element(move |_, cx| {
                                let checked = conversation.upgrade().is_some_and(|conversation| {
                                    conversation.read(cx).permission_selections.get(&request_id).is_some_and(|selection| selection.is_pattern_checked(index))
                                });
                                let conversation = conversation.clone();
                                let session = session.clone();
                                let popup = popup.clone();
                                div().when(nested, |row| row.pl_4()).child(
                                    gpui_kit::component::checkbox::Checkbox::new(("always-command", index)).label(label.clone()).checked(checked)
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            conversation.update(cx, |conversation, cx| conversation.toggle_permission_pattern(&session, request_id, index, cx)).log_err();
                                            popup.update(cx, |_, cx| cx.notify()).log_err();
                                        })
                                )
                            }).on_click(move |_, _, cx| {
                                keyboard_conversation.update(cx, |conversation, cx| conversation.toggle_permission_pattern(&keyboard_session, request_id, index, cx)).log_err();
                            }));
                        }
                        let conversation = conversation.clone();
                        let view = view.clone();
                        let session = menu_session.clone();
                        let popup = cx.weak_entity();
                        menu.separator().item(PopupMenuItem::element(move |_, cx| {
                            let enabled = conversation.upgrade().is_some_and(|conversation| {
                                matches!(conversation.read(cx).permission_selections.get(&request_id), Some(PermissionSelection::SelectedPatterns(patterns)) if !patterns.is_empty())
                            });
                            let view = view.clone();
                            let session = session.clone();
                            let popup = popup.clone();
                            KitButton::new("confirm-always-run").primary().small().w_full().label("Always Run").disabled(!enabled)
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    view.update(cx, |this, cx| {
                                        this.authorize_with_granularity(session.clone(), request_id, true, window, cx);
                                    }).log_err();
                                    popup.update(cx, |_, cx| cx.emit(gpui::DismissEvent)).log_err();
                                })
                        }))
                    }))
                })
        });
        let always_run = popup.map(|popup| {
            let session_id = session_id.clone();
            gpui_kit::base::h_flex()
                .min_w_0()
                .child(
                    button(("always-run", entry_ix), "Always Run")
                        .secondary()
                        .rounded_tr(px(0.))
                        .rounded_br(px(0.))
                        .tooltip(permanent_tooltip)
                        .disabled(allow_disabled || permanent_outcome.is_none())
                        .when_some(permanent_outcome, |button, outcome| {
                            button.on_click(cx.listener(move |this, _, window, cx| {
                                this.authorize_permission_request(
                                    session_id.clone(),
                                    request_id,
                                    outcome.clone(),
                                    window,
                                    cx,
                                );
                            }))
                        }),
                )
                .child(popup)
        });
        gpui_kit::base::h_flex()
            .w_full()
            .justify_end()
            .gap_1p5()
            .pt_1()
            .pb_2()
            .pr_2()
            .children(reject_once.map(|option| {
                let session_id = session_id.clone();
                button(("skip-command", entry_ix), "Skip").on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.authorize_permission_request(
                            session_id.clone(),
                            request_id,
                            SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                                option.kind.clone(),
                            ),
                            window,
                            cx,
                        );
                    },
                ))
            }))
            .children(always_run)
            .children(allow_once.map(|option| {
                button(("run-command", entry_ix), "Run")
                    .primary()
                    .disabled(allow_disabled)
                    .children((show_shortcuts && !allow_disabled).then(|| {
                        KeyBinding::for_action_in(&AllowOnce, focus_handle, cx)
                            .size(font_size * 0.75)
                            .color(Color::Custom(
                                KitTheme::global(cx).primary_foreground.opacity(0.65),
                            ))
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.authorize_permission_request(
                            session_id.clone(),
                            request_id,
                            SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                                option.kind.clone(),
                            ),
                            window,
                            cx,
                        );
                    }))
            }))
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
            .flex_wrap()
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
                            .label(
                                if selected_index == choices.len().saturating_sub(1)
                                    && !matches!(
                                        selection,
                                        Some(PermissionSelection::SelectedPatterns(_))
                                    )
                                {
                                    "Allow Once"
                                } else {
                                    "Allow"
                                },
                            )
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
        let choices = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| (index, choice.label()))
            .collect::<Vec<_>>();
        let focus_handle = self.focus_handle.clone();
        PopoverMenu::new(("permission-granularity", entry_ix))
            .with_handle(self.permission_dropdown_handle.clone())
            .trigger(self.permission_dropdown_trigger(current_label, entry_ix, is_first, cx))
            .menu(move |window, cx| {
                Some(PopupMenu::build(window, cx, |mut menu, _, _| {
                    menu = menu.action_context(focus_handle.clone());
                    for (index, label) in &choices {
                        menu = menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(*index == selected_index)
                                .action(Box::new(SelectPermissionGranularity {
                                    tool_call_id: tool_call_id.0.to_string(),
                                    request_id: Some(request_id),
                                    session_id: Some(session_id.0.to_string()),
                                    index: *index,
                                })),
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
        let default_index = choices.len().saturating_sub(1);
        let choices = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| (index, choice.label()))
            .collect::<Vec<_>>();
        let patterns = patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                (
                    index,
                    SharedString::from(format!("Always for `{}`", pattern.display_name)),
                )
            })
            .collect::<Vec<_>>();
        let conversation = self.conversation.downgrade();
        let focus_handle = self.focus_handle.clone();
        PopoverMenu::new(("permission-granularity", entry_ix))
            .with_handle(self.permission_dropdown_handle.clone())
            .trigger(self.permission_dropdown_trigger(current_label, entry_ix, is_first, cx))
            .menu(move |window, cx| {
                Some(PopupMenu::build(window, cx, |mut menu, _, cx| {
                    menu = menu.action_context(focus_handle.clone());
                    for (index, label) in &choices {
                        let conversation = conversation.clone();
                        let selection_conversation = conversation.clone();
                        let session_id = session_id.clone();
                        let index = *index;
                        let label = label.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                let selection =
                                    selection_conversation.upgrade().and_then(|conversation| {
                                        conversation
                                            .read(cx)
                                            .permission_selections
                                            .get(&request_id)
                                            .cloned()
                                    });
                                let checked = selection
                                    .as_ref()
                                    .and_then(|selection| selection.choice_index())
                                    .unwrap_or(default_index)
                                    == index
                                    && !matches!(
                                        selection,
                                        Some(PermissionSelection::SelectedPatterns(_))
                                    );
                                gpui_kit::base::h_flex()
                                    .gap_2()
                                    .child(div().w(rems(1.)).when(checked, |icon| {
                                        icon.child(
                                            KitIcon::empty()
                                                .path(IconName::Check.path())
                                                .size(rems(1.)),
                                        )
                                    }))
                                    .child(label.clone())
                            })
                            .on_click(move |_, _, cx| {
                                conversation
                                    .update(cx, |conversation, cx| {
                                        conversation.set_permission_choice(
                                            &session_id,
                                            request_id,
                                            index,
                                            cx,
                                        )
                                    })
                                    .log_err();
                            }),
                        );
                    }
                    menu = menu.separator().label("Allow matching commands");
                    for (index, label) in &patterns {
                        let conversation = conversation.clone();
                        let keyboard_conversation = conversation.clone();
                        let session_id = session_id.clone();
                        let keyboard_session_id = session_id.clone();
                        let index = *index;
                        let label = label.clone();
                        let popup = cx.weak_entity();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                let checked = conversation.upgrade().is_some_and(|conversation| {
                                    conversation
                                        .read(cx)
                                        .permission_selections
                                        .get(&request_id)
                                        .is_some_and(|selection| {
                                            selection.is_pattern_checked(index)
                                        })
                                });
                                let conversation = conversation.clone();
                                let session_id = session_id.clone();
                                let popup = popup.clone();
                                gpui_kit::component::checkbox::Checkbox::new((
                                    "command-pattern",
                                    index,
                                ))
                                .label(label.clone())
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    conversation
                                        .update(cx, |conversation, cx| {
                                            conversation.toggle_permission_pattern(
                                                &session_id,
                                                request_id,
                                                index,
                                                cx,
                                            )
                                        })
                                        .log_err();
                                    popup.update(cx, |_, cx| cx.notify()).log_err();
                                })
                            })
                            .on_click(move |_, _, cx| {
                                keyboard_conversation
                                    .update(cx, |conversation, cx| {
                                        conversation.toggle_permission_pattern(
                                            &keyboard_session_id,
                                            request_id,
                                            index,
                                            cx,
                                        );
                                    })
                                    .log_err();
                            }),
                        );
                    }
                    menu.separator().item(PopupMenuItem::new("Done"))
                }))
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
        layout: ToolCallLayout,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let subagent_thread_view = subagent_session_id.and_then(|session_id| {
            self.server_view
                .upgrade()
                .and_then(|server_view| server_view.read(cx).as_connected())
                .and_then(|connected| connected.threads.get(&session_id))
        });
        v_flex()
            .w_full()
            .when_some(layout.row_gap(), |this, gap| this.px(TEXT_INSET).pt(gap))
            .child(self.render_subagent_card(
                active_session_id,
                entry_ix,
                subagent_thread_view,
                tool_call,
                focus_handle,
                window,
                cx,
            ))
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
        let thread = thread_view.map(|view| view.read(cx).thread.clone());
        let subagent_session_id = tool_call
            .subagent_session_info
            .as_ref()
            .map(|info| info.session_id.clone())
            .or_else(|| {
                thread
                    .as_ref()
                    .map(|thread| thread.read(cx).session_id().clone())
            });
        let changed_buffers = thread
            .as_ref()
            .map(|thread| {
                thread
                    .read(cx)
                    .action_log()
                    .read(cx)
                    .changed_buffers(cx)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let files_changed = changed_buffers.len();
        let diff_stats = DiffStats::all_files(changed_buffers, cx);
        let pending_tool_call = subagent_session_id
            .as_ref()
            .and_then(|session_id| self.conversation.read(cx).pending_tool_call(session_id, cx));
        let is_expanded = self
            .entry_view_state
            .read(cx)
            .is_tool_call_expanded(&tool_call.id);
        let is_cancelled = matches!(tool_call.status(), ToolCallStatus::Canceled)
            || tool_call.content().iter().any(|content| {
                matches!(content, ToolCallContent::ContentBlock { block, .. } if block.text_content(cx) == Some("User canceled"))
            });
        let is_failed = matches!(
            tool_call.status(),
            ToolCallStatus::Failed | ToolCallStatus::Rejected
        ) && !is_cancelled;
        let is_running = !is_cancelled
            && matches!(
                tool_call.status(),
                ToolCallStatus::Pending
                    | ToolCallStatus::InProgress
                    | ToolCallStatus::WaitingForConfirmation
            );
        let waiting = is_running
            && (pending_tool_call.is_some()
                || tool_call.status() == ToolCallStatus::WaitingForConfirmation);
        let model_name = thread_view
            .and_then(|view| view.read(cx).as_native_thread(cx))
            .and_then(|thread| thread.read(cx).model().map(|model| model.name().0))
            .or_else(|| {
                tool_call
                    .raw_input
                    .as_ref()?
                    .get("model")?
                    .as_str()
                    .map(SharedString::from)
            });
        let title = tool_call
            .raw_input
            .as_ref()
            .and_then(|input| input.get("label"))
            .and_then(|label| label.as_str())
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(SharedString::from)
            .or_else(|| {
                let label = markdown_line(tool_call.label.read(cx)).0;
                (!label.trim().is_empty()).then_some(label)
            })
            .or_else(|| thread.as_ref().and_then(|thread| thread.read(cx).title()))
            .unwrap_or_else(|| "Subagent".into());
        let status: SharedString = if waiting {
            "Waiting for approval".into()
        } else if is_cancelled {
            "Stopped".into()
        } else if is_failed {
            "Stopped with error".into()
        } else if is_running {
            thread
                .as_ref()
                .and_then(|thread| subagent_activity(thread.read(cx), tool_call, cx))
                .unwrap_or_else(|| "Starting up…".into())
        } else {
            "Completed".into()
        };
        let has_entries = thread.as_ref().is_some_and(|thread| {
            !subagent_entry_range(thread.read(cx).entries().len(), tool_call).is_empty()
        });
        let has_preview = has_entries || !tool_call.content().is_empty();
        let theme = KitTheme::global(cx);
        let font_size = ThemeSettings::get_global(cx).agent_ui_font_size(cx);
        let status_color = if is_failed {
            theme.danger
        } else if waiting {
            theme.warning
        } else {
            theme.muted_foreground
        };
        let group: SharedString = format!("subagent-row-{}", tool_call.id.0).into();
        let indicator = if is_running && !waiting {
            Spinner::new()
                .icon(kit_icon(IconName::LoadCircle))
                .with_size(KitSize::Size(font_size * 0.7))
                .into_any_element()
        } else {
            div()
                .size(font_size * 0.4)
                .rounded_full()
                .bg(status_color)
                .when(is_cancelled, |this| this.opacity(0.65))
                .into_any_element()
        };
        let header = KitListItem::new(("subagent-summary", entry_ix))
            .accessibility_label(format!("{title}: {status}"))
            .w_full()
            .min_w_0()
            .px_0()
            .py_1()
            .gap_2()
            .text_size(font_size)
            .font_weight(gpui::FontWeight::NORMAL)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_none()
                            .w(font_size * 1.4)
                            .justify_center()
                            .child(indicator),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.foreground)
                                            .child(title),
                                    )
                                    .when_some(model_name, |this, model_name| {
                                        this.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.muted_foreground.opacity(0.7))
                                                .child(model_name),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .text_color(status_color)
                                    .child(div().min_w_0().truncate().child(
                                        if is_running && !waiting {
                                            ShimmerText::new(status).into_any_element()
                                        } else {
                                            status.into_any_element()
                                        },
                                    ))
                                    .when(files_changed > 0, |this| {
                                        this.child(div().flex_none().child(format!(
                                            "{files_changed} {}",
                                            if files_changed == 1 { "file" } else { "files" }
                                        )))
                                        .child(diff_stat(diff_stats, cx))
                                    }),
                            ),
                    )
                    .when(is_running, |this| {
                        this.when_some(thread.clone(), |this, thread| {
                            this.child(
                                div().visible_on_hover(group.clone()).child(
                                    PillButton::new(("stop-subagent", entry_ix))
                                        .size(KitSize::XSmall)
                                        .icon(IconName::Stop)
                                        .debug_selector("ICON-Stop")
                                        .tooltip(Tooltip::text("Stop Subagent"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            telemetry::event!("Subagent Stopped");
                                            this.cancel_thread(&thread, cx).detach();
                                        })),
                                ),
                            )
                        })
                    })
                    .when(has_preview, |this| {
                        this.child(
                            PillButton::new(("preview-subagent", entry_ix))
                                .size(KitSize::XSmall)
                                .icon(if is_expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .tooltip(Tooltip::text(if is_expanded {
                                    "Collapse Preview"
                                } else {
                                    "Preview Subagent"
                                }))
                                .on_click(cx.listener({
                                    let tool_call_id = tool_call.id.clone();
                                    move |this, _, window, cx| {
                                        this.hold_scroll_position_for_toggle();
                                        this.entry_view_state.update(cx, |state, _| {
                                            state.toggle_tool_call_expansion(&tool_call_id)
                                        });
                                        this.refresh_thread_search(window, cx);
                                        cx.notify();
                                    }
                                })),
                        )
                    }),
            )
            .when(subagent_session_id.is_some() || has_preview, |this| {
                this.on_click(cx.listener({
                    let tool_call_id = tool_call.id.clone();
                    move |this, _, window, cx| {
                        if let Some(session_id) = &subagent_session_id {
                            telemetry::event!("Subagent Maximized");
                            this.server_view
                                .update(cx, |server, cx| {
                                    server.navigate_to_thread(session_id.clone(), window, cx);
                                })
                                .log_err();
                        } else {
                            this.hold_scroll_position_for_toggle();
                            this.entry_view_state.update(cx, |state, _| {
                                state.toggle_tool_call_expansion(&tool_call_id)
                            });
                            this.refresh_thread_search(window, cx);
                            cx.notify();
                        }
                    }
                }))
            });

        let mut sections = Vec::new();
        if is_running
            && let Some(thread_view) = thread_view
            && let Some((_, child_tool_call_id, _)) = pending_tool_call
        {
            sections.push(
                SubagentPermission {
                    thread_view: thread_view.clone(),
                    tool_call_id: child_tool_call_id,
                    active_session_id: active_session_id.clone(),
                    focus_handle: focus_handle.clone(),
                }
                .into_any_element(),
            );
        } else if is_expanded {
            if has_entries && let Some(thread_view) = thread_view {
                sections.push(
                    self.render_subagent_expanded_content(thread_view, tool_call, window, cx)
                        .into_any_element(),
                );
            } else {
                sections.push(
                    self.render_tool_result_content(entry_ix, tool_call, false, window, cx)
                        .into_any_element(),
                );
            }
            if has_entries
                && let Some(message) =
                    self.subagent_error_message(&tool_call.status(), tool_call, cx)
            {
                sections.push(
                    AlertCard::new(("subagent-error", entry_ix), AlertVariant::Error, message)
                        .into_any_element(),
                );
            }
        }

        v_flex()
            .id(("subagent", entry_ix))
            .group(group)
            .w_full()
            .min_w_0()
            .child(header)
            .when(!sections.is_empty(), |this| {
                this.child(
                    v_flex()
                        .min_w_0()
                        .mt_1()
                        .ml_3()
                        .pl_3()
                        .border_l_1()
                        .border_color(theme.border.opacity(0.6))
                        .gap_2()
                        .children(sections),
                )
            })
            .into_any_element()
    }

    fn render_subagent_expanded_content(
        &self,
        thread_view: &Entity<ThreadView>,
        tool_call: &ToolCall,
        _window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        const MAX_PREVIEW_ENTRIES: usize = 8;
        let subagent_thread = thread_view.read(cx).thread.read(cx);
        let mut entry_range = subagent_entry_range(subagent_thread.entries().len(), tool_call);
        let first_entry_ix = entry_range.start;
        entry_range.start = entry_range
            .end
            .saturating_sub(MAX_PREVIEW_ENTRIES)
            .max(first_entry_ix);
        if let Some(group) = ActivityGroup::containing(subagent_thread, entry_range.start, cx) {
            entry_range.start = group.entries.start.max(first_entry_ix);
        }
        let scroll_handle = self
            .subagent_scroll_handles
            .borrow_mut()
            .entry(tool_call.id.clone())
            .or_insert_with(|| {
                let handle = ScrollHandle::new();
                handle.scroll_to_bottom();
                handle
            })
            .clone();
        let at_bottom = scroll_handle.offset().y + scroll_handle.max_offset().y <= px(1.);
        if at_bottom
            && matches!(
                tool_call.status(),
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            )
        {
            scroll_handle.scroll_to_bottom();
        }
        SubagentPreview {
            thread_view: thread_view.clone(),
            tool_call_id: tool_call.id.clone(),
            entry_range,
            scroll_handle,
        }
    }

    fn subagent_error_message(
        &self,
        status: &ToolCallStatus,
        tool_call: &ToolCall,
        cx: &App,
    ) -> Option<SharedString> {
        if matches!(status, ToolCallStatus::Failed | ToolCallStatus::Rejected) {
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

fn subagent_entry_range(total_entries: usize, tool_call: &ToolCall) -> Range<usize> {
    match tool_call.subagent_session_info.as_ref() {
        Some(info) => {
            let end = info
                .message_end_index
                .map(|index| index.saturating_add(1))
                .unwrap_or(total_entries)
                .min(total_entries);
            info.message_start_index.min(end)..end
        }
        None => 0..total_entries,
    }
}

fn subagent_activity(thread: &AcpThread, tool_call: &ToolCall, cx: &App) -> Option<SharedString> {
    thread
        .entries()
        .get(subagent_entry_range(thread.entries().len(), tool_call))?
        .iter()
        .rev()
        .find_map(|entry| match entry {
            AgentThreadEntry::ToolCall(tool_call) => {
                if tool_call.status() == ToolCallStatus::WaitingForConfirmation {
                    return Some("Waiting for approval".into());
                }
                let running = matches!(
                    tool_call.status(),
                    ToolCallStatus::Pending | ToolCallStatus::InProgress
                );
                if let Some((action, details)) = native_tool_row_label(tool_call, running) {
                    Some(match details {
                        Some(details) => format!("{action} {details}").into(),
                        None => action.into(),
                    })
                } else {
                    Some(markdown_line(tool_call.label.read(cx)).0)
                }
            }
            AgentThreadEntry::AssistantMessage(message) => {
                message.chunks.iter().rev().find_map(|chunk| match chunk {
                    AssistantMessageChunk::Thought { block, .. } if block.visible_content(cx) => {
                        Some("Thinking…".into())
                    }
                    AssistantMessageChunk::Message { block, .. } if block.visible_content(cx) => {
                        Some("Writing response…".into())
                    }
                    _ => None,
                })
            }
            _ => None,
        })
}

#[derive(IntoElement)]
struct SubagentPreview {
    thread_view: Entity<ThreadView>,
    tool_call_id: acp_v2::ToolCallId,
    entry_range: Range<usize>,
    scroll_handle: ScrollHandle,
}

#[derive(IntoElement)]
struct SubagentPermission {
    thread_view: Entity<ThreadView>,
    tool_call_id: acp_v2::ToolCallId,
    active_session_id: acp_v2::SessionId,
    focus_handle: FocusHandle,
}

impl RenderOnce for SubagentPermission {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.thread_view.update(cx, |view, cx| {
            let Some((entry_ix, tool_call)) = view.thread.read(cx).tool_call(&self.tool_call_id)
            else {
                return Empty.into_any_element();
            };
            view.render_any_tool_call(
                &self.active_session_id,
                entry_ix,
                tool_call,
                &self.focus_handle,
                ToolCallLayout::Embedded,
                window,
                cx,
            )
            .into_any_element()
        })
    }
}

impl RenderOnce for SubagentPreview {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Bind listeners to the child view. Rendering with the parent's Context sends
        // nested disclosures and file-navigation clicks to the wrong conversation.
        let entries = self.thread_view.update(cx, |view, cx| {
            let entries = view.thread.read(cx).entries();
            self.entry_range
                .filter_map(|entry_ix| {
                    entries.get(entry_ix).map(|entry| {
                        view.render_unfolded_entry(entry_ix, entries.len(), entry, window, cx)
                    })
                })
                .collect::<Vec<_>>()
        });
        div()
            .id(SharedString::from(format!(
                "subagent-preview-{}",
                self.tool_call_id
            )))
            .w_full()
            .min_h_0()
            .max_h(px(300.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .pb_1()
            .children(entries)
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
        .text_size(ThemeSettings::get_global(cx).agent_ui_font_size(cx))
        .text_color(KitTheme::global(cx).foreground)
        .child(command.lines().next().unwrap_or_default().to_string())
}

/// What a command does, in the agent's words, when it said: the Zed Agent's terminal tool and some
/// other agents' (e.g. Claude Code's Bash) take a description beside the command.
fn command_description(tool_call: &ToolCall) -> Option<String> {
    tool_call
        .raw_input
        .as_ref()
        .and_then(|input| input.get("description"))
        .and_then(|description| description.as_str())
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_string)
}

fn command_verb(needs_confirmation: bool, running: bool) -> &'static str {
    if needs_confirmation {
        "Run"
    } else if running {
        "Running"
    } else {
        "Ran"
    }
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
        .text_size(ThemeSettings::get_global(cx).agent_ui_font_size(cx))
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
    compact: bool,
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
            compact: false,
            on_click: None,
        }
    }

    fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
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
            .min_h_8()
            .py_1()
            .px_2()
            .gap_1p5()
            .text_size(ThemeSettings::get_global(cx).agent_ui_font_size(cx))
            .when(self.compact, |this| {
                let font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
                this.min_h(font_size * 1.75).text_size(font_size * 0.875)
            })
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
    command_approval: bool,
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
            command_approval: false,
        }
    }

    fn command_approval(mut self, pending: bool) -> Self {
        self.command_approval = pending;
        self
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
        let border = if self.command_approval {
            KitTheme::global(cx).foreground.opacity(0.2)
        } else {
            KitTheme::global(cx).border
        };
        let command_approval = self.command_approval;
        let dashed = self.dashed;
        let section = move |content: AnyElement| {
            div()
                .w_full()
                .when(!command_approval, |section| section.border_t_1())
                .border_color(border)
                .when(dashed, |this| this.border_dashed())
                .child(content)
        };

        let card = if self.bare {
            v_flex().w_full()
        } else {
            transcript_card(cx)
                .border_color(border)
                .when(command_approval, |card| card.rounded(rems(0.5)))
                .when(dashed, |this| this.border_dashed())
        };
        card.id(self.id)
            .child(self.header)
            .children(self.sections.into_iter().map(section))
            .children(self.footer.map(|footer| {
                section(
                    div()
                        .w_full()
                        .when(!command_approval, |footer| footer.p_1p5())
                        .child(footer)
                        .into_any_element(),
                )
            }))
    }
}
