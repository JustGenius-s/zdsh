use std::{ops::Range, sync::Arc};

use acp_thread::{
    AcpThread, AgentThreadEntry, AssistantMessage, AssistantMessageChunk, ThreadStatus, ToolCall,
    ToolCallContent, ToolCallStatus,
};
use agent::ThreadStore;
use agent_client_protocol::schema::v2 as acp_v2;
use agent_settings::AgentSettings;
use collections::{HashMap, HashSet};
use editor::{
    Editor, EditorEvent, EditorMode, HiddenUnstagedDiffHunkRenderer, MinimapVisibility,
    SizingBehavior,
};
use gpui::{
    AnyEntity, App, AppContext as _, Corners, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, ScrollHandle, TextStyleRefinement, WeakEntity, Window,
};
use language::language_settings::SoftWrap;
use multi_buffer::MultiBuffer;
use project::{AgentId, Project, project_settings::DiagnosticSeverity};
use rope::Point;
use settings::{Settings as _, ThinkingBlockDisplay};
use terminal_view::TerminalView;
use theme_settings::ThemeSettings;
use ui::{Context, TextSize};
use workspace::Workspace;

use crate::message_editor::{MessageEditor, MessageEditorEvent, SharedSessionCapabilities};

/// Top padding, in rems, inside agent terminals. The tool card adds it to the
/// scrollable terminal's height so the number of visible rows doesn't change.
pub(crate) const TERMINAL_CONTENT_TOP_PADDING: f32 = 0.5;

/// How many lines of output an agent terminal shows before it scrolls instead, so long output
/// doesn't stretch its tool card down the transcript.
pub(crate) const TERMINAL_MAX_INLINE_LINES: usize = 10;

/// Maps an entry index through the removal of `removed` (a contiguous range of
/// entries), returning `None` if the index referred to a removed entry.
fn reindex_after_removal(index: usize, removed: &Range<usize>) -> Option<usize> {
    if index < removed.start {
        Some(index)
    } else if index < removed.end {
        None
    } else {
        Some(index - removed.len())
    }
}

/// Consecutive tool calls, with the thinking between them, which the transcript folds into one
/// line that sums them up, such as "Edited 2 files, explored 3 files".
pub(crate) struct ActivityGroup {
    /// The thread entries from the group's first member through its last.
    pub(crate) entries: Range<usize>,
    /// The entries of the group's tool calls.
    pub(crate) tool_call_entries: Vec<usize>,
    pub(crate) first_tool_call_id: acp_v2::ToolCallId,
    pub(crate) running: bool,
    /// Whether nothing shows after the group.
    pub(crate) ends_thread: bool,
}

impl ActivityGroup {
    /// The group the entry at `entry_ix` belongs to, when it's a tool call or thinking with at
    /// least one more beside it, and the group has a tool call. Entries that show nothing, such
    /// as an empty assistant message, don't split a group.
    pub(crate) fn containing(thread: &AcpThread, entry_ix: usize, cx: &App) -> Option<Self> {
        let entries = thread.entries();
        activity_member(thread, entries.get(entry_ix)?, cx)?;

        let mut start = entry_ix;
        for (ix, entry) in entries.iter().enumerate().take(entry_ix).rev() {
            if activity_member(thread, entry, cx).is_some() {
                start = ix;
            } else if !shows_nothing(entry, cx) {
                break;
            }
        }
        let mut end = entry_ix + 1;
        let mut ends_thread = true;
        for (ix, entry) in entries.iter().enumerate().skip(entry_ix + 1) {
            if activity_member(thread, entry, cx).is_some() {
                end = ix + 1;
            } else if !shows_nothing(entry, cx) {
                ends_thread = false;
                break;
            }
        }

        let mut member_count = 0;
        let mut tool_calls = Vec::new();
        for (ix, entry) in entries.iter().enumerate().take(end).skip(start) {
            match activity_member(thread, entry, cx) {
                Some(ActivityMember::ToolCall(tool_call)) => {
                    member_count += 1;
                    tool_calls.push((ix, tool_call));
                }
                Some(ActivityMember::Thought) => member_count += 1,
                None => {}
            }
        }
        if member_count < 2 {
            return None;
        }
        let first_tool_call_id = tool_calls.first()?.1.id.clone();
        let running = tool_calls.iter().any(|(_, tool_call)| {
            matches!(
                tool_call.status(),
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            )
        });
        Some(Self {
            entries: start..end,
            tool_call_entries: tool_calls.into_iter().map(|(ix, _)| ix).collect(),
            first_tool_call_id,
            running,
            ends_thread,
        })
    }
}

enum ActivityMember<'a> {
    ToolCall(&'a ToolCall),
    Thought,
}

fn activity_member<'a>(
    thread: &AcpThread,
    entry: &'a AgentThreadEntry,
    cx: &App,
) -> Option<ActivityMember<'a>> {
    match entry {
        AgentThreadEntry::ToolCall(tool_call) => {
            let has_settled_on_showing = match tool_call.status() {
                ToolCallStatus::Pending
                | ToolCallStatus::InProgress
                | ToolCallStatus::Completed
                | ToolCallStatus::Failed
                | ToolCallStatus::Rejected => true,
                ToolCallStatus::Canceled => !is_hidden_canceled_tool_call(tool_call, cx),
                ToolCallStatus::WaitingForConfirmation => false,
            };
            // A subagent shows its own work in its card, and a question the user answered
            // belongs with the user's messages.
            let stands_alone =
                tool_call.is_subagent() || thread.is_user_authored_scroll_target(entry);
            (has_settled_on_showing && !stands_alone).then_some(ActivityMember::ToolCall(tool_call))
        }
        AgentThreadEntry::AssistantMessage(message) if is_thought_only(message, cx) => {
            Some(ActivityMember::Thought)
        }
        _ => None,
    }
}

/// An assistant message with thinking and nothing else to show.
fn is_thought_only(message: &AssistantMessage, cx: &App) -> bool {
    let mut has_thought = false;
    for chunk in &message.chunks {
        match chunk {
            AssistantMessageChunk::Message { block, .. } => {
                if block.visible_content(cx) {
                    return false;
                }
            }
            AssistantMessageChunk::Thought { block, .. } => {
                has_thought |= block.visible_content(cx);
            }
        }
    }
    has_thought && !message.indented && !message.is_subagent_output
}

/// The work of a finished turn, from its prompt through the agent's last tool call or thinking,
/// which the transcript folds into one "Worked for 2m 15s" line above the agent's last reply.
pub(crate) struct TurnFold {
    /// The entry of the user message that started the turn.
    pub(crate) prompt_entry_ix: usize,
    /// The folded entries.
    pub(crate) entries: Range<usize>,
    pub(crate) is_last_turn: bool,
}

impl TurnFold {
    pub(crate) fn containing(thread: &AcpThread, entry_ix: usize, cx: &App) -> Option<Self> {
        // A subagent's thread is opened to look at its work.
        if thread.parent_session_id().is_some() {
            return None;
        }
        let entries = thread.entries();
        let prompt_entry_ix = entries.get(..entry_ix)?.iter().rposition(is_turn_prompt)?;
        let turn_end = entries
            .iter()
            .enumerate()
            .skip(prompt_entry_ix + 1)
            .find_map(|(ix, entry)| is_turn_prompt(entry).then_some(ix))
            .unwrap_or(entries.len());
        let is_last_turn = turn_end == entries.len();
        if is_last_turn && thread.status() == ThreadStatus::Generating {
            return None;
        }

        let turn = prompt_entry_ix + 1..turn_end;
        let reply_ix = turn.clone().rev().find(|&ix| {
            matches!(
                entries.get(ix),
                Some(AgentThreadEntry::AssistantMessage(message)) if has_reply(message, cx)
            )
        })?;
        let last_work_ix = (turn.start..reply_ix).rev().find(|&ix| {
            entries
                .get(ix)
                .is_some_and(|entry| is_work(entry, cx) && is_foldable(thread, entry))
        })?;
        // Elicitations, compactions and answered questions stay in view, so only the work after
        // the last of them folds.
        let start = (turn.start..last_work_ix)
            .rev()
            .find(|&ix| {
                entries
                    .get(ix)
                    .is_some_and(|entry| !is_foldable(thread, entry))
            })
            .map_or(turn.start, |ix| ix + 1);
        let folded = start..last_work_ix + 1;
        folded.contains(&entry_ix).then_some(Self {
            prompt_entry_ix,
            entries: folded,
            is_last_turn,
        })
    }

    /// Whether the fold holds all of the turn's work before its reply, so the turn's duration
    /// describes it.
    pub(crate) fn starts_at_prompt(&self) -> bool {
        self.entries.start == self.prompt_entry_ix + 1
    }
}

fn is_turn_prompt(entry: &AgentThreadEntry) -> bool {
    matches!(entry, AgentThreadEntry::UserMessage(message) if !message.indented)
}

fn has_reply(message: &AssistantMessage, cx: &App) -> bool {
    !message.indented
        && message.chunks.iter().any(|chunk| {
            matches!(chunk, AssistantMessageChunk::Message { block, .. } if block.visible_content(cx))
        })
}

fn is_work(entry: &AgentThreadEntry, cx: &App) -> bool {
    match entry {
        AgentThreadEntry::ToolCall(tool_call) => !is_hidden_canceled_tool_call(tool_call, cx),
        AgentThreadEntry::AssistantMessage(message) => is_thought_only(message, cx),
        _ => false,
    }
}

fn is_foldable(thread: &AcpThread, entry: &AgentThreadEntry) -> bool {
    match entry {
        AgentThreadEntry::AssistantMessage(message) => !message.indented,
        AgentThreadEntry::ToolCall(_) => !thread.is_user_authored_scroll_target(entry),
        AgentThreadEntry::UserMessage(_)
        | AgentThreadEntry::Elicitation(_)
        | AgentThreadEntry::ContextCompaction(_) => false,
    }
}

/// The thought chunk at the end of the thread, which is the one streaming while the agent works.
fn streaming_thought_key(thread: &AcpThread) -> Option<(usize, usize)> {
    let last_ix = thread.entries().len().checked_sub(1)?;
    match thread.entries().get(last_ix)? {
        AgentThreadEntry::AssistantMessage(message) => match message.chunks.last()? {
            AssistantMessageChunk::Thought { .. } => Some((last_ix, message.chunks.len() - 1)),
            AssistantMessageChunk::Message { .. } => None,
        },
        _ => None,
    }
}

fn shows_nothing(entry: &AgentThreadEntry, cx: &App) -> bool {
    match entry {
        AgentThreadEntry::AssistantMessage(message) => {
            message.chunks.iter().all(|chunk| match chunk {
                AssistantMessageChunk::Message { block, .. }
                | AssistantMessageChunk::Thought { block, .. } => !block.visible_content(cx),
            })
        }
        AgentThreadEntry::ToolCall(tool_call) => is_hidden_canceled_tool_call(tool_call, cx),
        _ => false,
    }
}

/// A tool call canceled before it produced anything, which the transcript leaves out, since it
/// would only show as a "Canceled" line.
pub(crate) fn is_hidden_canceled_tool_call(tool_call: &ToolCall, cx: &App) -> bool {
    matches!(tool_call.status(), ToolCallStatus::Canceled)
        && !tool_call.content().iter().any(|content| match content {
            ToolCallContent::ContentBlock { block, .. } => block.visible_content(cx),
            ToolCallContent::Diff(_)
            | ToolCallContent::LegacyDiff { .. }
            | ToolCallContent::Terminal { .. } => true,
            ToolCallContent::DiffPatch { render, .. } => {
                !render.files.is_empty()
                    || render
                        .fallback
                        .as_ref()
                        .is_some_and(|markdown| !markdown.read(cx).source().is_empty())
            }
            ToolCallContent::Other { markdown, .. } => !markdown.read(cx).source().is_empty(),
        })
}

pub struct EntryViewState {
    workspace: WeakEntity<Workspace>,
    project: WeakEntity<Project>,
    thread_store: Option<Entity<ThreadStore>>,
    entries: Vec<Entry>,
    session_capabilities: SharedSessionCapabilities,
    agent_id: AgentId,
    expanded_thinking_blocks: HashSet<(usize, usize)>,
    auto_expanded_thinking_block: Option<(usize, usize)>,
    user_toggled_thinking_blocks: HashSet<(usize, usize)>,
    expanded_compactions: HashSet<usize>,
    expanded_tool_calls: HashSet<acp_v2::ToolCallId>,
    expanded_activity_groups: HashSet<acp_v2::ToolCallId>,
    /// Turn folds the user opened or closed, by their prompt's entry index.
    toggled_turn_folds: HashMap<usize, bool>,
    saw_live_turn: bool,
}

impl EntryViewState {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: WeakEntity<Project>,
        thread_store: Option<Entity<ThreadStore>>,
        session_capabilities: SharedSessionCapabilities,
        agent_id: AgentId,
    ) -> Self {
        Self {
            workspace,
            project,
            thread_store,
            entries: Vec::new(),
            session_capabilities,
            agent_id,
            expanded_thinking_blocks: HashSet::default(),
            auto_expanded_thinking_block: None,
            user_toggled_thinking_blocks: HashSet::default(),
            expanded_compactions: HashSet::default(),
            expanded_tool_calls: HashSet::default(),
            expanded_activity_groups: HashSet::default(),
            toggled_turn_folds: HashMap::default(),
            saw_live_turn: false,
        }
    }

    pub(crate) fn is_tool_call_expanded(&self, tool_call_id: &acp_v2::ToolCallId) -> bool {
        self.expanded_tool_calls.contains(tool_call_id)
    }

    pub(crate) fn is_tool_call_content_visible(&self, tool_call: &ToolCall) -> bool {
        self.is_tool_call_expanded(&tool_call.id) || tool_call.authorization_id().is_some()
    }

    pub(crate) fn expand_tool_call(&mut self, tool_call_id: acp_v2::ToolCallId) {
        self.expanded_tool_calls.insert(tool_call_id);
    }

    pub(crate) fn collapse_tool_call(&mut self, tool_call_id: &acp_v2::ToolCallId) {
        self.expanded_tool_calls.remove(tool_call_id);
    }

    pub(crate) fn is_activity_group_expanded(&self, group: &ActivityGroup) -> bool {
        self.expanded_activity_groups
            .contains(&group.first_tool_call_id)
    }

    pub(crate) fn toggle_activity_group_expansion(&mut self, group: &ActivityGroup) {
        if !self
            .expanded_activity_groups
            .remove(&group.first_tool_call_id)
        {
            self.expanded_activity_groups
                .insert(group.first_tool_call_id.clone());
        }
    }

    /// Turns start folded, except the last one when this view watched the agent work.
    pub(crate) fn is_turn_fold_expanded(&self, fold: &TurnFold) -> bool {
        self.toggled_turn_folds
            .get(&fold.prompt_entry_ix)
            .copied()
            .unwrap_or(fold.is_last_turn && self.saw_live_turn)
    }

    pub(crate) fn toggle_turn_fold_expansion(&mut self, fold: &TurnFold) {
        let expanded = self.is_turn_fold_expanded(fold);
        self.toggled_turn_folds
            .insert(fold.prompt_entry_ix, !expanded);
    }

    pub(crate) fn note_live_turn(&mut self) {
        self.saw_live_turn = true;
    }

    /// Whether the entry at `entry_ix` is folded away, into a collapsed turn or activity group.
    pub(crate) fn is_entry_folded(&self, thread: &AcpThread, entry_ix: usize, cx: &App) -> bool {
        TurnFold::containing(thread, entry_ix, cx)
            .is_some_and(|fold| !self.is_turn_fold_expanded(&fold))
            || ActivityGroup::containing(thread, entry_ix, cx)
                .is_some_and(|group| !self.is_activity_group_expanded(&group))
    }

    pub(crate) fn toggle_tool_call_expansion(&mut self, tool_call_id: &acp_v2::ToolCallId) {
        if !self.expanded_tool_calls.remove(tool_call_id) {
            self.expanded_tool_calls.insert(tool_call_id.clone());
        }
    }

    pub(crate) fn is_compaction_expanded(&self, entry_ix: usize) -> bool {
        self.expanded_compactions.contains(&entry_ix)
    }

    pub(crate) fn collapse_compaction(&mut self, entry_ix: usize) {
        self.expanded_compactions.remove(&entry_ix);
    }

    pub(crate) fn toggle_compaction_expansion(&mut self, entry_ix: usize) {
        if !self.expanded_compactions.remove(&entry_ix) {
            self.expanded_compactions.insert(entry_ix);
        }
    }

    pub(crate) fn clear_auto_expand_tracking(&mut self) {
        self.auto_expanded_thinking_block = None;
    }

    pub(crate) fn is_auto_expanded_thinking_block(&self, key: (usize, usize)) -> bool {
        self.auto_expanded_thinking_block == Some(key)
    }

    pub(crate) fn auto_expand_streaming_thought(&mut self, thread: &AcpThread, cx: &App) -> bool {
        let thinking_display = AgentSettings::get_global(cx).thinking_display;

        if !matches!(
            thinking_display,
            ThinkingBlockDisplay::Auto | ThinkingBlockDisplay::Preview
        ) {
            return false;
        }

        if let Some(key) = streaming_thought_key(thread) {
            if self.auto_expanded_thinking_block != Some(key) {
                self.auto_expanded_thinking_block = Some(key);
                self.expanded_thinking_blocks.insert(key);
                return true;
            }
        } else if self.auto_expanded_thinking_block.is_some() {
            if thinking_display == ThinkingBlockDisplay::Auto
                && let Some(key) = self.auto_expanded_thinking_block
                && !self.user_toggled_thinking_blocks.contains(&key)
            {
                self.expanded_thinking_blocks.remove(&key);
            }
            self.auto_expanded_thinking_block = None;
            return true;
        }

        false
    }

    pub(crate) fn toggle_thinking_block_expansion(&mut self, key: (usize, usize), cx: &App) {
        match AgentSettings::get_global(cx).thinking_display {
            ThinkingBlockDisplay::Auto => {
                let is_open = self.expanded_thinking_blocks.contains(&key)
                    || self.user_toggled_thinking_blocks.contains(&key);

                if is_open {
                    self.expanded_thinking_blocks.remove(&key);
                    self.user_toggled_thinking_blocks.remove(&key);
                } else {
                    self.expanded_thinking_blocks.insert(key);
                    self.user_toggled_thinking_blocks.insert(key);
                }
            }
            ThinkingBlockDisplay::Preview => {
                let is_user_expanded = self.user_toggled_thinking_blocks.contains(&key);
                let is_in_expanded_set = self.expanded_thinking_blocks.contains(&key);

                if is_user_expanded {
                    self.user_toggled_thinking_blocks.remove(&key);
                    self.expanded_thinking_blocks.remove(&key);
                } else if is_in_expanded_set {
                    self.user_toggled_thinking_blocks.insert(key);
                } else {
                    self.expanded_thinking_blocks.insert(key);
                    self.user_toggled_thinking_blocks.insert(key);
                }
            }
            ThinkingBlockDisplay::AlwaysExpanded => {
                if self.user_toggled_thinking_blocks.contains(&key) {
                    self.user_toggled_thinking_blocks.remove(&key);
                } else {
                    self.user_toggled_thinking_blocks.insert(key);
                }
            }
            ThinkingBlockDisplay::AlwaysCollapsed => {
                if self.user_toggled_thinking_blocks.contains(&key) {
                    self.user_toggled_thinking_blocks.remove(&key);
                    self.expanded_thinking_blocks.remove(&key);
                } else {
                    self.expanded_thinking_blocks.insert(key);
                    self.user_toggled_thinking_blocks.insert(key);
                }
            }
        }
    }

    pub(crate) fn thinking_block_state(&self, key: (usize, usize), cx: &App) -> (bool, bool) {
        let is_user_toggled = self.user_toggled_thinking_blocks.contains(&key);
        let is_in_expanded_set = self.expanded_thinking_blocks.contains(&key);

        match AgentSettings::get_global(cx).thinking_display {
            ThinkingBlockDisplay::Auto => {
                let is_open = is_user_toggled || is_in_expanded_set;
                (is_open, false)
            }
            ThinkingBlockDisplay::Preview => {
                let is_open = is_user_toggled || is_in_expanded_set;
                let is_constrained = is_in_expanded_set && !is_user_toggled;
                (is_open, is_constrained)
            }
            ThinkingBlockDisplay::AlwaysExpanded => (!is_user_toggled, false),
            ThinkingBlockDisplay::AlwaysCollapsed => (is_user_toggled, false),
        }
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    pub fn sync_entry(
        &mut self,
        index: usize,
        thread: &Entity<AcpThread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread_entry) = thread.read(cx).entries().get(index) else {
            return;
        };

        match thread_entry {
            AgentThreadEntry::UserMessage(message) => {
                let can_rewind = thread
                    .read(cx)
                    .can_rewind_to(message.client_id.as_ref(), cx);
                let source_blocks = message.content.source_blocks();
                let source_version = message.content.source_version();
                let source_is_representable = source_blocks
                    .iter()
                    .all(acp_thread::content::can_convert_to_v1);
                let is_editable = can_rewind && source_is_representable;
                if let Some(Entry::UserMessage {
                    editor,
                    synced_source_version,
                }) = self.entries.get_mut(index)
                {
                    // Read-only messages cannot hold drafts, so focus must not
                    // block new content, nor should unchanged content reset their selection.
                    let was_read_only = editor.read(cx).editor().read(cx).read_only(cx);
                    let refreshed_source = ((!was_read_only
                        || *synced_source_version != source_version)
                        && (!is_editable || !editor.focus_handle(cx).is_focused(window)))
                    .then(|| source_blocks.to_vec());
                    editor.update(cx, |editor, cx| editor.set_read_only(!is_editable, cx));
                    if let Some(source_blocks) = refreshed_source {
                        editor.update(cx, |editor, cx| {
                            editor.set_source_message(source_blocks, window, cx);
                        });
                        *synced_source_version = source_version;
                    }
                } else {
                    let source_blocks = source_blocks.to_vec();
                    let message_editor = cx.new(|cx| {
                        let mut editor = MessageEditor::new(
                            self.workspace.clone(),
                            self.project.clone(),
                            self.thread_store.clone(),
                            self.session_capabilities.clone(),
                            self.agent_id.clone(),
                            "Edit message － @ to include context",
                            editor::EditorMode::AutoHeight {
                                min_lines: 1,
                                max_lines: None,
                            },
                            window,
                            cx,
                        );
                        if !is_editable {
                            editor.set_read_only(true, cx);
                        }
                        editor.set_source_message(source_blocks, window, cx);
                        editor
                    });
                    cx.subscribe(&message_editor, move |_, editor, event, cx| {
                        cx.emit(EntryViewEvent {
                            entry_index: index,
                            view_event: ViewEvent::MessageEditorEvent(editor, event.clone()),
                        })
                    })
                    .detach();
                    self.set_entry(
                        index,
                        Entry::UserMessage {
                            editor: message_editor,
                            synced_source_version: source_version,
                        },
                    );
                }
            }
            AgentThreadEntry::ToolCall(tool_call) => {
                let id = tool_call.id.clone();
                let terminals = tool_call.terminals().cloned().collect::<Vec<_>>();
                let diffs = tool_call.diffs().cloned().collect::<Vec<_>>();
                let patch_hunk_buffers = tool_call
                    .content()
                    .iter()
                    .filter_map(|content| match content {
                        acp_thread::ToolCallContent::DiffPatch { render, .. } => {
                            Some(&render.files)
                        }
                        _ => None,
                    })
                    .flat_map(|files| files.iter().flat_map(|file| &file.hunks))
                    .map(|hunk| hunk.buffer.clone())
                    .collect::<Vec<_>>();
                let patch_hunk_ids: HashSet<_> = patch_hunk_buffers
                    .iter()
                    .map(|buffer| buffer.entity_id())
                    .collect();

                let views = if let Some(Entry::ToolCall(tool_call)) = self.entries.get_mut(index) {
                    &mut tool_call.content
                } else {
                    self.set_entry(
                        index,
                        Entry::ToolCall(ToolCallEntry {
                            content: HashMap::default(),
                            patch_hunk_ids: HashSet::default(),
                            focus_handle: cx.focus_handle(),
                        }),
                    );
                    let Some(Entry::ToolCall(tool_call)) = self.entries.get_mut(index) else {
                        unreachable!()
                    };
                    &mut tool_call.content
                };

                let is_tool_call_completed =
                    matches!(tool_call.status(), acp_thread::ToolCallStatus::Completed);

                for terminal in terminals {
                    match views.entry(terminal.entity_id()) {
                        collections::hash_map::Entry::Vacant(entry) => {
                            let element = create_terminal(
                                self.workspace.clone(),
                                self.project.clone(),
                                terminal.clone(),
                                window,
                                cx,
                            )
                            .into_any();
                            cx.emit(EntryViewEvent {
                                entry_index: index,
                                view_event: ViewEvent::NewTerminal(id.clone()),
                            });
                            entry.insert(element);
                        }
                        collections::hash_map::Entry::Occupied(_entry) => {
                            let terminal = terminal.read(cx);
                            if is_tool_call_completed
                                && terminal.is_process_backed()
                                && terminal.output().is_none()
                            {
                                cx.emit(EntryViewEvent {
                                    entry_index: index,
                                    view_event: ViewEvent::TerminalMovedToBackground(id.clone()),
                                });
                            }
                        }
                    }
                }

                for diff in diffs {
                    views.entry(diff.entity_id()).or_insert_with(|| {
                        let editor = create_editor_diff(diff.clone(), window, cx);
                        cx.subscribe(&editor, {
                            let diff = diff.clone();
                            let entry_index = index;
                            move |_this, _editor, event: &EditorEvent, cx| {
                                if let EditorEvent::OpenExcerptsRequested {
                                    selections_by_buffer,
                                    split,
                                } = event
                                {
                                    let multibuffer = diff.read(cx).multibuffer();
                                    if let Some((buffer_id, (ranges, _))) =
                                        selections_by_buffer.iter().next()
                                    {
                                        if let Some(buffer) =
                                            multibuffer.read(cx).buffer(*buffer_id)
                                        {
                                            if let Some(range) = ranges.first() {
                                                let point =
                                                    buffer.read(cx).offset_to_point(range.start.0);
                                                if let Some(path) = diff.read(cx).file_path(cx) {
                                                    cx.emit(EntryViewEvent {
                                                        entry_index,
                                                        view_event: ViewEvent::OpenDiffLocation {
                                                            path,
                                                            position: point,
                                                            split: *split,
                                                        },
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        })
                        .detach();
                        cx.emit(EntryViewEvent {
                            entry_index: index,
                            view_event: ViewEvent::NewDiff(id.clone()),
                        });
                        editor.into_any()
                    });
                }
                for buffer in patch_hunk_buffers {
                    views.entry(buffer.entity_id()).or_insert_with(|| {
                        let editor = create_multibuffer_diff_editor(buffer, window, cx);
                        cx.emit(EntryViewEvent {
                            entry_index: index,
                            view_event: ViewEvent::NewDiff(id.clone()),
                        });
                        editor.into_any()
                    });
                }
                if let Some(Entry::ToolCall(entry)) = self.entries.get_mut(index) {
                    for stale_id in entry.patch_hunk_ids.difference(&patch_hunk_ids) {
                        entry.content.remove(stale_id);
                    }
                    entry.patch_hunk_ids = patch_hunk_ids;
                }
            }
            AgentThreadEntry::Elicitation(_) => {
                if !matches!(self.entries.get(index), Some(Entry::Elicitation { .. })) {
                    self.set_entry(
                        index,
                        Entry::Elicitation {
                            focus_handle: cx.focus_handle(),
                        },
                    );
                }
            }
            AgentThreadEntry::AssistantMessage(message) => {
                let entry = if let Some(Entry::AssistantMessage(entry)) =
                    self.entries.get_mut(index)
                {
                    entry
                } else {
                    self.set_entry(
                        index,
                        Entry::AssistantMessage(AssistantMessageEntry {
                            scroll_handles_by_chunk_index: HashMap::default(),
                            last_thought_source_version: None,
                            focus_handle: cx.focus_handle(),
                        }),
                    );
                    let Some(Entry::AssistantMessage(entry)) = self.entries.get_mut(index) else {
                        unreachable!()
                    };
                    entry
                };
                entry.sync(message);
            }
            AgentThreadEntry::ContextCompaction(_) => {
                if !matches!(self.entries.get(index), Some(Entry::ContextCompaction)) {
                    self.set_entry(index, Entry::ContextCompaction);
                }
            }
        };
    }

    fn set_entry(&mut self, index: usize, entry: Entry) {
        if index == self.entries.len() {
            self.entries.push(entry);
        } else {
            self.entries[index] = entry;
        }
    }

    pub fn remove(&mut self, range: Range<usize>) {
        self.entries.drain(range.clone());

        self.expanded_compactions = self
            .expanded_compactions
            .iter()
            .filter_map(|&entry_ix| reindex_after_removal(entry_ix, &range))
            .collect();
        self.toggled_turn_folds = self
            .toggled_turn_folds
            .iter()
            .filter_map(|(&entry_ix, &expanded)| {
                reindex_after_removal(entry_ix, &range).map(|entry_ix| (entry_ix, expanded))
            })
            .collect();
        self.expanded_thinking_blocks = self
            .expanded_thinking_blocks
            .iter()
            .filter_map(|&(entry_ix, chunk_ix)| {
                reindex_after_removal(entry_ix, &range).map(|entry_ix| (entry_ix, chunk_ix))
            })
            .collect();
        self.user_toggled_thinking_blocks = self
            .user_toggled_thinking_blocks
            .iter()
            .filter_map(|&(entry_ix, chunk_ix)| {
                reindex_after_removal(entry_ix, &range).map(|entry_ix| (entry_ix, chunk_ix))
            })
            .collect();
        self.auto_expanded_thinking_block =
            self.auto_expanded_thinking_block
                .and_then(|(entry_ix, chunk_ix)| {
                    reindex_after_removal(entry_ix, &range).map(|entry_ix| (entry_ix, chunk_ix))
                });
    }

    pub fn agent_ui_font_size_changed(&mut self, cx: &mut App) {
        for entry in self.entries.iter() {
            match entry {
                Entry::UserMessage { .. }
                | Entry::AssistantMessage { .. }
                | Entry::Elicitation { .. }
                | Entry::ContextCompaction => {}
                Entry::ToolCall(ToolCallEntry { content, .. }) => {
                    for view in content.values() {
                        if let Ok(diff_editor) = view.clone().downcast::<Editor>() {
                            diff_editor.update(cx, |diff_editor, cx| {
                                diff_editor.set_text_style_refinement(
                                    diff_editor_text_style_refinement(cx),
                                );
                                cx.notify();
                            })
                        }
                    }
                }
            }
        }
    }
}

impl EventEmitter<EntryViewEvent> for EntryViewState {}

pub struct EntryViewEvent {
    pub entry_index: usize,
    pub view_event: ViewEvent,
}

pub enum ViewEvent {
    NewDiff(acp_v2::ToolCallId),
    NewTerminal(acp_v2::ToolCallId),
    TerminalMovedToBackground(acp_v2::ToolCallId),
    MessageEditorEvent(Entity<MessageEditor>, MessageEditorEvent),
    OpenDiffLocation {
        path: String,
        position: Point,
        split: bool,
    },
}

#[derive(Debug)]
pub struct AssistantMessageEntry {
    scroll_handles_by_chunk_index: HashMap<usize, ScrollHandle>,
    last_thought_source_version: Option<acp_thread::MessageContentVersion>,
    focus_handle: FocusHandle,
}

impl AssistantMessageEntry {
    pub fn scroll_handle_for_chunk(&self, ix: usize) -> Option<ScrollHandle> {
        self.scroll_handles_by_chunk_index.get(&ix).cloned()
    }

    pub fn sync(&mut self, message: &acp_thread::AssistantMessage) {
        if let Some(acp_thread::AssistantMessageChunk::Thought { block, .. }) =
            message.chunks.last()
            && self.last_thought_source_version != Some(block.source_version())
        {
            let ix = message.chunks.len() - 1;
            let handle = self.scroll_handles_by_chunk_index.entry(ix).or_default();
            handle.scroll_to_bottom();
            self.last_thought_source_version = Some(block.source_version());
        }
    }
}

#[derive(Debug)]
pub struct ToolCallEntry {
    content: HashMap<EntityId, AnyEntity>,
    patch_hunk_ids: HashSet<EntityId>,
    focus_handle: FocusHandle,
}

#[derive(Debug)]
pub enum Entry {
    UserMessage {
        editor: Entity<MessageEditor>,
        synced_source_version: acp_thread::MessageContentVersion,
    },
    AssistantMessage(AssistantMessageEntry),
    ToolCall(ToolCallEntry),
    Elicitation {
        focus_handle: FocusHandle,
    },
    ContextCompaction,
}

impl Entry {
    pub fn focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        match self {
            Self::UserMessage { editor, .. } => Some(editor.read(cx).focus_handle(cx)),
            Self::AssistantMessage(message) => Some(message.focus_handle.clone()),
            Self::ToolCall(tool_call) => Some(tool_call.focus_handle.clone()),
            Self::Elicitation { focus_handle } => Some(focus_handle.clone()),
            Self::ContextCompaction => None,
        }
    }

    pub fn message_editor(&self) -> Option<&Entity<MessageEditor>> {
        match self {
            Self::UserMessage { editor, .. } => Some(editor),
            Self::AssistantMessage(_)
            | Self::ToolCall(_)
            | Self::Elicitation { .. }
            | Self::ContextCompaction => None,
        }
    }

    pub fn editor_for_diff(&self, diff: &Entity<acp_thread::Diff>) -> Option<Entity<Editor>> {
        self.content_map()?
            .get(&diff.entity_id())
            .cloned()
            .and_then(|entity| entity.downcast::<Editor>().ok())
    }

    pub fn editor_for_patch_hunk(&self, buffer: &Entity<MultiBuffer>) -> Option<Entity<Editor>> {
        self.content_map()?
            .get(&buffer.entity_id())
            .cloned()
            .and_then(|entity| entity.downcast::<Editor>().ok())
    }

    pub fn terminal(
        &self,
        terminal: &Entity<acp_thread::Terminal>,
    ) -> Option<Entity<TerminalView>> {
        self.content_map()?
            .get(&terminal.entity_id())
            .cloned()
            .and_then(|entity| entity.downcast::<TerminalView>().ok())
    }

    pub fn scroll_handle_for_assistant_message_chunk(
        &self,
        chunk_ix: usize,
    ) -> Option<ScrollHandle> {
        match self {
            Self::AssistantMessage(message) => message.scroll_handle_for_chunk(chunk_ix),
            Self::UserMessage { .. }
            | Self::ToolCall(_)
            | Self::Elicitation { .. }
            | Self::ContextCompaction => None,
        }
    }

    fn content_map(&self) -> Option<&HashMap<EntityId, AnyEntity>> {
        match self {
            Self::ToolCall(ToolCallEntry { content, .. }) => Some(content),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn has_content(&self) -> bool {
        match self {
            Self::ToolCall(ToolCallEntry { content, .. }) => !content.is_empty(),
            Self::UserMessage { .. }
            | Self::AssistantMessage(_)
            | Self::Elicitation { .. }
            | Self::ContextCompaction => false,
        }
    }
}

impl Focusable for ToolCallEntry {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Focusable for Entry {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::UserMessage { editor, .. } => editor.read(cx).focus_handle(cx),
            Self::AssistantMessage(message) => message.focus_handle.clone(),
            Self::ToolCall(tool_call) => tool_call.focus_handle.clone(),
            Self::Elicitation { focus_handle } => focus_handle.clone(),
            Self::ContextCompaction => cx.focus_handle(),
        }
    }
}

fn create_terminal(
    workspace: WeakEntity<Workspace>,
    project: WeakEntity<Project>,
    terminal: Entity<acp_thread::Terminal>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalView> {
    cx.new(|cx| {
        let read_only = !terminal.read(cx).is_process_backed();
        let mut view = TerminalView::new(
            terminal.read(cx).inner().clone(),
            workspace,
            None,
            project,
            window,
            cx,
        )
        .with_read_only(read_only);

        // GPUI can't clip children to rounded corners, so the terminal has to
        // round its own background to avoid painting over the corners of the
        // tool card it sits in.
        // This matches the `rounded_md`/`rounded_b_md` on that card, which GPUI
        // doesn't expose as a value, so if the card's corner radii ever change,
        // this also needs to be updated.
        view.set_background_corner_radii(
            Some(Corners {
                bottom_left: gpui::rems(0.375),
                bottom_right: gpui::rems(0.375),
                ..Default::default()
            }),
            cx,
        );

        view.set_content_top_padding(gpui::rems(TERMINAL_CONTENT_TOP_PADDING), cx);
        view.set_max_embedded_lines(TERMINAL_MAX_INLINE_LINES, cx);
        view.set_embedded_mode(Some(1000), cx);
        view
    })
}

fn create_editor_diff(
    diff: Entity<acp_thread::Diff>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Editor> {
    create_multibuffer_diff_editor(diff.read(cx).multibuffer().clone(), window, cx)
}

fn create_multibuffer_diff_editor(
    multibuffer: Entity<MultiBuffer>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Editor> {
    cx.new(|cx| {
        let mut editor = Editor::new(
            EditorMode::Full {
                scale_ui_elements_with_buffer_font_size: false,
                show_active_line_background: false,
                sizing_behavior: SizingBehavior::SizeByContent,
            },
            multibuffer,
            None,
            window,
            cx,
        );
        editor.set_show_gutter(false, cx);
        editor.disable_diagnostics(cx);
        editor.set_max_diagnostics_severity(DiagnosticSeverity::Off, cx);
        editor.disable_expand_excerpt_buttons(cx);
        editor.set_show_vertical_scrollbar(false, cx);
        editor.set_minimap_visibility(MinimapVisibility::Disabled, window, cx);
        editor.set_soft_wrap_mode(SoftWrap::None, cx);
        editor.set_forbid_vertical_scroll(true);
        editor.set_show_indent_guides(false, cx);
        editor.set_read_only(true);
        editor.set_delegate_open_excerpts(true);
        editor.set_show_bookmarks(false, cx);
        editor.set_show_breakpoints(false, cx);
        editor.set_show_code_actions(false, cx);
        editor.set_show_git_diff_gutter(false, cx);
        editor.set_expand_all_diff_hunks(cx);
        editor.set_diff_hunk_renderer(Some(Arc::new(HiddenUnstagedDiffHunkRenderer)), cx);
        editor.set_text_style_refinement(diff_editor_text_style_refinement(cx));
        editor
    })
}

fn diff_editor_text_style_refinement(cx: &mut App) -> TextStyleRefinement {
    TextStyleRefinement {
        font_size: Some(
            TextSize::Small
                .rems(cx)
                .to_pixels(ThemeSettings::get_global(cx).agent_ui_font_size(cx))
                .into(),
        ),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::Arc;

    use acp_thread::{AgentConnection, StubAgentConnection};
    use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
    use buffer_diff::{DiffHunkStatus, DiffHunkStatusKind};
    use editor::RowInfo;
    use fs::FakeFs;
    use gpui::{AppContext as _, TestAppContext};
    use parking_lot::RwLock;

    use crate::entry_view_state::{Entry, EntryViewState};
    use crate::message_editor::SessionCapabilities;
    use multi_buffer::MultiBufferRow;
    use pretty_assertions::assert_matches;
    use project::Project;
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use workspace::{MultiWorkspace, PathList};

    #[test]
    fn test_reindex_after_removal() {
        use super::reindex_after_removal;

        // Entries before the removed range keep their index.
        assert_eq!(reindex_after_removal(0, &(2..4)), Some(0));
        assert_eq!(reindex_after_removal(1, &(2..4)), Some(1));
        // Entries inside the removed range are dropped.
        assert_eq!(reindex_after_removal(2, &(2..4)), None);
        assert_eq!(reindex_after_removal(3, &(2..4)), None);
        // Entries after the removed range slide down by its length.
        assert_eq!(reindex_after_removal(4, &(2..4)), Some(2));
        assert_eq!(reindex_after_removal(5, &(2..4)), Some(3));
        // An empty removal range leaves indices untouched.
        assert_eq!(reindex_after_removal(3, &(2..2)), Some(3));
    }

    #[gpui::test]
    async fn test_legacy_diff_sync(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/project",
            json!({
                "hello.txt": "hi world"
            }),
        )
        .await;
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());

        let tool_call = acp_v1::ToolCall::new("tool", "Tool call")
            .status(acp_v1::ToolCallStatus::InProgress)
            .content(vec![acp_v1::ToolCallContent::Diff(
                acp_v1::Diff::new("/project/hello.txt", "hello world").old_text("hi world"),
            )]);
        let connection = Rc::new(StubAgentConnection::new());
        let thread = cx
            .update(|_, cx| {
                connection.clone().new_session(
                    project.clone(),
                    PathList::new(&[Path::new(path!("/project"))]),
                    cx,
                )
            })
            .await
            .unwrap();
        let session_id = thread.update(cx, |thread, _| thread.session_id().clone());

        cx.update(|_, cx| {
            connection.send_update(session_id, acp_v1::SessionUpdate::ToolCall(tool_call), cx)
        });

        let thread_store = None;

        let view_state = cx.new(|_cx| {
            EntryViewState::new(
                workspace.downgrade(),
                project.downgrade(),
                thread_store,
                Arc::new(RwLock::new(SessionCapabilities::default())),
                "Test Agent".into(),
            )
        });

        view_state.update_in(cx, |view_state, window, cx| {
            view_state.sync_entry(0, &thread, window, cx)
        });

        let diff = thread.read_with(cx, |thread, _| {
            thread
                .entries()
                .get(0)
                .unwrap()
                .diffs()
                .next()
                .unwrap()
                .clone()
        });

        cx.run_until_parked();

        let diff_editor = view_state.read_with(cx, |view_state, _cx| {
            view_state.entry(0).unwrap().editor_for_diff(&diff).unwrap()
        });
        assert_eq!(
            diff_editor.read_with(cx, |editor, cx| editor.text(cx)),
            "hi world\nhello world"
        );
        let row_infos = diff_editor.read_with(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx);
            multibuffer
                .snapshot(cx)
                .row_infos(MultiBufferRow(0))
                .collect::<Vec<_>>()
        });
        assert_matches!(
            row_infos.as_slice(),
            [
                RowInfo {
                    multibuffer_row: Some(MultiBufferRow(0)),
                    diff_status: Some(DiffHunkStatus {
                        kind: DiffHunkStatusKind::Deleted,
                        ..
                    }),
                    ..
                },
                RowInfo {
                    multibuffer_row: Some(MultiBufferRow(1)),
                    diff_status: Some(DiffHunkStatus {
                        kind: DiffHunkStatusKind::Added,
                        ..
                    }),
                    ..
                }
            ]
        );
    }

    #[gpui::test]
    async fn test_elicitation_preserves_entry_index(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/project", json!({})).await;
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());

        let connection = Rc::new(StubAgentConnection::new());
        let thread = cx
            .update(|_, cx| {
                connection.clone().new_session(
                    project.clone(),
                    PathList::new(&[Path::new(path!("/project"))]),
                    cx,
                )
            })
            .await
            .unwrap();
        let session_id = thread.update(cx, |thread, _| thread.session_id().clone());

        let _response_task = thread.update(cx, |thread, cx| {
            thread
                .request_elicitation(
                    acp_v2::CreateElicitationRequest::new(
                        acp_v2::ElicitationFormMode::new(
                            acp_v2::ElicitationSessionScope::new(session_id.clone()),
                            acp_v2::ElicitationSchema::new().string("name", true),
                        ),
                        "Provide a name",
                    ),
                    cx,
                )
                .unwrap()
        });
        thread.update(cx, |thread, cx| {
            thread
                .append_message_chunk(
                    acp_thread::MessageKind::Assistant,
                    acp_v2::ContentChunk::new("hello".into(), "after-elicitation"),
                    cx,
                )
                .expect("append current message after elicitation");
        });

        let view_state = cx.new(|_cx| {
            EntryViewState::new(
                workspace.downgrade(),
                project.downgrade(),
                None,
                Arc::new(RwLock::new(SessionCapabilities::default())),
                "Test Agent".into(),
            )
        });

        view_state.update_in(cx, |view_state, window, cx| {
            view_state.sync_entry(0, &thread, window, cx);
            view_state.sync_entry(1, &thread, window, cx);
        });

        view_state.read_with(cx, |view_state, _cx| {
            assert!(matches!(
                view_state.entry(0),
                Some(Entry::Elicitation { .. })
            ));
            assert!(matches!(
                view_state.entry(1),
                Some(Entry::AssistantMessage(_))
            ));
        });
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut settings_store = SettingsStore::test(cx);
            settings_store.register_setting::<feature_flags::FeatureFlagsSettings>();
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
        });
    }
}
