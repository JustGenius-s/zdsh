use crate::{
    DEFAULT_THREAD_TITLE, SelectPermissionGranularity,
    conversation_view::thread_search_bar::{ThreadSearchBar, ThreadSearchBarEvent},
    entry_view_state::{TERMINAL_CONTENT_TOP_PADDING, TERMINAL_MAX_INLINE_LINES},
    open_abs_path_at_point, project_path_for_file_link,
    thread_metadata_store::{ThreadId, ThreadMetadataStore},
};
use agent_client_protocol::schema::v2 as acp_v2;
use std::{
    cell::{Cell, RefCell},
    path::Path,
};

use acp_thread::{
    AgentModelId, AgentModelInfo, AgentModelList, Elicitation, ElicitationEntryId,
    ElicitationStatus, ForegroundActivity, SandboxAuthorizationDetails,
    SandboxFallbackAuthorizationDetails, SandboxNotAppliedReason, SubmissionId, SubmissionResponse,
    SubmissionState, ToolCallLocation, decode_path_escapes,
};
use agent::{
    SandboxStatusKey, SandboxStatusRefresh, SkillLoadingIssue, SkillLoadingIssueKind,
    SkillLoadingIssuesUpdated, ThreadSandbox, VerifiedSandboxStatus,
};
use agent_settings::AgentPermissions;
use agent_skills::MAX_SKILL_DESCRIPTION_LEN;
use cloud_api_types::{SubmitAgentThreadFeedbackBody, SubmitAgentThreadFeedbackCommentsBody};
use editor::actions::OpenExcerpts;
use sandbox::{SandboxFsPolicy, SandboxPolicy};

use crate::completion_provider::{AvailableSkill, PromptLocalCommand, pluralize};
use crate::message_editor::SharedSessionCapabilities;
use crate::unicode_confusables;
use tool_calls::TerminalSandboxWarning;

use db::kvp::KeyValueStore;
use gpui::List;
use gpui::Stateful;
use gpui::TaskExt;
use gpui_kit::component::{
    Icon as KitIcon, Root as KitRoot, Side, Sizable as _, Size as KitSize, Theme as KitTheme,
    WindowExt as _,
    alert::AlertVariant,
    button::{Button as KitButton, ButtonRounded, ButtonVariant, ButtonVariants as _},
    clipboard::Clipboard as KitClipboard,
    menu::{PopupMenu, PopupMenuItem},
    notification::Notification,
    popover::Popover,
    separator::Separator,
    shimmer::ShimmerText,
    switch::Switch as KitSwitch,
};
use heapless::Vec as ArrayVec;
use itertools::Itertools;
use language_model::{
    FastModeConfirmation, LanguageModel, LanguageModelEffortLevel, LanguageModelId,
    LanguageModelProvider, LanguageModelProviderId, LanguageModelRegistry, Speed,
};
use project::ResolvedPath;
use settings::{update_settings_file, update_settings_file_with_completion};
use ui::{ButtonLike, Checkbox, SplitButton, SplitButtonStyle, Tab, ToggleState};
use url::Url;
use util::markdown::source_position_from_fragment;
use workspace::{OpenOptions, SERIALIZATION_THROTTLE_TIME};

use super::elicitation::{
    ElicitationCard, ElicitationCardHandlers, ElicitationFormState, should_render_elicitation,
};
use super::*;
use composer::PillButton;
use transcript::AlertCard;

mod activity_bar;
mod composer;
mod tool_calls;
mod transcript;

const DATA_RETENTION_LEARN_MORE_URL: &str = "https://support.claude.com/en/articles/15425996-data-retention-practices-for-mythos-class-models";

#[derive(Default)]
struct ThreadFeedbackState {
    feedback: Option<ThreadFeedback>,
    comments_editor: Option<Entity<Editor>>,
}

impl ThreadFeedbackState {
    pub fn submit(
        &mut self,
        thread: Entity<AcpThread>,
        feedback: ThreadFeedback,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(telemetry) = thread.read(cx).connection().telemetry() else {
            return;
        };

        let project = thread.read(cx).project().read(cx);
        let client = project.client();
        let user_store = project.user_store();
        let organization = user_store.read(cx).current_organization();

        if self.feedback == Some(feedback) {
            return;
        }

        self.feedback = Some(feedback);
        match feedback {
            ThreadFeedback::Positive => {
                self.comments_editor = None;
            }
            ThreadFeedback::Negative => {
                self.comments_editor = Some(Self::build_feedback_comments_editor(window, cx));
            }
        }
        let session_id = thread.read(cx).session_id().clone();
        let parent_session_id = thread.read(cx).parent_session_id().cloned();
        let agent_telemetry_id = thread.read(cx).connection().telemetry_id();
        let task = telemetry.thread_data(&session_id, cx);
        let rating = match feedback {
            ThreadFeedback::Positive => "positive",
            ThreadFeedback::Negative => "negative",
        };
        cx.background_spawn(async move {
            let thread = task.await?;

            client
                .cloud_client()
                .submit_agent_feedback(SubmitAgentThreadFeedbackBody {
                    organization_id: organization.map(|organization| organization.id.clone()),
                    agent: agent_telemetry_id.to_string(),
                    session_id: session_id.to_string(),
                    parent_session_id: parent_session_id.map(|id| id.to_string()),
                    rating: rating.to_string(),
                    thread,
                })
                .await?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn submit_comments(&mut self, thread: Entity<AcpThread>, cx: &mut App) {
        let Some(telemetry) = thread.read(cx).connection().telemetry() else {
            return;
        };

        let Some(comments) = self
            .comments_editor
            .as_ref()
            .map(|editor| editor.read(cx).text(cx))
            .filter(|text| !text.trim().is_empty())
        else {
            return;
        };

        self.comments_editor.take();

        let project = thread.read(cx).project().read(cx);
        let client = project.client();
        let user_store = project.user_store();
        let organization = user_store.read(cx).current_organization();

        let session_id = thread.read(cx).session_id().clone();
        let agent_telemetry_id = thread.read(cx).connection().telemetry_id();
        let task = telemetry.thread_data(&session_id, cx);
        cx.background_spawn(async move {
            let thread = task.await?;

            client
                .cloud_client()
                .submit_agent_feedback_comments(SubmitAgentThreadFeedbackCommentsBody {
                    organization_id: organization.map(|organization| organization.id.clone()),
                    agent: agent_telemetry_id.to_string(),
                    session_id: session_id.to_string(),
                    comments,
                    thread,
                })
                .await?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn clear(&mut self) {
        *self = Self::default()
    }

    pub fn dismiss_comments(&mut self) {
        self.comments_editor.take();
    }

    fn build_feedback_comments_editor(window: &mut Window, cx: &mut App) -> Entity<Editor> {
        let buffer = cx.new(|cx| {
            let empty_string = String::new();
            MultiBuffer::singleton(cx.new(|cx| Buffer::local(empty_string, cx)), cx)
        });

        let editor = cx.new(|cx| {
            let mut editor = Editor::new(
                editor::EditorMode::AutoHeight {
                    min_lines: 1,
                    max_lines: Some(4),
                },
                buffer,
                None,
                window,
                cx,
            );
            editor.set_placeholder_text(
                "What went wrong? Share your feedback so we can improve.",
                window,
                cx,
            );
            editor
        });

        editor.read(cx).focus_handle(cx).focus(window, cx);
        editor
    }
}

pub enum AcpThreadViewEvent {
    Interacted,
}

impl EventEmitter<AcpThreadViewEvent> for ThreadView {}

/// `cat -n`-style numbered code block, already stripped of its line-number
/// prefixes and ready to render. Line numbers are guaranteed to be contiguous
/// starting at `first_number`, so we only store the first number and the line
/// count rather than allocating a per-line `Vec`.
struct ParsedCatNumberedCode {
    code: String,
    first_number: u32,
    line_count: usize,
}

fn parse_cat_numbered_markdown_code_block(markdown: &str) -> Option<ParsedCatNumberedCode> {
    let (_tag, code) = parse_single_fenced_code_block(markdown)?;
    parse_cat_numbered_code(code)
}

fn parse_single_fenced_code_block(markdown: &str) -> Option<(&str, &str)> {
    let first_non_backtick = markdown.find(|character| character != '`')?;
    if first_non_backtick < 3 {
        return None;
    }

    let fence = &markdown[..first_non_backtick];
    let after_opening_fence = &markdown[first_non_backtick..];
    let tag_end = after_opening_fence.find('\n')?;
    let tag = &after_opening_fence[..tag_end];
    let after_tag = &after_opening_fence[tag_end + 1..];
    let closing_fence = format!("\n{fence}\n");
    let code = after_tag.strip_suffix(&closing_fence)?;
    Some((tag, code))
}

/// Walks `code` exactly once: for each line it validates and strips the
/// `NNN\t` prefix, then pushes the line's content into the accumulating
/// code buffer (with `\n` between lines, no trailing newline). Verifies that
/// the line numbers form a contiguous, increasing sequence.
fn parse_cat_numbered_code(code: &str) -> Option<ParsedCatNumberedCode> {
    if code.is_empty() {
        return None;
    }

    let mut output = String::with_capacity(code.len());
    let mut first_number = None;
    let mut expected_number = None;
    let mut line_count: usize = 0;
    for raw_line in code.split_inclusive('\n') {
        let line = strip_line_ending(raw_line);
        let (number, text) = parse_cat_numbered_line(line)?;
        if let Some(expected) = expected_number {
            if number != expected {
                return None;
            }
        } else {
            first_number = Some(number);
        }
        expected_number = number.checked_add(1);
        if line_count > 0 {
            output.push('\n');
        }
        output.push_str(text);
        line_count += 1;
    }

    Some(ParsedCatNumberedCode {
        code: output,
        first_number: first_number?,
        line_count,
    })
}

fn strip_line_ending(line: &str) -> &str {
    let without_lf = line.strip_suffix('\n').unwrap_or(line);
    without_lf.strip_suffix('\r').unwrap_or(without_lf)
}

fn parse_cat_numbered_line(line: &str) -> Option<(u32, &str)> {
    let (prefix, text) = line.split_once('\t')?;
    let number = prefix.trim();
    if number.is_empty()
        || !prefix
            .chars()
            .all(|character| character == ' ' || character.is_ascii_digit())
    {
        return None;
    }

    Some((number.parse().ok()?, text))
}

fn render_cat_numbered_code_block(
    parsed: ParsedCatNumberedCode,
    language: Option<Arc<Language>>,
    markdown_style: MarkdownStyle,
    copy_button_id: String,
    cx: &App,
) -> AnyElement {
    use std::fmt::Write as _;

    let ParsedCatNumberedCode {
        code,
        first_number,
        line_count,
    } = parsed;

    // Line numbers are contiguous (verified during parsing), so the largest
    // line number is `first_number + line_count - 1`. Sizing the gutter to
    // that number's digit count means every rendered line contributes exactly
    // `gutter_width` bytes to the gutter, plus a newline between adjacent
    // lines.
    let last_number = first_number
        .saturating_add(u32::try_from(line_count.saturating_sub(1)).unwrap_or(u32::MAX));
    let gutter_width = last_number.to_string().len().max(1);
    let gutter_capacity = line_count * gutter_width + line_count.saturating_sub(1);

    let mut gutter = String::with_capacity(gutter_capacity);
    for i in 0..line_count {
        if i > 0 {
            gutter.push('\n');
        }
        let line_number = first_number.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
        // Writes to a `String` are infallible, so the `Result` can be ignored.
        let _ = write!(&mut gutter, "{line_number:>gutter_width$}");
    }

    let mut code_text_style = markdown_style.base_text_style.clone();
    code_text_style.refine(&markdown_style.code_block.text);

    let mut gutter_text_style = code_text_style.clone();
    gutter_text_style.color = cx.theme().colors().text_muted;

    let gutter_len = gutter.len();
    let gutter = StyledText::new(gutter).with_runs(vec![gutter_text_style.to_run(gutter_len)]);

    // Share `code` between syntax highlighting, the rendered `StyledText`, and
    // the copy button via a single `SharedString` (cheap `Arc` clones) instead
    // of cloning the underlying `String`.
    let code: SharedString = code.into();
    let code_runs = highlight_code_runs(&code, language.as_ref(), code_text_style, &markdown_style);
    let code_text = StyledText::new(code.clone()).with_runs(code_runs);

    let code_block_id = format!("read-file-code-block-{copy_button_id}");
    let code_scroll_id = format!("read-file-code-scroll-{copy_button_id}");
    let mut container = div()
        .id(code_block_id)
        .group("read-file-code-block")
        .relative()
        .w_full()
        .whitespace_nowrap();
    container.style().refine(&markdown_style.code_block);

    // `overflow_x_scroll` only actually scrolls when the container is laid out
    // as a flex container: in GPUI the default `Display` is `Block`, and a
    // block-level child fills its parent's content width instead of overflowing
    // it, so there is nothing for the scroll viewport to scroll. Using `flex()`
    // on the scroll wrapper plus `flex_none()` on the inner item lets the inner
    // item take its natural width (the unwrapped code), which is what overflows.
    // `restrict_scroll_to_axis` then keeps vertical wheel events flowing through
    // to the outer thread scroller. This mirrors the standard markdown
    // code-block path in `crates/markdown/src/markdown.rs`.
    let code_scroll = div()
        .id(code_scroll_id)
        .flex()
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .child(div().flex_none().child(code_text));

    container
        .child(
            h_flex()
                .items_start()
                .min_w_0()
                .w_full()
                .child(div().flex_none().pr_3().child(gutter))
                .child(code_scroll),
        )
        .child(
            h_flex()
                .w_4()
                .absolute()
                .top_0()
                .right_0()
                .justify_end()
                .visible_on_hover("read-file-code-block")
                .child(CopyButton::new(copy_button_id, code).tooltip_label("Copy Code")),
        )
        .into_any_element()
}

fn highlight_code_runs(
    code: &str,
    language: Option<&Arc<Language>>,
    code_text_style: TextStyle,
    markdown_style: &MarkdownStyle,
) -> Vec<TextRun> {
    if code.is_empty() {
        return Vec::new();
    }

    let Some(language) = language else {
        return vec![code_text_style.to_run(code.len())];
    };

    let mut runs = Vec::new();
    let mut offset = 0;
    for (range, highlight_id) in language.highlight_text(code, 0..code.len()) {
        if range.start > offset {
            runs.push(code_text_style.to_run(range.start - offset));
        }

        let mut run_style = code_text_style.clone();
        if let Some(highlight) = markdown_style.syntax.get(highlight_id).cloned() {
            run_style = run_style.highlight(highlight);
        }
        runs.push(run_style.to_run(range.len()));
        offset = range.end;
    }

    if offset < code.len() {
        runs.push(code_text_style.to_run(code.len() - offset));
    }

    runs
}

#[cfg(test)]
mod numbered_code_block_tests {
    use super::*;

    #[test]
    fn parses_cat_numbered_markdown_code_block() {
        let parsed = parse_cat_numbered_markdown_code_block(
            "```rs zed/crates/example.rs\n     2\tfn main() {\n     3\t    println!(\"hi\");\n     4\t}\n```\n",
        )
        .expect("cat-numbered block should parse");

        assert_eq!(parsed.line_count, 3);
        assert_eq!(parsed.first_number, 2);
        assert_eq!(parsed.code, "fn main() {\n    println!(\"hi\");\n}");
    }

    #[test]
    fn parses_cat_numbered_code_with_crlf_line_endings() {
        let parsed = parse_cat_numbered_code("     1\tline one\r\n     2\tline two\r\n")
            .expect("crlf-terminated cat-numbered code should parse");

        assert_eq!(parsed.line_count, 2);
        assert_eq!(parsed.first_number, 1);
        assert_eq!(parsed.code, "line one\nline two");
    }

    #[test]
    fn rejects_non_cat_numbered_code_block() {
        assert!(parse_cat_numbered_markdown_code_block("```rs\nfn main() {}\n```\n").is_none());
    }

    #[test]
    fn rejects_non_contiguous_cat_numbers() {
        assert!(
            parse_cat_numbered_markdown_code_block(
                "```rs\n     2\tlet a = 1;\n     4\tlet b = 2;\n```\n"
            )
            .is_none()
        );
    }
}

/// Tracks the user's permission dropdown selection state for a specific request.
///
/// Default (no entry in the map) means the last dropdown choice is selected,
/// which is typically "Only this time".
#[derive(Clone)]
pub(crate) enum PermissionSelection {
    /// A specific choice from the dropdown (e.g., "Always for terminal", "Only this time").
    /// The index corresponds to the position in the `choices` list from `PermissionOptions`.
    Choice(usize),
    /// "Select options…" mode where individual command patterns can be toggled.
    /// Contains the indices of checked patterns in the `patterns` list.
    /// All patterns start checked when this mode is first activated.
    SelectedPatterns(Vec<usize>),
}

impl PermissionSelection {
    /// Returns the choice index if a specific dropdown choice is selected,
    /// or `None` if in per-command pattern mode.
    pub(crate) fn choice_index(&self) -> Option<usize> {
        match self {
            Self::Choice(index) => Some(*index),
            Self::SelectedPatterns(_) => None,
        }
    }

    fn is_pattern_checked(&self, index: usize) -> bool {
        match self {
            Self::SelectedPatterns(checked) => checked.contains(&index),
            _ => false,
        }
    }

    fn has_any_checked_patterns(&self) -> bool {
        match self {
            Self::SelectedPatterns(checked) => !checked.is_empty(),
            _ => false,
        }
    }

    pub(super) fn toggle_pattern(&mut self, index: usize) {
        if let Self::SelectedPatterns(checked) = self {
            if let Some(pos) = checked.iter().position(|&i| i == index) {
                checked.swap_remove(pos);
            } else {
                checked.push(index);
            }
        }
    }
}

pub struct ThreadView {
    pub(crate) root_thread_id: ThreadId,
    pub session_id: acp_v2::SessionId,
    pub parent_session_id: Option<acp_v2::SessionId>,
    pub thread: Entity<AcpThread>,
    pub(crate) conversation: Entity<super::Conversation>,
    pub server_view: WeakEntity<ConversationView>,
    pub agent_icon: IconName,
    pub agent_icon_from_external_svg: Option<SharedString>,
    pub agent_id: AgentId,
    pub agent_display_name: SharedString,
    pub focus_handle: FocusHandle,
    pub workspace: WeakEntity<Workspace>,
    pub entry_view_state: Entity<EntryViewState>,
    pub title_editor: Entity<Editor>,
    title_editor_sync_version: Option<(gpui::EntityId, clock::Global)>,
    pub config_options_view: Option<Entity<ConfigOptionsView>>,
    pub mode_selector: Option<Entity<ModeSelector>>,
    pub model_selector: Option<Entity<ModelSelectorPopover>>,
    pub profile_selector: Option<Entity<ProfileSelector>>,
    pub permission_dropdown_handle: PopoverMenuHandle<ContextMenu>,
    pub thread_retry_status: Option<RetryStatus>,
    pub(super) thread_error: Option<ThreadError>,
    pub thread_error_markdown: Option<Entity<Markdown>>,
    pub token_limit_callout_dismissed: bool,
    pub last_token_limit_telemetry: Option<acp_thread::TokenUsageRatio>,
    thread_feedback: ThreadFeedbackState,
    pub list_state: ListState,
    /// Whether the last render showed the transcript as scrolled away from the latest entry, so
    /// the scroll handler only re-renders when the jump-to-latest overlay has to change.
    transcript_scrolled_up: bool,
    pub session_capabilities: SharedSessionCapabilities,
    pub expanded_tool_call_raw_inputs: HashSet<acp_v2::ToolCallId>,
    collapsed_sandbox_authorization_details: HashSet<acp_v2::ToolCallId>,
    /// Sandbox escalation prompts whose "surprising Unicode" warning the user
    /// has explicitly acknowledged. Until a prompt's tool call is in this set,
    /// its allow buttons stay disabled. See [`Self::sandbox_confusable_findings`].
    acknowledged_confusable_warnings: HashSet<acp_v2::ToolCallId>,
    pub subagent_scroll_handles: RefCell<HashMap<acp_v2::SessionId, ScrollHandle>>,
    pub edits_expanded: bool,
    branch_chip_hovered: bool,
    pub plan_expanded: bool,
    pub queue_expanded: bool,
    pub editor_expanded: bool,
    pub should_be_following: bool,
    pub editing_message: Option<usize>,
    pub message_queue: MessageQueue,
    pub turn_fields: TurnFields,
    pub discarded_partial_edits: HashSet<acp_v2::ToolCallId>,
    pub is_loading_contents: bool,
    pub new_server_version_available: Option<SharedString>,
    pub resumed_without_history: bool,
    elicitation_form_states: HashMap<ElicitationEntryId, ElicitationFormState>,
    pub _cancel_task: Option<Task<()>>,
    _save_task: Option<Task<()>>,
    _draft_resolve_task: Option<Task<()>>,
    _sandbox_status_refresh_task: Option<Task<()>>,
    pub current_submission: Option<SubmissionId>,
    pub _subscriptions: Vec<Subscription>,
    pub message_editor: Entity<MessageEditor>,
    empty_thread_placeholder: String,
    placeholder_shows_follow_up: Option<bool>,
    add_menu: Option<Entity<gpui_kit::component::list::ListState<composer::AddMenuDelegate>>>,
    _add_menu_subscriptions: Vec<Subscription>,
    composer_bounds: Rc<Cell<Option<gpui::Bounds<Pixels>>>>,
    context_usage_expanded: bool,
    cursor_usage_expanded: bool,
    /// Whether the docked composer puts its controls on a row below the input, as it does once
    /// the input takes more than one line.
    composer_stacked: bool,
    pub thinking_effort_menu_handle: PopoverMenuHandle<ContextMenu>,
    model_menu: Option<Entity<PopupMenu>>,
    _model_menu_subscription: Option<Subscription>,
    /// The permissions menu, or the confirmation that replaces it when switching to full access.
    permissions_menu: Option<Entity<PopupMenu>>,
    _permissions_menu_subscription: Option<Subscription>,
    pub fast_mode_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub project: WeakEntity<Project>,
    /// Cache + worktree snapshot for resolving paths in markdown code spans.
    /// Cloned from the parent `ConversationView` so the cache is shared and the
    /// snapshot stays in sync via the parent's project-event subscription.
    pub(crate) code_span_resolver: AgentCodeSpanResolver,
    pub show_external_source_prompt_warning: bool,
    pub show_codex_windows_warning: bool,
    sandbox_status: Option<VerifiedSandboxStatus>,
    sandbox_status_key: Option<SandboxStatusKey>,
    pending_sandbox_status_key: Option<SandboxStatusKey>,
    pub multi_root_callout_dismissed: bool,
    pub generating_indicator_in_list: bool,
    pub skill_loading_issues: Vec<SkillLoadingIssue>,
    /// Issues the user has explicitly dismissed. Each entry is matched against
    /// emitted issues by full equality; when an issue no longer appears in the
    /// latest replacement list (because the underlying file was fixed/removed), it's
    /// dropped from this set so a future regression of the same kind would
    /// re-show.
    dismissed_skill_loading_issues: HashSet<SkillLoadingIssue>,
    pub(crate) thread_search_bar: Option<Entity<super::thread_search_bar::ThreadSearchBar>>,
    pub(crate) thread_search_visible: bool,
}
impl Focusable for ThreadView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ThreadView {
    pub(crate) fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        if self.parent_session_id.is_some() {
            self.focus_handle.clone()
        } else {
            self.active_editor(cx).focus_handle(cx)
        }
    }
}

#[derive(Default)]
pub struct TurnFields {
    pub _turn_timer_task: Option<Task<()>>,
    pub last_turn_duration: Option<Duration>,
    pub last_turn_tokens: Option<u64>,
    pub turn_generation: usize,
    pub turn_started_at: Option<Instant>,
    pub turn_tokens: Option<u64>,
    pub reported_activity_generation: Option<u64>,
}

/// How a tool call is rendered relative to its surroundings.
///
/// `Standalone` draws its own border/margin/location header. `Grouped` is like
/// `Standalone`, for a tool call shown in an expanded activity group, closer
/// to the rest of the group. `Embedded` is hosted by a container that provides
/// its own framing (e.g. the subagent card). `Floating` is like `Embedded`, but
/// used for the floating awaiting-permission row above the message editor: the
/// tool call's content is height-capped and scrollable so the row can never
/// grow to consume the entire panel and squeeze the conversation list out of
/// view.
#[derive(Copy, Clone, PartialEq, Eq)]
enum ToolCallLayout {
    Standalone,
    Grouped,
    Embedded,
    Floating,
}

impl ToolCallLayout {
    /// Stable discriminant used to disambiguate element ids when the same tool
    /// call is rendered in more than one layout at once (e.g. inline in the
    /// list *and* in the floating awaiting-permission row).
    fn id_str(self) -> &'static str {
        match self {
            // A tool call is in one or the other, and keeping its ids as it joins a group keeps
            // the state of what it shows, such as a terminal's scroll position.
            ToolCallLayout::Standalone | ToolCallLayout::Grouped => "standalone",
            ToolCallLayout::Embedded => "embedded",
            ToolCallLayout::Floating => "floating",
        }
    }

    /// The gap above a tool call that is a row of the transcript, rather than part of a
    /// container.
    fn row_gap(self) -> Option<Pixels> {
        match self {
            ToolCallLayout::Standalone => Some(transcript::ROW_GAP),
            ToolCallLayout::Grouped => Some(transcript::STEP_GAP),
            ToolCallLayout::Embedded | ToolCallLayout::Floating => None,
        }
    }
}

fn full_path_for_empty_project_path(file: &dyn language::File, cx: &App) -> Option<String> {
    if file.path().file_name().is_some() {
        return None;
    }

    let full_path = file.full_path(cx).display().to_string();
    (!full_path.is_empty()).then_some(full_path)
}

fn skill_issue_file_label(path: &std::path::Path) -> String {
    let file_name = path.file_name().and_then(|name| name.to_str());
    let parent_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());

    match (parent_name, file_name) {
        (Some(parent_name), Some(file_name)) => format!("{parent_name}/{file_name}"),
        (_, Some(file_name)) => file_name.to_string(),
        _ => path.display().to_string(),
    }
}

pub fn open_markdown_in_workspace(
    title: String,
    markdown: String,
    workspace: Entity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<()>> {
    let markdown_language_task = workspace
        .read(cx)
        .app_state()
        .languages
        .language_for_name("Markdown");
    let project = workspace.read(cx).project().clone();

    window.spawn(cx, async move |cx| {
        let markdown_language = markdown_language_task.await?;

        let buffer = project
            .update(cx, |project, cx| {
                project.create_buffer(Some(markdown_language), false, cx)
            })
            .await?;

        buffer.update(cx, |buffer, cx| {
            buffer.set_text(markdown, cx);
            buffer.set_capability(language::Capability::ReadWrite, cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx).with_title(title.clone()));

            workspace.add_item_to_active_pane(
                Box::new(cx.new(|cx| {
                    let mut editor =
                        Editor::for_multibuffer(buffer, Some(project.clone()), window, cx);
                    editor.set_breadcrumb_header(title);
                    editor.disable_mouse_wheel_zoom();
                    editor
                })),
                None,
                true,
                window,
                cx,
            );
        })?;
        anyhow::Ok(())
    })
}

impl ThreadView {
    pub(crate) fn new(
        root_thread_id: ThreadId,
        thread: Entity<AcpThread>,
        conversation: Entity<super::Conversation>,
        server_view: WeakEntity<ConversationView>,
        agent_icon: IconName,
        agent_icon_from_external_svg: Option<SharedString>,
        agent_id: AgentId,
        agent_display_name: SharedString,
        workspace: WeakEntity<Workspace>,
        entry_view_state: Entity<EntryViewState>,
        config_options_view: Option<Entity<ConfigOptionsView>>,
        mode_selector: Option<Entity<ModeSelector>>,
        model_selector: Option<Entity<ModelSelectorPopover>>,
        profile_selector: Option<Entity<ProfileSelector>>,
        list_state: ListState,
        session_capabilities: SharedSessionCapabilities,
        resumed_without_history: bool,
        project: WeakEntity<Project>,
        code_span_resolver: AgentCodeSpanResolver,
        thread_store: Option<Entity<ThreadStore>>,
        initial_content: Option<AgentInitialContent>,
        mut subscriptions: Vec<Subscription>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_id = thread.read(cx).session_id().clone();
        let parent_session_id = thread.read(cx).parent_session_id().cloned();

        subscriptions.push(cx.observe(&thread, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(&conversation, |_, _, cx| cx.notify()));

        let has_slash_completions = session_capabilities.read().has_slash_completions();
        let placeholder = placeholder_text(agent_display_name.as_ref(), has_slash_completions);

        let mut should_auto_submit = false;
        let mut show_external_source_prompt_warning = false;

        let message_editor = cx.new(|cx| {
            let mut editor = MessageEditor::new(
                workspace.clone(),
                project.clone(),
                thread_store,
                session_capabilities.clone(),
                agent_id.clone(),
                &placeholder,
                editor::EditorMode::AutoHeight {
                    min_lines: AgentSettings::get_global(cx).message_editor_min_lines,
                    max_lines: Some(AgentSettings::get_global(cx).set_message_editor_max_lines()),
                },
                window,
                cx,
            );
            let content_blocks = if let Some(content) = initial_content {
                match content {
                    AgentInitialContent::ThreadSummary { session_id, title } => {
                        editor.insert_thread_summary(session_id, title, window, cx);
                        None
                    }
                    AgentInitialContent::ContentBlock {
                        blocks,
                        auto_submit,
                    } => {
                        should_auto_submit = auto_submit;
                        Some(blocks)
                    }
                    AgentInitialContent::FromExternalSource(prompt) => {
                        show_external_source_prompt_warning = true;
                        // SECURITY: Be explicit about not auto submitting prompt from external source.
                        should_auto_submit = false;
                        Some(vec![acp_v2::ContentBlock::Text(acp_v2::TextContent::new(
                            prompt.into_string(),
                        ))])
                    }
                }
            } else {
                thread.read(cx).draft_prompt().map(|draft| draft.to_vec())
            };
            if let Some(blocks) = content_blocks {
                if blocks.iter().all(acp_thread::content::can_convert_to_v1) {
                    editor.set_message(blocks, window, cx);
                } else {
                    should_auto_submit = false;
                    thread.update(cx, |thread, cx| {
                        thread.set_draft_prompt(Some(blocks.clone()), cx);
                    });
                    editor.set_read_only(true, cx);
                    editor.set_source_message(blocks, window, cx);
                }
            }
            // The composer draws the `@` and `/` menus itself, as a card beside it.
            editor.editor().update(cx, |editor, _| {
                editor.set_completions_menu_drawn_externally(true);
            });
            editor
        });

        let show_codex_windows_warning = cfg!(windows)
            && project.upgrade().is_some_and(|p| p.read(cx).is_local())
            && agent_id.as_ref() == "Codex";

        if let Some(project) = project.upgrade() {
            subscriptions.push(cx.subscribe(&project, {
                let resolver = code_span_resolver.clone();
                move |_this: &mut Self, _project, event: &project::Event, cx| {
                    if matches!(
                        event,
                        project::Event::WorktreeAdded(_)
                            | project::Event::WorktreeRemoved(_)
                            | project::Event::WorktreeUpdatedEntries(_, _)
                    ) {
                        resolver.clear_cache();
                        cx.notify();
                    }
                }
            }));
        }

        let (title_editor, title_editor_sync_version) = {
            let metadata = ThreadMetadataStore::try_global(cx)
                .and_then(|store| store.read(cx).entry(root_thread_id).cloned());
            let initial_title = if parent_session_id.is_none() {
                metadata
                    .as_ref()
                    .and_then(|metadata| metadata.title_override.clone())
            } else {
                None
            }
            .or_else(|| thread.read(cx).title())
            .unwrap_or_else(|| DEFAULT_THREAD_TITLE.into());
            let editor = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_text(initial_title, window, cx);
                editor
            });
            let version = Self::title_editor_version(editor.read(cx), cx);
            subscriptions.push(cx.subscribe_in(&editor, window, Self::handle_title_editor_event));
            (editor, version)
        };

        subscriptions.push(cx.subscribe_in(
            &entry_view_state,
            window,
            Self::handle_entry_view_event,
        ));

        subscriptions.push(cx.subscribe_in(
            &message_editor,
            window,
            Self::handle_message_editor_event,
        ));

        // If this thread is backed by a NativeAgent, listen for skill loading
        // issues so we can surface them as banners. The agent emits a single
        // replacement-style event per project refresh, so we overwrite our
        // local list rather than appending — this also clears stale issues
        // once a user resolves them.
        if let Some(native_connection) = thread
            .read(cx)
            .connection()
            .clone()
            .downcast::<agent::NativeAgentConnection>()
        {
            let project_id = thread.read(cx).project().entity_id();
            subscriptions.push(cx.subscribe(
                &native_connection.0,
                move |this: &mut Self, _agent, event: &SkillLoadingIssuesUpdated, cx| {
                    if event.project_id != project_id {
                        return;
                    }
                    // Drop dismissals for issues that no longer appear in the emitted
                    // list — the underlying file must have been fixed or removed, so a
                    // future regression should re-show.
                    this.dismissed_skill_loading_issues
                        .retain(|dismissed| event.issues.contains(dismissed));

                    // Show only issues that haven't been dismissed.
                    this.skill_loading_issues = event
                        .issues
                        .iter()
                        .filter(|issue| !this.dismissed_skill_loading_issues.contains(issue))
                        .cloned()
                        .collect();
                    cx.notify();
                },
            ));

            // A "no model selected" error is stale as soon as the thread has a
            // usable model
            if let Some(native_thread) = native_connection.thread(thread.read(cx).session_id(), cx)
            {
                subscriptions.push(cx.subscribe(
                    &native_thread,
                    |this: &mut Self, _thread, _event: &agent::ModelChanged, cx| {
                        if matches!(this.thread_error, Some(ThreadError::NoModelSelected)) {
                            this.clear_thread_error(cx);
                        }
                    },
                ));
            }
        }

        if let Some(cursor_state) = cursor_agent::State::global(cx) {
            subscriptions.push(cx.observe(&cursor_state, |_, _, cx| cx.notify()));
        }
        subscriptions.push(cx.subscribe(
            &thread,
            |this, _, event: &acp_thread::AcpThreadEvent, cx| {
                if matches!(event, acp_thread::AcpThreadEvent::Stopped { .. }) {
                    this.refresh_cursor_usage_after_turn(cx);
                }
            },
        ));

        subscriptions.push(cx.observe(&message_editor, |this, editor, cx| {
            if editor.read(cx).editor().read(cx).read_only(cx) {
                this._draft_resolve_task.take();
                return;
            }
            let is_empty = editor.read(cx).text(cx).is_empty();
            let draft_contents_task = if is_empty {
                None
            } else {
                Some(editor.update(cx, |editor, cx| editor.draft_contents(cx)))
            };
            this._draft_resolve_task = Some(cx.spawn(async move |this, cx| {
                let draft = if let Some(task) = draft_contents_task {
                    let blocks = task.await.ok().filter(|b| !b.is_empty());
                    blocks
                } else {
                    None
                };
                this.update(cx, |this, cx| {
                    if this.message_editor.read(cx).editor().read(cx).read_only(cx) {
                        return;
                    }
                    this.thread.update(cx, |thread, cx| {
                        thread.set_draft_prompt(draft, cx);
                    });
                    this.schedule_save(cx);
                })
                .ok();
            }));
        }));

        let current_submission = thread.read(cx).latest_submission_id();
        let mut this = Self {
            root_thread_id,
            session_id,
            parent_session_id,
            focus_handle: cx.focus_handle(),
            thread,
            conversation,
            server_view,
            agent_icon,
            agent_icon_from_external_svg,
            agent_id,
            agent_display_name,
            workspace,
            entry_view_state,
            title_editor,
            title_editor_sync_version,
            config_options_view,
            mode_selector,
            model_selector,
            profile_selector,
            list_state,
            transcript_scrolled_up: false,
            session_capabilities,
            resumed_without_history,
            _subscriptions: subscriptions,
            permission_dropdown_handle: PopoverMenuHandle::default(),
            thread_retry_status: None,
            thread_error: None,
            thread_error_markdown: None,
            token_limit_callout_dismissed: false,
            last_token_limit_telemetry: None,
            thread_feedback: Default::default(),
            expanded_tool_call_raw_inputs: HashSet::default(),
            collapsed_sandbox_authorization_details: HashSet::default(),
            acknowledged_confusable_warnings: HashSet::default(),
            subagent_scroll_handles: RefCell::new(HashMap::default()),
            edits_expanded: false,
            branch_chip_hovered: false,
            plan_expanded: false,
            queue_expanded: true,
            editor_expanded: false,
            should_be_following: false,
            editing_message: None,
            message_queue: MessageQueue::default(),
            turn_fields: TurnFields::default(),
            discarded_partial_edits: HashSet::default(),
            is_loading_contents: false,
            new_server_version_available: None,
            elicitation_form_states: HashMap::default(),
            _cancel_task: None,
            _save_task: None,
            _draft_resolve_task: None,
            _sandbox_status_refresh_task: None,
            current_submission,
            message_editor,
            add_menu: None,
            _add_menu_subscriptions: Vec::new(),
            composer_bounds: Rc::default(),
            context_usage_expanded: false,
            cursor_usage_expanded: false,
            composer_stacked: false,
            empty_thread_placeholder: placeholder,
            placeholder_shows_follow_up: None,
            thinking_effort_menu_handle: PopoverMenuHandle::default(),
            model_menu: None,
            _model_menu_subscription: None,
            permissions_menu: None,
            _permissions_menu_subscription: None,
            fast_mode_menu_handle: PopoverMenuHandle::default(),
            project,
            code_span_resolver,
            show_external_source_prompt_warning,
            show_codex_windows_warning,
            sandbox_status: None,
            sandbox_status_key: None,
            pending_sandbox_status_key: None,
            multi_root_callout_dismissed: false,
            generating_indicator_in_list: false,
            skill_loading_issues: Vec::new(),
            dismissed_skill_loading_issues: HashSet::default(),
            thread_search_bar: None,
            thread_search_visible: false,
        };

        this.sync_reported_activity(cx);
        this.sync_generating_indicator(cx);
        this.sync_editor_mode(cx);
        this.sync_placeholder(window, cx);
        this.sync_existing_elicitation_states(window, cx);
        let list_state_for_scroll = this.list_state.clone();
        let thread_view = cx.entity().downgrade();

        this.list_state
            .set_scroll_handler(move |_event, _window, cx| {
                let list_state = list_state_for_scroll.clone();
                let thread_view = thread_view.clone();
                // N.B. We must defer because the scroll handler is called while the
                // ListState's RefCell is mutably borrowed. Reading logical_scroll_top()
                // directly would panic from a double borrow.
                cx.defer(move |cx| {
                    let scroll_top = list_state.logical_scroll_top();
                    let _ = thread_view.update(cx, |this, cx| {
                        if transcript::is_scrolled_up(&list_state) != this.transcript_scrolled_up {
                            cx.notify();
                        }
                        if let Some(thread) = this.as_native_thread(cx) {
                            thread.update(cx, |thread, _cx| {
                                thread.set_ui_scroll_position(Some(scroll_top));
                            });
                        }
                        this.schedule_save(cx);
                    });
                });
            });

        if should_auto_submit {
            this.send(window, cx);
        }
        this
    }

    /// Schedule a throttled save of the thread state (draft prompt, scroll position, etc.).
    /// Multiple calls within `SERIALIZATION_THROTTLE_TIME` are coalesced into a single save.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SERIALIZATION_THROTTLE_TIME)
                .await;
            this.update(cx, |this, cx| {
                if let Some(thread) = this.as_native_thread(cx) {
                    thread.update(cx, |_thread, cx| cx.notify());
                }
            })
            .ok();
        }));
    }

    pub fn handle_message_editor_event(
        &mut self,
        _editor: &Entity<MessageEditor>,
        event: &MessageEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The three skill-watcher trigger points all live here:
        // - `Focus` fires when the user clicks into the input box.
        // - `SlashAutocompleteOpened` fires when the completion
        //   provider is asked for slash commands.
        // - `Send` fires when the user submits the conversation.
        // All three triggers are idempotent; firing the same one
        // repeatedly is a no-op once a scan or watch is active.
        if matches!(
            event,
            MessageEditorEvent::Focus
                | MessageEditorEvent::SlashAutocompleteOpened
                | MessageEditorEvent::Send
        ) {
            if let Some(connection) = self.as_native_connection(cx) {
                connection.ensure_skills_scan_started(cx);
                if let Some(project) = self.project.upgrade() {
                    connection.refresh_skills_for_project(project, cx);
                }
            }
        }

        match event {
            MessageEditorEvent::Send => self.send(window, cx),
            MessageEditorEvent::SendImmediately => self.interrupt_and_send(window, cx),
            MessageEditorEvent::Cancel => {
                if !self.close_thread_search(window, cx) {
                    self.cancel_generation(cx);
                }
            }
            MessageEditorEvent::Focus => {
                self.cancel_editing(&Default::default(), window, cx);
            }
            MessageEditorEvent::LostFocus => {}
            MessageEditorEvent::SlashAutocompleteOpened => {}
            MessageEditorEvent::LocalCommandInvoked(command) => {
                self.run_local_command(*command, window, cx);
            }
            MessageEditorEvent::InputAttempted { .. } => {}
            MessageEditorEvent::Edited => {}
        }
    }

    fn run_local_command(
        &mut self,
        command: PromptLocalCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            PromptLocalCommand::ThumbsUp => {
                self.handle_feedback_click(ThreadFeedback::Positive, window, cx);
                self.show_success_toast("Thanks for your feedback!", window, cx);
            }
            PromptLocalCommand::ThumbsDown => {
                self.handle_feedback_click(ThreadFeedback::Negative, window, cx);
            }
        }
    }

    fn show_success_toast(
        &self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        struct SuccessToast;

        // Kit notifications live in the window's Kit `Root`, which test windows don't have, and
        // pushing one without it panics.
        if window.root::<KitRoot>().flatten().is_none() {
            return;
        }
        window.push_notification(Notification::success(message).id::<SuccessToast>(), cx);
    }

    pub(crate) fn as_native_connection(
        &self,
        cx: &App,
    ) -> Option<Rc<agent::NativeAgentConnection>> {
        let acp_thread = self.thread.read(cx);
        acp_thread.connection().clone().downcast()
    }

    pub fn as_native_thread(&self, cx: &App) -> Option<Entity<agent::Thread>> {
        let acp_thread = self.thread.read(cx);
        self.as_native_connection(cx)?
            .thread(acp_thread.session_id(), cx)
    }

    /// Resolves the message editor's contents into content blocks. For profiles
    /// that do not enable any tools, directory mentions are expanded to inline
    /// file contents since the agent can't read files on its own.
    fn resolve_message_contents(
        &self,
        message_editor: &Entity<MessageEditor>,
        cx: &mut App,
    ) -> Task<Result<(Vec<acp_v2::ContentBlock>, Vec<Entity<Buffer>>)>> {
        if message_editor.read(cx).editor().read(cx).read_only(cx) {
            return Task::ready(Err(anyhow!(
                "This draft contains unsupported content and cannot be edited or sent. Discard the draft to write a new message."
            )));
        }
        let expand = self.as_native_thread(cx).is_some_and(|thread| {
            let thread = thread.read(cx);
            AgentSettings::get_global(cx)
                .profiles
                .get(thread.profile())
                .is_some_and(|profile| profile.tools.is_empty())
        });
        message_editor.update(cx, |message_editor, cx| message_editor.contents(expand, cx))
    }

    pub fn current_model_id(&self, cx: &App) -> Option<String> {
        let selector = self.model_selector.as_ref()?;
        let model = selector.read(cx).active_model(cx)?;
        Some(model.id.to_string())
    }

    pub fn current_mode_id(&self, cx: &App) -> Option<Arc<str>> {
        if let Some(thread) = self.as_native_thread(cx) {
            Some(thread.read(cx).profile().0.clone())
        } else {
            let mode_selector = self.mode_selector.as_ref()?;
            Some(mode_selector.read(cx).mode().0)
        }
    }

    fn is_subagent(&self) -> bool {
        self.parent_session_id.is_some()
    }

    fn has_visible_conversation(&self, cx: &App) -> bool {
        self.generating_indicator_in_list
            || self.thread.read(cx).entries().iter().any(|entry| {
                !matches!(entry, AgentThreadEntry::UserMessage(message)
                    if message.content.source_blocks().is_empty())
            })
    }

    pub(super) fn can_edit_user_message(&self, index: usize, cx: &App) -> bool {
        let thread = self.thread.read(cx);
        let Some(message) = thread
            .entries()
            .get(index)
            .and_then(|entry| entry.user_message())
        else {
            return false;
        };
        thread.can_rewind_to(message.client_id.as_ref(), cx)
            && !message.content.source_blocks().is_empty()
            && message
                .content
                .source_blocks()
                .iter()
                .all(acp_thread::content::can_convert_to_v1)
    }

    /// Returns the currently active editor, either for a message that is being
    /// edited or the editor for a new message.
    pub(crate) fn active_editor(&self, cx: &App) -> Entity<MessageEditor> {
        if let Some(index) = self.editing_message
            && let Some(editor) = self
                .entry_view_state
                .read(cx)
                .entry(index)
                .and_then(|entry| entry.message_editor())
                .cloned()
        {
            editor
        } else {
            self.message_editor.clone()
        }
    }

    pub fn has_queued_messages(&self) -> bool {
        !self.message_queue.is_empty()
    }

    // events

    pub fn handle_entry_view_event(
        &mut self,
        _: &Entity<EntryViewState>,
        event: &EntryViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &event.view_event {
            ViewEvent::NewDiff(tool_call_id) => {
                if AgentSettings::get_global(cx).expand_edit_card {
                    self.entry_view_state.update(cx, |state, _cx| {
                        state.expand_tool_call(tool_call_id.clone());
                    });
                }
            }
            ViewEvent::NewTerminal(tool_call_id) => {
                if AgentSettings::get_global(cx).expand_terminal_card {
                    self.entry_view_state.update(cx, |state, _cx| {
                        state.expand_tool_call(tool_call_id.clone());
                    });
                }
            }
            ViewEvent::TerminalMovedToBackground(tool_call_id) => {
                self.entry_view_state.update(cx, |state, _cx| {
                    state.collapse_tool_call(tool_call_id);
                });
            }
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::Focus) => {
                if self.can_edit_user_message(event.entry_index, cx) {
                    self.editing_message = Some(event.entry_index);
                    cx.notify();
                }
            }
            ViewEvent::MessageEditorEvent(editor, MessageEditorEvent::LostFocus) => {
                if let Some(AgentThreadEntry::UserMessage(user_message)) =
                    self.thread.read(cx).entries().get(event.entry_index)
                    && self.can_edit_user_message(event.entry_index, cx)
                {
                    if editor.read(cx).text(cx).as_str() == user_message.content.to_markdown(cx) {
                        self.editing_message = None;
                        cx.notify();
                    }
                }
            }
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::SendImmediately) => {}
            ViewEvent::MessageEditorEvent(editor, MessageEditorEvent::Send) => {
                if self.can_edit_user_message(event.entry_index, cx) {
                    self.regenerate(event.entry_index, editor.clone(), window, cx);
                }
            }
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::Cancel) => {
                self.cancel_editing(&Default::default(), window, cx);
            }
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::SlashAutocompleteOpened) => {
            }
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::LocalCommandInvoked(_)) => {}
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::Edited) => {}
            ViewEvent::MessageEditorEvent(_editor, MessageEditorEvent::InputAttempted { .. }) => {}
            ViewEvent::OpenDiffLocation {
                path,
                position,
                split,
            } => {
                self.open_diff_location(path, *position, *split, window, cx);
            }
        }
    }

    fn open_diff_location(
        &self,
        path: &str,
        position: Point,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let Some(project_path) = project.read(cx).find_project_path(path, cx) else {
            return;
        };

        let open_task = if split {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.split_path(project_path, window, cx)
                })
                .log_err()
        } else {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .log_err()
        };

        let Some(open_task) = open_task else {
            return;
        };

        window
            .spawn(cx, async move |cx| {
                let item = open_task.await?;
                let Some(editor) = item.downcast::<Editor>() else {
                    return anyhow::Ok(());
                };
                editor.update_in(cx, |editor, window, cx| {
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()),
                        window,
                        cx,
                        |selections| {
                            selections.select_ranges([position..position]);
                        },
                    );
                })?;
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
    }

    // turns

    pub fn start_turn(&mut self, cx: &mut Context<Self>) -> usize {
        // A previous response may have settled while the new prompt's contents were loading.
        self.thread_error.take();
        self.initialize_turn(cx)
    }

    fn initialize_turn(&mut self, cx: &mut Context<Self>) -> usize {
        self.turn_fields.turn_generation += 1;
        let generation = self.turn_fields.turn_generation;
        self.turn_fields.turn_started_at = Some(Instant::now());
        self.turn_fields.last_turn_duration = None;
        self.turn_fields.last_turn_tokens = None;
        self.turn_fields.turn_tokens = Some(0);
        self.entry_view_state
            .update(cx, |state, _cx| state.note_live_turn());
        self.turn_fields._turn_timer_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        generation
    }

    pub fn stop_turn(&mut self, generation: usize, cx: &mut Context<Self>) {
        if self.turn_fields.turn_generation != generation {
            return;
        }
        self.turn_fields.last_turn_duration = self
            .turn_fields
            .turn_started_at
            .take()
            .map(|started| started.elapsed());
        self.turn_fields.last_turn_tokens = self.turn_fields.turn_tokens.take();
        self.turn_fields._turn_timer_task = None;
        self.record_last_turn_duration(cx);
    }

    /// Keeps the last turn's duration for its "Worked for" line.
    fn record_last_turn_duration(&mut self, cx: &mut Context<Self>) {
        let Some(duration) = self.turn_fields.last_turn_duration else {
            return;
        };
        let prompt_entry_ix = self.thread.read(cx).entries().iter().rposition(
            |entry| matches!(entry, AgentThreadEntry::UserMessage(message) if !message.indented),
        );
        if let Some(prompt_entry_ix) = prompt_entry_ix {
            self.entry_view_state.update(cx, |state, _cx| {
                state.record_turn_duration(prompt_entry_ix, duration);
            });
        }
    }

    /// Re-measures the turn holding the entry at `entry_ix`, and the turn before it. How an
    /// entry shows depends on the entries around it, since activity groups and folded turns
    /// form from them, so a change to one entry can change the height of others.
    pub(crate) fn remeasure_turn(&self, entry_ix: usize, cx: &App) {
        let entries = self.thread.read(cx).entries();
        let start = entries
            .get(..entry_ix)
            .and_then(|entries| {
                entries.iter().rposition(|entry| {
                    matches!(entry, AgentThreadEntry::UserMessage(message) if !message.indented)
                })
            })
            .unwrap_or(0);
        self.list_state.remeasure_items(start..entries.len());
    }

    pub(crate) fn sync_reported_activity(&mut self, cx: &mut Context<Self>) {
        let thread = self.thread.read(cx);
        if !thread.uses_reported_activity() {
            return;
        }
        let activity = thread.foreground_activity();
        let activity_generation = thread.activity_generation();
        let started_at = thread.activity_started_at();
        let duration = thread.activity_duration();

        if activity != ForegroundActivity::Idle {
            if self.turn_fields.reported_activity_generation != Some(activity_generation) {
                self.initialize_turn(cx);
                self.turn_fields.turn_started_at = started_at;
                self.turn_fields.reported_activity_generation = Some(activity_generation);
            }
        } else {
            if self.turn_fields.turn_started_at.is_some() {
                self.stop_turn(self.turn_fields.turn_generation, cx);
            }
            if self.turn_fields.reported_activity_generation != Some(activity_generation) {
                self.turn_fields.last_turn_tokens = None;
            }
            self.turn_fields.reported_activity_generation = Some(activity_generation);
            self.turn_fields.last_turn_duration = duration;
            self.record_last_turn_duration(cx);
        }
        cx.notify();
    }

    pub(crate) fn sync_follow_agent_state(&mut self, cx: &mut Context<Self>) {
        self.should_be_following = self
            .workspace
            .update(cx, |workspace, _| {
                workspace.is_being_followed(CollaboratorId::Agent)
            })
            .unwrap_or_default();
    }

    pub(crate) fn report_activity_completion(
        &self,
        stop_reason: &Option<acp_v2::StopReason>,
        duration: Option<Duration>,
        cx: &App,
    ) {
        let thread = self.thread.read(cx);
        telemetry::event!(
            "Agent Turn Completed",
            agent = thread.connection().telemetry_id(),
            session = thread.session_id().clone(),
            parent_session_id = thread.parent_session_id().map(|id| id.to_string()),
            model = self.current_model_id(cx),
            mode = self.current_mode_id(cx),
            status = Self::activity_completion_status(stop_reason.as_ref()),
            turn_time_ms = duration.unwrap_or_default().as_millis(),
            side = crate::agent_sidebar_side(cx)
        );
    }

    fn activity_completion_status(stop_reason: Option<&acp_v2::StopReason>) -> &'static str {
        match stop_reason {
            Some(acp_v2::StopReason::EndTurn) => "success",
            Some(acp_v2::StopReason::Cancelled) => "cancelled",
            Some(
                acp_v2::StopReason::MaxTokens
                | acp_v2::StopReason::MaxTurnRequests
                | acp_v2::StopReason::Refusal
                | acp_v2::StopReason::Error(_),
            ) => "failure",
            _ => "unknown",
        }
    }

    pub(crate) fn in_flight_prompt(&self, cx: &App) -> Option<Arc<[acp_v2::ContentBlock]>> {
        let record = self.thread.read(cx);
        let record = record.submission(self.current_submission?)?;
        (!matches!(record.state, SubmissionState::Completed)).then(|| record.content.clone())
    }

    fn submission_text_parts(content: &[acp_v2::ContentBlock]) -> impl Iterator<Item = &str> {
        content.iter().map(|block| match block {
            acp_v2::ContentBlock::Text(text) => text.text.as_str(),
            acp_v2::ContentBlock::ResourceLink(link) => link.name.as_str(),
            acp_v2::ContentBlock::Resource(resource) => match &resource.resource {
                acp_v2::EmbeddedResourceResource::TextResourceContents(resource) => {
                    resource.uri.as_str()
                }
                acp_v2::EmbeddedResourceResource::BlobResourceContents(resource) => {
                    resource.uri.as_str()
                }
                _ => "[Resource attachment]",
            },
            acp_v2::ContentBlock::Image(_) => "[Image attachment]",
            acp_v2::ContentBlock::Audio(_) => "[Audio attachment]",
            _ => "[Unsupported attachment]",
        })
    }

    fn restore_submission(
        &mut self,
        submission_id: SubmissionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.message_editor.read(cx).text(cx).is_empty() {
            return;
        }
        let content = self
            .thread
            .read(cx)
            .submission(submission_id)
            .and_then(|record| {
                matches!(
                    record.state,
                    SubmissionState::Failed(_) | SubmissionState::Cancelled
                )
                .then(|| record.content.to_vec())
            });
        if let Some(content) = content {
            if !content.iter().all(acp_thread::content::can_convert_to_v1) {
                self.handle_thread_error(
                    anyhow!(
                        "This saved submission contains unsupported content and cannot be restored. The original submission has been kept."
                    ),
                    cx,
                );
                return;
            }
            self.message_editor.update(cx, |editor, cx| {
                editor.set_message(content, window, cx);
            });
            // The editor is not a lossless ACP round trip. Keep the original until explicit discard.
            cx.notify();
        }
    }

    fn render_recoverable_submissions(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let submissions = {
            let thread = self.thread.read(cx);
            if !thread.uses_reported_activity() {
                return None;
            }
            thread
                .recoverable_submissions()
                .map(|(submission_id, record)| {
                    let (status, settled): (SharedString, bool) = match &record.state {
                        SubmissionState::Pending => ("Sending message…".into(), false),
                        SubmissionState::Accepted { echoed: false, .. } => {
                            ("Waiting for message to appear…".into(), false)
                        }
                        SubmissionState::Disconnected { receipt: Some(_) } => {
                            ("Message accepted; agent connection lost".into(), true)
                        }
                        SubmissionState::Disconnected { receipt: None } => (
                            "Connection lost; message acceptance is unknown".into(),
                            true,
                        ),
                        SubmissionState::Failed(error) => (
                            format!(
                                "Message failed to send: {}",
                                util::truncate_and_trailoff(error, 160)
                            )
                            .into(),
                            true,
                        ),
                        SubmissionState::Cancelled => ("Message cancelled".into(), true),
                        _ => ("Message".into(), false),
                    };
                    (
                        submission_id,
                        status,
                        settled,
                        matches!(
                            record.state,
                            SubmissionState::Failed(_) | SubmissionState::Cancelled
                        ),
                        Itertools::intersperse(Self::submission_text_parts(&record.content), "\n")
                            .flat_map(str::chars)
                            .take(161)
                            .collect::<String>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        if submissions.is_empty() {
            return None;
        }
        let rows = submissions
            .into_iter()
            .map(|(submission_id, status, settled, restorable, text)| {
                let preview: SharedString = util::truncate_and_trailoff(&text, 160).into();
                let row_id = submission_id.as_u64();
                v_flex()
                    .id(("recoverable-submission", row_id))
                    .px_3()
                    .py_1()
                    .gap_1()
                    .child(Label::new(status).size(LabelSize::Small))
                    .child(Label::new(preview).size(LabelSize::Small))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .debug_selector(move || format!("copy-submission-{row_id}"))
                                    .child(
                                        Button::new(("copy-submission", row_id), "Copy")
                                            .label_size(LabelSize::Small)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(record) =
                                                    this.thread.read(cx).submission(submission_id)
                                                {
                                                    let text = Self::submission_text_parts(
                                                        &record.content,
                                                    )
                                                    .join("\n");
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(text),
                                                    );
                                                }
                                            })),
                                    ),
                            )
                            .when(restorable, |this| {
                                this.child(
                                    div()
                                        .debug_selector(move || {
                                            format!("restore-submission-{row_id}")
                                        })
                                        .child(
                                            Button::new(("restore-submission", row_id), "Restore")
                                                .label_size(LabelSize::Small)
                                                .tooltip(Tooltip::text("Copy into an empty composer. The saved submission is kept until discarded."))
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.restore_submission(
                                                            submission_id,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        ),
                                )
                            })
                            .when(settled, |this| {
                                this.child(
                                    Button::new(("discard-submission", row_id), "Discard")
                                        .label_size(LabelSize::Small)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.thread.update(cx, |thread, cx| {
                                                thread.forget_submission(submission_id, cx);
                                            });
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            v_flex()
                .id("recoverable-submissions")
                .max_h(px(180.))
                .overflow_y_scroll()
                .children(rows),
        )
    }

    pub fn update_turn_tokens(&mut self, cx: &App) {
        if let Some(usage) = self.thread.read(cx).token_usage() {
            if let Some(tokens) = &mut self.turn_fields.turn_tokens {
                *tokens += usage.output_tokens;
                self.emit_token_limit_telemetry_if_needed(cx);
            }
        }
    }

    fn emit_token_limit_telemetry_if_needed(&mut self, cx: &App) {
        let (ratio, agent_telemetry_id, session_id) = {
            let thread_data = self.thread.read(cx);
            let Some(token_usage) = thread_data.token_usage() else {
                return;
            };
            (
                token_usage.ratio(),
                thread_data.connection().telemetry_id(),
                thread_data.session_id().clone(),
            )
        };

        let kind = match ratio {
            acp_thread::TokenUsageRatio::Normal => {
                self.last_token_limit_telemetry = None;
                return;
            }
            acp_thread::TokenUsageRatio::Warning => "warning",
            acp_thread::TokenUsageRatio::Exceeded => "exceeded",
        };

        let should_skip = self
            .last_token_limit_telemetry
            .as_ref()
            .is_some_and(|last| *last >= ratio);
        if should_skip {
            return;
        }

        self.last_token_limit_telemetry = Some(ratio);

        telemetry::event!(
            "Agent Token Limit Warning",
            agent = agent_telemetry_id,
            session_id = session_id,
            kind = kind,
        );
    }

    // sending

    fn clear_external_source_prompt_warning(&mut self, cx: &mut Context<Self>) {
        if self.show_external_source_prompt_warning {
            self.show_external_source_prompt_warning = false;
            cx.notify();
        }
    }

    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_editor.read(cx).editor().read(cx).read_only(cx) {
            self.handle_thread_error(
                anyhow!(
                    "This draft contains unsupported content and cannot be edited or sent. Discard the draft to write a new message."
                ),
                cx,
            );
            return;
        }
        let thread = &self.thread;

        if self.is_loading_contents {
            return;
        }

        let message_editor = self.message_editor.clone();

        let is_editor_empty = message_editor.read(cx).is_empty(cx);
        let is_generating = thread.read(cx).status() != ThreadStatus::Idle;

        if is_editor_empty {
            if self.message_queue.can_fast_track()
                && let Some(id) = self.message_queue.first_id()
                && !self.validate_queued_entry(id, cx)
            {
                return;
            }
            if let Some(entry) = self.message_queue.try_fast_track(is_generating) {
                self.dispatch_queued_entry(entry, window, cx);
            }
            return;
        }

        if is_generating {
            cx.emit(AcpThreadViewEvent::Interacted);
            self.queue_message(message_editor, window, cx);
            return;
        }

        let text = message_editor.read(cx).text(cx);
        let text = text.trim();
        if text == "/login" || text == "/logout" {
            let connection = thread.read(cx).connection().clone();
            let can_login = connection
                .auth_methods()
                .iter()
                .any(acp_thread::auth_methods::is_supported);
            // Does the agent have a specific logout command? Prefer that in case they need to reset internal state.
            let logout_supported = text == "/logout"
                && self
                    .session_capabilities
                    .read()
                    .available_commands()
                    .iter()
                    .any(|available_command| available_command.name == "logout");
            if can_login && !logout_supported {
                message_editor.update(cx, |editor, cx| editor.clear(window, cx));
                self.clear_external_source_prompt_warning(cx);

                let connection = self.thread.read(cx).connection().clone();
                window.defer(cx, {
                    let server_view = self.server_view.clone();
                    move |window, cx| {
                        ConversationView::handle_auth_required(
                            server_view.clone(),
                            AuthRequired::new(),
                            connection,
                            window,
                            cx,
                        );
                    }
                });
                cx.notify();
                return;
            }
        }

        // A built-in command (e.g. `/compact`): run the bare command without
        // echoing it as a user message, and queue any trailing text the user
        // typed so it isn't silently dropped.
        let native_command =
            leading_native_command(text, self.session_capabilities.read().available_commands());
        if let Some(command_name) = native_command {
            cx.emit(AcpThreadViewEvent::Interacted);
            self.send_command_queueing_remainder(message_editor, command_name, window, cx);
            return;
        }

        cx.emit(AcpThreadViewEvent::Interacted);
        self.send_impl(message_editor, window, cx)
    }

    /// Sends a bare `/command` turn and queues everything the user typed after
    /// it as a follow-up message. The queued remainder auto-processes when the
    /// command turn stops, so e.g. `/compact do X` compacts and then runs `do X`
    /// rather than discarding it.
    fn send_command_queueing_remainder(
        &mut self,
        message_editor: Entity<MessageEditor>,
        command_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Resolve the editor contents before clearing it: the resolve task
        // reads the editor lazily, so clearing first would wipe the contents.
        let contents = self.resolve_message_contents(&message_editor, cx);
        self.thread_error.take();
        self.thread_feedback.clear();
        self.editing_message.take();

        cx.spawn_in(window, async move |this, cx| {
            let (mut content, tracked_buffers) = contents.await?;

            cx.update(|window, cx| {
                message_editor.update(cx, |message_editor, cx| {
                    message_editor.clear(window, cx);
                });
            })?;

            // Strip the leading `/command` from the first text block; whatever
            // remains (including any later mention blocks) becomes the queued
            // follow-up message.
            if let Some(acp_v2::ContentBlock::Text(text_content)) = content.first_mut() {
                text_content.text = strip_leading_command(&text_content.text, &command_name);
            }
            if matches!(
                content.first(),
                Some(acp_v2::ContentBlock::Text(text)) if text.text.trim().is_empty()
            ) {
                content.remove(0);
            }

            let command_block =
                acp_v2::ContentBlock::Text(acp_v2::TextContent::new(format!("/{command_name}")));

            this.update_in(cx, |this, window, cx| {
                // Queue the remainder first, then start the command turn; the
                // queue auto-processes when the command turn stops.
                if !content.is_empty() {
                    this.add_to_queue(content, tracked_buffers, window, cx);
                }
                this.send_content(
                    Task::ready(Ok(Some((vec![command_block], Vec::new())))),
                    true,
                    window,
                    cx,
                );
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn send_impl(
        &mut self,
        message_editor: Entity<MessageEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let contents = self.resolve_message_contents(&message_editor, cx);

        self.thread_error.take();
        self.thread_feedback.clear();
        self.editing_message.take();
        // Sending a message is active engagement: un-freeze the queue if it
        // was paused by a manual stop.
        self.message_queue.resume();

        if self.should_be_following {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.follow(CollaboratorId::Agent, window, cx);
                })
                .ok();
        }

        let contents_task = cx.spawn_in(window, async move |_this, cx| {
            let (contents, tracked_buffers) = contents.await?;

            if contents.is_empty() {
                return Ok(None);
            }

            let _ = cx.update(|window, cx| {
                message_editor.update(cx, |message_editor, cx| {
                    message_editor.clear(window, cx);
                });
            });

            Ok(Some((contents, tracked_buffers)))
        });

        self.send_content(contents_task, false, window, cx);
    }

    pub fn send_content(
        &mut self,
        contents_task: Task<
            anyhow::Result<Option<(Vec<acp_v2::ContentBlock>, Vec<Entity<Buffer>>)>>,
        >,
        is_native_command: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_id = self.thread.read(cx).session_id().clone();
        let parent_session_id = self.thread.read(cx).parent_session_id().cloned();
        let agent_telemetry_id = self.thread.read(cx).connection().telemetry_id();
        let is_first_message = self.thread.read(cx).entries().is_empty();
        let thread = self.thread.downgrade();

        self.is_loading_contents = true;

        let model_id = self.current_model_id(cx);
        let mode_id = self.current_mode_id(cx);
        let guard = cx.new(|_| ());
        cx.observe_release(&guard, |this, _guard, cx| {
            this.is_loading_contents = false;
            cx.notify();
        })
        .detach();

        let side = crate::agent_sidebar_side(cx);

        let task = cx.spawn_in(window, async move |this, cx| {
            let Some((contents, tracked_buffers)) = contents_task.await? else {
                return Ok(());
            };

            let uses_reported_activity =
                thread.read_with(cx, |thread, _| thread.uses_reported_activity())?;
            let generation = this.update(cx, |this, cx| {
                this.clear_external_source_prompt_warning(cx);
                this.thread_error.take();
                (!uses_reported_activity).then(|| this.start_turn(cx))
            })?;

            this.update_in(cx, |this, _window, cx| {
                this.set_editor_is_expanded(false, cx);
            })?;

            let _ = this.update(cx, |this, cx| {
                this.list_state.scroll_to_end();
                cx.notify();
            });

            let _stop_turn = (!uses_reported_activity).then(|| {
                let this = this.clone();
                let mut cx = cx.clone();
                defer(move || {
                    this.update(&mut cx, |this, cx| {
                        if let Some(generation) = generation {
                            this.stop_turn(generation, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                })
            });
            if is_first_message && thread.read_with(cx, |thread, _cx| thread.title().is_none())? {
                let text: String = contents
                    .iter()
                    .filter_map(|block| match block {
                        acp_v2::ContentBlock::Text(text_content) => Some(text_content.text.clone()),
                        acp_v2::ContentBlock::ResourceLink(resource_link) => {
                            Some(format!("@{}", resource_link.name))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let text = text.lines().next().unwrap_or("").trim();
                if !text.is_empty() {
                    let title: SharedString = util::truncate_and_trailoff(text, 200).into();
                    thread.update(cx, |thread, cx| {
                        thread.set_provisional_title(title, cx);
                    })?;
                }
            }

            let turn_start_time = Instant::now();
            let send = thread.update(cx, |thread, cx| {
                thread.action_log().update(cx, |action_log, cx| {
                    for buffer in tracked_buffers {
                        action_log.buffer_read(buffer, cx)
                    }
                });
                drop(guard);

                telemetry::event!(
                    "Agent Message Sent",
                    agent = agent_telemetry_id,
                    session = session_id,
                    parent_session_id = parent_session_id.as_ref().map(|id| id.to_string()),
                    model = model_id,
                    mode = mode_id,
                    side = side
                );

                if is_native_command {
                    thread.send_command(contents, cx)
                } else {
                    thread.send(contents, cx)
                }
            })?;
            let submission_id = send.id;

            let _ = this.update(cx, |this, cx| {
                this.current_submission = Some(submission_id);
                this.sync_generating_indicator(cx);
                cx.notify();
            });

            let res = send.await;
            let turn_time_ms = turn_start_time.elapsed().as_millis();
            drop(_stop_turn);
            if !uses_reported_activity {
                let status = if res.is_ok() { "success" } else { "failure" };
                telemetry::event!(
                    "Agent Turn Completed",
                    agent = agent_telemetry_id,
                    session = session_id,
                    parent_session_id = parent_session_id.as_ref().map(|id| id.to_string()),
                    model = model_id,
                    mode = mode_id,
                    status,
                    turn_time_ms,
                    side = side
                );
            }
            this.update(cx, |this, cx| {
                // Cancellation can return control to the UI before this response settles.
                if this.current_submission != Some(submission_id)
                    || generation
                        .is_some_and(|generation| this.turn_fields.turn_generation != generation)
                {
                    if let Err(error) = res {
                        log::debug!("Ignoring error from a superseded agent send: {error:#}");
                    }
                    return;
                }
                match res {
                    Ok(Some(SubmissionResponse::LegacyCompleted(_))) => {
                        this.current_submission.take();
                        this.sync_follow_agent_state(cx);
                    }
                    Ok(Some(SubmissionResponse::Accepted(_))) => {}
                    Ok(None) => {}
                    Err(error) => this.handle_thread_error(error, cx),
                }
            })?;
            anyhow::Ok(())
        });

        cx.spawn(async move |this, cx| {
            if let Err(err) = task.await {
                this.update(cx, |this, cx| {
                    this.handle_thread_error(err, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn interrupt_and_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_editor.read(cx).editor().read(cx).read_only(cx) {
            self.handle_thread_error(
                anyhow!(
                    "This draft contains unsupported content and cannot be edited or sent. Discard the draft to write a new message."
                ),
                cx,
            );
            return;
        }
        let thread = &self.thread;

        if self.is_loading_contents {
            return;
        }

        cx.emit(AcpThreadViewEvent::Interacted);

        let message_editor = self.message_editor.clone();
        if thread.read(cx).status() == ThreadStatus::Idle {
            self.send_impl(message_editor, window, cx);
            return;
        }

        self.stop_current_and_send_new_message(message_editor, window, cx);
    }

    fn stop_current_and_send_new_message(
        &mut self,
        message_editor: Entity<MessageEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread = self.thread.clone();
        self.message_queue.pause();

        let cancelled = thread.update(cx, |thread, cx| thread.cancel(cx));

        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = cancelled.await {
                this.update(cx, |this, cx| this.handle_thread_error(error, cx))
                    .log_err();
                return;
            }

            this.update_in(cx, |this, window, cx| {
                this.send_impl(message_editor, window, cx);
            })
            .log_err();
        })
        .detach();
    }

    pub(crate) fn handle_thread_error(
        &mut self,
        error: impl Into<ThreadError>,
        cx: &mut Context<Self>,
    ) {
        let error = error.into();
        self.emit_thread_error_telemetry(&error, cx);
        self.thread_error = Some(error);
        cx.notify();
    }

    fn emit_thread_error_telemetry(&self, error: &ThreadError, cx: &mut Context<Self>) {
        let (error_kind, acp_error_code, message): (&str, Option<SharedString>, SharedString) =
            match error {
                ThreadError::ZedPaymentRequired => (
                    "payment_required",
                    None,
                    "You reached your free usage limit. Upgrade to Zed Pro for more prompts."
                        .into(),
                ),
                ThreadError::Refusal => {
                    let model_or_agent_name = self.current_model_name(cx);
                    let message = format!(
                        "{} refused to respond to this prompt. This can happen when a model believes the prompt violates its content policy or safety guidelines, so rephrasing it can sometimes address the issue.",
                        model_or_agent_name
                    );
                    ("refusal", None, message.into())
                }
                ThreadError::DataRetentionConsentRequired => {
                    let message = format!(
                        "{} is not available with Zero Data Retention.",
                        self.current_model_name(cx)
                    );
                    ("data_retention_consent_required", None, message.into())
                }
                ThreadError::AuthenticationRequired(message) => {
                    ("authentication_required", None, message.clone())
                }
                ThreadError::RateLimitExceeded { provider } => (
                    "rate_limit_exceeded",
                    None,
                    format!("{provider}'s rate limit was reached.").into(),
                ),
                ThreadError::ServerOverloaded { provider } => (
                    "server_overloaded",
                    None,
                    format!("{provider}'s servers are temporarily unavailable.").into(),
                ),
                ThreadError::PromptTooLarge => (
                    "prompt_too_large",
                    None,
                    "Context too large for the model's context window.".into(),
                ),
                ThreadError::NoCredentials { provider } => (
                    "no_api_key",
                    None,
                    format!("No credentials configured for {provider}.").into(),
                ),
                ThreadError::StreamError { provider } => (
                    "stream_error",
                    None,
                    format!("Connection to {provider}'s API was interrupted.").into(),
                ),
                ThreadError::AuthenticationFailed { provider } => (
                    "invalid_api_key",
                    None,
                    format!("Authentication with {provider} failed.").into(),
                ),
                ThreadError::PermissionDenied { provider, message } => (
                    "permission_denied",
                    None,
                    message.clone().unwrap_or_else(|| {
                        format!(
                            "{provider}'s API rejected the request due to insufficient permissions."
                        )
                        .into()
                    }),
                ),
                ThreadError::ProviderRejection { message } => {
                    ("provider_rejection", None, message.clone())
                }
                ThreadError::MaxOutputTokens => (
                    "max_output_tokens",
                    None,
                    "Model reached its maximum output length.".into(),
                ),
                ThreadError::NoModelSelected => {
                    ("no_model_selected", None, "No model selected.".into())
                }
                ThreadError::ApiError { provider } => (
                    "api_error",
                    None,
                    format!("{provider}'s API returned an unexpected error.").into(),
                ),
                ThreadError::Other {
                    acp_error_code,
                    message,
                } => ("other", acp_error_code.clone(), message.clone()),
            };

        let agent_telemetry_id = self.thread.read(cx).connection().telemetry_id();
        let session_id = self.thread.read(cx).session_id().clone();
        let parent_session_id = self
            .thread
            .read(cx)
            .parent_session_id()
            .map(|id| id.to_string());

        telemetry::event!(
            "Agent Panel Error Shown",
            agent = agent_telemetry_id,
            session_id = session_id,
            parent_session_id = parent_session_id,
            kind = error_kind,
            acp_error_code = acp_error_code,
            message = message,
        );
    }

    pub fn cancel_generation(&mut self, cx: &mut Context<Self>) {
        self.thread_retry_status.take();
        self.thread_error.take();
        self.message_queue.pause();
        self._cancel_task = Some(self.cancel_thread(&self.thread, cx));
        self.sync_generating_indicator(cx);
        cx.notify();
    }

    fn cancel_thread(&self, thread: &Entity<AcpThread>, cx: &mut Context<Self>) -> Task<()> {
        let cancellation = thread.update(cx, |thread, cx| thread.cancel(cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = cancellation.await {
                this.update(cx, |this, cx| this.handle_thread_error(error, cx))
                    .log_err();
            }
        })
    }

    pub fn retry_generation(&mut self, cx: &mut Context<Self>) {
        self.thread_error.take();

        let thread = &self.thread;
        if !thread.read(cx).can_retry(cx) {
            return;
        }

        let task = thread.update(cx, |thread, cx| thread.retry(cx));
        let submission_id = task.id;
        self.current_submission = Some(submission_id);
        cx.emit(AcpThreadViewEvent::Interacted);
        self.sync_generating_indicator(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;

            this.update(cx, |this, cx| {
                if this.current_submission != Some(submission_id) {
                    return;
                }
                match result {
                    Err(error) => this.handle_thread_error(error, cx),
                    Ok(Some(SubmissionResponse::LegacyCompleted(_))) => {
                        this.current_submission = None;
                    }
                    Ok(Some(SubmissionResponse::Accepted(_)) | None) => {}
                }
            })
        })
        .detach();
    }

    pub fn regenerate(
        &mut self,
        entry_ix: usize,
        message_editor: Entity<MessageEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_loading_contents || !self.can_edit_user_message(entry_ix, cx) {
            return;
        }
        let thread = self.thread.clone();

        let Some(client_id) = thread.update(cx, |thread, _| {
            thread
                .entries()
                .get(entry_ix)?
                .user_message()?
                .client_id
                .clone()
        }) else {
            return;
        };

        cx.spawn_in(window, async move |this, cx| {
            // Check if there are any edits from prompts before the one being regenerated.
            //
            // If there are, we keep/accept them since we're not regenerating the prompt that created them.
            //
            // If editing the prompt that generated the edits, they are auto-rejected
            // through the `rewind` function in the `acp_thread`.
            //
            // Subagent edits never show up as diffs in the parent thread's entries (they
            // are only forwarded to the parent's action log), so treat any earlier
            // subagent tool call as potentially having edits. Keeping all edits is a
            // no-op when the subagent didn't make any.
            let has_earlier_edits = thread.read_with(cx, |thread, _| {
                thread.entries().iter().take(entry_ix).any(|entry| {
                    entry.diffs().next().is_some()
                        || matches!(
                            entry,
                            AgentThreadEntry::ToolCall(tool_call) if tool_call.is_subagent()
                        )
                })
            });

            if has_earlier_edits {
                thread.update(cx, |thread, cx| {
                    thread.action_log().update(cx, |action_log, cx| {
                        action_log.keep_all_edits(None, cx);
                    });
                });
            }

            thread
                .update(cx, |thread, cx| thread.rewind(client_id, cx))
                .await?;
            this.update_in(cx, |thread, window, cx| {
                // Rewind can outlive a source update that replaces the editor with a fallback.
                if message_editor.read(cx).editor().read(cx).read_only(cx) {
                    thread.handle_thread_error(
                        anyhow!(
                            "The conversation was rewound, but the message became read-only and was not resent."
                        ),
                        cx,
                    );
                    return;
                }
                cx.emit(AcpThreadViewEvent::Interacted);
                thread.send_impl(message_editor, window, cx);
                thread.activation_focus_handle(cx).focus(window, cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    // message queueing

    fn queue_message(
        &mut self,
        message_editor: Entity<MessageEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_idle = self.thread.read(cx).status() == acp_thread::ThreadStatus::Idle;

        if is_idle {
            self.send_impl(message_editor, window, cx);
            return;
        }

        let contents = self.resolve_message_contents(&message_editor, cx);

        cx.spawn_in(window, async move |this, cx| {
            let (content, tracked_buffers) = contents.await?;

            if content.is_empty() {
                return Ok::<(), anyhow::Error>(());
            }

            this.update_in(cx, |this, window, cx| {
                this.add_to_queue(content, tracked_buffers, window, cx);
                message_editor.update(cx, |message_editor, cx| {
                    message_editor.clear(window, cx);
                });
                cx.notify();
            })?;
            Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn add_to_queue(
        &mut self,
        content: Vec<acp_v2::ContentBlock>,
        tracked_buffers: Vec<Entity<Buffer>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The ID must be allocated up front so the editor event subscription
        // can capture it before the entry (which owns the subscription) exists.
        let id = self.message_queue.next_id();

        let editor = cx.new(|cx| {
            let mut editor = MessageEditor::new(
                self.workspace.clone(),
                self.project.clone(),
                None,
                self.session_capabilities.clone(),
                self.agent_id.clone(),
                "",
                EditorMode::AutoHeight {
                    min_lines: 1,
                    max_lines: Some(10),
                },
                window,
                cx,
            );
            editor.set_read_only(true, cx);
            editor.set_source_message(content.clone(), window, cx);
            editor
        });

        let subscription =
            cx.subscribe_in(&editor, window, move |this, _editor, event, window, cx| {
                this.handle_queue_editor_event(id, event, window, cx);
            });

        self.message_queue.enqueue(QueueEntry {
            id,
            content,
            tracked_buffers,
            steer: false,
            editor,
            _subscription: subscription,
        });
        self.sync_queue_flag_to_native_thread(cx);
        cx.notify();
    }

    fn handle_queue_editor_event(
        &mut self,
        id: QueueEntryId,
        event: &MessageEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MessageEditorEvent::InputAttempted {
                attempt,
                cursor_offset,
            } => {
                self.move_queued_message_to_main_editor(
                    id,
                    Some(attempt.clone()),
                    Some(*cursor_offset),
                    window,
                    cx,
                );
            }
            MessageEditorEvent::LostFocus => {
                self.save_queued_message(id, cx);
            }
            MessageEditorEvent::Cancel | MessageEditorEvent::Send => {
                window.focus(&self.message_editor.focus_handle(cx), cx);
            }
            MessageEditorEvent::SendImmediately => {
                self.send_queued_message_now(id, window, cx);
            }
            _ => {}
        }
    }

    fn save_queued_message(&mut self, id: QueueEntryId, cx: &mut Context<Self>) {
        let Some(entry) = self.message_queue.entry_by_id(id) else {
            return;
        };
        if !entry
            .content
            .iter()
            .all(acp_thread::content::can_convert_to_v1)
        {
            return;
        }
        let contents_task = entry
            .editor
            .update(cx, |editor, cx| editor.contents(false, cx));

        cx.spawn(async move |this, cx| {
            let (content, tracked_buffers) = contents_task.await?;

            this.update(cx, |this, cx| {
                if let Some(entry) = this.message_queue.entry_by_id_mut(id) {
                    if !entry
                        .content
                        .iter()
                        .all(acp_thread::content::can_convert_to_v1)
                    {
                        return;
                    }
                    entry.content = content;
                    entry.tracked_buffers = tracked_buffers;
                }
                cx.notify();
            })?;

            Ok::<(), anyhow::Error>(())
        })
        .detach_and_log_err(cx);
    }

    pub fn remove_from_queue(
        &mut self,
        id: QueueEntryId,
        cx: &mut Context<Self>,
    ) -> Option<QueueEntry> {
        let removed = self.message_queue.remove(id);
        if removed.is_some() {
            self.sync_queue_flag_to_native_thread(cx);
        }
        removed
    }

    fn toggle_queue_entry_steer(&mut self, id: QueueEntryId, cx: &mut Context<Self>) {
        self.message_queue.toggle_steer(id);
        self.sync_queue_flag_to_native_thread(cx);
        cx.notify();
    }

    pub fn sync_queue_flag_to_native_thread(&self, cx: &mut Context<Self>) {
        if let Some(native_thread) = self.as_native_thread(cx) {
            // By default queued messages wait for the turn to fully complete.
            // Only a "steering" front message ends the turn at the next boundary.
            let end_at_boundary = self.message_queue.front_wants_steer()
                && self.message_queue.first().is_some_and(|entry| {
                    self.thread
                        .read(cx)
                        .validate_prompt_content(&entry.content)
                        .is_ok()
                });
            native_thread.update(cx, |thread, _| {
                thread.set_end_turn_at_next_boundary(end_at_boundary);
            });
        }
    }

    fn validate_queued_entry(&mut self, id: QueueEntryId, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.message_queue.entry_by_id(id) else {
            return false;
        };
        if let Err(error) = self.thread.read(cx).validate_prompt_content(&entry.content) {
            self.handle_thread_error(error, cx);
            return false;
        }
        true
    }

    pub fn send_queued_message_after_generation_stopped(
        &mut self,
        is_first_editor_focused: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(id) = self
            .message_queue
            .auto_send_candidate(is_first_editor_focused)
            .map(|entry| entry.id)
            && !self.validate_queued_entry(id, cx)
        {
            self.message_queue.pause();
        }
        if let Some(entry) = self
            .message_queue
            .on_generation_stopped(is_first_editor_focused)
        {
            self.dispatch_queued_entry(entry, window, cx);
            true
        } else {
            false
        }
    }

    pub fn send_queued_message_now(
        &mut self,
        id: QueueEntryId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.validate_queued_entry(id, cx) {
            return;
        }
        let is_generating = self.thread.read(cx).status() == acp_thread::ThreadStatus::Generating;
        if let Some(entry) = self.message_queue.send_now(id, is_generating) {
            self.dispatch_queued_entry(entry, window, cx);
        }
    }

    /// The shared "actually send this entry" path, used by fast-track,
    /// auto-processing on Stopped, and "Send Now". The entry must already have
    /// been removed from the queue.
    fn dispatch_queued_entry(
        &mut self,
        entry: QueueEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_queue_flag_to_native_thread(cx);

        cx.emit(AcpThreadViewEvent::Interacted);

        self.message_editor.focus_handle(cx).focus(window, cx);

        // A queued message can itself be a built-in command (e.g. the user typed
        // `/compact` while a turn was generating). Detect that so we run it as a
        // command turn without echoing it as a user message, matching the
        // non-queued path.
        let is_native_command = entry
            .content
            .first()
            .and_then(|block| match block {
                acp_v2::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .and_then(|text| {
                leading_native_command(text, self.session_capabilities.read().available_commands())
            })
            .is_some();

        let cancelled = self.thread.update(cx, |thread, cx| thread.cancel(cx));

        let workspace = self.workspace.clone();

        let should_be_following = self.should_be_following;
        let contents_task = cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = cancelled.await {
                this.update(cx, |this, cx| {
                    this.message_queue.restore_after_failed_send(entry);
                    this.sync_queue_flag_to_native_thread(cx);
                    cx.notify();
                })
                .log_err();
                return Err(error);
            }
            if should_be_following {
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.follow(CollaboratorId::Agent, window, cx);
                    })
                    .ok();
            }

            Ok(Some((entry.content, entry.tracked_buffers)))
        });

        self.send_content(contents_task, is_native_command, window, cx);
    }

    pub fn move_queued_message_to_main_editor(
        &mut self,
        id: QueueEntryId,
        attempt: Option<InputAttempt>,
        cursor_offset: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.message_editor.read(cx).editor().read(cx).read_only(cx) {
            self.handle_thread_error(
                anyhow!("Discard the unsupported draft before moving a queued message into the composer."),
                cx,
            );
            return false;
        }
        if self.message_queue.entry_by_id(id).is_some_and(|entry| {
            !entry
                .content
                .iter()
                .all(acp_thread::content::can_convert_to_v1)
        }) {
            self.handle_thread_error(
                anyhow!("This queued message contains unsupported content and cannot be edited."),
                cx,
            );
            return false;
        }
        let Some(queued_message) = self.remove_from_queue(id, cx) else {
            return false;
        };
        let queued_content = queued_message.content;
        let message_editor = self.message_editor.clone();

        window.focus(&message_editor.focus_handle(cx), cx);

        let adjusted_cursor_offset = if message_editor.read(cx).is_empty(cx) {
            message_editor.update(cx, |editor, cx| {
                editor.set_message(queued_content, window, cx);
            });
            cursor_offset
        } else {
            let existing_len = message_editor.read(cx).text(cx).len();
            let separator = "\n\n";
            message_editor.update(cx, |editor, cx| {
                editor.append_message(queued_content, Some(separator), window, cx);
            });
            cursor_offset.map(|offset| existing_len + separator.len() + offset)
        };

        message_editor.update(cx, |editor, cx| {
            if let Some(offset) = adjusted_cursor_offset {
                editor.set_cursor_offset(offset, window, cx);
            }
            match attempt {
                Some(InputAttempt::Text(text)) => {
                    editor.insert_text(&text, window, cx);
                }
                Some(InputAttempt::Paste(clipboard)) => {
                    editor.paste_item(&clipboard, window, cx);
                }
                None => {}
            }
        });

        cx.notify();
        true
    }

    fn handle_message_editor_move_up(
        &mut self,
        _: &zed_actions::editor::MoveUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.message_editor.read(cx).is_empty(cx) {
            cx.propagate();
            return;
        }
        let Some(last_id) = self.message_queue.last_id() else {
            cx.propagate();
            return;
        };
        self.move_queued_message_to_main_editor(last_id, None, None, window, cx);
    }

    // editor methods

    pub fn expand_message_editor(
        &mut self,
        _: &ExpandMessageEditor,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_visible_conversation(cx) {
            return;
        }
        self.set_editor_is_expanded(!self.editor_expanded, cx);
        cx.stop_propagation();
        cx.notify();
    }

    pub fn set_editor_is_expanded(&mut self, is_expanded: bool, cx: &mut Context<Self>) {
        self.editor_expanded = is_expanded;
        self.sync_editor_mode(cx);
        cx.notify();
    }

    fn title_editor_version(editor: &Editor, cx: &App) -> Option<(gpui::EntityId, clock::Global)> {
        editor
            .buffer()
            .read(cx)
            .as_singleton()
            .map(|buffer| (buffer.entity_id(), buffer.read(cx).version()))
    }

    pub(super) fn sync_title_editor(
        &mut self,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_editor.read(cx).text(cx) == title {
            return;
        }
        self.title_editor_sync_version = self.title_editor.update(cx, |editor, cx| {
            editor.set_text(title, window, cx);
            Self::title_editor_version(editor, cx)
        });
    }

    pub fn handle_title_editor_event(
        &mut self,
        title_editor: &Entity<Editor>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorEvent::BufferEdited => {
                if !title_editor.read(cx).is_focused(window) {
                    return;
                }

                // BufferEdited has no origin; equal text can still be an explicit user rename.
                let version = Self::title_editor_version(title_editor.read(cx), cx);
                if version.is_some() && version == self.title_editor_sync_version {
                    return;
                }
                let new_title = title_editor.read(cx).text(cx);
                if new_title.is_empty() {
                    return;
                }
                self.apply_renamed_title(SharedString::from(new_title), cx);
            }
            EditorEvent::Blurred => {
                if title_editor.read(cx).text(cx).is_empty() {
                    self.sync_title_editor(DEFAULT_THREAD_TITLE.into(), window, cx);
                }
            }
            _ => {}
        }
    }

    /// Renames the thread, mirroring the editor text and persisting the new
    /// title. Used by callers outside of the title editor (e.g. the sidebar's
    /// inline rename) so that they go through the same persistence path as
    /// the in-thread title editor.
    pub fn rename(&mut self, title: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_title_editor(title.clone(), window, cx);
        self.apply_renamed_title(title, cx);
    }

    fn apply_renamed_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        if let Some(store) = ThreadMetadataStore::try_global(cx)
            && !self.is_subagent()
        {
            let thread_id = self.root_thread_id;
            store.update(cx, |store, cx| {
                store.set_title_override(thread_id, title.clone(), cx);
            });
        }
        self.thread.update(cx, |thread, cx| {
            if thread.can_set_title(cx) {
                thread.set_title(title, cx).detach_and_log_err(cx);
            }
        });
    }

    pub fn cancel_editing(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.editing_message.take()
            && let Some(editor) = &self
                .entry_view_state
                .read(cx)
                .entry(index)
                .and_then(|e| e.message_editor())
                .cloned()
        {
            editor.update(cx, |editor, cx| {
                if let Some(user_message) = self
                    .thread
                    .read(cx)
                    .entries()
                    .get(index)
                    .and_then(|e| e.user_message())
                {
                    editor.set_source_message(
                        user_message.content.source_blocks().to_vec(),
                        window,
                        cx,
                    );
                }
            })
        };
        self.message_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub fn authorize_permission_request(
        &mut self,
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
        outcome: SelectedPermissionOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conversation.update(cx, |conversation, cx| {
            conversation.authorize_permission_request(session_id, request_id, outcome, cx);
        });
        if self.should_be_following {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.follow(CollaboratorId::Agent, window, cx);
                })
                .ok();
        }
        cx.notify();
    }

    pub fn allow_always(&mut self, _: &AllowAlways, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_allow_blocked_by_confusables(cx) {
            return;
        }
        self.authorize_pending_tool_call(acp_v2::PermissionOptionKind::AllowAlways, window, cx);
    }

    pub fn allow_once(&mut self, _: &AllowOnce, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_allow_blocked_by_confusables(cx) {
            return;
        }
        self.authorize_pending_with_granularity(true, window, cx);
    }

    /// Whether the currently pending permission prompt is blocked by an
    /// unacknowledged surprising-Unicode warning, so the keyboard allow
    /// shortcuts must be ignored (mirroring the disabled allow buttons).
    fn pending_allow_blocked_by_confusables(&self, cx: &Context<Self>) -> bool {
        let session_id = self.thread.read(cx).session_id().clone();
        let Some((_, tool_call_id, _)) = self
            .conversation
            .read(cx)
            .pending_tool_call(&session_id, cx)
        else {
            return false;
        };
        self.thread.read(cx).entries().iter().any(|entry| {
            matches!(
                entry,
                AgentThreadEntry::ToolCall(call)
                    if call.id == tool_call_id && self.sandbox_confusables_block_allow(call, cx)
            )
        })
    }

    pub fn reject_once(&mut self, _: &RejectOnce, window: &mut Window, cx: &mut Context<Self>) {
        self.authorize_pending_with_granularity(false, window, cx);
    }

    pub fn authorize_pending_tool_call(
        &mut self,
        kind: acp_v2::PermissionOptionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let session_id = self.thread.read(cx).session_id().clone();
        self.conversation.update(cx, |conversation, cx| {
            conversation.authorize_pending_tool_call(&session_id, kind, cx)
        })?;
        if self.should_be_following {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.follow(CollaboratorId::Agent, window, cx);
                })
                .ok();
        }
        cx.notify();
        Some(())
    }

    fn has_pending_request_elicitation(&self, cx: &App) -> bool {
        self.server_view
            .read_with(cx, |server_view, cx| {
                server_view
                    .request_elicitation_store()
                    .is_some_and(|store| {
                        store.read(cx).elicitations().iter().any(|elicitation| {
                            matches!(elicitation.status, ElicitationStatus::Pending { .. })
                        })
                    })
            })
            .unwrap_or(false)
    }

    pub fn sync_elicitation_state_for_entry(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let elicitation_id = {
            let thread = self.thread.read(cx);
            let Some(AgentThreadEntry::Elicitation(elicitation_id)) = thread.entries().get(index)
            else {
                return;
            };
            elicitation_id.clone()
        };

        let thread = self.thread.read(cx);
        let entry = thread.elicitation(&elicitation_id).map(|(_, elicitation)| {
            (
                elicitation_id.clone(),
                matches!(elicitation.status, ElicitationStatus::Pending { .. }),
                match &elicitation.request.mode {
                    acp_v2::ElicitationMode::Form(mode) => Some(mode.requested_schema.clone()),
                    _ => None,
                },
            )
        });

        let Some((id, is_pending, schema)) = entry else {
            return;
        };

        if is_pending
            && let Some(schema) = schema
            && !self.elicitation_form_states.contains_key(&id)
        {
            self.elicitation_form_states
                .insert(id, ElicitationFormState::new(&schema, window, cx));
        } else if !is_pending {
            self.elicitation_form_states.remove(&id);
        }
    }

    fn sync_existing_elicitation_states(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entry_count = self.thread.read(cx).entries().len();
        for index in 0..entry_count {
            self.sync_elicitation_state_for_entry(index, window, cx);
        }
    }

    #[cfg(test)]
    pub(crate) fn has_elicitation_form_state(&self, id: &ElicitationEntryId) -> bool {
        self.elicitation_form_states.contains_key(id)
    }

    fn submit_elicitation(
        &mut self,
        elicitation_id: ElicitationEntryId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self
            .thread
            .read(cx)
            .elicitation(&elicitation_id)
            .map(|(_, elicitation)| elicitation.request.mode.clone());

        let Some(mode) = mode else {
            return;
        };

        match mode {
            acp_v2::ElicitationMode::Form(mode) => {
                let Some(state) = self.elicitation_form_states.get_mut(&elicitation_id) else {
                    return;
                };
                let Some(submission) = state.begin_submission(cx) else {
                    return;
                };
                let schema = mode.requested_schema;
                let validation_task = cx.background_spawn(async move {
                    let result = submission.validate(&schema);
                    (submission, result)
                });
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let (submission, result) = validation_task.await;
                    this.update(cx, |this, cx| {
                        let is_current = this
                            .elicitation_form_states
                            .get_mut(&elicitation_id)
                            .is_some_and(|state| {
                                state.validation_matches_current_values(&submission, cx)
                            });
                        if !is_current {
                            cx.notify();
                            return;
                        }
                        match result {
                            Ok(content) => {
                                this.respond_to_elicitation(
                                    elicitation_id,
                                    acp_v2::CreateElicitationResponse::new(
                                        acp_v2::ElicitationAction::Accept(
                                            acp_v2::ElicitationAcceptAction::new().content(content),
                                        ),
                                    ),
                                    cx,
                                );
                            }
                            Err(errors) => {
                                if let Some(state) =
                                    this.elicitation_form_states.get_mut(&elicitation_id)
                                {
                                    state.set_errors(errors);
                                }
                                cx.notify();
                            }
                        }
                    })
                    .log_err();
                })
                .detach();
            }
            acp_v2::ElicitationMode::Url(_) => {
                self.respond_to_elicitation(
                    elicitation_id,
                    acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Accept(
                        acp_v2::ElicitationAcceptAction::new(),
                    )),
                    cx,
                );
            }
            _ => {}
        }
    }

    fn decline_elicitation(
        &mut self,
        elicitation_id: ElicitationEntryId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.respond_to_elicitation(
            elicitation_id,
            acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Decline),
            cx,
        );
    }

    fn cancel_elicitation(
        &mut self,
        elicitation_id: ElicitationEntryId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.respond_to_elicitation(
            elicitation_id,
            acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Cancel),
            cx,
        );
    }

    fn dismiss_url_elicitation(
        &mut self,
        elicitation_id: ElicitationEntryId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.elicitation_form_states.remove(&elicitation_id);
        self.thread.update(cx, |thread, cx| {
            thread.cancel_elicitation(&elicitation_id, cx);
        });
        cx.notify();
    }

    fn respond_to_elicitation(
        &mut self,
        elicitation_id: ElicitationEntryId,
        response: acp_v2::CreateElicitationResponse,
        cx: &mut Context<Self>,
    ) {
        let session_id = self.session_id.clone();
        self.elicitation_form_states.remove(&elicitation_id);
        self.conversation.update(cx, |conversation, cx| {
            conversation.respond_to_elicitation(session_id, elicitation_id, response, cx);
        });
        cx.notify();
    }

    fn handle_authorize_tool_call(
        &mut self,
        action: &AuthorizeToolCall,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, request_id)) = self.permission_action_target(
            action.session_id.as_deref(),
            action.request_id,
            &action.tool_call_id,
            cx,
        ) else {
            return;
        };
        let option_id = acp_v2::PermissionOptionId::new(action.option_id.clone());
        let option_kind = match action.option_kind.as_str() {
            "AllowOnce" => acp_v2::PermissionOptionKind::AllowOnce,
            "AllowAlways" => acp_v2::PermissionOptionKind::AllowAlways,
            "RejectOnce" => acp_v2::PermissionOptionKind::RejectOnce,
            "RejectAlways" => acp_v2::PermissionOptionKind::RejectAlways,
            _ => return,
        };

        self.authorize_permission_request(
            session_id,
            request_id,
            SelectedPermissionOutcome::new(option_id, option_kind),
            window,
            cx,
        );
    }

    fn permission_action_target(
        &self,
        session_id: Option<&str>,
        request_id: Option<PermissionRequestId>,
        tool_call_id: &str,
        cx: &App,
    ) -> Option<(acp_v2::SessionId, PermissionRequestId)> {
        let session_id = session_id
            .map(acp_v2::SessionId::new)
            .unwrap_or_else(|| self.thread.read(cx).session_id().clone());
        let conversation = self.conversation.read(cx);
        let request = if let Some(id) = request_id {
            conversation.permission_request(&session_id, id, cx)?
        } else {
            conversation
                .threads
                .get(&session_id)?
                .read(cx)
                .permission_request_for_tool(&acp_v2::ToolCallId::new(tool_call_id))?
        };
        request.legacy_options()?;
        Some((session_id, request.id))
    }

    pub fn handle_select_permission_granularity(
        &mut self,
        action: &SelectPermissionGranularity,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, request_id)) = self.permission_action_target(
            action.session_id.as_deref(),
            action.request_id,
            &action.tool_call_id,
            cx,
        ) else {
            return;
        };
        self.conversation.update(cx, |conversation, cx| {
            conversation.set_permission_choice(&session_id, request_id, action.index, cx);
        });
    }

    pub fn handle_toggle_command_pattern(
        &mut self,
        action: &crate::ToggleCommandPattern,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, request_id)) = self.permission_action_target(
            action.session_id.as_deref(),
            action.request_id,
            &action.tool_call_id,
            cx,
        ) else {
            return;
        };
        self.conversation.update(cx, |conversation, cx| {
            conversation.toggle_permission_pattern(
                &session_id,
                request_id,
                action.pattern_index,
                cx,
            );
        });
    }

    fn authorize_pending_with_granularity(
        &mut self,
        is_allow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let session_id = self.thread.read(cx).session_id().clone();
        let (returned_session_id, request) = self
            .conversation
            .read(cx)
            .pending_permission_request(&session_id, cx)?;
        self.authorize_with_granularity(returned_session_id, request.id, is_allow, window, cx)
    }

    fn authorize_with_granularity(
        &mut self,
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
        is_allow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        self.conversation.update(cx, |conversation, cx| {
            conversation.authorize_with_granularity(session_id, request_id, is_allow, cx)
        })?;
        if self.should_be_following {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.follow(CollaboratorId::Agent, window, cx);
                })
                .ok();
        }
        cx.notify();
        Some(())
    }

    // edits

    pub fn keep_all(&mut self, _: &KeepAll, _window: &mut Window, cx: &mut Context<Self>) {
        let thread = &self.thread;
        let telemetry = ActionLogTelemetry::from(thread.read(cx));
        let action_log = thread.read(cx).action_log().clone();
        action_log.update(cx, |action_log, cx| {
            action_log.keep_all_edits(Some(telemetry), cx)
        });
    }

    pub fn reject_all(&mut self, _: &RejectAll, _window: &mut Window, cx: &mut Context<Self>) {
        let thread = &self.thread;
        let telemetry = ActionLogTelemetry::from(thread.read(cx));
        let action_log = thread.read(cx).action_log().clone();
        let has_changes = action_log.read(cx).changed_buffers(cx).next().is_some();

        action_log
            .update(cx, |action_log, cx| {
                action_log.reject_all_edits(Some(telemetry), cx)
            })
            .detach();

        if has_changes {
            if let Some(workspace) = self.workspace.upgrade() {
                workspace.update(cx, |workspace, cx| {
                    crate::ui::show_undo_reject_toast(workspace, action_log, cx);
                });
            }
        }
    }

    pub fn undo_last_reject(
        &mut self,
        _: &UndoLastReject,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread = &self.thread;
        let action_log = thread.read(cx).action_log().clone();
        action_log
            .update(cx, |action_log, cx| action_log.undo_last_reject(cx))
            .detach()
    }

    pub fn open_edited_buffer(
        &mut self,
        buffer: &Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread = &self.thread;

        let Some(diff) =
            AgentDiffPane::deploy(thread.clone(), self.workspace.clone(), window, cx).log_err()
        else {
            return;
        };

        diff.update(cx, |diff, cx| {
            diff.move_to_path(PathKey::for_buffer(buffer, cx), window, cx)
        })
    }

    // thread stuff

    pub fn restore_checkpoint(&mut self, client_id: &ClientUserMessageId, cx: &mut Context<Self>) {
        self.thread
            .update(cx, |thread, cx| {
                telemetry::event!(
                    "Agent Checkpoint Restored",
                    agent = thread.connection().telemetry_id(),
                    session = thread.session_id().clone(),
                );
                thread.restore_checkpoint(client_id.clone(), cx)
            })
            .detach_and_log_err(cx);
    }

    pub fn clear_thread_error(&mut self, cx: &mut Context<Self>) {
        self.thread_error = None;
        self.thread_error_markdown = None;
        self.token_limit_callout_dismissed = true;
        cx.notify();
    }

    fn is_following(&self, cx: &App) -> bool {
        match self.thread.read(cx).status() {
            ThreadStatus::Generating => self
                .workspace
                .read_with(cx, |workspace, _| {
                    workspace.is_being_followed(CollaboratorId::Agent)
                })
                .unwrap_or(false),
            _ => self.should_be_following,
        }
    }

    fn toggle_following(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let following = self.is_following(cx);

        self.should_be_following = !following;
        if self.thread.read(cx).status() == ThreadStatus::Generating {
            self.workspace
                .update(cx, |workspace, cx| {
                    if following {
                        workspace.unfollow(CollaboratorId::Agent, window, cx);
                    } else {
                        workspace.follow(CollaboratorId::Agent, window, cx);
                    }
                })
                .ok();
        }

        telemetry::event!("Follow Agent Selected", following = !following);
    }

    fn render_thread_retry_status_callout(&self, cx: &mut Context<Self>) -> Option<AlertCard> {
        let state = self.thread_retry_status.as_ref()?;

        if let Some(fallback_model) = acp_thread::refusal_fallback_model_from_meta(&state.meta) {
            return Some(
                AlertCard::new(
                    "refusal-fallback",
                    AlertVariant::Warning,
                    state.last_error.clone(),
                )
                .description(format!("Retrying with {fallback_model}"))
                .on_dismiss(cx.listener(|this, _, _, cx| {
                    this.thread_retry_status = None;
                    cx.notify();
                })),
            );
        }

        let next_attempt_in = state
            .duration
            .saturating_sub(Instant::now().saturating_duration_since(state.started_at));
        if next_attempt_in.is_zero() {
            return None;
        }

        let next_attempt_in_secs = next_attempt_in.as_secs() + 1;

        let retry_message = if state.max_attempts == 1 {
            if next_attempt_in_secs == 1 {
                "Retrying. Next attempt in 1 second.".to_string()
            } else {
                format!("Retrying. Next attempt in {next_attempt_in_secs} seconds.")
            }
        } else if next_attempt_in_secs == 1 {
            format!(
                "Retrying. Next attempt in 1 second (Attempt {} of {}).",
                state.attempt, state.max_attempts,
            )
        } else {
            format!(
                "Retrying. Next attempt in {next_attempt_in_secs} seconds (Attempt {} of {}).",
                state.attempt, state.max_attempts,
            )
        };

        Some(
            AlertCard::new(
                "retry-status",
                AlertVariant::Warning,
                state.last_error.clone(),
            )
            .description(retry_message),
        )
    }

    fn collect_subagent_items_for_sessions(
        entries: &[AgentThreadEntry],
        awaiting_session_ids: &[acp_v2::SessionId],
        cx: &App,
    ) -> Vec<(SharedString, usize)> {
        let tool_calls_by_session: HashMap<_, _> = entries
            .iter()
            .enumerate()
            .filter_map(|(entry_ix, entry)| {
                let AgentThreadEntry::ToolCall(tool_call) = entry else {
                    return None;
                };
                let info = tool_call.subagent_session_info.as_ref()?;
                let summary_text = tool_call.label.read(cx).source().to_string();
                let subagent_summary = if summary_text.is_empty() {
                    SharedString::from("Subagent")
                } else {
                    SharedString::from(summary_text)
                };
                Some((info.session_id.clone(), (subagent_summary, entry_ix)))
            })
            .collect();

        awaiting_session_ids
            .iter()
            .filter_map(|session_id| tool_calls_by_session.get(session_id).cloned())
            .collect()
    }

    fn render_generic_permissions(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let conversation = self.conversation.read(cx);
        let mut cards = Vec::new();
        for (session_id, request_ids) in &conversation.permission_requests {
            if self.is_subagent() && session_id != &self.session_id {
                continue;
            }
            let Some(thread) = conversation.threads.get(session_id) else {
                continue;
            };
            let thread = thread.read(cx);
            for request_id in request_ids {
                let Some(request) = thread
                    .permission_request(*request_id)
                    .and_then(PermissionRequest::generic_request)
                else {
                    continue;
                };
                let source = if session_id == &self.session_id {
                    None
                } else {
                    Some(format!(
                        "Subagent: {} ({session_id})",
                        thread.title().unwrap_or_else(|| "Subagent".into())
                    ))
                };
                cards.push(self.render_generic_permission_card(
                    session_id.clone(),
                    *request_id,
                    request,
                    source,
                    cx,
                ));
            }
        }
        if cards.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .id("generic-permissions")
                .max_h(px(320.0))
                .overflow_y_scroll()
                .children(cards)
                .into_any(),
        )
    }

    pub(super) fn render_generic_permission_card(
        &self,
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
        request: &acp_v2::RequestPermissionRequest,
        source: Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let subject = match &request.subject {
            Some(acp_v2::RequestPermissionSubject::Command(command)) => Some(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .debug_selector(|| {
                                format!("generic-permission-command-{}", command.command)
                            })
                            .child(command.command.clone()),
                    )
                    .child(format!("Working directory: {}", command.cwd.0.display())),
            ),
            Some(acp_v2::RequestPermissionSubject::ToolCall(subject)) => Some(
                v_flex()
                    .debug_selector(|| {
                        format!("generic-permission-tool-{}", subject.tool_call.tool_call_id)
                    })
                    .child(format!("Tool call: {}", subject.tool_call.tool_call_id)),
            ),
            Some(acp_v2::RequestPermissionSubject::Other(subject)) => Some(
                v_flex()
                    .debug_selector(|| format!("generic-permission-unknown-{}", subject.type_))
                    .child(format!("Unknown permission subject: {}", subject.type_)),
            ),
            Some(_) => Some(v_flex().child("Unknown permission subject")),
            None => None,
        };
        v_flex()
            .id(format!("generic-permission-{request_id:?}"))
            .debug_selector(|| format!("generic-permission-{request_id:?}"))
            .p_2()
            .gap_2()
            .w_full()
            .min_w_0()
            .text_ui_sm(cx)
            .children(source.map(|source| {
                div()
                    .debug_selector(|| format!("generic-permission-source-{session_id}"))
                    .text_color(cx.theme().colors().text_muted)
                    .child(source)
            }))
            .child(
                div()
                    .debug_selector(|| format!("generic-permission-title-{}", request.title))
                    .child(request.title.clone()),
            )
            .children(request.description.as_ref().map(|description| {
                div()
                    .debug_selector(|| format!("generic-permission-description-{description}"))
                    .child(description.clone())
            }))
            .children(subject)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .flex_wrap()
                    .children(request.options.iter().map(|option| {
                        let option_id = option.option_id.clone();
                        let session_id = session_id.clone();
                        div()
                            .min_w_0()
                            .max_w_full()
                            .debug_selector(|| {
                                format!("generic-permission-option-{request_id:?}-{option_id}")
                            })
                            .child(
                                Button::new(
                                    format!("generic-permission-option-{request_id:?}-{option_id}"),
                                    option.name.clone(),
                                )
                                .label_size(LabelSize::Small)
                                .full_width()
                                .truncate(true)
                                .tooltip(Tooltip::text(option.name.clone()))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.conversation.update(cx, |conversation, cx| {
                                            conversation.select_permission_option(
                                                &session_id,
                                                request_id,
                                                option_id.clone(),
                                                cx,
                                            );
                                        });
                                        cx.notify();
                                    },
                                )),
                            )
                    }))
                    .child(
                        div()
                            .debug_selector(|| format!("generic-permission-cancel-{request_id:?}"))
                            .child(
                                Button::new(
                                    format!("generic-permission-cancel-{request_id:?}"),
                                    "Cancel",
                                )
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.conversation.update(cx, |conversation, cx| {
                                            conversation.cancel_permission_request(
                                                &session_id,
                                                request_id,
                                                cx,
                                            );
                                        });
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            )
            .into_any()
    }

    fn render_subagents_awaiting_permission(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let awaiting = self.conversation.read(cx).subagents_awaiting_permission(cx);

        if awaiting.is_empty() {
            return None;
        }

        let awaiting_session_ids: Vec<_> = awaiting
            .iter()
            .map(|(session_id, _)| session_id.clone())
            .collect();

        let thread = self.thread.read(cx);
        let entries = thread.entries();
        let subagent_items =
            Self::collect_subagent_items_for_sessions(entries, &awaiting_session_ids, cx);

        if subagent_items.is_empty() {
            return None;
        }

        let item_count = subagent_items.len();
        let theme = KitTheme::global(cx);

        Some(
            v_flex()
                .text_size(rems_from_px(13_f32))
                .text_color(theme.muted_foreground)
                .child(
                    h_flex()
                        .h_8()
                        .pl_3()
                        .pr_1p5()
                        .w_full()
                        .gap_1()
                        .child("Subagents Awaiting Permission:")
                        .child(
                            div()
                                .text_color(theme.foreground)
                                .child(item_count.to_string()),
                        ),
                )
                .child(v_flex().pb_1().children(subagent_items.into_iter().map(
                    |(label, entry_ix)| {
                        let group = format!("group-{}", entry_ix);

                        h_flex()
                            .cursor_pointer()
                            .id(format!("subagent-permission-{}", entry_ix))
                            .group(&group)
                            .h_7()
                            .pl_3()
                            .pr_2()
                            .min_w_0()
                            .w_full()
                            .gap_1p5()
                            .justify_between()
                            .text_xs()
                            .hover(|style| style.bg(theme.list_hover))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1p5()
                                    .child(
                                        transcript::kit_icon(IconName::Circle)
                                            .with_size(KitSize::XSmall)
                                            .text_color(theme.warning)
                                            .flex_none(),
                                    )
                                    .child(div().min_w_0().truncate().child(label)),
                            )
                            .child(
                                div()
                                    .id("scroll-to-subagent")
                                    .flex_none()
                                    .visible_on_hover(&group)
                                    .child("Scroll to Subagent"),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.list_state.scroll_to(ListOffset {
                                    item_ix: entry_ix,
                                    offset_in_item: px(0.0),
                                });
                                cx.notify();
                            }))
                    },
                )))
                .into_any(),
        )
    }

    pub(crate) fn render_main_agent_awaiting_permission(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if self.is_subagent() {
            return None;
        }

        let active_session_id = self.thread.read(cx).session_id().clone();
        let conversation = self.conversation.read(cx);
        let tool_call_id = conversation.pending_tool_call_for_session(&active_session_id, cx)?;
        let pending_count =
            conversation.pending_tool_call_count_for_session(&active_session_id, cx);

        let thread = self.thread.read(cx);
        let (entry_ix, tool_call) = thread.tool_call(&tool_call_id)?;

        let scroll_icon = if self.list_state.item_is_above_viewport(entry_ix)? {
            IconName::ArrowUp
        } else if self.list_state.item_is_below_viewport(entry_ix)? {
            IconName::ArrowDown
        } else {
            return None;
        };

        let focus_handle = self.focus_handle(cx);

        let card = self.render_any_tool_call(
            &active_session_id,
            entry_ix,
            tool_call,
            &focus_handle,
            ToolCallLayout::Floating,
            window,
            cx,
        );

        let label: SharedString = if pending_count > 1 {
            format!("Awaiting Confirmation ({pending_count})").into()
        } else {
            "Awaiting Confirmation".into()
        };

        let theme = KitTheme::global(cx);
        let header = h_flex()
            .h_8()
            .pl_3()
            .pr_1p5()
            .w_full()
            .gap_1p5()
            .justify_between()
            .text_size(rems_from_px(13_f32))
            .text_color(theme.muted_foreground)
            .child(ShimmerText::new(label))
            .child(
                PillButton::new("main-agent-permission-scroll-to")
                    .size(KitSize::XSmall)
                    .icon(scroll_icon)
                    .label("Scroll")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.list_state.scroll_to(ListOffset {
                            item_ix: entry_ix,
                            offset_in_item: px(0.0),
                        });
                        cx.notify();
                    })),
            );

        Some(v_flex().child(header).child(card).into_any())
    }

    fn clear_queue(&mut self, cx: &mut Context<Self>) {
        self.message_queue.clear();
        self.sync_queue_flag_to_native_thread(cx);
        cx.notify();
    }

    /// Call before expanding or collapsing something in the transcript. While
    /// tail-following, the list keeps its bottom pinned, so a row that grows in
    /// place pushes itself up under the cursor instead of opening downward.
    /// Collapsing at the bottom lands in the same place either way, and the list
    /// resumes following on its own once it is back at the bottom.
    pub(super) fn hold_scroll_position_for_toggle(&self) {
        self.list_state.pause_following_tail();
    }

    pub(super) fn toggle_compaction_expansion(
        &mut self,
        entry_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hold_scroll_position_for_toggle();
        self.entry_view_state.update(cx, |state, _cx| {
            state.toggle_compaction_expansion(entry_ix);
        });
        self.list_state.remeasure_items(entry_ix..entry_ix + 1);
        self.refresh_thread_search(window, cx);
        cx.notify();
    }

    fn is_subagent_canceled_or_failed(&self, cx: &App) -> bool {
        let Some(parent_session_id) = self.parent_session_id.as_ref() else {
            return false;
        };

        let my_session_id = self.thread.read(cx).session_id().clone();

        self.server_view
            .upgrade()
            .and_then(|sv| sv.read(cx).thread_view(parent_session_id))
            .is_some_and(|parent_view| {
                parent_view
                    .read(cx)
                    .thread
                    .read(cx)
                    .tool_call_for_subagent(&my_session_id)
                    .is_some_and(|tc| {
                        matches!(
                            tc.status(),
                            ToolCallStatus::Canceled
                                | ToolCallStatus::Failed
                                | ToolCallStatus::Rejected
                        )
                    })
            })
    }

    pub(crate) fn render_subagent_titlebar(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if self.parent_session_id.is_none() {
            return None;
        }
        let parent_session_id = self.thread.read(cx).parent_session_id()?.clone();

        let server_view = self.server_view.clone();
        let thread = self.thread.clone();
        let is_done = thread.read(cx).status() == ThreadStatus::Idle;
        let is_canceled_or_failed = self.is_subagent_canceled_or_failed(cx);

        let max_content_width = AgentSettings::get_global(cx).max_content_width;

        Some(
            h_flex()
                .w_full()
                .h(Tab::container_height(cx))
                .border_b_1()
                .when(is_done && is_canceled_or_failed, |this| {
                    this.border_dashed()
                })
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().editor_background.opacity(0.2))
                .child(
                    h_flex()
                        .size_full()
                        .when_some(max_content_width, |this, max_w| this.max_w(max_w).mx_auto())
                        .pl_2()
                        .pr_1()
                        .flex_shrink_0()
                        .justify_between()
                        .gap_1()
                        .child(
                            h_flex()
                                .flex_1()
                                .gap_2()
                                .child(
                                    Icon::new(IconName::ForwardArrowUp)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(self.title_editor.clone())
                                .when(is_done && is_canceled_or_failed, |this| {
                                    this.child(Icon::new(IconName::Close).color(Color::Error))
                                })
                                .when(is_done && !is_canceled_or_failed, |this| {
                                    this.child(Icon::new(IconName::Check).color(Color::Success))
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_0p5()
                                .when(!is_done, |this| {
                                    this.child(
                                        IconButton::new("stop_subagent", IconName::Stop)
                                            .icon_size(IconSize::Small)
                                            .icon_color(Color::Error)
                                            .tooltip(Tooltip::text("Stop Subagent"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.cancel_thread(&thread, cx).detach();
                                            })),
                                    )
                                })
                                .child(
                                    IconButton::new("minimize_subagent", IconName::Dash)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text("Minimize Subagent"))
                                        .on_click(move |_, window, cx| {
                                            let _ = server_view.update(cx, |server_view, cx| {
                                                server_view.navigate_to_thread(
                                                    parent_session_id.clone(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }),
                                ),
                        ),
                ),
        )
    }

    fn fast_mode_available(&self, cx: &Context<Self>) -> bool {
        self.as_native_thread(cx)
            .and_then(|thread| thread.read(cx).model())
            .map(|model| model.supports_fast_mode())
            .unwrap_or(false)
    }

    fn refresh_sandbox_status(&mut self, cx: &mut Context<Self>) -> Option<VerifiedSandboxStatus> {
        let thread = self.as_native_thread(cx)?;
        let (key, refresh) =
            thread.update(cx, |thread, cx| thread.refresh_verified_sandbox_status(cx))?;

        if self.sandbox_status_key.as_ref() == Some(&key) {
            return self.sandbox_status.clone();
        }

        match refresh {
            SandboxStatusRefresh::Ready(status) => {
                self.sandbox_status = Some(status.clone());
                self.sandbox_status_key = Some(key);
                self.pending_sandbox_status_key = None;
                Some(status)
            }
            SandboxStatusRefresh::Pending(task) => {
                if self.pending_sandbox_status_key.as_ref() != Some(&key) {
                    self.sandbox_status = None;
                    self.sandbox_status_key = None;
                    self.pending_sandbox_status_key = Some(key.clone());
                    self._sandbox_status_refresh_task = Some(cx.spawn(async move |this, cx| {
                        let status = task.await;
                        this.update(cx, |this, cx| {
                            if this.pending_sandbox_status_key.as_ref() == Some(&key) {
                                this.sandbox_status = Some(status);
                                this.sandbox_status_key = Some(key);
                                this.pending_sandbox_status_key = None;
                                cx.notify();
                            }
                        })
                        .ok();
                    }));
                }
                None
            }
        }
    }

    /// The thread's permissions, chosen from a menu that also shows what the
    /// sandbox lets the thread do.
    pub fn render_permissions_control(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = self.as_native_thread(cx)?;
        let permissions = thread.read(cx).permissions();
        let details = self.sandbox_details(&thread, permissions, cx);
        let icon = if permissions == AgentPermissions::FullAccess
            || matches!(details, Some(SandboxDetails::UnsandboxedForThread))
        {
            IconName::LockOff
        } else {
            IconName::Lock
        };
        let menu = self.permissions_menu.clone();

        Some(
            h_flex()
                .gap_1()
                .child(
                    Popover::new("thread-permissions")
                        .appearance(false)
                        .overlay_closable(false)
                        .anchor(gpui::Anchor::BottomRight)
                        .offset(px(6.))
                        .open(menu.is_some())
                        .on_open_change(cx.listener(|this, open: &bool, window, cx| {
                            if *open {
                                this.open_permissions_menu(window, cx);
                            } else {
                                this.close_permissions_menu(cx);
                            }
                        }))
                        .trigger(
                            KitButton::new("thread-permissions-trigger")
                                .ghost()
                                .label(<&'static str>::from(permissions))
                                .icon(KitIcon::empty().path(icon.path()))
                                .dropdown_caret(true),
                        )
                        .content(move |_, _, _| match &menu {
                            Some(menu) => menu.clone().into_any_element(),
                            None => div().into_any_element(),
                        }),
                )
                .child(Separator::vertical().h_4())
                .into_any_element(),
        )
    }

    fn open_permissions_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.permissions_menu.is_some() {
            return;
        }
        let Some(thread) = self.as_native_thread(cx) else {
            return;
        };
        let permissions = thread.read(cx).permissions();
        let details = self.sandbox_details(&thread, permissions, cx);
        let this = cx.weak_entity();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            permissions_menu(menu, permissions, details.as_ref(), &this)
        });
        self.show_permissions_menu(menu, window, cx);
    }

    /// Also swaps the confirmation in for the menu, so the popover stays open
    /// while the clicked menu dismisses itself.
    fn show_permissions_menu(
        &mut self,
        menu: Entity<PopupMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._permissions_menu_subscription = Some(cx.subscribe_in(
            &menu,
            window,
            |this, menu, _: &gpui::DismissEvent, _, cx| {
                if this.permissions_menu.as_ref() == Some(menu) {
                    this.close_permissions_menu(cx);
                }
            },
        ));
        menu.focus_handle(cx).focus(window, cx);
        self.permissions_menu = Some(menu);
        cx.notify();
    }

    fn close_permissions_menu(&mut self, cx: &mut Context<Self>) {
        self._permissions_menu_subscription = None;
        if self.permissions_menu.take().is_some() {
            cx.notify();
        }
    }

    fn select_permissions(
        &mut self,
        permissions: AgentPermissions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.as_native_thread(cx) else {
            return;
        };
        if permissions == AgentPermissions::FullAccess
            && thread.read(cx).permissions() != AgentPermissions::FullAccess
            && !full_access_warning_dismissed(cx)
        {
            self.confirm_full_access(window, cx);
            return;
        }
        thread.update(cx, |thread, cx| thread.set_permissions(permissions, cx));
    }

    fn confirm_full_access(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let allow_full_access = move |cx: &mut App| {
            this.update(cx, |this, cx| {
                if let Some(thread) = this.as_native_thread(cx) {
                    thread.update(cx, |thread, cx| {
                        thread.set_permissions(AgentPermissions::FullAccess, cx)
                    });
                }
            })
            .log_err();
        };
        let allow_and_dont_ask = allow_full_access.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            menu.item(
                PopupMenuItem::element(|_, cx| {
                    v_flex()
                        .w(PERMISSIONS_MENU_TEXT_WIDTH)
                        .py_1()
                        .gap_0p5()
                        .child(
                            div()
                                .text_color(KitTheme::global(cx).popover_foreground)
                                .child("Give This Thread Full Access?"),
                        )
                        .child(div().text_xs().child(
                            "Its terminal commands will run without a sandbox and without \
                                 asking first, so they can change anything you can.",
                        ))
                })
                .disabled(true),
            )
            .separator()
            .item(
                danger_menu_item("Allow Full Access")
                    .on_click(move |_, _, cx| allow_full_access(cx)),
            )
            .item(
                danger_menu_item("Allow and Don't Ask Again").on_click(move |_, _, cx| {
                    allow_and_dont_ask(cx);
                    set_full_access_warning_dismissed(cx);
                }),
            )
        });
        self.show_permissions_menu(menu, window, cx);
    }

    fn sandbox_details(
        &mut self,
        thread: &Entity<agent::Thread>,
        permissions: AgentPermissions,
        cx: &mut Context<Self>,
    ) -> Option<SandboxDetails> {
        if permissions == AgentPermissions::FullAccess {
            return Some(SandboxDetails::Note(FULL_ACCESS_NOTE));
        }
        if !thread.read(cx).sandboxing_available(cx) {
            return Some(SandboxDetails::Note(match permissions {
                AgentPermissions::ReadOnly => {
                    "This project can't be sandboxed, so terminal commands are refused."
                }
                AgentPermissions::Workspace | AgentPermissions::FullAccess => {
                    "This project can't be sandboxed, so each terminal command asks first."
                }
            }));
        }

        let status = self.refresh_sandbox_status(cx)?;
        Some(match (status.settings_sandbox, status.thread_sandbox) {
            (ThreadSandbox::Unsandboxed, _) => SandboxDetails::Note(FULL_ACCESS_NOTE),
            (ThreadSandbox::Sandboxed(_), ThreadSandbox::Unsandboxed) => {
                SandboxDetails::UnsandboxedForThread
            }
            (
                ThreadSandbox::Sandboxed(settings_policy),
                ThreadSandbox::Sandboxed(thread_policy),
            ) => {
                let thread_grants = SandboxPolicyDisplay::from_policy(&thread_policy).fs;
                SandboxDetails::Sandboxed {
                    writable: augment_settings_sandbox_policy(
                        &settings_policy,
                        status.baseline_writable_paths,
                    )
                    .fs,
                    thread_grants: (!fs_grants_nothing(&thread_grants)).then_some(thread_grants),
                }
            }
        })
    }

    fn render_fast_mode_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.fast_mode_available(cx) {
            return None;
        }

        let thread = self.as_native_thread(cx)?.read(cx);
        let is_fast = matches!(thread.speed(), Some(Speed::Fast));

        let model_identity = thread
            .model()
            .map(|model| (model.provider_id(), model.id()));

        let (tooltip_label, color, icon, new_speed) = if is_fast {
            (
                "Disable Fast Mode",
                Color::Accent,
                IconName::FastForward,
                Speed::Standard,
            )
        } else {
            (
                "Enable Fast Mode",
                Color::Custom(cx.theme().colors().icon_disabled.opacity(0.8)),
                IconName::FastForwardOff,
                Speed::Fast,
            )
        };

        let focus_handle = self.message_editor.focus_handle(cx);

        let pending_confirmation = (!is_fast)
            .then(|| self.pending_fast_mode_confirmation(cx))
            .flatten();

        let icon_button = IconButton::new("fast-mode", icon)
            .icon_size(IconSize::Small)
            .icon_color(color);

        if let Some((provider_id, model_id, confirmation)) = pending_confirmation {
            let weak_self = cx.entity().downgrade();
            let tooltip_focus = focus_handle;

            return Some(
                PopoverMenu::new("fast-mode-warning")
                    .with_handle(self.fast_mode_menu_handle.clone())
                    .trigger_with_tooltip(icon_button, move |_, cx| {
                        Tooltip::for_action_in(tooltip_label, &ToggleFastMode, &tooltip_focus, cx)
                    })
                    .menu(move |window, cx| {
                        let weak_self = weak_self.clone();
                        let confirmation = confirmation.clone();
                        let provider_id = provider_id.clone();
                        let model_id = model_id.clone();

                        Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                            let message = confirmation.message.clone();
                            menu.custom_row(move |_window, _cx| {
                                div()
                                    .max_w_72()
                                    .child(Label::new(confirmation.title.clone()))
                                    .child(Label::new(message.clone()).color(Color::Muted))
                                    .into_any_element()
                            })
                            .separator()
                            .item(ContextMenuEntry::new("Enable Now").handler({
                                let weak_self = weak_self.clone();
                                move |_window, cx| {
                                    weak_self
                                        .update(cx, |this, cx| {
                                            this.apply_fast_mode_speed(Speed::Fast, cx);
                                        })
                                        .log_err();
                                }
                            }))
                            .item(
                                ContextMenuEntry::new("Enable and Don't Show Again").handler({
                                    let weak_self = weak_self.clone();
                                    let provider_id = provider_id.clone();
                                    let model_id = model_id;
                                    move |_window, cx| {
                                        weak_self
                                            .update(cx, |this, cx| {
                                                this.apply_fast_mode_speed(Speed::Fast, cx);
                                            })
                                            .log_err();
                                        set_fast_mode_warning_dismissed(
                                            &provider_id,
                                            &model_id,
                                            cx,
                                        );
                                    }
                                }),
                            )
                        }))
                    })
                    .offset(gpui::Point {
                        x: px(0.0),
                        y: px(-2.0),
                    })
                    .anchor(gpui::Anchor::BottomLeft)
                    .into_any_element(),
            );
        }

        let _ = model_identity;

        Some(
            icon_button
                .tooltip(move |_, cx| {
                    Tooltip::for_action_in(tooltip_label, &ToggleFastMode, &focus_handle, cx)
                })
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.apply_fast_mode_speed(new_speed, cx);
                }))
                .into_any_element(),
        )
    }

    fn pending_fast_mode_confirmation(
        &self,
        cx: &App,
    ) -> Option<(
        LanguageModelProviderId,
        LanguageModelId,
        FastModeConfirmation,
    )> {
        let thread = self.as_native_thread(cx)?.read(cx);
        let model = thread.model()?;
        let provider_id = model.provider_id();
        let model_id = model.id();
        let confirmation = LanguageModelRegistry::read_global(cx)
            .provider(&provider_id)
            .and_then(|provider| provider.fast_mode_confirmation(cx))?;
        if fast_mode_warning_dismissed(&provider_id, &model_id, cx) {
            return None;
        }
        Some((provider_id, model_id, confirmation))
    }

    fn thinking_state(&self, cx: &App) -> Option<(LanguageModel, bool, Option<String>)> {
        let thread = self.as_native_thread(cx)?;
        let thread = thread.read(cx);
        Some((
            thread.model()?.clone(),
            thread.thinking_enabled(),
            thread.thinking_effort().cloned(),
        ))
    }

    fn persist_native_thinking_selection(thread: &Entity<agent::Thread>, cx: &App) {
        let thread = thread.read(cx);
        let Some(model) = thread.model() else {
            return;
        };
        let provider = model.provider_id.0.to_string();
        let model = model.id.0.to_string();
        let enabled = thread.thinking_enabled();
        let effort = thread.thinking_effort().cloned();
        update_settings_file(
            thread.project().read(cx).fs().clone(),
            cx,
            move |settings, _| {
                if let Some(agent) = settings.agent.as_mut() {
                    if let Some(default_model) = agent.default_model.as_mut() {
                        default_model.enable_thinking = enabled;
                        default_model.effort = effort.clone();
                    }
                    agent.update_favorite_model(&provider, &model, |favorite| {
                        favorite.enable_thinking = enabled;
                        favorite.effort = effort;
                    });
                }
            },
        );
    }

    fn toggle_thinking(&mut self, cx: &mut Context<Self>) {
        let Some((model, enabled, effort)) = self.thinking_state(cx) else {
            return;
        };
        if !model.supports_thinking
            || !model.supports_disabling_thinking_at_effort(effort.as_deref())
        {
            return;
        }
        if let Some(thread) = self.as_native_thread(cx) {
            thread.update(cx, |thread, cx| thread.set_thinking_enabled(!enabled, cx));
            Self::persist_native_thinking_selection(&thread, cx);
        }
    }

    fn select_thinking_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        if let Some(thread) = self.as_native_thread(cx) {
            thread.update(cx, |thread, cx| {
                thread.set_thinking_effort(Some(effort), cx)
            });
            Self::persist_native_thinking_selection(&thread, cx);
        }
    }

    /// Renders the model and thinking effort as a single control. The trigger
    /// shows the active model and effort; the menu exposes effort, model, and
    /// thinking as one panel instead of separate toolbar entries.
    fn render_model_and_effort_control(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.model_selector.is_none() {
            return self
                .render_thinking_control(cx)
                .unwrap_or_else(|| div().into_any_element());
        }

        let thinking_state = self.thinking_state(cx);
        let effort_label = thinking_state.as_ref().and_then(|(model, _, effort)| {
            let effort_levels = model.supported_effort_levels();
            effort_levels
                .iter()
                .find(|level| effort.as_deref() == Some(level.value.as_ref()))
                .or_else(|| effort_levels.iter().find(|level| level.is_default))
                .map(|level| level.name.clone())
        });
        let model_name = thinking_state
            .as_ref()
            .map(|(model, _, _)| model.name().0)
            .unwrap_or_else(|| SharedString::from("Select a Model"));
        let trigger_label = match effort_label {
            Some(effort_label) => format!("{model_name} {effort_label}"),
            None => model_name.to_string(),
        };

        let menu = self.model_menu.clone();
        Popover::new("model-effort-selector")
            .appearance(false)
            .overlay_closable(false)
            .anchor(gpui::Anchor::BottomRight)
            .offset(px(6.))
            .open(menu.is_some())
            .on_open_change(cx.listener(|this, open: &bool, window, cx| {
                if *open {
                    this.open_model_menu(window, cx);
                } else {
                    this.close_model_menu(cx);
                }
            }))
            .trigger(
                KitButton::new("model-effort-trigger")
                    .with_variant(ButtonVariant::Ghost)
                    .with_size(KitSize::Small)
                    .rounded(ButtonRounded::Size(px(12.)))
                    .label(trigger_label)
                    .dropdown_caret(true),
            )
            .content(move |_, _, _| match &menu {
                Some(menu) => menu.clone().into_any_element(),
                None => div().into_any_element(),
            })
            .into_any_element()
    }

    /// The menu lives on the thread view rather than in its trigger so the model selector
    /// shortcut can open it too.
    fn open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_menu.is_some() {
            return;
        }
        let Some(model_selector) = self.model_selector.clone() else {
            return;
        };

        let thinking_state = self.thinking_state(cx);
        let thinking_enabled = thinking_state
            .as_ref()
            .is_some_and(|(_, enabled, _)| *enabled);
        let supports_disabling_thinking =
            thinking_state.as_ref().is_some_and(|(model, _, effort)| {
                model.supports_disabling_thinking_at_effort(effort.as_deref())
            });
        let effort_levels = thinking_state
            .as_ref()
            .map(|(model, _, _)| model.supported_effort_levels())
            .unwrap_or_default();
        let selected_effort = thinking_state.and_then(|(_, _, effort)| effort);
        let model_list = model_selector
            .read(cx)
            .available_models(cx)
            .filter(|model_list| !model_list.is_empty());
        let hidden_model_ids = model_selector.read(cx).hidden_model_ids(cx);
        let selected_model_id = model_selector
            .read(cx)
            .active_model(cx)
            .map(|model| model.id.clone());
        let focus_handle = self.message_editor.focus_handle(cx);
        let weak_self = cx.weak_entity();

        let menu = PopupMenu::build(window, cx, move |mut menu, window, cx| {
            menu = menu
                .action_context(focus_handle)
                .check_side(Side::Right)
                .min_w(px(200.));

            if supports_disabling_thinking {
                let weak_self = weak_self.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .flex_1()
                            .gap_4()
                            .justify_between()
                            .child("Thinking")
                            .child(
                                KitSwitch::new("thinking-switch")
                                    .checked(thinking_enabled)
                                    .with_size(KitSize::Small),
                            )
                    })
                    .on_click(move |_, _, cx| {
                        weak_self
                            .update(cx, |this, cx| this.toggle_thinking(cx))
                            .log_err();
                    }),
                );
            }

            if !effort_levels.is_empty() {
                let weak_self = weak_self.clone();
                let selected_effort = selected_effort.clone();
                let effort_levels = effort_levels.clone();
                menu = menu.submenu("Effort", window, cx, move |mut menu, _, _| {
                    menu = menu.check_side(Side::Right);
                    for level in &effort_levels {
                        let weak_self = weak_self.clone();
                        let value = level.value.to_string();
                        menu = menu.item(
                            PopupMenuItem::new(level.name.clone())
                                .checked(selected_effort.as_deref() == Some(level.value.as_ref()))
                                .on_click(move |_, _, cx| {
                                    weak_self
                                        .update(cx, |this, cx| {
                                            this.select_thinking_effort(value.clone(), cx)
                                        })
                                        .log_err();
                                }),
                        );
                    }
                    menu
                });
            }

            let has_models = model_list.is_some();
            if let Some(model_list) = model_list.clone() {
                let selected_model_id = selected_model_id.clone();
                let hidden_model_ids = hidden_model_ids.clone();
                let model_selector = model_selector.clone();
                menu = menu.submenu("Model", window, cx, move |menu, _, _| {
                    Self::push_model_items(
                        menu.check_side(Side::Right)
                            .max_h(px(420.))
                            .scrollable(true),
                        model_list.clone(),
                        selected_model_id.as_ref(),
                        &hidden_model_ids,
                        model_selector.clone(),
                    )
                });
            }

            if !supports_disabling_thinking && effort_levels.is_empty() && !has_models {
                menu = menu.item(PopupMenuItem::new("No models available").disabled(true));
            }
            menu
        });

        self._model_menu_subscription = Some(cx.subscribe_in(
            &menu,
            window,
            |this, _, _: &gpui::DismissEvent, _, cx| this.close_model_menu(cx),
        ));
        menu.focus_handle(cx).focus(window, cx);
        self.model_menu = Some(menu);
        cx.notify();
    }

    fn close_model_menu(&mut self, cx: &mut Context<Self>) {
        if self.model_menu.take().is_some() {
            cx.notify();
        }
    }

    fn push_model_items(
        mut menu: PopupMenu,
        models: AgentModelList,
        selected_model_id: Option<&AgentModelId>,
        hidden_model_ids: &HashSet<AgentModelId>,
        model_selector: Entity<ModelSelectorPopover>,
    ) -> PopupMenu {
        // The model in use stays listed even when hidden, so the checkmark never vanishes.
        let is_listed = |model: &AgentModelInfo| {
            !hidden_model_ids.contains(&model.id) || Some(&model.id) == selected_model_id
        };
        match models {
            AgentModelList::Flat(list) => {
                for model in list.into_iter().filter(|model| is_listed(model)) {
                    menu = menu.item(Self::model_menu_item(
                        model,
                        selected_model_id,
                        model_selector.clone(),
                    ));
                }
            }
            AgentModelList::Grouped(groups) => {
                for (group, list) in groups {
                    let (listed, hidden): (Vec<_>, Vec<_>) =
                        list.into_iter().partition(|model| is_listed(model));
                    if listed.is_empty() {
                        continue;
                    }
                    menu = menu.label(group.0);
                    for model in listed {
                        menu = menu.item(Self::model_menu_item(
                            model,
                            selected_model_id,
                            model_selector.clone(),
                        ));
                    }
                    if !hidden.is_empty() {
                        let label = SharedString::from(format!("{} hidden", hidden.len()));
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                div()
                                    .text_xs()
                                    .text_color(KitTheme::global(cx).muted_foreground)
                                    .child(label.clone())
                            })
                            .on_click(|_, window, cx| {
                                Self::open_llm_providers_settings(window, cx)
                            }),
                        );
                    }
                }
            }
        }

        menu.separator().item(
            PopupMenuItem::new("Manage Models…")
                .on_click(|_, window, cx| Self::open_llm_providers_settings(window, cx)),
        )
    }

    fn open_llm_providers_settings(window: &mut Window, cx: &mut App) {
        window.dispatch_action(
            Box::new(zed_actions::OpenSettingsAt {
                path: "llm_providers".to_string(),
                target: None,
            }),
            cx,
        );
    }

    fn model_menu_item(
        model: AgentModelInfo,
        selected_model_id: Option<&AgentModelId>,
        model_selector: Entity<ModelSelectorPopover>,
    ) -> PopupMenuItem {
        let is_selected = selected_model_id == Some(&model.id);
        let disabled = model.disabled.is_some();
        let model_id = model.id.clone();
        PopupMenuItem::new(model.name)
            .checked(is_selected)
            .disabled(disabled)
            .on_click(move |_, _, cx| {
                model_selector.update(cx, |selector, cx| {
                    selector.select_model(model_id.clone(), cx);
                });
            })
    }

    fn render_thinking_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (model, thinking_enabled, selected_effort) = self.thinking_state(cx)?;

        let supports_thinking = model.supports_thinking();
        if !supports_thinking {
            return None;
        }

        let effort_levels = model.supported_effort_levels();
        let supports_disabling_thinking =
            model.supports_disabling_thinking_at_effort(selected_effort.as_deref());
        let supports_disabling_thinking_at_any_effort = supports_disabling_thinking
            || effort_levels.iter().any(|effort_level| {
                model.supports_disabling_thinking_at_effort(Some(effort_level.value.as_ref()))
            });

        // A toggle would be dishonest for models that always think at every
        // effort level: only offer the effort selector.
        if !supports_disabling_thinking_at_any_effort {
            if effort_levels.is_empty() {
                return None;
            }
            return Some(
                self.render_effort_selector(effort_levels, selected_effort, true, cx)
                    .into_any_element(),
            );
        }

        let thinking = thinking_enabled || !supports_disabling_thinking;

        let (tooltip_label, icon, color) = if !supports_disabling_thinking {
            (None, IconName::ThinkingMode, Color::Accent)
        } else if thinking {
            (
                Some("Disable Thinking Mode"),
                IconName::ThinkingMode,
                Color::Accent,
            )
        } else {
            (
                Some("Enable Thinking Mode"),
                IconName::ThinkingModeOff,
                Color::Custom(cx.theme().colors().icon_disabled.opacity(0.8)),
            )
        };

        let focus_handle = self.message_editor.focus_handle(cx);

        let thinking_toggle = IconButton::new("thinking-mode", icon)
            .icon_size(IconSize::Small)
            .icon_color(color)
            .map(|this| {
                if let Some(tooltip_label) = tooltip_label {
                    this.tooltip(move |_, cx| {
                        Tooltip::for_action_in(
                            tooltip_label,
                            &ToggleThinkingMode,
                            &focus_handle,
                            cx,
                        )
                    })
                } else {
                    this.tooltip(Tooltip::text(
                        "Thinking is always on at this effort level. Select a lower effort level to make it optional.",
                    ))
                }
            });
        let thinking_toggle = if supports_disabling_thinking {
            thinking_toggle.on_click(cx.listener(|this, _, _, cx| this.toggle_thinking(cx)))
        } else {
            thinking_toggle.cursor_style(gpui::CursorStyle::Arrow)
        };

        if effort_levels.is_empty() {
            return Some(thinking_toggle.into_any_element());
        }

        if !thinking {
            return Some(thinking_toggle.into_any_element());
        }

        let left_btn = thinking_toggle;
        let right_btn = self.render_effort_selector(effort_levels, selected_effort, false, cx);

        Some(
            SplitButton::new(left_btn, right_btn.into_any_element())
                .style(SplitButtonStyle::Transparent)
                .into_any_element(),
        )
    }

    fn render_effort_selector(
        &self,
        supported_effort_levels: Vec<LanguageModelEffortLevel>,
        selected_effort: Option<String>,
        standalone: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let weak_self = cx.weak_entity();

        let default_effort_level = supported_effort_levels
            .iter()
            .find(|effort_level| effort_level.is_default)
            .cloned();

        let selected = selected_effort.and_then(|effort| {
            supported_effort_levels
                .iter()
                .find(|level| level.value == effort)
                .cloned()
        });

        let label = selected
            .clone()
            .or(default_effort_level)
            .map_or("Select Effort".into(), |effort| effort.name);

        let (label_color, icon) = if self.thinking_effort_menu_handle.is_deployed() {
            (Color::Accent, IconName::ChevronUp)
        } else {
            (Color::Muted, IconName::ChevronDown)
        };

        let focus_handle = self.message_editor.focus_handle(cx);
        let show_cycle_row = supported_effort_levels.len() > 1;

        let tooltip = Tooltip::element({
            move |_, cx| {
                let mut content = v_flex().gap_1().child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(Label::new("Change Thinking Effort"))
                        .child(KeyBinding::for_action_in(
                            &ToggleThinkingEffortMenu,
                            &focus_handle,
                            cx,
                        )),
                );

                if show_cycle_row {
                    content = content.child(
                        h_flex()
                            .pt_1()
                            .gap_2()
                            .justify_between()
                            .border_t_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(Label::new("Cycle Thinking Effort"))
                            .child(KeyBinding::for_action_in(
                                &CycleThinkingEffort,
                                &focus_handle,
                                cx,
                            )),
                    );
                }

                content.into_any_element()
            }
        });

        let trigger = if standalone {
            ButtonLike::new("effort-selector-trigger").child(
                h_flex()
                    .gap_1()
                    .child(
                        Icon::new(IconName::ThinkingMode)
                            .size(IconSize::Small)
                            .color(Color::Accent),
                    )
                    .child(Label::new(label).size(LabelSize::Small).color(label_color))
                    .child(Icon::new(icon).size(IconSize::XSmall).color(Color::Muted)),
            )
        } else {
            ButtonLike::new_rounded_right("effort-selector-trigger")
                .child(Label::new(label).size(LabelSize::Small).color(label_color))
                .child(Icon::new(icon).size(IconSize::XSmall).color(Color::Muted))
        };

        PopoverMenu::new("effort-selector")
            .trigger_with_tooltip(
                trigger.selected_style(ButtonStyle::Tinted(TintColor::Accent)),
                tooltip,
            )
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |mut menu, _window, _cx| {
                    menu = menu.header("Change Thinking Effort");

                    for effort_level in supported_effort_levels.clone() {
                        let is_selected = selected
                            .as_ref()
                            .is_some_and(|selected| selected.value == effort_level.value);
                        let entry = ContextMenuEntry::new(effort_level.name)
                            .toggleable(IconPosition::End, is_selected);

                        menu.push_item(entry.handler({
                            let effort = effort_level.value.clone();
                            let weak_self = weak_self.clone();
                            move |_window, cx| {
                                let effort = effort.clone();
                                weak_self
                                    .update(cx, |this, cx| {
                                        this.select_thinking_effort(effort.to_string(), cx);
                                    })
                                    .ok();
                            }
                        }));
                    }

                    menu
                }))
            })
            .with_handle(self.thinking_effort_menu_handle.clone())
            .offset(gpui::Point {
                x: px(0.0),
                y: px(-2.0),
            })
            .anchor(gpui::Anchor::BottomLeft)
    }

    /// Following is switched on from the add menu; the footer only shows that it's on, and
    /// stops it on click.
    fn render_follow_indicator(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.is_following(cx) {
            return None;
        }
        let tooltip_label = if self.agent_id.as_ref() == agent::ZED_AGENT_ID.as_ref() {
            format!("Stop Following the {}", self.agent_id)
        } else {
            format!("Stop Following {}", self.agent_id)
        };

        Some(
            composer::PillButton::new("follow-agent")
                .size(gpui_kit::component::Size::Small)
                .icon(IconName::Crosshair)
                .toggle_state(true)
                .tooltip(move |_window, cx| Tooltip::for_action(tooltip_label.clone(), &Follow, cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_following(window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}

/// A display-ready snapshot of a sandbox policy for the permissions menu.
///
/// The opaque `HostFilesystemLocation`s in a policy are stringified up front,
/// when this is built, so the menu state (which outlives the build and is
/// captured by the lazy menu builder) never holds the locations' fds open.
#[derive(Clone)]
struct SandboxPolicyDisplay {
    fs: SandboxFsDisplay,
}

/// The filesystem write-access portion of a [`SandboxPolicyDisplay`].
#[derive(Clone)]
enum SandboxFsDisplay {
    Unrestricted,
    Restricted(Vec<WritableEntryDisplay>),
}

/// A single writable entry to display in the permissions menu: either a real host
/// location (already stringified for display) or the Linux-only host-isolated
/// `/tmp` overlay, which has no backing host path and is purely a label.
#[derive(Clone)]
enum WritableEntryDisplay {
    Path(String),
    // Only ever constructed on Linux (the bwrap `--tmpfs /tmp` overlay), so the
    // variant is gated to match and avoid dead-code warnings elsewhere.
    #[cfg(target_os = "linux")]
    IsolatedTmp,
}

impl SandboxPolicyDisplay {
    /// Display a policy verbatim (used for the per-thread overrides, which carry
    /// no implicit baseline grants). Takes the policy by reference and stringifies
    /// its locations immediately, so no fd is retained past this call.
    fn from_policy(policy: &SandboxPolicy) -> Self {
        let fs = match &policy.fs {
            SandboxFsPolicy::Unrestricted { .. } => SandboxFsDisplay::Unrestricted,
            SandboxFsPolicy::Restricted { writable_paths, .. } => SandboxFsDisplay::Restricted(
                writable_paths
                    .iter()
                    .map(|location| {
                        WritableEntryDisplay::Path(location.untrusted_path_display().to_string())
                    })
                    .collect(),
            ),
        };
        SandboxPolicyDisplay { fs }
    }
}

/// Fold the always-granted baseline writable paths (the project's worktree
/// roots, derived from the same source the terminal tool uses) and, on Linux,
/// the host-isolated `/tmp` overlay into a settings policy for display. These
/// are part of what the sandbox grants whenever it's active but aren't
/// persistent-settings entries, so they're shown in the "from your settings"
/// section rather than stored. A no-op when the fs is unrestricted (rendered as
/// "All paths"), since there's nothing to scope.
fn augment_settings_sandbox_policy(
    policy: &SandboxPolicy,
    baseline: Vec<PathBuf>,
) -> SandboxPolicyDisplay {
    let fs = match &policy.fs {
        SandboxFsPolicy::Unrestricted { .. } => SandboxFsDisplay::Unrestricted,
        SandboxFsPolicy::Restricted { writable_paths, .. } => {
            // Dedup by display string. We deliberately don't open the locations'
            // fds to dedup by inode here: this is a display-only menu and the
            // string is the location's identity for that purpose. The string can
            // only diverge from the captured inode while a symlink-swap is
            // actively in progress, and in that case the bind validator refuses
            // to run the command at all (see the `sandbox` crate) — so showing the
            // requested path is always safe, and not worth a blocking syscall on
            // the render path.
            let mut merged: Vec<String> = Vec::new();
            let baseline_paths = baseline.iter().map(|path| path.display().to_string());
            let granted_paths = writable_paths
                .iter()
                .map(|location| location.untrusted_path_display().to_string());
            for path in baseline_paths.chain(granted_paths) {
                if !merged.contains(&path) {
                    merged.push(path);
                }
            }
            // `mut` is only needed on Linux, where the isolated `/tmp` entry is
            // pushed below.
            #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
            let mut entries: Vec<WritableEntryDisplay> =
                merged.into_iter().map(WritableEntryDisplay::Path).collect();
            // The ephemeral, host-isolated tmpfs at /tmp is Linux-specific (the
            // bwrap `--tmpfs /tmp` overlay). It's a display-only label, not a
            // real host path, so it can't be a captured location.
            #[cfg(target_os = "linux")]
            entries.push(WritableEntryDisplay::IsolatedTmp);
            SandboxFsDisplay::Restricted(entries)
        }
    };
    SandboxPolicyDisplay { fs }
}

fn fs_grants_nothing(fs: &SandboxFsDisplay) -> bool {
    matches!(fs, SandboxFsDisplay::Restricted(entries) if entries.is_empty())
}

const FULL_ACCESS_NOTE: &str = "Commands run without a sandbox.";

/// What the permissions menu shows under the choices.
#[derive(Clone)]
enum SandboxDetails {
    Sandboxed {
        writable: SandboxFsDisplay,
        /// Omitted when the thread was granted nothing beyond `writable`.
        thread_grants: Option<SandboxFsDisplay>,
    },
    UnsandboxedForThread,
    Note(&'static str),
}

fn permissions_menu(
    mut menu: PopupMenu,
    current: AgentPermissions,
    details: Option<&SandboxDetails>,
    this: &WeakEntity<ThreadView>,
) -> PopupMenu {
    menu = menu.check_side(Side::Right).min_w(px(240.));
    for permissions in [
        AgentPermissions::ReadOnly,
        AgentPermissions::Workspace,
        AgentPermissions::FullAccess,
    ] {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(<&'static str>::from(permissions))
                .checked(permissions == current)
                .on_click(move |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.select_permissions(permissions, window, cx)
                    })
                    .log_err();
                }),
        );
    }

    match details {
        Some(SandboxDetails::Sandboxed {
            writable,
            thread_grants,
        }) => {
            menu = push_writable_items(menu.separator().label("Write Access"), writable);
            if let Some(thread_grants) = thread_grants {
                menu = push_writable_items(menu.label("Allowed for This Thread"), thread_grants);
            }
            menu.separator()
                .item(
                    PopupMenuItem::new("Sandbox Settings…").on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(zed_actions::OpenSettingsAt {
                                path: zed_actions::AGENT_SANDBOX_SETTINGS_PATH.to_string(),
                                target: None,
                            }),
                            cx,
                        );
                    }),
                )
        }
        Some(SandboxDetails::UnsandboxedForThread) => menu
            .separator()
            .item(permissions_note("Sandboxing is disabled for this thread.")),
        Some(SandboxDetails::Note(note)) => menu.separator().item(permissions_note(note)),
        None => menu,
    }
}

/// A message for the "all"/"none" cases, or one row per granted path.
fn push_writable_items(mut menu: PopupMenu, fs: &SandboxFsDisplay) -> PopupMenu {
    match fs {
        SandboxFsDisplay::Unrestricted => {
            menu.item(permissions_note("All paths except protected Git metadata"))
        }
        SandboxFsDisplay::Restricted(entries) if entries.is_empty() => {
            menu.item(permissions_note("None"))
        }
        SandboxFsDisplay::Restricted(entries) => {
            for entry in entries {
                let path = match entry {
                    // The display string was captured up front.
                    WritableEntryDisplay::Path(path) => SharedString::from(path.clone()),
                    #[cfg(target_os = "linux")]
                    WritableEntryDisplay::IsolatedTmp => SharedString::from("/tmp (isolated)"),
                };
                menu = menu.item(writable_path_item(path));
            }
            menu
        }
    }
}

/// The width of the wrapping text in the permissions menus. A menu sizes its items to their
/// content, and GPUI only wraps text that has been given a width, so the text sets one itself.
const PERMISSIONS_MENU_TEXT_WIDTH: Pixels = px(248.);

fn permissions_note(note: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .w(PERMISSIONS_MENU_TEXT_WIDTH)
            .py_1()
            .text_xs()
            .child(note)
    })
    .disabled(true)
}

fn danger_menu_item(label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| div().text_color(KitTheme::global(cx).danger).child(label))
}

fn writable_path_item(path: SharedString) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let icon = FileIcons::get_icon(Path::new(path.as_ref()), cx)
            .unwrap_or_else(|| IconName::Folder.path().into());
        h_flex()
            .w(PERMISSIONS_MENU_TEXT_WIDTH)
            .items_start()
            .gap_1p5()
            .py_1()
            .child(KitIcon::empty().path(icon).xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_family(KitTheme::global(cx).mono_font_family.clone())
                    .child(path.clone()),
            )
    })
    .disabled(true)
}

impl ThreadView {
    fn render_elicitation(
        &self,
        entry_ix: usize,
        elicitation: &Elicitation,
        _window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        ElicitationCard::new(
            entry_ix,
            elicitation,
            self.agent_display_name.clone(),
            self.elicitation_form_states.get(&elicitation.id),
            self.elicitation_card_handlers(cx),
        )
        .render(cx)
    }

    fn elicitation_card_handlers(&self, cx: &Context<Self>) -> ElicitationCardHandlers {
        let view = cx.entity().downgrade();

        ElicitationCardHandlers::new(
            {
                let view = view.clone();
                move |elicitation_id, window, cx| {
                    view.update(cx, |this, cx| {
                        this.submit_elicitation(elicitation_id, window, cx);
                    })
                    .log_err();
                }
            },
            {
                let view = view.clone();
                move |elicitation_id, window, cx| {
                    view.update(cx, |this, cx| {
                        this.decline_elicitation(elicitation_id, window, cx);
                    })
                    .log_err();
                }
            },
            {
                let view = view.clone();
                move |elicitation_id, window, cx| {
                    view.update(cx, |this, cx| {
                        this.cancel_elicitation(elicitation_id, window, cx);
                    })
                    .log_err();
                }
            },
            {
                let view = view.clone();
                move |elicitation_id, window, cx| {
                    view.update(cx, |this, cx| {
                        this.dismiss_url_elicitation(elicitation_id, window, cx);
                    })
                    .log_err();
                }
            },
            move |_elicitation_id, url, _window, cx| cx.open_url(&url),
            {
                let view = view.clone();
                move |elicitation_id, field_name, value, cx| {
                    view.update(cx, |this, cx| {
                        if let Some(form) = this.elicitation_form_states.get_mut(&elicitation_id) {
                            form.set_boolean(&field_name, value);
                            cx.notify();
                        }
                    })
                    .log_err();
                }
            },
            {
                let view = view.clone();
                move |elicitation_id, field_name, value, cx| {
                    view.update(cx, |this, cx| {
                        if let Some(form) = this.elicitation_form_states.get_mut(&elicitation_id) {
                            form.set_single_select(&field_name, value);
                            cx.notify();
                        }
                    })
                    .log_err();
                }
            },
            move |elicitation_id, field_name, value, selected, cx| {
                view.update(cx, |this, cx| {
                    if let Some(form) = this.elicitation_form_states.get_mut(&elicitation_id) {
                        form.set_multi_select(&field_name, value, selected);
                        cx.notify();
                    }
                })
                .log_err();
            },
        )
    }

    fn render_feedback_feedback_editor(editor: Entity<Editor>, cx: &Context<Self>) -> Div {
        h_flex()
            .key_context("AgentFeedbackMessageEditor")
            .on_action(cx.listener(move |this, _: &menu::Cancel, _, cx| {
                this.thread_feedback.dismiss_comments();
                cx.notify();
            }))
            .on_action(cx.listener(move |this, _: &menu::Confirm, _window, cx| {
                this.submit_feedback_message(cx);
            }))
            .p_2()
            .mb_2()
            .mx_4()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().editor_background)
            .child(div().w_full().child(editor))
            .child(
                h_flex()
                    .child(
                        IconButton::new("dismiss-feedback-message", IconName::Close)
                            .icon_color(Color::Error)
                            .icon_size(IconSize::XSmall)
                            .shape(ui::IconButtonShape::Square)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.thread_feedback.dismiss_comments();
                                cx.notify();
                            })),
                    )
                    .child(
                        IconButton::new("submit-feedback-message", IconName::Return)
                            .icon_size(IconSize::XSmall)
                            .shape(ui::IconButtonShape::Square)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.submit_feedback_message(cx);
                            })),
                    ),
            )
    }

    fn is_thread_feedback_enabled(&self, cx: &App) -> bool {
        util::maybe!({
            let project = self.thread.read(cx).project().read(cx);
            let user_store = project.user_store();
            if let Some(configuration) = user_store.read(cx).current_organization_configuration() {
                if !configuration.is_agent_thread_feedback_enabled {
                    return false;
                }
            }

            AgentSettings::get_global(cx).enable_feedback
                && self.thread.read(cx).connection().telemetry().is_some()
        })
    }

    // The local slash commands the message editor should currently expose.
    // Kept in sync with the availability of the corresponding actions via
    // `sync_local_commands`.
    fn available_local_commands(&self, cx: &App) -> Vec<PromptLocalCommand> {
        let mut commands = Vec::new();

        if self.is_thread_feedback_enabled(cx) {
            commands.push(PromptLocalCommand::ThumbsUp);
            commands.push(PromptLocalCommand::ThumbsDown);
        }

        commands
    }

    // Pushes the current set of available local commands to the message
    // editor so they appear in its slash-command popup.
    pub(crate) fn sync_local_commands(&self, cx: &App) {
        let commands = self.available_local_commands(cx);
        self.message_editor.read(cx).set_local_commands(commands);
    }

    fn render_request_elicitations(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let server_view = self.server_view.clone();
        let handlers_view = server_view.clone();
        server_view
            .read_with(cx, |server_view, cx| {
                let Some(connection) = server_view.request_elicitation_connection() else {
                    return Vec::new();
                };
                server_view.render_request_elicitations(&connection, handlers_view, cx)
            })
            .unwrap_or_default()
    }

    pub(crate) fn scroll_to_user_message_index(
        &mut self,
        user_message_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let thread = self.thread.read(cx);
        let entries = thread.entries();
        if entries.is_empty() {
            return;
        }

        // Scroll to the provided user message, or fall back to the most recent one.
        // (Fallback: if no user message exists, scroll to the bottom.)
        if let Some(ix) = user_message_index.or_else(|| {
            entries
                .iter()
                .rposition(|entry| thread.is_user_authored_scroll_target(entry))
        }) {
            self.list_state.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.0),
            });
            cx.notify();
        } else {
            self.scroll_to_end(cx);
        }
    }

    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.list_state.scroll_to_end();
        cx.notify();
    }

    fn handle_feedback_click(
        &mut self,
        feedback: ThreadFeedback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.thread_feedback
            .submit(self.thread.clone(), feedback, window, cx);
        cx.notify();
    }

    fn submit_feedback_message(&mut self, cx: &mut Context<Self>) {
        let thread = self.thread.clone();
        self.thread_feedback.submit_comments(thread, cx);
        cx.notify();
    }

    pub(crate) fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        self.list_state.scroll_to(ListOffset::default());
        cx.notify();
    }

    fn scroll_output_page_up(
        &mut self,
        _: &ScrollOutputPageUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page_height = self.list_state.viewport_bounds().size.height;
        self.list_state.scroll_by(-page_height * 0.9);
        cx.notify();
    }

    fn scroll_output_page_down(
        &mut self,
        _: &ScrollOutputPageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page_height = self.list_state.viewport_bounds().size.height;
        self.list_state.scroll_by(page_height * 0.9);
        cx.notify();
    }

    fn scroll_output_line_up(
        &mut self,
        _: &ScrollOutputLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list_state.scroll_by(-window.line_height() * 3.);
        cx.notify();
    }

    fn scroll_output_line_down(
        &mut self,
        _: &ScrollOutputLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list_state.scroll_by(window.line_height() * 3.);
        cx.notify();
    }

    fn scroll_output_to_top(
        &mut self,
        _: &ScrollOutputToTop,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_to_top(cx);
    }

    fn scroll_output_to_bottom(
        &mut self,
        _: &ScrollOutputToBottom,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_to_end(cx);
    }

    fn scroll_output_to_previous_message(
        &mut self,
        _: &ScrollOutputToPreviousMessage,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread = self.thread.read(cx);
        let entries = thread.entries();
        let current_ix = self.list_state.logical_scroll_top().item_ix;
        if let Some(target_ix) = (0..current_ix).rev().find(|&i| {
            entries
                .get(i)
                .is_some_and(|entry| thread.is_user_authored_scroll_target(entry))
        }) {
            self.list_state.scroll_to(ListOffset {
                item_ix: target_ix,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
    }

    fn scroll_output_to_next_message(
        &mut self,
        _: &ScrollOutputToNextMessage,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread = self.thread.read(cx);
        let entries = thread.entries();
        let current_ix = self.list_state.logical_scroll_top().item_ix;
        if let Some(target_ix) = (current_ix + 1..entries.len()).find(|&i| {
            entries
                .get(i)
                .is_some_and(|entry| thread.is_user_authored_scroll_target(entry))
        }) {
            self.list_state.scroll_to(ListOffset {
                item_ix: target_ix,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
    }

    fn refresh_thread_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.thread_search_visible {
            return;
        }
        if let Some(bar) = self.thread_search_bar.clone() {
            bar.update(cx, |bar, cx| bar.update_matches(window, cx));
        }
    }

    /// Hides the thread search bar, clears its highlights, and returns focus to
    /// the message editor. Returns `true` if the search bar was visible.
    pub(crate) fn close_thread_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.thread_search_visible {
            return false;
        }

        if let Some(bar) = self.thread_search_bar.clone() {
            bar.update(cx, |bar, cx| bar.clear_highlights(cx));
        }

        self.thread_search_visible = false;
        self.message_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
        true
    }

    pub(crate) fn toggle_search(
        &mut self,
        _: &crate::ToggleSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thread_search_bar.is_none() {
            let thread = self.thread.clone();
            let view = cx.entity().downgrade();
            let on_activate =
                Arc::new(move |entry_ix: usize, _window: &mut Window, cx: &mut App| {
                    // Avoid re-entering `ThreadView` when search navigation is forwarded
                    // from a `ThreadView` action handler.
                    let view = view.clone();
                    cx.defer(move |cx| {
                        view.update(cx, |this, cx| {
                            this.list_state.scroll_to(gpui::ListOffset {
                                item_ix: entry_ix,
                                offset_in_item: gpui::px(0.),
                            });
                            cx.notify();
                        })
                        .ok();
                    });
                });
            let search_bar = cx.new(|cx| {
                ThreadSearchBar::new(
                    thread,
                    self.entry_view_state.clone(),
                    on_activate,
                    window,
                    cx,
                )
            });
            self._subscriptions.push(cx.subscribe_in(
                &search_bar,
                window,
                |this, _bar, event, window, cx| {
                    if matches!(event, ThreadSearchBarEvent::Dismissed) {
                        this.thread_search_visible = false;
                        this.message_editor.focus_handle(cx).focus(window, cx);
                        cx.notify();
                    }
                },
            ));
            self.thread_search_bar = Some(search_bar);
        }

        // Re-focus an open bar unless it already owns focus.
        let search_bar_focused = self
            .thread_search_bar
            .as_ref()
            .is_some_and(|bar| bar.focus_handle(cx).contains_focused(window, cx));

        if self.thread_search_visible && search_bar_focused {
            if let Some(bar) = &self.thread_search_bar {
                bar.update(cx, |bar, cx| bar.clear_highlights(cx));
            }
            self.thread_search_visible = false;
            self.message_editor.focus_handle(cx).focus(window, cx);
            cx.notify();
        } else {
            self.thread_search_visible = true;
            if let Some(bar) = self.thread_search_bar.clone() {
                bar.update(cx, |bar, cx| bar.focus_and_refresh(window, cx));
            }
            cx.notify();
        }
    }

    pub fn open_thread_as_markdown(
        &self,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let thread = self.thread.read(cx);
        let thread_title = thread
            .title()
            .unwrap_or_else(|| DEFAULT_THREAD_TITLE.into())
            .to_string();
        let markdown = thread.to_markdown(cx);

        open_markdown_in_workspace(thread_title, markdown, workspace, window, cx)
    }

    pub(crate) fn sync_editor_mode(&mut self, cx: &mut Context<Self>) {
        let has_messages = self.has_visible_conversation(cx);

        if !has_messages {
            self.editor_expanded = false;
        }

        let mode = if self.editor_expanded {
            EditorMode::Full {
                scale_ui_elements_with_buffer_font_size: false,
                show_active_line_background: false,
                sizing_behavior: SizingBehavior::ExcludeOverscrollMargin,
            }
        } else {
            EditorMode::AutoHeight {
                min_lines: AgentSettings::get_global(cx).message_editor_min_lines,
                max_lines: Some(AgentSettings::get_global(cx).set_message_editor_max_lines()),
            }
        };
        self.message_editor.update(cx, |editor, cx| {
            editor.set_mode(mode, cx);
        });
    }

    /// Ensures the list item count includes (or excludes) an extra item for the generating indicator
    pub(crate) fn sync_generating_indicator(&mut self, cx: &App) {
        let thread = self.thread.read(cx);

        let is_generating =
            matches!(thread.status(), ThreadStatus::Generating) && !thread.is_compacting();

        if is_generating && !self.generating_indicator_in_list {
            let entries_count = self.thread.read(cx).entries().len();
            self.list_state.splice(entries_count..entries_count, 1);
            self.generating_indicator_in_list = true;
        } else if !is_generating && self.generating_indicator_in_list {
            let entries_count = self.thread.read(cx).entries().len();
            self.list_state.splice(entries_count..entries_count + 1, 0);
            self.generating_indicator_in_list = false;
        }
    }

    pub(crate) fn auto_expand_streaming_thought(&mut self, cx: &mut Context<Self>) {
        let thread = self.thread.clone();
        let changed = self.entry_view_state.update(cx, |state, cx| {
            let thread = thread.read(cx);
            if thread.status() != ThreadStatus::Generating {
                return false;
            }
            state.auto_expand_streaming_thought(thread, cx)
        });
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn sync_thought_timing(&mut self, cx: &mut Context<Self>) {
        let thread = self.thread.clone();
        self.entry_view_state.update(cx, |state, cx| {
            state.sync_thought_timing(thread.read(cx));
        });
    }

    pub(crate) fn clear_auto_expand_tracking(&mut self, cx: &mut Context<Self>) {
        self.entry_view_state.update(cx, |state, _cx| {
            state.clear_auto_expand_tracking();
        });
    }

    fn toggle_thinking_block_expansion(
        &mut self,
        key: (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hold_scroll_position_for_toggle();
        self.entry_view_state.update(cx, |state, cx| {
            state.toggle_thinking_block_expansion(key, cx);
        });
        self.refresh_thread_search(window, cx);
        cx.notify();
    }

    fn render_message_context_menu(
        &self,
        entry_ix: usize,
        markdown: Option<Entity<Markdown>>,
        message_body: AnyElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let workspace = self.workspace.clone();

        right_click_menu(format!("agent_context_menu-{}", entry_ix))
            .trigger(move |_, _, _| message_body)
            .menu(move |window, cx| {
                let focus = window.focused(cx);
                let entity = entity.clone();
                let workspace = workspace.clone();
                let markdown = markdown.clone();

                ContextMenu::build(window, cx, move |menu, _, cx| {
                    let this = entity.read(cx);
                    let is_at_top = this.list_state.logical_scroll_top().item_ix == 0;
                    let markdown = markdown.as_ref().map(|markdown| markdown.read(cx));
                    let context_menu_link =
                        markdown.and_then(|markdown| markdown.context_menu_link().cloned());
                    let selected_text = markdown
                        .and_then(|markdown| markdown.context_menu_selected_text().cloned());
                    let selected_markdown = markdown
                        .and_then(|markdown| markdown.context_menu_selected_markdown().cloned());

                    let copy_this_agent_response =
                        ContextMenuEntry::new("Copy This Agent Response").handler({
                            let entity = entity.clone();
                            move |_, cx| {
                                entity.update(cx, |this, cx| {
                                    let entries = this.thread.read(cx).entries();
                                    if let Some(text) =
                                        Self::get_agent_message_content(entries, entry_ix, cx)
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    }
                                });
                            }
                        });

                    let scroll_item = if is_at_top {
                        ContextMenuEntry::new("Scroll to Bottom").handler({
                            let entity = entity.clone();
                            move |_, cx| {
                                entity.update(cx, |this, cx| {
                                    this.scroll_to_end(cx);
                                });
                            }
                        })
                    } else {
                        ContextMenuEntry::new("Scroll to Top").handler({
                            let entity = entity.clone();
                            move |_, cx| {
                                entity.update(cx, |this, cx| {
                                    this.scroll_to_top(cx);
                                });
                            }
                        })
                    };

                    let open_thread_as_markdown = ContextMenuEntry::new("Open Thread as Markdown")
                        .handler({
                            let entity = entity.clone();
                            let workspace = workspace.clone();
                            move |window, cx| {
                                if let Some(workspace) = workspace.upgrade() {
                                    entity
                                        .update(cx, |this, cx| {
                                            this.open_thread_as_markdown(workspace, window, cx)
                                        })
                                        .detach_and_log_err(cx);
                                }
                            }
                        });

                    menu.when_some(focus, |menu, focus| menu.context(focus))
                        .when_some(context_menu_link, |menu, url| {
                            menu.entry("Copy Link", None, move |_, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
                            })
                            .separator()
                        })
                        .when_some(selected_text, |menu, selected_text| {
                            menu.entry("Copy", Some(Box::new(markdown::Copy)), move |_, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    selected_text.to_string(),
                                ));
                            })
                        })
                        .when_some(selected_markdown, |menu, selected_markdown| {
                            menu.entry(
                                "Copy as Markdown",
                                Some(Box::new(markdown::CopyAsMarkdown)),
                                move |_, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        selected_markdown.to_string(),
                                    ));
                                },
                            )
                        })
                        .item(copy_this_agent_response)
                        .separator()
                        .item(scroll_item)
                        .item(open_thread_as_markdown)
                })
            })
            .into_any_element()
    }

    fn get_agent_message_content(
        entries: &[AgentThreadEntry],
        entry_index: usize,
        cx: &App,
    ) -> Option<String> {
        let entry = entries.get(entry_index)?;
        if matches!(entry, AgentThreadEntry::UserMessage(_)) {
            return None;
        }

        let start_index = (0..entry_index)
            .rev()
            .find(|&i| matches!(entries.get(i), Some(AgentThreadEntry::UserMessage(_))))
            .map(|i| i + 1)
            .unwrap_or(0);

        let end_index = (entry_index + 1..entries.len())
            .find(|&i| matches!(entries.get(i), Some(AgentThreadEntry::UserMessage(_))))
            .map(|i| i - 1)
            .unwrap_or(entries.len() - 1);

        let parts: Vec<String> = (start_index..=end_index)
            .filter_map(|i| entries.get(i))
            .filter_map(|entry| {
                if let AgentThreadEntry::AssistantMessage(message) = entry {
                    let text: String = message
                        .chunks
                        .iter()
                        .filter_map(|chunk| match chunk {
                            AssistantMessageChunk::Message { block, .. } => {
                                let markdown = block.to_markdown(cx);
                                if markdown.trim().is_empty() {
                                    None
                                } else {
                                    Some(markdown)
                                }
                            }
                            AssistantMessageChunk::Thought { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");

                    if text.is_empty() { None } else { Some(text) }
                } else {
                    None
                }
            })
            .collect();

        let text = parts.join("\n\n");
        if text.is_empty() { None } else { Some(text) }
    }

    fn sandbox_not_applied_warning(
        &self,
        reason: &SandboxNotAppliedReason,
        cx: &Context<Self>,
    ) -> TerminalSandboxWarning {
        // (title, detail line, docs section slug)
        let (title, detail, docs_section): (SharedString, SharedString, Option<&'static str>) =
            match reason {
                SandboxNotAppliedReason::ErrorLinuxWsl(error) => (
                    "Couldn't create a sandbox".into(),
                    error.user_facing_message().into(),
                    Some(error.docs_section()),
                ),
                SandboxNotAppliedReason::DisabledForThisThread => {
                    // The grant only exists because an earlier command failed to
                    // create a sandbox; surface that same explanation here.
                    let thread_error = self.find_thread_sandbox_error(cx);
                    let detail = thread_error
                        .as_ref()
                        .map(|error| {
                            SharedString::from(format!(
                                "Allowed for this thread after the sandbox failed: {}",
                                error.user_facing_message()
                            ))
                        })
                        .unwrap_or_else(|| {
                            "Unsandboxed execution is allowed for the rest of this thread.".into()
                        });
                    let docs_section = thread_error.as_ref().map(|error| error.docs_section());
                    ("Ran without sandbox".into(), detail, docs_section)
                }
            };

        TerminalSandboxWarning {
            title,
            detail,
            docs_url: zed_urls::sandboxing_docs(docs_section, cx).into(),
        }
    }

    /// Find the first terminal tool call in the thread whose sandbox couldn't be
    /// created, so a later "disabled for this thread" warning can reuse the same
    /// explanation of *why* the sandbox failed.
    fn find_thread_sandbox_error(&self, cx: &App) -> Option<acp_thread::LinuxWslSandboxError> {
        self.thread.read(cx).entries().iter().find_map(|entry| {
            if let AgentThreadEntry::ToolCall(tool_call) = entry
                && let Some(SandboxNotAppliedReason::ErrorLinuxWsl(error)) =
                    &tool_call.sandbox_not_applied
            {
                return Some(error.clone());
            }
            None
        })
    }

    /// A small "Learn more" link to the sandboxing docs, deep-linked to
    /// `section` when provided. Shared by the sandbox warning and the two
    /// sandbox approval prompts so the user can always reach an explanation of
    /// what they're being asked about.
    fn render_sandbox_docs_link(
        &self,
        id: &'static str,
        section: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let url = zed_urls::sandboxing_docs(section, cx);

        Button::new(id, "View Sandboxing Docs")
            .label_size(LabelSize::Small)
            .color(Color::Muted)
            .end_icon(
                Icon::new(IconName::ArrowUpRight)
                    .color(Color::Muted)
                    .size(IconSize::XSmall),
            )
            .tooltip({
                let url = url.clone();
                move |_, cx| Tooltip::with_meta("Open Docs", None, url.clone(), cx)
            })
            .on_click(move |_, _, cx| cx.open_url(&url))
            .into_any_element()
    }

    fn render_sandbox_authorization_details(
        &self,
        entry_ix: usize,
        tool_call_id: &acp_v2::ToolCallId,
        details: &SandboxAuthorizationDetails,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let has_write = details.allow_fs_write_all || !details.write_paths.is_empty();
        // The dedicated Windows-drive warning prompt is only ever sent while the
        // warning is enabled, so key the banner on the prompt itself. Keeping it
        // visible even after the "Don't show again" checkbox flips the setting
        // avoids the card disappearing out from under the user mid-decision.
        let has_windows_fs_warning = details.warn_windows_fs;
        if !has_write
            && !details.unsandboxed
            && details.reason.is_empty()
            && !has_windows_fs_warning
        {
            return Empty.into_any_element();
        }

        let confusable_findings = if Self::confusable_warning_enabled(cx) {
            Self::sandbox_confusable_findings(details)
        } else {
            Vec::new()
        };

        let write_section = has_write.then(|| {
            let summary = if details.allow_fs_write_all {
                "unrestricted except Git metadata".to_string()
            } else {
                format!(
                    "{} {}",
                    details.write_paths.len(),
                    if details.write_paths.len() == 1 {
                        "path"
                    } else {
                        "paths"
                    }
                )
            };
            let has_path_list = !details.allow_fs_write_all && !details.write_paths.is_empty();
            let is_open = !self
                .collapsed_sandbox_authorization_details
                .contains(tool_call_id);
            let mut paths = details.write_paths.clone();
            // Sort by the path that is actually granted (the resolved canonical
            // when present, else the requested path).
            paths.sort_by(|a, b| a.canonical_or_requested().cmp(b.canonical_or_requested()));

            v_flex()
                .child(
                    h_flex()
                        .id(("sandbox-authorization-details-header", entry_ix))
                        .px_2()
                        .py_1()
                        .justify_between()
                        .when(has_path_list, |this| {
                            this.cursor_pointer()
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .on_click(cx.listener({
                                    let tool_call_id = tool_call_id.clone();
                                    move |this, _event, _window, cx| {
                                        if this
                                            .collapsed_sandbox_authorization_details
                                            .remove(&tool_call_id)
                                        {
                                            cx.notify();
                                            return;
                                        }

                                        this.collapsed_sandbox_authorization_details
                                            .insert(tool_call_id.clone());
                                        cx.notify();
                                    }
                                }))
                        })
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Label::new("Write Access")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new("•")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Disabled),
                                )
                                .child(
                                    Label::new(summary)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                        .when(has_path_list, |this| {
                            this.child(
                                Disclosure::new(
                                    ("sandbox-authorization-details", entry_ix),
                                    is_open,
                                )
                                .opened_icon(IconName::ChevronUp)
                                .closed_icon(IconName::ChevronDown),
                            )
                        }),
                )
                .when(has_path_list && is_open, |this| {
                    this.child(v_flex().children(paths.iter().enumerate().map(
                        |(path_ix, path)| {
                            self.render_sandbox_authorization_path_row(entry_ix, path_ix, path, cx)
                        },
                    )))
                })
        });

        let unsandboxed_section = details.unsandboxed.then(|| {
            h_flex()
                .px_2()
                .py_1()
                .gap_1p5()
                .child(
                    Icon::new(IconName::Warning)
                        .color(Color::Warning)
                        .size(IconSize::Small),
                )
                .child(
                    Label::new("Runs without the OS sandbox")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
        });

        let reason_section = (!details.reason.is_empty()).then(|| {
            v_flex()
                .px_2()
                .py_1()
                .gap_0p5()
                .child(
                    Label::new("Reason")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(Label::new(details.reason.clone()).size(LabelSize::Small))
        });

        // The command stays in the tool-call title above; here we show what the
        // command is asking for (paths) and the agent's reason.
        v_flex()
            .border_t_1()
            .border_color(self.tool_card_border_color(cx))
            .when(has_windows_fs_warning, |this| {
                this.child(self.render_sandbox_windows_fs_warning(cx))
            })
            .when(!confusable_findings.is_empty(), |this| {
                this.child(self.render_sandbox_confusable_warning(
                    tool_call_id,
                    &confusable_findings,
                    window,
                    cx,
                ))
            })
            .children(write_section)
            .children(unsandboxed_section)
            .children(reason_section)
            .when(!has_windows_fs_warning, |this| {
                // The Windows-drive warning banner carries its own docs link, so
                // skip the default one that every other sandbox prompt appends.
                this.child(
                    h_flex()
                        .px_1()
                        .py_0p5()
                        .child(self.render_sandbox_docs_link(
                            "sandbox-authorization-docs-link",
                            None,
                            cx,
                        )),
                )
            })
            .into_any_element()
    }

    /// Scan the paths in a sandbox escalation request for surprising Unicode
    /// characters (homoglyphs, invisible characters, bidi overrides). Returns,
    /// for each offending value, the display string shown to the user and the
    /// distinct suspicious characters it contains. Empty when nothing is
    /// surprising.
    fn sandbox_confusable_findings(
        details: &SandboxAuthorizationDetails,
    ) -> Vec<(String, Vec<unicode_confusables::SuspiciousChar>)> {
        let mut findings = Vec::new();
        for granted in &details.write_paths {
            // Scan both the requested path and the resolved target (when they
            // differ), so a confusable in either the shown request or the real
            // grant destination is surfaced.
            let requested = granted.requested.display().to_string();
            let resolved = granted.canonical_or_requested().display().to_string();
            for display in [requested, resolved]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
            {
                let suspicious = unicode_confusables::scan(&display);
                if !suspicious.is_empty() {
                    findings.push((display, suspicious));
                }
            }
        }
        findings
    }

    /// Whether the surprising-Unicode warning is enabled in settings (on by
    /// default). When off, prompts neither show the banner nor gate their allow
    /// buttons on it.
    fn confusable_warning_enabled(cx: &App) -> bool {
        AgentSettings::get_global(cx)
            .sandbox_permissions
            .warn_confusable_unicode
    }

    /// Whether this tool call's sandbox escalation shows surprising Unicode that
    /// the user hasn't acknowledged yet. While true, the prompt's allow buttons
    /// stay disabled so the user can't grant access to a lookalike target
    /// without first ticking the acknowledgement checkbox.
    fn sandbox_confusables_block_allow(&self, tool_call: &ToolCall, cx: &App) -> bool {
        if !Self::confusable_warning_enabled(cx) {
            return false;
        }
        let Some(details) = tool_call.sandbox_authorization_details.as_ref() else {
            return false;
        };
        if self
            .acknowledged_confusable_warnings
            .contains(&tool_call.id)
        {
            return false;
        }
        !Self::sandbox_confusable_findings(details).is_empty()
    }

    /// Red banner warning that a requested path contains surprising
    /// Unicode characters, with a checkbox the user must tick to unlock the
    /// allow buttons. See [`Self::sandbox_confusables_block_allow`].
    fn render_sandbox_confusable_warning(
        &self,
        tool_call_id: &acp_v2::ToolCallId,
        findings: &[(String, Vec<unicode_confusables::SuspiciousChar>)],
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let acknowledged = self.acknowledged_confusable_warnings.contains(tool_call_id);
        let line_height = window.line_height();

        v_flex()
            .w_full()
            .p_2()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().status().error_border)
            .bg(cx.theme().status().error_background.opacity(0.15))
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .items_start()
                    .child(
                        h_flex()
                            .h(line_height)
                            .flex_none()
                            .justify_center()
                            .child(
                                Icon::new(IconName::Warning)
                                    .size(IconSize::Small)
                                    .color(Color::Error),
                            ),
                    )
                    .child(
                        v_flex().min_w_0().flex_1().gap_1().children(findings.iter().map(
                            |(value, suspicious)| {
                                v_flex()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        Label::new(format!(
                                            "“{value}” contains potentially surprising Unicode characters"
                                        ))
                                        .size(LabelSize::Small)
                                        .color(Color::Error),
                                    )
                                    .child(v_flex().min_w_0().pl_2().children(
                                        suspicious.iter().map(|character| {
                                            Label::new(format!("• {}", character.description()))
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted)
                                                .buffer_font(cx)
                                        }),
                                    ))
                            },
                        )),
                    )
                    .child(
                        IconButton::new("configure-confusable-warning", IconName::Settings)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Configure unicode confusables warning"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(zed_actions::OpenSettingsAt {
                                        path: zed_actions::AGENT_SANDBOX_SETTINGS_PATH.to_string(),
                                        target: None,
                                    }),
                                    cx,
                                );
                            }),
                    ),
            )
            .child(
                Checkbox::new(
                    SharedString::from(format!("confusable-ack-{}", tool_call_id.0)),
                    if acknowledged {
                        ToggleState::Selected
                    } else {
                        ToggleState::Unselected
                    },
                )
                .label("I understand and wish to proceed")
                .label_size(LabelSize::Small)
                .on_click(cx.listener({
                    let tool_call_id = tool_call_id.clone();
                    move |this, state: &ToggleState, _window, cx| {
                        if *state == ToggleState::Selected {
                            this.acknowledged_confusable_warnings
                                .insert(tool_call_id.clone());
                        } else {
                            this.acknowledged_confusable_warnings.remove(&tool_call_id);
                        }
                        cx.notify();
                    }
                })),
            )
            .into_any_element()
    }

    /// Whether the Windows-drive (DrvFs) weaker-guarantee warning is enabled in
    /// settings (on by default). Windows-only in effect: `warn_windows_fs` is
    /// never set on other platforms.
    fn ntfs_warning_enabled(cx: &App) -> bool {
        AgentSettings::get_global(cx)
            .sandbox_permissions
            .warn_ntfs_grants
    }

    /// Informational banner shown on a sandbox approval prompt when the command
    /// will write to a file on a Windows drive (reached inside WSL via DrvFs),
    /// whose sandbox-integrity guarantees are weaker than the distro's native
    /// filesystem. Unlike the confusable-Unicode banner this does not gate the
    /// allow buttons: the approval itself is the acknowledgement. A settings gear
    /// links to where the warning can be suppressed.
    fn render_sandbox_windows_fs_warning(&self, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().status().warning_border)
            .bg(cx.theme().status().warning_background.opacity(0.15))
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .items_start()
                    .child(
                        Icon::new(IconName::Warning)
                            .size(IconSize::Small)
                            .color(Color::Warning),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_0p5()
                            .child(
                                Label::new("This command can write to a file on a Windows drive")
                                    .size(LabelSize::Small)
                                    .color(Color::Warning),
                            )
                            .child(
                                Label::new(
                                    "Sandboxes with write access to a location on a Windows \
                                     drive may not provide full filesystem isolation.",
                                )
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                            )
                            .child(h_flex().child(self.render_sandbox_docs_link(
                                "sandbox-windows-fs-docs-link",
                                Some("windows"),
                                cx,
                            ))),
                    )
                    .child(
                        IconButton::new("configure-ntfs-warning", IconName::Settings)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Configure Windows-drive warning"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    Box::new(zed_actions::OpenSettingsAt {
                                        path: zed_actions::AGENT_SANDBOX_SETTINGS_PATH.to_string(),
                                        target: None,
                                    }),
                                    cx,
                                );
                            }),
                    ),
            )
            .child(
                Checkbox::new(
                    "sandbox-windows-fs-dont-warn",
                    if Self::ntfs_warning_enabled(cx) {
                        ToggleState::Unselected
                    } else {
                        ToggleState::Selected
                    },
                )
                .label("Don't show this warning again")
                .label_size(LabelSize::Small)
                .on_click(cx.listener(|this, state: &ToggleState, _window, cx| {
                    let disable = *state == ToggleState::Selected;
                    let fs = this.thread.read(cx).project().read(cx).fs().clone();
                    update_settings_file(fs, cx, move |settings, _| {
                        settings
                            .agent
                            .get_or_insert_default()
                            .sandbox_permissions
                            .get_or_insert_default()
                            .warn_ntfs_grants = Some(!disable);
                    });
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn render_sandbox_fallback_authorization_details(
        &self,
        details: &SandboxFallbackAuthorizationDetails,
        cx: &Context<Self>,
    ) -> AnyElement {
        // The command itself is shown in the tool-call header (a collapsible
        // command), so here we only explain *why* the sandbox couldn't be
        // created — the user needs both to decide whether to run unsandboxed.
        if details.reason.is_empty() {
            return Empty.into_any_element();
        }

        h_flex()
            .p_1p5()
            .gap_1p5()
            .items_start()
            .border_t_1()
            .border_color(self.tool_card_border_color(cx))
            .child(
                Icon::new(IconName::Warning)
                    .color(Color::Warning)
                    .size(IconSize::Small),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        Label::new("Couldn't create a sandbox")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(details.reason.clone()).size(LabelSize::Small))
                    .child(self.render_sandbox_docs_link(
                        "sandbox-fallback-docs-link",
                        details.docs_section.as_deref(),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_sandbox_authorization_path_row(
        &self,
        entry_ix: usize,
        path_ix: usize,
        granted: &settings::GrantedWritePath,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        // The path that is actually granted is the resolved canonical target.
        // When the request went through a symlink to a *different* target, both
        // paths are shown, each explicitly captioned, so it's unmistakable which
        // string was requested and which location write access is really granted
        // to.
        let granted_path = granted.canonical_or_requested();
        let requested_path = granted.requested.clone();
        // Grants are stored in the request's own namespace (a Windows path stays
        // `C:\...`, a WSL path stays `/...`), so a genuine symlink/junction
        // redirect is just a plain inequality between the request and its
        // resolved canonical.
        let is_redirected = granted
            .resolved
            .as_deref()
            .is_some_and(|resolved| resolved != requested_path.as_path());

        let granted_display = granted_path.display().to_string();
        let requested_display = requested_path.display().to_string();

        let captioned_path = |caption: SharedString, path: String, cx: &Context<Self>| {
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(
                    Label::new(caption)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(Label::new(path).size(LabelSize::Small).buffer_font(cx))
        };

        v_flex()
            .id(format!("sandbox-authorization-path-{entry_ix}-{path_ix}"))
            .min_w_0()
            .gap_1()
            .px_2()
            .py_1p5()
            .bg(cx.theme().colors().editor_background)
            .map(|this| {
                if is_redirected {
                    this.child(captioned_path("Source".into(), requested_display, cx))
                        .child(
                            Icon::new(IconName::ArrowDown)
                                .color(Color::Muted)
                                .size(IconSize::Small),
                        )
                        .child(captioned_path("Target".into(), granted_display, cx))
                } else {
                    // Not a genuine redirect: show what the user asked for (e.g.
                    // the `C:\...` path), not the internal Linux canonical.
                    this.child(captioned_path("Write Path".into(), requested_display, cx))
                }
            })
            .child(Divider::horizontal())
    }

    fn open_tool_call_location(
        &self,
        entry_ix: usize,
        location_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let (tool_call_location, agent_location) = self
            .thread
            .read(cx)
            .entries()
            .get(entry_ix)?
            .location(location_ix)?;

        let project_path = self
            .project
            .upgrade()?
            .read(cx)
            .find_project_path(&tool_call_location.path, cx);

        let open_task = self
            .workspace
            .update(cx, |workspace, cx| {
                if let Some(project_path) = project_path {
                    workspace.open_path(project_path, None, true, window, cx)
                } else {
                    workspace.open_abs_path(
                        tool_call_location.path.clone(),
                        OpenOptions {
                            focus: Some(true),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                }
            })
            .log_err()?;
        window
            .spawn(cx, async move |cx| {
                let item = open_task.await?;

                let Some(active_editor) = item.downcast::<Editor>() else {
                    return anyhow::Ok(());
                };

                active_editor.update_in(cx, |editor, window, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    if snapshot.as_singleton().is_some()
                        && let Some(anchor) = snapshot.anchor_in_excerpt(agent_location.position)
                    {
                        editor.change_selections(Default::default(), window, cx, |selections| {
                            selections.select_anchor_ranges([anchor..anchor]);
                        })
                    } else {
                        let row = tool_call_location.line.unwrap_or_default();
                        editor.change_selections(Default::default(), window, cx, |selections| {
                            selections.select_ranges([Point::new(row, 0)..Point::new(row, 0)]);
                        })
                    }
                })?;

                anyhow::Ok(())
            })
            .detach_and_log_err(cx);

        None
    }

    fn render_tool_call_content(
        &self,
        session_id: &acp_v2::SessionId,
        entry_ix: usize,
        content: &ToolCallContent,
        context_ix: usize,
        tool_call: &ToolCall,
        card_layout: bool,
        focus_handle: &FocusHandle,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        match content {
            ToolCallContent::ContentBlock { block, .. } => self.render_output_content_block(
                entry_ix,
                context_ix,
                block.as_view(),
                Some(tool_call),
                card_layout,
                window,
                cx,
            ),
            ToolCallContent::Diff(diff) | ToolCallContent::LegacyDiff { diff, .. } => {
                self.render_diff_editor(entry_ix, diff, tool_call, cx)
            }
            ToolCallContent::Terminal { terminal, .. } => self.render_terminal_tool_call(
                session_id,
                entry_ix,
                terminal,
                tool_call,
                focus_handle,
                ToolCallLayout::Standalone,
                window,
                cx,
            ),
            ToolCallContent::DiffPatch { source, render } => {
                let files = render.files.iter().filter_map(|file| {
                    let change = source.changes.get(file.change_index)?;
                    Some(
                        v_flex()
                            .gap_1()
                            .p_2()
                            .when(context_ix > 0, |this| {
                                this.border_t_1()
                                    .border_color(self.tool_card_border_color(cx))
                            })
                            .child(
                                Label::new(acp_thread::diff_change_label(change))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .when(file.hunks.is_empty(), |this| {
                                this.child(
                                    Label::new("No text preview available")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                            })
                            .children(file.hunks.iter().map(|hunk| {
                                v_flex()
                                    .child(
                                        Label::new(hunk.header.clone())
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                    .when_some(
                                        self.entry_view_state.read(cx).entry(entry_ix).and_then(
                                            |entry| entry.editor_for_patch_hunk(&hunk.buffer),
                                        ),
                                        |this, editor| this.child(editor),
                                    )
                            })),
                    )
                });
                v_flex()
                    .children(files)
                    .when(
                        render.files.is_empty() && render.fallback.is_none(),
                        |this| {
                            this.p_2().child(
                                Label::new("No text preview available")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            )
                        },
                    )
                    .when_some(render.fallback.as_ref(), |this, markdown| {
                        this.child(self.render_markdown_output(
                            markdown.clone(),
                            entry_ix,
                            context_ix,
                            tool_call,
                            card_layout,
                            window,
                            cx,
                        ))
                    })
                    .into_any_element()
            }
            ToolCallContent::Other { markdown, .. } => self.render_markdown_output(
                markdown.clone(),
                entry_ix,
                context_ix,
                tool_call,
                card_layout,
                window,
                cx,
            ),
        }
    }

    fn render_output_content_block(
        &self,
        entry_ix: usize,
        context_ix: usize,
        content: acp_thread::ContentBlockView<'_>,
        tool_call: Option<&ToolCall>,
        card_layout: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        if let Some(markdown) = content.markdown() {
            if let Some(tool_call) = tool_call {
                self.render_markdown_output(
                    markdown.clone(),
                    entry_ix,
                    context_ix,
                    tool_call,
                    card_layout,
                    window,
                    cx,
                )
            } else {
                self.render_markdown(
                    markdown.clone(),
                    MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                    cx,
                )
                .into_any()
            }
        } else if let Some((resource, _)) = content.embedded_resource() {
            if tool_call.is_some() {
                self.render_embedded_resource_output(resource, context_ix, card_layout, cx)
            } else {
                self.render_embedded_resource_label(resource)
            }
        } else if let Some(resource_link) = content.resource_link() {
            self.render_resource_link(resource_link, cx)
        } else if let Some((image, _)) = content.image() {
            let location = tool_call.and_then(|tool_call| tool_call.locations.first().cloned());
            self.render_image_output(entry_ix, image.clone(), location, card_layout, cx)
        } else {
            Empty.into_any_element()
        }
    }

    fn render_embedded_resource_output(
        &self,
        resource: &acp_v2::EmbeddedResource,
        context_ix: usize,
        card_layout: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        v_flex()
            .gap_1()
            .map(|this| {
                if card_layout {
                    this.p_2().when(context_ix > 0, |this| {
                        this.border_t_1()
                            .border_color(self.tool_card_border_color(cx))
                    })
                } else {
                    this.ml(rems(0.4))
                        .px_3p5()
                        .border_l_1()
                        .border_color(self.tool_card_border_color(cx))
                }
            })
            .child(self.render_embedded_resource_label(resource))
            .into_any_element()
    }

    fn render_embedded_resource_label(&self, resource: &acp_v2::EmbeddedResource) -> AnyElement {
        let uri = match &resource.resource {
            acp_v2::EmbeddedResourceResource::BlobResourceContents(blob) => blob.uri.as_str(),
            acp_v2::EmbeddedResourceResource::TextResourceContents(text) => text.uri.as_str(),
            _ => "",
        };
        if uri.is_empty() {
            Empty.into_any_element()
        } else {
            Label::new(uri.to_string())
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .into_any_element()
        }
    }

    fn render_resource_link(
        &self,
        resource_link: &acp_v2::ResourceLink,
        cx: &Context<Self>,
    ) -> AnyElement {
        let uri: SharedString = resource_link.uri.clone().into();
        let is_file = resource_link.uri.strip_prefix("file://");

        let Some(project) = self.project.upgrade() else {
            return Empty.into_any_element();
        };

        let label: SharedString = if let Some(abs_path) = is_file {
            // Split off an optional `#L<line>` fragment so the path still resolves.
            let (abs_path, fragment) = abs_path
                .split_once('#')
                .map_or((abs_path, None), |(path, fragment)| (path, Some(fragment)));

            let path_label = if let Some(project_path) = project
                .read(cx)
                .project_path_for_absolute_path(&Path::new(abs_path), cx)
                && let Some(worktree) = project
                    .read(cx)
                    .worktree_for_id(project_path.worktree_id, cx)
            {
                worktree
                    .read(cx)
                    .full_path(&project_path.path)
                    .to_string_lossy()
                    .to_string()
            } else {
                abs_path.to_string()
            };

            match fragment {
                Some(fragment) => format!("{path_label}#{fragment}").into(),
                None => path_label.into(),
            }
        } else {
            uri.clone()
        };

        let button_id = SharedString::from(format!("item-{}", uri));

        div()
            .ml(rems(0.4))
            .pl_2p5()
            .border_l_1()
            .border_color(self.tool_card_border_color(cx))
            .overflow_hidden()
            .child(
                Button::new(button_id, label)
                    .label_size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate(true)
                    .when(is_file.is_none(), |this| {
                        this.end_icon(
                            Icon::new(IconName::ArrowUpRight)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                    })
                    .on_click(cx.listener({
                        let workspace = self.workspace.clone();
                        move |_, _, window, cx: &mut Context<Self>| {
                            open_link(uri.clone(), &workspace, window, cx);
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_markdown_output(
        &self,
        markdown: Entity<Markdown>,
        entry_ix: usize,
        context_ix: usize,
        tool_call: &ToolCall,
        card_layout: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let markdown_style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        let output = self
            .render_numbered_read_file_output(
                markdown.clone(),
                entry_ix,
                context_ix,
                tool_call,
                markdown_style.clone(),
                cx,
            )
            .unwrap_or_else(|| {
                self.render_markdown(markdown, markdown_style, cx)
                    .into_any()
            });

        v_flex()
            .gap_2()
            .map(|this| {
                if card_layout {
                    this.p_2().when(context_ix > 0, |this| {
                        this.border_t_1()
                            .border_color(self.tool_card_border_color(cx))
                    })
                } else {
                    this.ml(rems(0.4))
                        .px_3p5()
                        .border_l_1()
                        .border_color(self.tool_card_border_color(cx))
                }
            })
            .text_xs()
            .text_color(cx.theme().colors().text_muted)
            .child(output)
            .into_any_element()
    }

    fn render_numbered_read_file_output(
        &self,
        markdown: Entity<Markdown>,
        entry_ix: usize,
        context_ix: usize,
        tool_call: &ToolCall,
        markdown_style: MarkdownStyle,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let is_read_file = tool_call
            .tool_name
            .as_ref()
            .is_some_and(|tool_name| tool_name.as_ref() == "read_file");
        if !is_read_file {
            return None;
        }

        let markdown = markdown.read(cx);
        let parsed = parse_cat_numbered_markdown_code_block(markdown.source())?;
        let language = markdown.first_code_block_language();
        Some(render_cat_numbered_code_block(
            parsed,
            language,
            markdown_style,
            format!("copy-read-file-output-{entry_ix}-{context_ix}"),
            cx,
        ))
    }

    fn render_image_output(
        &self,
        entry_ix: usize,
        image: Arc<gpui::Image>,
        location: Option<ToolCallLocation>,
        card_layout: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        v_flex()
            .debug_selector(|| "agent-output-image".into())
            .gap_2()
            .map(|this| {
                if card_layout {
                    this
                } else {
                    this.ml(rems(0.4))
                        .px_3p5()
                        .border_l_1()
                        .border_color(self.tool_card_border_color(cx))
                }
            })
            .when_some(location, |this, _loc| {
                this.child(
                    h_flex().w_full().justify_end().child(
                        Button::new(("go-to-file", entry_ix), "Go to File")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_tool_call_location(entry_ix, 0, window, cx);
                            })),
                    ),
                )
            })
            .child(
                img(image)
                    .max_w_96()
                    .max_h_96()
                    .object_fit(ObjectFit::ScaleDown),
            )
            .into_any_element()
    }

    fn tool_card_border_color(&self, cx: &Context<Self>) -> Hsla {
        cx.theme().colors().border.opacity(0.8)
    }

    fn provider_by_name(name: &SharedString, cx: &App) -> Option<Arc<dyn LanguageModelProvider>> {
        LanguageModelRegistry::read_global(cx)
            .providers()
            .into_iter()
            .find(|provider| provider.name().0 == *name)
    }

    fn render_thread_error(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AlertCard> {
        let alert = match self.thread_error.as_ref()? {
            ThreadError::Other { message, .. } => {
                self.render_any_thread_error(message.clone(), window, cx)
            }
            ThreadError::Refusal => self.render_refusal_error(cx),
            ThreadError::DataRetentionConsentRequired => {
                self.render_data_retention_consent_error(cx)
            }
            ThreadError::AuthenticationRequired(error) => {
                self.render_authentication_required_error(error.clone(), cx)
            }
            ThreadError::ZedPaymentRequired => self.render_zed_payment_required_error(cx),
            ThreadError::RateLimitExceeded { provider } => self.render_error_callout(
                "Rate Limit Reached",
                format!(
                    "{provider}'s rate limit was reached. Zed will retry automatically. \
                    You can also wait a moment and try again."
                )
                .into(),
                true,
                true,
                cx,
            ),
            ThreadError::ServerOverloaded { provider } => self.render_error_callout(
                "Provider Unavailable",
                format!(
                    "{provider}'s servers are temporarily unavailable. Zed will retry \
                    automatically. If the problem persists, check the provider's status page."
                )
                .into(),
                true,
                true,
                cx,
            ),
            ThreadError::PromptTooLarge => self.render_prompt_too_large_error(cx),
            ThreadError::NoCredentials { provider } => {
                let message = Self::provider_by_name(provider, cx)
                    .map(|provider| provider.missing_credentials_error_message())
                    .unwrap_or_else(|| {
                        format!("No credentials are configured for {provider}.").into()
                    });
                self.render_error_callout("Credentials Missing", message, false, true, cx)
            }
            ThreadError::StreamError { provider } => self.render_error_callout(
                "Connection Interrupted",
                format!(
                    "The connection to {provider}'s API was interrupted. Zed will retry \
                    automatically. If the problem persists, check your network connection."
                )
                .into(),
                true,
                true,
                cx,
            ),
            ThreadError::AuthenticationFailed { provider } => {
                let message = Self::provider_by_name(provider, cx)
                    .map(|provider| provider.authentication_error_message())
                    .unwrap_or_else(|| format!("Could not authenticate with {provider}.").into());
                self.render_error_callout("Authentication Failed", message, false, false, cx)
            }
            ThreadError::PermissionDenied { provider, message } => {
                let message: SharedString = message.clone().unwrap_or_else(|| {
                    format!("{provider} rejected the request due to insufficient permissions.")
                        .into()
                });

                self.render_error_callout("Permission Denied", message, false, false, cx)
            }
            ThreadError::ProviderRejection { message } => {
                self.render_error_callout("Request Failed", message.clone(), true, false, cx)
            }
            ThreadError::MaxOutputTokens => self.render_error_callout(
                "Output Limit Reached",
                "The model stopped because it reached its maximum output length. \
                You can ask it to continue where it left off."
                    .into(),
                false,
                false,
                cx,
            ),
            ThreadError::NoModelSelected => self
                .render_model_not_available_error(cx)
                .unwrap_or_else(|| {
                    self.render_error_callout(
                        "No Model Selected",
                        "Select a model from the model picker below to get started.".into(),
                        false,
                        false,
                        cx,
                    )
                }),
            ThreadError::ApiError { provider } => self.render_error_callout(
                "API Error",
                format!(
                    "{provider}'s API returned an unexpected error. \
                    If the problem persists, try switching models or restarting Zed."
                )
                .into(),
                true,
                true,
                cx,
            ),
        };

        Some(alert)
    }

    fn render_refusal_error(&self, cx: &mut Context<'_, Self>) -> AlertCard {
        let model_or_agent_name = self.current_model_name(cx);
        let refusal_message = format!(
            "{} refused to respond to this prompt. \
            This can happen when a model believes the prompt violates its content policy \
            or safety guidelines, so rephrasing it can sometimes address the issue.",
            model_or_agent_name
        );

        AlertCard::new("thread-error", AlertVariant::Error, "Request Refused")
            .description(refusal_message.clone())
            .action(self.create_copy_button(refusal_message))
            .on_dismiss(self.dismiss_error(cx))
    }

    fn render_authentication_required_error(
        &self,
        error: SharedString,
        cx: &mut Context<Self>,
    ) -> AlertCard {
        AlertCard::new(
            "thread-error",
            AlertVariant::Error,
            "Authentication Required",
        )
        .description(error.clone())
        .action(self.create_copy_button(error))
        .action(self.authenticate_button(cx))
        .on_dismiss(self.dismiss_error(cx))
    }

    fn render_zed_payment_required_error(&self, cx: &mut Context<Self>) -> AlertCard {
        const ERROR_MESSAGE: &str =
            "You reached your free usage limit. Upgrade to Zed Pro for more prompts.";

        AlertCard::new("thread-error", AlertVariant::Error, "Free Usage Exceeded")
            .description(ERROR_MESSAGE)
            .action(self.create_copy_button(ERROR_MESSAGE))
            .action(self.upgrade_button(cx))
            .on_dismiss(self.dismiss_error(cx))
    }

    fn render_error_callout(
        &self,
        title: &'static str,
        message: SharedString,
        show_retry: bool,
        show_copy: bool,
        cx: &mut Context<Self>,
    ) -> AlertCard {
        let can_resume = show_retry && self.thread.read(cx).can_retry(cx);

        AlertCard::new("thread-error", AlertVariant::Error, title)
            .description(message.clone())
            .when(show_copy, |alert| {
                alert.action(self.create_copy_button(message))
            })
            .when(can_resume, |alert| alert.action(self.retry_button(cx)))
            .on_dismiss(self.dismiss_error(cx))
    }

    fn render_model_not_available_error(&self, cx: &mut Context<Self>) -> Option<AlertCard> {
        let thread = self.as_native_thread(cx)?;

        let has_authenticated_provider =
            LanguageModelRegistry::read_global(cx).has_authenticated_provider(cx);

        let (title, description): (SharedString, SharedString) =
            match thread.read(cx).thread_model() {
                agent::ThreadModel::Ready(_) => return None,
                agent::ThreadModel::Unresolved(selected_model) => {
                    if let Some(provider) = LanguageModelRegistry::global(cx)
                        .read(cx)
                        .provider(&&selected_model.provider)
                    {
                        if !provider.is_authenticated(cx) {
                            (
                                format!("Failed to authenticate with {} provider", provider.name())
                                    .into(),
                                "Open the settings to configure the selected provider".into(),
                            )
                        } else {
                            (
                                format!("Model {} was not found", selected_model.model.0).into(),
                                "You may need to reconfigure authentication for this provider"
                                    .into(),
                            )
                        }
                    } else {
                        (
                            format!("Provider {} was not found", selected_model.provider).into(),
                            "Open the settings to configure providers".into(),
                        )
                    }
                }
                agent::ThreadModel::Unset => {
                    if has_authenticated_provider {
                        (
                            "No model selected".into(),
                            "Choose a different model or configure other providers to get started"
                                .into(),
                        )
                    } else {
                        (
                            "No model selected".into(),
                            "Configure a provider to get started".into(),
                        )
                    }
                }
            };

        let alert = AlertCard::new("thread-error", AlertVariant::Error, title)
            .description(description)
            .action(self.open_llm_providers_settings_button(cx))
            .when(has_authenticated_provider, |alert| {
                alert.action(self.open_model_selector_button(cx))
            })
            .on_dismiss(self.dismiss_error(cx));

        Some(alert)
    }

    fn open_llm_providers_settings_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        PillButton::new("configure-llm-provider")
            .size(KitSize::Small)
            .variant(ButtonVariant::Secondary)
            .label("Configure Provider")
            .on_click(cx.listener(|this, _, window, cx| {
                this.clear_thread_error(cx);
                window.dispatch_action(
                    Box::new(zed_actions::OpenSettingsAt {
                        path: "llm_providers".to_string(),
                        target: None,
                    }),
                    cx,
                );
            }))
    }

    fn open_model_selector_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        PillButton::new("open-model-selector")
            .size(KitSize::Small)
            .variant(ButtonVariant::Secondary)
            .label("Select Model")
            .child(KeyBinding::for_action(&ToggleModelSelector, cx).size(rems_from_px(12_f32)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.clear_thread_error(cx);
                window.dispatch_action(ToggleModelSelector.boxed_clone(), cx);
            }))
    }

    fn render_prompt_too_large_error(&self, cx: &mut Context<Self>) -> AlertCard {
        const MESSAGE: &str = "This conversation is too long for the model's context window. \
            Start a new thread or remove some attached files to continue.";

        AlertCard::new("thread-error", AlertVariant::Error, "Context Too Large")
            .description(MESSAGE)
            .action(self.create_copy_button(MESSAGE))
            .action(self.new_thread_button(cx))
            .on_dismiss(self.dismiss_error(cx))
    }

    fn retry_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        PillButton::new("retry")
            .size(KitSize::Small)
            .variant(ButtonVariant::Secondary)
            .label("Retry")
            .on_click(cx.listener(|this, _, _, cx| {
                this.retry_generation(cx);
            }))
    }

    fn new_thread_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        PillButton::new("new_thread")
            .size(KitSize::Small)
            .variant(ButtonVariant::Secondary)
            .label("New Thread")
            .on_click(cx.listener(|this, _, window, cx| {
                this.clear_thread_error(cx);
                window.dispatch_action(NewThread.boxed_clone(), cx);
            }))
    }

    fn upgrade_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        PillButton::new("upgrade")
            .size(KitSize::Small)
            .variant(ButtonVariant::Primary)
            .label("Upgrade")
            .on_click(cx.listener({
                move |this, _, _, cx| {
                    this.clear_thread_error(cx);
                    cx.open_url(&zed_urls::upgrade_to_zed_pro_url(cx));
                }
            }))
    }

    fn authenticate_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        PillButton::new("authenticate")
            .size(KitSize::Small)
            .variant(ButtonVariant::Secondary)
            .label("Authenticate")
            .debug_selector("authenticate-submission")
            .on_click(cx.listener({
                move |this, _, window, cx| {
                    let server_view = this.server_view.clone();

                    this.clear_thread_error(cx);
                    if let Some(message) = this.in_flight_prompt(cx) {
                        if !message.iter().all(acp_thread::content::can_convert_to_v1)
                            || this.message_editor.read(cx).editor().read(cx).read_only(cx)
                        {
                            this.handle_thread_error(
                                anyhow!(
                                    "This saved submission cannot be restored into the composer. The original submission and draft have been kept."
                                ),
                                cx,
                            );
                            return;
                        }
                        if !this.thread.read(cx).uses_reported_activity()
                            && let Some(submission_id) = this.current_submission
                            && this.thread.read(cx).submission(submission_id).is_some_and(
                                |record| {
                                    matches!(
                                        record.state,
                                        SubmissionState::Completed
                                            | SubmissionState::Failed(_)
                                            | SubmissionState::Cancelled
                                    )
                                },
                            )
                        {
                            this.thread.update(cx, |thread, cx| {
                                thread.forget_submission(submission_id, cx);
                            });
                            this.current_submission = None;
                        }
                        this.message_editor.update(cx, |editor, cx| {
                            editor.set_message(message.to_vec(), window, cx);
                        });
                    }
                    let connection = this.thread.read(cx).connection().clone();
                    window.defer(cx, |window, cx| {
                        ConversationView::handle_auth_required(
                            server_view,
                            AuthRequired::new(),
                            connection,
                            window,
                            cx,
                        );
                    })
                }
            }))
    }

    fn current_model_name(&self, cx: &App) -> SharedString {
        // For native agent (Zed Agent), use the specific model name (e.g., "Claude 3.5 Sonnet")
        // For ACP agents, use the agent name (e.g., "Claude Agent", "Gemini CLI")
        // This provides better clarity about what refused the request
        if self.as_native_connection(cx).is_some() {
            self.model_selector
                .clone()
                .and_then(|selector| selector.read(cx).active_model(cx))
                .map(|model| model.name.clone())
                .unwrap_or_else(|| SharedString::from("The model"))
        } else {
            // ACP agent - use the agent name (e.g., "Claude Agent", "Gemini CLI")
            self.agent_id.0.clone()
        }
    }

    fn render_any_thread_error(
        &mut self,
        error: SharedString,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AlertCard {
        let can_resume = self.thread.read(cx).can_retry(cx);

        let markdown = if let Some(markdown) = &self.thread_error_markdown {
            markdown.clone()
        } else {
            let markdown = cx.new(|cx| Markdown::new(error.clone(), None, None, cx));
            self.thread_error_markdown = Some(markdown.clone());
            markdown
        };

        let markdown_style =
            MarkdownStyle::themed(MarkdownFont::Agent, window, cx).with_muted_text(cx);
        let description = self
            .render_markdown(markdown, markdown_style, cx)
            .into_any_element();

        AlertCard::new("thread-error", AlertVariant::Error, "An Error Happened")
            .description(description)
            .action(self.create_copy_button(error))
            .when(can_resume, |alert| alert.action(self.retry_button(cx)))
            .on_dismiss(self.dismiss_error(cx))
    }

    fn render_markdown(
        &self,
        markdown: Entity<Markdown>,
        style: MarkdownStyle,
        cx: &App,
    ) -> MarkdownElement {
        let list_state = self.list_state.clone();
        render_agent_markdown(
            markdown,
            style,
            &self.workspace,
            &self.code_span_resolver,
            cx,
        )
        // Zooming a diagram grows/shrinks its block; pause tail-following so the
        // viewport stays put instead of snapping back to the bottom. The list
        // resumes following on its own once the content returns to the bottom.
        .on_mermaid_zoom(move |_window, _cx| {
            list_state.pause_following_tail();
        })
    }

    fn create_copy_button(&self, message: impl Into<SharedString>) -> impl IntoElement {
        div()
            .id("copy-error-message")
            .flex_none()
            .tooltip(Tooltip::text("Copy Error Message"))
            .child(KitClipboard::new("copy-error-message").value(message))
    }

    fn dismiss_error(
        &self,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(|this, _, _, cx| {
            this.clear_thread_error(cx);
            cx.notify();
        })
    }

    fn render_resume_notice() -> AlertCard {
        let description = "This agent does not support viewing previous messages. However, your session will still continue from where you last left off.";

        AlertCard::new("resume-notice", AlertVariant::Info, "Resumed Session")
            .description(description)
    }

    fn render_codex_windows_warning(&self, cx: &mut Context<Self>) -> AlertCard {
        AlertCard::new(
            "codex-windows-warning",
            AlertVariant::Warning,
            "Codex on Windows",
        )
        .description("For best performance, run Codex in Windows Subsystem for Linux (WSL2)")
        .action(
            PillButton::new("open-wsl-modal")
                .size(KitSize::Small)
                .variant(ButtonVariant::Secondary)
                .label("Open in WSL")
                .on_click(cx.listener({
                    move |_, _, _window, cx| {
                        #[cfg(windows)]
                        _window.dispatch_action(
                            zed_actions::wsl_actions::OpenWsl::default().boxed_clone(),
                            cx,
                        );
                        cx.notify();
                    }
                })),
        )
        .on_dismiss(cx.listener({
            move |this, _, _, cx| {
                this.show_codex_windows_warning = false;
                cx.notify();
            }
        }))
    }

    fn render_session_notices(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notices = self.thread.read(cx).notices();
        if notices.is_empty() {
            return None;
        }

        Some(
            crate::ui::session_notice_list("session-notices")
                .debug_selector(|| "session-notices".into())
                .children(notices.iter().map(|(notice_id, notice)| {
                    let notice_id = *notice_id;
                    let thread = self.thread.clone();
                    let focus_handle = self.activation_focus_handle(cx);
                    crate::ui::SessionNotice::new(
                        ("session-notice", notice_id),
                        notice,
                        move |event, window, cx| {
                            thread.update(cx, |thread, cx| {
                                thread.dismiss_notice(notice_id, cx);
                            });
                            if event.is_keyboard() {
                                focus_handle.focus(window, cx);
                            }
                        },
                    )
                }))
                .into_any_element(),
        )
    }

    fn render_skill_loading_issues(&self, cx: &mut Context<Self>) -> Vec<AlertCard> {
        let description_warnings = self
            .skill_loading_issues
            .iter()
            .filter(|issue| issue.kind == SkillLoadingIssueKind::DescriptionTooLong)
            .cloned()
            .collect::<Vec<_>>();

        let long_description_warning =
            self.render_skill_description_warnings(description_warnings, cx);

        let other_warnings = self
            .skill_loading_issues
            .iter()
            .filter(|issue| issue.kind != SkillLoadingIssueKind::DescriptionTooLong)
            .enumerate()
            .map(|(index, issue)| {
                let abs_path = issue.path.clone();
                let workspace = self.workspace.clone();
                let path_label = issue.path.display().to_string();
                let target = issue.clone();

                let title = match issue.kind {
                    SkillLoadingIssueKind::LoadFailed => "Skill Failed to Load",
                    SkillLoadingIssueKind::DescriptionTooLong => unreachable!(),
                    SkillLoadingIssueKind::CatalogBudgetExceeded => {
                        "Skill Omitted from Model Catalog"
                    }
                };

                AlertCard::new(("skill-issue", index), AlertVariant::Warning, title)
                    .description(format!("{}\n{path_label}", issue.message))
                    .action(
                        PillButton::new(("open-skill-file", index))
                            .size(KitSize::Small)
                            .outline()
                            .label("Open Skill")
                            .on_click(cx.listener(move |_, _, window, cx| {
                                let abs_path = abs_path.clone();
                                workspace
                                    .update(cx, |workspace, cx| {
                                        workspace
                                            .open_abs_path(
                                                abs_path,
                                                workspace::OpenOptions::default(),
                                                window,
                                                cx,
                                            )
                                            .detach_and_log_err(cx);
                                    })
                                    .ok();
                            })),
                    )
                    .on_dismiss(cx.listener(move |this, _, _, cx| {
                        this.skill_loading_issues.retain(|issue| *issue != target);
                        this.dismissed_skill_loading_issues.insert(target.clone());
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        long_description_warning
            .into_iter()
            .chain(other_warnings)
            .collect()
    }

    fn render_skill_description_warnings(
        &self,
        description_warnings: Vec<SkillLoadingIssue>,
        cx: &mut Context<Self>,
    ) -> Option<AlertCard> {
        if description_warnings.is_empty() {
            return None;
        }

        let warning_count = description_warnings.len();
        let title = if warning_count == 1 {
            "1 Skill Loaded with a Long Description".to_string()
        } else {
            format!("{warning_count} Skills Loaded with Long Descriptions")
        };

        let rows = description_warnings
            .iter()
            .enumerate()
            .map(|(index, issue)| {
                let abs_path = issue.path.clone();
                let workspace = self.workspace.clone();
                let full_path = issue.path.display().to_string();
                let file_label = skill_issue_file_label(&issue.path);

                ButtonLike::new(("skill-description-warning-file", index))
                    .full_width()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                Icon::new(IconName::Dash)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .child(Label::new(file_label).size(LabelSize::Small)),
                    )
                    .tooltip(move |_, cx| {
                        Tooltip::with_meta("Open Skill", None, full_path.clone(), cx)
                    })
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let abs_path = abs_path.clone();
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace
                                    .open_abs_path(
                                        abs_path,
                                        workspace::OpenOptions::default(),
                                        window,
                                        cx,
                                    )
                                    .detach_and_log_err(cx);
                            })
                            .ok();
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let targets = description_warnings;

        Some(
            AlertCard::new("skill-description-warnings", AlertVariant::Warning, title)
                .description(
                    v_flex()
                        .gap_1()
                        .child(format!(
                            "Ensure skill descriptions are at most {MAX_SKILL_DESCRIPTION_LEN} characters; longer ones may consume more model-context tokens."
                        ))
                        .children(rows),
                )
                .on_dismiss(cx.listener(move |this, _, _, cx| {
                    this.skill_loading_issues
                        .retain(|issue| !targets.contains(issue));
                    for target in &targets {
                        this.dismissed_skill_loading_issues.insert(target.clone());
                    }
                    cx.notify();
                })),
        )
    }

    fn render_external_source_prompt_warning(&self, cx: &mut Context<Self>) -> AlertCard {
        AlertCard::new(
            "external-source-prompt-warning",
            AlertVariant::Warning,
            "Review Before Sending",
        )
        .description("This prompt was pre-filled by an external link. Read it carefully before you submit it to the model.")
        .on_dismiss(cx.listener({
            move |this, _, _, cx| {
                this.show_external_source_prompt_warning = false;
                cx.notify();
            }
        }))
    }

    fn render_multi_root_callout(&self, cx: &mut Context<Self>) -> Option<AlertCard> {
        if self.multi_root_callout_dismissed {
            return None;
        }

        if self.as_native_connection(cx).is_some() {
            return None;
        }

        if self
            .thread
            .read(cx)
            .connection()
            .supports_session_additional_directories()
        {
            return None;
        }

        let project = self.project.upgrade()?;
        let worktree_count = project.read(cx).visible_worktrees(cx).count();
        if worktree_count <= 1 {
            return None;
        }

        let work_dirs = self.thread.read(cx).work_dirs()?;
        let active_dir = work_dirs
            .ordered_paths()
            .next()
            .and_then(|p| p.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "one folder".to_string());

        Some(
            AlertCard::new(
                "multi-root-callout",
                AlertVariant::Warning,
                "This agent doesn't currently support multi-root workspaces",
            )
            .description(format!(
                "It currently only operates by default on \"{}\".",
                active_dir
            ))
            .on_dismiss(cx.listener(|this, _, _, cx| {
                this.multi_root_callout_dismissed = true;
                cx.notify();
            })),
        )
    }

    fn render_new_version_callout(&self, version: &SharedString, cx: &mut Context<Self>) -> Div {
        let server_view = self.server_view.clone();
        let has_version = !version.is_empty();
        let title = if has_version {
            "New Version Available"
        } else {
            "Agent Update Available"
        };
        let button_label = if has_version {
            format!("Update to v{}", version)
        } else {
            "Reconnect".to_string()
        };

        v_flex().w_full().justify_end().child(
            h_flex()
                .p_2()
                .pr_3()
                .w_full()
                .gap_1p5()
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().element_background)
                .child(
                    h_flex()
                        .flex_1()
                        .gap_1p5()
                        .child(
                            Icon::new(IconName::Download)
                                .color(Color::Accent)
                                .size(IconSize::Small),
                        )
                        .child(Label::new(title).size(LabelSize::Small)),
                )
                .child(
                    Button::new("update-button", button_label)
                        .label_size(LabelSize::Small)
                        .style(ButtonStyle::Tinted(TintColor::Accent))
                        .on_click(move |_, window, cx| {
                            server_view
                                .update(cx, |view, cx| view.reset(window, cx))
                                .ok();
                        }),
                ),
        )
    }

    fn render_token_limit_callout(&self, cx: &mut Context<Self>) -> Option<AlertCard> {
        if self.token_limit_callout_dismissed || self.as_native_thread(cx).is_none() {
            return None;
        }

        let token_usage = self.thread.read(cx).token_usage()?;

        // When auto-compaction is available (the model's context window is large
        // enough), the thread is compacted automatically before it reaches the
        // limit, so there's no need to warn the user. Models with a context
        // window that's too small can't be auto-compacted, so we fall back to
        // the normal warning.
        if token_usage.max_tokens >= agent::MIN_COMPACTION_CONTEXT_WINDOW {
            return None;
        }

        let ratio = token_usage.ratio();

        let (variant, title) = match ratio {
            acp_thread::TokenUsageRatio::Normal => return None,
            acp_thread::TokenUsageRatio::Warning => (
                AlertVariant::Warning,
                "Thread reaching the token limit soon",
            ),
            acp_thread::TokenUsageRatio::Exceeded => {
                (AlertVariant::Error, "Thread reached the token limit")
            }
        };

        let description = "To continue, run /compact or start a new thread and @-mention this one";

        Some(
            AlertCard::new("token-limit-callout", variant, title)
                .description(description)
                .action(
                    PillButton::new("start-new-thread")
                        .size(KitSize::Small)
                        .variant(ButtonVariant::Secondary)
                        .label("Start New Thread")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let session_id = this.thread.read(cx).session_id().clone();
                            window.dispatch_action(
                                crate::NewNativeAgentThreadFromSummary {
                                    from_session_id: session_id,
                                }
                                .boxed_clone(),
                                cx,
                            );
                        })),
                )
                .on_dismiss(self.dismiss_error(cx)),
        )
    }

    /// Returns the model to offer as a downgrade target when the current model
    /// requires data retention consent (e.g. Opus 4.8 for Fable).
    fn data_retention_fallback_model(&self, cx: &App) -> Option<LanguageModel> {
        let thread = self.as_native_thread(cx)?;
        let model = thread.read(cx).model()?.clone();
        let fallback_id = model.refusal_fallback_model_id()?;
        LanguageModelRegistry::read_global(cx)
            .available_models(cx)
            .find(|fallback| {
                fallback.provider_id() == model.provider_id()
                    && fallback.id().0.as_ref() == fallback_id
            })
    }

    fn render_data_retention_consent_error(&self, cx: &mut Context<Self>) -> AlertCard {
        let fallback_model = self.data_retention_fallback_model(cx);

        AlertCard::new(
            "thread-error",
            AlertVariant::Warning,
            format!(
                "Note: {} cannot be offered with Zero Data Retention.",
                self.current_model_name(cx)
            ),
        )
        .description(
            h_flex()
                .gap_1()
                .child("Anthropic will retain inference logs.")
                .child(
                    PillButton::new("data-retention-learn-more")
                        .size(KitSize::XSmall)
                        .variant(ButtonVariant::Link)
                        .label("Learn More")
                        .on_click(|_, _, cx| {
                            cx.open_url(DATA_RETENTION_LEARN_MORE_URL);
                        }),
                ),
        )
        .when_some(fallback_model, |alert, fallback| {
            alert.action(
                PillButton::new("switch-data-retention-fallback")
                    .size(KitSize::Small)
                    .outline()
                    .label(format!("Switch to {}", fallback.name().0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.switch_to_data_retention_fallback_and_resend(cx);
                    })),
            )
        })
        .action(
            PillButton::new("accept-data-retention")
                .size(KitSize::Small)
                .variant(ButtonVariant::Warning)
                .label("Accept")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.accept_data_retention_and_resend(cx);
                })),
        )
        .on_dismiss(self.dismiss_error(cx))
    }

    fn accept_data_retention_and_resend(&mut self, cx: &mut Context<Self>) {
        let fs = self.thread.read(cx).project().read(cx).fs().clone();
        // Resume the failed turn only once the in-memory settings reflect
        // consent, otherwise the resent request would be rejected again.
        let completion = update_settings_file_with_completion(fs, cx, |settings, _| {
            settings
                .telemetry
                .get_or_insert_default()
                .anthropic_retention = Some(true);
        });
        cx.spawn(async move |this, cx| {
            completion.await??;
            this.update(cx, |this, cx| this.retry_generation(cx))?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn switch_to_data_retention_fallback_and_resend(&mut self, cx: &mut Context<Self>) {
        let Some(fallback) = self.data_retention_fallback_model(cx) else {
            return;
        };
        let model_id = acp_thread::AgentModelId::new(format!(
            "{}/{}",
            fallback.provider_id().0,
            fallback.id().0
        ));
        let session_id = self.thread.read(cx).session_id().clone();
        let Some(selector) = self
            .thread
            .read(cx)
            .connection()
            .model_selector(&session_id)
        else {
            return;
        };
        let select = selector.select_model(model_id, cx);
        cx.spawn(async move |this, cx| {
            select.await?;
            this.update(cx, |this, cx| this.retry_generation(cx))?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn open_permission_dropdown(
        &mut self,
        _: &crate::OpenPermissionDropdown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu_handle = self.permission_dropdown_handle.clone();
        window.defer(cx, move |window, cx| {
            menu_handle.toggle(window, cx);
        });
    }

    fn open_add_context_menu(
        &mut self,
        _action: &OpenAddContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_add_menu(window, cx);
    }

    fn toggle_fast_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.fast_mode_available(cx) {
            return;
        }

        let Some(thread) = self.as_native_thread(cx) else {
            return;
        };

        let current_speed = thread.read(cx).speed().unwrap_or_default();
        let new_speed = current_speed.toggle();

        if new_speed == Speed::Fast && self.pending_fast_mode_confirmation(cx).is_some() {
            let menu_handle = self.fast_mode_menu_handle.clone();
            window.defer(cx, move |window, cx| {
                menu_handle.toggle(window, cx);
            });
            return;
        }

        self.apply_fast_mode_speed(new_speed, cx);
    }

    fn apply_fast_mode_speed(&mut self, new_speed: Speed, cx: &mut Context<Self>) {
        let Some(thread) = self.as_native_thread(cx) else {
            return;
        };
        thread.update(cx, |thread, cx| {
            thread.set_speed(new_speed, cx);

            let favorite_key = thread
                .model()
                .map(|model| (model.provider_id().0.to_string(), model.id().0.to_string()));
            let fs = thread.project().read(cx).fs().clone();
            update_settings_file(fs, cx, move |settings, _| {
                if let Some(agent) = settings.agent.as_mut() {
                    if let Some(default_model) = agent.default_model.as_mut() {
                        default_model.speed = Some(new_speed);
                    }
                    if let Some((provider_id, model_id)) = &favorite_key {
                        agent.update_favorite_model(provider_id, model_id, |favorite| {
                            favorite.speed = Some(new_speed)
                        });
                    }
                }
            });
        });
    }

    fn cycle_agent_thinking_effort(&mut self, cx: &mut Context<Self>) {
        let Some((model, enabled, current_effort)) = self.thinking_state(cx) else {
            return;
        };
        if !model.supports_thinking
            || (!enabled && model.supports_disabling_thinking_at_effort(current_effort.as_deref()))
        {
            return;
        }
        let levels = model.supported_effort_levels();
        if levels.is_empty() {
            return;
        }
        let next_index = current_effort
            .and_then(|current| levels.iter().position(|level| level.value == current))
            .map_or(0, |index| (index + 1) % levels.len());
        if let Some(next) = levels.get(next_index) {
            self.select_thinking_effort(next.value.to_string(), cx);
        }
    }
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the message editor's local slash commands in sync with the
        // current availability of feedback/sharing, which can change between
        // renders (settings, connection state, feature flags).
        self.sync_local_commands(cx);

        let has_messages = self.has_visible_conversation(cx);
        let has_docked_composer = has_messages && !self.is_subagent();
        let list_state = self.list_state.clone();
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
        let in_composer_column = |alert: AlertCard| composer_column(alert, max_content_width);

        let mut above_composer: Vec<AnyElement> = Vec::new();
        above_composer.extend(
            self.render_multi_root_callout(cx)
                .map(|alert| in_composer_column(alert).into_any_element()),
        );
        above_composer.extend(self.render_activity_bar(window, cx));
        above_composer.extend(
            self.render_recoverable_submissions(cx)
                .map(IntoElement::into_any_element),
        );
        above_composer.extend(self.render_session_notices(cx));
        if self.show_external_source_prompt_warning {
            above_composer.push(
                in_composer_column(self.render_external_source_prompt_warning(cx))
                    .into_any_element(),
            );
        }
        if self.show_codex_windows_warning {
            above_composer
                .push(in_composer_column(self.render_codex_windows_warning(cx)).into_any_element());
        }
        above_composer.extend(
            self.render_skill_loading_issues(cx)
                .into_iter()
                .map(|alert| in_composer_column(alert).into_any_element()),
        );
        above_composer.extend(
            self.render_thread_retry_status_callout(cx)
                .map(|alert| in_composer_column(alert).into_any_element()),
        );
        above_composer.extend(
            self.render_thread_error(window, cx)
                .map(|alert| in_composer_column(alert).into_any_element()),
        );
        if !has_messages && let Some(version) = self.new_server_version_available.clone() {
            above_composer.push(
                self.render_new_version_callout(&version, cx)
                    .into_any_element(),
            );
        }
        above_composer.extend(
            self.render_token_limit_callout(cx)
                .map(|alert| in_composer_column(alert).into_any_element()),
        );
        above_composer.extend(self.render_request_elicitations(cx));

        let (message_editor, changes_chips) =
            self.render_message_editor(!above_composer.is_empty(), window, cx);
        let (centered_message_editor, docked_message_editor) = if has_messages {
            (None, Some(message_editor))
        } else {
            (Some(message_editor), None)
        };
        let sticky_prompt = has_messages
            .then(|| self.render_sticky_prompt(cx))
            .flatten();
        let has_tool_row = changes_chips.is_some();
        let transcript_bottom_overlay = has_messages
            .then(|| self.render_transcript_bottom_overlay(changes_chips, window, cx))
            .flatten();

        let conversation = v_flex()
            .flex_1()
            .min_h_0()
            .when(self.resumed_without_history, |this| {
                this.child(in_composer_column(Self::render_resume_notice()))
            })
            .map(|this| {
                if has_messages {
                    this.flex_1()
                        .size_full()
                        .relative()
                        .child(self.render_entries(has_tool_row, cx))
                        .when_some(sticky_prompt, |this, sticky_prompt| {
                            this.child(
                                h_flex()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .right_0()
                                    .justify_center()
                                    .child(
                                        div()
                                            .when_some(max_content_width, |this, max_width| {
                                                this.max_w(max_width)
                                            })
                                            .w_full()
                                            .px_4()
                                            .pt_2()
                                            .child(sticky_prompt),
                                    ),
                            )
                        })
                        .children(transcript_bottom_overlay)
                        // A conversation reads like a document whose right edge is usually
                        // empty, so the thumb floats instead of taking a gutter.
                        .custom_scrollbars(
                            Scrollbars::new(ScrollAxes::Vertical)
                                .overlay()
                                .tracked_scroll_handle(&list_state),
                            window,
                            cx,
                        )
                        .into_any()
                } else {
                    this.child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .justify_center()
                            .children(centered_message_editor),
                    )
                    .into_any()
                }
            });

        v_flex()
            .key_context("AcpThread")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &menu::Cancel, _, cx| {
                if this.parent_session_id.is_none() {
                    this.cancel_generation(cx);
                }
            }))
            .on_action(cx.listener(
                |this, _: &super::thread_search_bar::DismissThreadSearch, window, cx| {
                    this.close_thread_search(window, cx);
                },
            ))
            // Esc can arrive as `editor::Cancel` from the query editor.
            .on_action(
                cx.listener(|this, _: &editor::actions::Cancel, window, cx| {
                    if !this.close_thread_search(window, cx) {
                        cx.propagate();
                    }
                }),
            )
            .on_action(cx.listener(
                |this, action: &super::thread_search_bar::SelectNextThreadMatch, window, cx| {
                    if !this.thread_search_visible {
                        cx.propagate();
                        return;
                    }
                    if let Some(bar) = this.thread_search_bar.clone() {
                        bar.update(cx, |bar, cx| bar.select_next_match(action, window, cx));
                    }
                },
            ))
            .on_action(cx.listener(
                |this, action: &super::thread_search_bar::SelectPreviousThreadMatch, window, cx| {
                    if !this.thread_search_visible {
                        cx.propagate();
                        return;
                    }
                    if let Some(bar) = this.thread_search_bar.clone() {
                        bar.update(cx, |bar, cx| bar.select_prev_match(action, window, cx));
                    }
                },
            ))
            .on_action(
                cx.listener(|this, _: &search::ToggleCaseSensitive, window, cx| {
                    if !this.thread_search_visible {
                        cx.propagate();
                        return;
                    }
                    if let Some(bar) = this.thread_search_bar.clone() {
                        bar.update(cx, |bar, cx| {
                            bar.toggle_case_sensitive(&search::ToggleCaseSensitive, window, cx)
                        });
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &search::ToggleWholeWord, window, cx| {
                    if !this.thread_search_visible {
                        cx.propagate();
                        return;
                    }
                    if let Some(bar) = this.thread_search_bar.clone() {
                        bar.update(cx, |bar, cx| {
                            bar.toggle_whole_word(&search::ToggleWholeWord, window, cx)
                        });
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &search::ToggleRegex, window, cx| {
                if !this.thread_search_visible {
                    cx.propagate();
                    return;
                }
                if let Some(bar) = this.thread_search_bar.clone() {
                    bar.update(cx, |bar, cx| {
                        bar.toggle_regex(&search::ToggleRegex, window, cx)
                    });
                }
            }))
            .on_action(
                cx.listener(|this, action: &search::FocusSearch, window, cx| {
                    if !this.thread_search_visible {
                        cx.propagate();
                        return;
                    }
                    if let Some(bar) = this.thread_search_bar.clone() {
                        bar.update(cx, |bar, cx| bar.focus_search(action, window, cx));
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &workspace::GoBack, window, cx| {
                if let Some(parent_session_id) = this.thread.read(cx).parent_session_id().cloned() {
                    this.server_view
                        .update(cx, |view, cx| {
                            view.navigate_to_thread(parent_session_id, window, cx);
                        })
                        .ok();
                }
            }))
            .on_action(cx.listener(Self::keep_all))
            .on_action(cx.listener(Self::reject_all))
            .on_action(cx.listener(Self::undo_last_reject))
            .on_action(cx.listener(Self::allow_always))
            .on_action(cx.listener(Self::allow_once))
            .on_action(cx.listener(Self::reject_once))
            .on_action(cx.listener(Self::handle_authorize_tool_call))
            .on_action(cx.listener(Self::handle_select_permission_granularity))
            .on_action(cx.listener(Self::handle_toggle_command_pattern))
            .on_action(cx.listener(Self::open_permission_dropdown))
            .on_action(cx.listener(Self::open_add_context_menu))
            .on_action(cx.listener(Self::scroll_output_page_up))
            .on_action(cx.listener(Self::scroll_output_page_down))
            .on_action(cx.listener(Self::scroll_output_line_up))
            .on_action(cx.listener(Self::scroll_output_line_down))
            .on_action(cx.listener(Self::scroll_output_to_top))
            .on_action(cx.listener(Self::scroll_output_to_bottom))
            .on_action(cx.listener(Self::scroll_output_to_previous_message))
            .on_action(cx.listener(Self::scroll_output_to_next_message))
            .on_action(cx.listener(Self::toggle_search))
            .on_action(cx.listener(|this, _: &ToggleFastMode, window, cx| {
                this.toggle_fast_mode(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleThinkingMode, _, cx| {
                if this.thread.read(cx).status() == ThreadStatus::Idle {
                    this.toggle_thinking(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CycleThinkingEffort, _window, cx| {
                if this.thread.read(cx).status() != ThreadStatus::Idle {
                    return;
                }
                if let Some(config_options_view) = this.config_options_view.clone() {
                    let handled = config_options_view.update(cx, |view, cx| {
                        view.cycle_category_option(
                            acp_v2::SessionConfigOptionCategory::ThoughtLevel,
                            false,
                            cx,
                        )
                    });
                    if handled {
                        return;
                    }
                }
                this.cycle_agent_thinking_effort(cx);
            }))
            .on_action(
                cx.listener(|this, _: &ToggleThinkingEffortMenu, window, cx| {
                    if this.thread.read(cx).status() != ThreadStatus::Idle {
                        return;
                    }
                    if let Some(config_options_view) = this.config_options_view.clone() {
                        let handled = config_options_view.update(cx, |view, cx| {
                            view.toggle_category_picker(
                                acp_v2::SessionConfigOptionCategory::ThoughtLevel,
                                window,
                                cx,
                            )
                        });
                        if handled {
                            return;
                        }
                    }
                    let menu_handle = this.thinking_effort_menu_handle.clone();
                    window.defer(cx, move |window, cx| {
                        menu_handle.toggle(window, cx);
                    });
                }),
            )
            .on_action(cx.listener(|this, _: &SendNextQueuedMessage, window, cx| {
                if let Some(id) = this.message_queue.first_id() {
                    this.send_queued_message_now(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RemoveFirstQueuedMessage, _, cx| {
                if let Some(id) = this.message_queue.first_id() {
                    this.remove_from_queue(id, cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &EditFirstQueuedMessage, window, cx| {
                if let Some(id) = this.message_queue.first_id() {
                    this.move_queued_message_to_main_editor(id, None, None, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &ToggleSteerFirstQueuedMessage, _, cx| {
                    if this.as_native_thread(cx).is_none() {
                        return;
                    }
                    if let Some(id) = this.message_queue.first_id() {
                        this.toggle_queue_entry_steer(id, cx);
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &ClearMessageQueue, _, cx| {
                this.clear_queue(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleProfileSelector, window, cx| {
                if let Some(config_options_view) = this.config_options_view.clone() {
                    let handled = config_options_view.update(cx, |view, cx| {
                        view.toggle_category_picker(
                            acp_v2::SessionConfigOptionCategory::Mode,
                            window,
                            cx,
                        )
                    });
                    if handled {
                        return;
                    }
                }

                // Native profiles are picked from the composer's add menu.
                if this.profile_selector.is_some() {
                    this.toggle_add_menu(window, cx);
                } else if let Some(mode_selector) = this.mode_selector.clone() {
                    mode_selector.read(cx).menu_handle().toggle(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CycleModeSelector, window, cx| {
                if this.thread.read(cx).status() != ThreadStatus::Idle {
                    return;
                }
                if let Some(config_options_view) = this.config_options_view.clone() {
                    let handled = config_options_view.update(cx, |view, cx| {
                        view.cycle_category_option(
                            acp_v2::SessionConfigOptionCategory::Mode,
                            false,
                            cx,
                        )
                    });
                    if handled {
                        return;
                    }
                }

                if let Some(profile_selector) = this.profile_selector.clone() {
                    profile_selector.update(cx, |profile_selector, cx| {
                        profile_selector.cycle_profile(cx);
                    });
                } else if let Some(mode_selector) = this.mode_selector.clone() {
                    mode_selector.update(cx, |mode_selector, cx| {
                        mode_selector.cycle_mode(window, cx);
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleModelSelector, window, cx| {
                if this.thread.read(cx).status() != ThreadStatus::Idle {
                    return;
                }
                if let Some(config_options_view) = this.config_options_view.clone() {
                    let handled = config_options_view.update(cx, |view, cx| {
                        view.toggle_category_picker(
                            acp_v2::SessionConfigOptionCategory::Model,
                            window,
                            cx,
                        )
                    });
                    if handled {
                        return;
                    }
                }

                if this.model_menu.is_some() {
                    this.close_model_menu(cx);
                } else {
                    this.open_model_menu(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CycleFavoriteModels, window, cx| {
                if this.thread.read(cx).status() != ThreadStatus::Idle {
                    return;
                }
                if let Some(config_options_view) = this.config_options_view.clone() {
                    let handled = config_options_view.update(cx, |view, cx| {
                        view.cycle_category_option(
                            acp_v2::SessionConfigOptionCategory::Model,
                            true,
                            cx,
                        )
                    });
                    if handled {
                        return;
                    }
                }

                if let Some(model_selector) = this.model_selector.clone() {
                    model_selector.update(cx, |model_selector, cx| {
                        model_selector.cycle_favorite_models(window, cx);
                    });
                }
            }))
            .size_full()
            .when(!has_docked_composer, |this| this.pb_2())
            .bg(cx.theme().colors().surface_background)
            .children(self.render_subagent_titlebar(cx))
            .when_some(
                self.thread_search_visible
                    .then(|| self.thread_search_bar.clone())
                    .flatten(),
                |this, bar| this.child(bar),
            )
            .child(conversation)
            .children(above_composer)
            .children(docked_message_editor)
    }
}

/// Lines `content` up with the composer, in the column it shares with the transcript.
fn composer_column(content: impl IntoElement, max_content_width: Option<Pixels>) -> Div {
    h_flex().w_full().px_4().pt_2().justify_center().child(
        div()
            .w_full()
            .min_w_0()
            .when_some(max_content_width, |this, max_width| this.max_w(max_width))
            .child(content),
    )
}

pub(crate) fn open_link(
    url: SharedString,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(workspace) = workspace.upgrade() else {
        return;
    };

    let path_style = workspace.read(cx).path_style(cx);
    let path_url = url
        .strip_prefix('`')
        .and_then(|path| path.strip_suffix('`'))
        .unwrap_or(&url);
    if let Some((path, fragment)) = file_link_parts(path_url, path_style) {
        if !path.is_empty() {
            let fragment_point = fragment
                .and_then(source_position_from_fragment)
                .map(|(row, column)| Point::new(row, column));
            let candidates = file_link_candidates(path, fragment_point, path_style);
            let project = workspace.read(cx).project().downgrade();
            let Ok((roots, root_names)) = project.read_with(cx, |project, cx| {
                project
                    .visible_worktrees(cx)
                    .filter_map(|worktree| {
                        let worktree = worktree.read(cx);
                        (!worktree.is_single_file())
                            .then(|| (worktree.abs_path(), worktree.root_name().to_owned()))
                    })
                    .unzip::<_, _, Vec<_>, Vec<_>>()
            }) else {
                return;
            };
            let workspace = workspace.downgrade();
            window
                .spawn(cx, async move |cx| {
                    let mut target = None;
                    let mut failures = Vec::new();
                    'resolve: for (path, point) in candidates {
                        let Ok(project_path) = project.read_with(cx, |project, cx| {
                            project_path_for_file_link(project, &path, cx)
                        }) else {
                            return Ok(());
                        };
                        if let Some(project_path) = project_path {
                            target = Some((
                                ResolvedPath::ProjectPath {
                                    project_path,
                                    is_dir: false,
                                },
                                point,
                            ));
                            break;
                        }
                        let paths = if path_style.has_root(&path)
                            || path.starts_with("~")
                        {
                            vec![path]
                        } else {
                            roots
                                .iter()
                                .zip(&root_names)
                                .flat_map(|(root, root_name)| {
                                    [
                                        path.strip_prefix(root_name.as_std_path()).ok(),
                                        Some(path.as_path()),
                                    ]
                                    .into_iter()
                                    .flatten()
                                    .filter_map(move |path| {
                                        path_style.join_path_preserving_components(root, path).ok()
                                    })
                                })
                                .collect::<Vec<_>>()
                        };
                        for path in paths {
                            let Some(path_string) = path.to_str() else {
                                failures.push(format!("{path:?}: path is not valid UTF-8"));
                                continue;
                            };
                            let Ok(task) = project.update(cx, |project, cx| {
                                project.resolve_abs_file_link(path_string, cx)
                            }) else {
                                return Ok(());
                            };
                            let resolved_path = match task.await {
                                Ok(Some(path)) => path,
                                Ok(None) => {
                                    failures.push(format!("{path:?}: no matching file"));
                                    continue;
                                }
                                Err(error) => {
                                    failures.push(format!("{path:?}: {error:#}"));
                                    continue;
                                }
                            };
                            target = Some((resolved_path, point));
                            break 'resolve;
                        }
                    }
                    let Some((target, point)) = target else {
                        let details = if failures.is_empty() {
                            "no candidate file paths".to_string()
                        } else {
                            failures.join("; ")
                        };
                        log::warn!(
                            "Could not resolve agent file link {url:?} against project roots {roots:?}: {details}"
                        );
                        return anyhow::Ok(());
                    };
                    let Some(task) = workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_resolved_path(target, window, cx)
                        })
                        .ok()
                    else {
                        return Ok(());
                    };
                    let item = task.await?;
                    if let Some(point) = point
                        && let Some(editor) = item.downcast::<Editor>()
                    {
                        editor
                            .update_in(cx, |editor, window, cx| {
                                if let Some(buffer) = editor.buffer().read(cx).as_singleton() {
                                    let point = buffer
                                        .read(cx)
                                        .snapshot()
                                        .point_from_external_input(point.row, point.column);
                                    editor.go_to_singleton_buffer_point(point, window, cx);
                                }
                            })
                            .ok();
                    }
                    Ok(())
                })
                .detach_and_log_err(cx);
            return;
        }
    }

    if let Some(mention) = MentionUri::parse_hyperlink(&url, path_style).log_err() {
        // Percent escapes in bare paths are ambiguous: prefer the decoded
        // interpretation, falling back to the literal one (e.g. a file
        // actually named `a%20b.rs`) only when the decoded path doesn't
        // resolve in the project but the literal one does.
        let resolves_in_project = |mention: &MentionUri, cx: &App| {
            mention.abs_path().is_some_and(|abs_path| {
                let project = workspace.read(cx).project().read(cx);
                project
                    .find_project_path(abs_path, cx)
                    .is_some_and(|path| project.entry_for_path(&path, cx).is_some())
            })
        };
        let mention = match MentionUri::parse_hyperlink_literal(&url, path_style) {
            Some(literal)
                if !resolves_in_project(&mention, cx) && resolves_in_project(&literal, cx) =>
            {
                literal
            }
            _ => mention,
        };
        workspace.update(cx, |workspace, cx| match mention {
            MentionUri::File { abs_path } => {
                open_abs_path_at_point(workspace, abs_path, None, window, cx);
            }
            MentionUri::PastedImage { .. } => {}
            MentionUri::Directory { abs_path } => {
                let project = workspace.project();
                let Some(entry_id) = project.update(cx, |project, cx| {
                    let path = project.find_project_path(abs_path, cx)?;
                    project.entry_for_path(&path, cx).map(|entry| entry.id)
                }) else {
                    return;
                };

                project.update(cx, |_, cx| {
                    cx.emit(project::Event::RevealInProjectPanel(entry_id));
                });
            }
            MentionUri::Symbol {
                abs_path: path,
                line_range,
                ..
            } => {
                open_abs_path_at_point(
                    workspace,
                    path,
                    Some(Point::new(*line_range.start(), 0)),
                    window,
                    cx,
                );
            }
            MentionUri::Selection {
                abs_path: Some(path),
                line_range,
                column,
            } => {
                open_abs_path_at_point(
                    workspace,
                    path,
                    Some(Point::new(*line_range.start(), column.unwrap_or(0))),
                    window,
                    cx,
                );
            }
            MentionUri::Selection { abs_path: None, .. } => {}
            MentionUri::Thread { id, name } => {
                if let Some(panel) = workspace.panel::<AgentPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.open_thread(id, None, Some(name.into()), window, cx)
                    });
                }
            }
            MentionUri::Fetch { url } => {
                cx.open_url(url.as_str());
            }
            MentionUri::Diagnostics { .. } => {}
            MentionUri::TerminalSelection { .. } => {}
            MentionUri::GitDiff { .. } => {}
            MentionUri::MergeConflict { .. } => {}
            MentionUri::Rule { name, .. } => {
                crate::ui::open_migrated_rule(workspace, &name, window, cx);
            }
            MentionUri::Skill {
                skill_file_path, ..
            } => {
                workspace
                    .open_abs_path(
                        skill_file_path,
                        workspace::OpenOptions {
                            focus: Some(true),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                    .detach_and_log_err(cx);
            }
        })
    } else {
        workspace.update(cx, |workspace, cx| {
            workspace.open_url_or_file(&url, None, window, cx);
        });
    }
}

fn file_link_parts(input: &str, path_style: PathStyle) -> Option<(&str, Option<&str>)> {
    let (path, fragment) = input
        .split_once('#')
        .map_or((input, None), |(path, fragment)| (path, Some(fragment)));
    if !path_style.has_root(Path::new(path))
        && let Ok(url) = Url::parse(input)
        && (!url.scheme().contains('.')
            || url.path().trim_matches(':').is_empty()
            || !PathWithPosition::parse_str(path)
                .path
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(url.scheme())))
    {
        return None;
    }
    Some((path, fragment))
}

fn file_link_candidates(
    path: &str,
    fragment_point: Option<Point>,
    path_style: PathStyle,
) -> Vec<(PathBuf, Option<Point>)> {
    if path_style.is_windows() && path_style.has_root(Path::new(path)) {
        return [
            MentionUri::parse_hyperlink(path, path_style).ok(),
            MentionUri::parse_hyperlink_literal(path, path_style),
        ]
        .into_iter()
        .flatten()
        .filter_map(|mention| match mention {
            MentionUri::File { abs_path } => Some((abs_path, fragment_point)),
            MentionUri::Selection {
                abs_path: Some(abs_path),
                line_range,
                column,
            } => Some((
                abs_path,
                Some(
                    fragment_point.unwrap_or(Point::new(*line_range.start(), column.unwrap_or(0))),
                ),
            )),
            _ => None,
        })
        .collect();
    }
    let decoded_path = decode_path_escapes(path);
    let mut candidates = Vec::new();
    for path in std::iter::once(decoded_path.as_ref()).chain((decoded_path != path).then_some(path))
    {
        let path = if path_style.is_windows() {
            PathBuf::from(path.replace('\\', "/"))
        } else {
            PathBuf::from(path)
        };
        candidates.push((path.clone(), fragment_point));
        let position = PathWithPosition::parse_str(&path.to_string_lossy());
        if let Some(row) = position.row.and_then(|row| row.checked_sub(1)) {
            candidates.push((
                position.path,
                Some(fragment_point.unwrap_or(Point::new(
                    row,
                    position.column.unwrap_or(1).saturating_sub(1),
                ))),
            ));
        }
    }
    candidates
}

/// Returns the name of the leading built-in (native-category) slash command —
/// e.g. `compact` for `/compact` or `/compact summarize the API work` — whether
/// or not the user typed any trailing text after it. Built-in commands ignore
/// trailing arguments, so the caller sends the bare command and queues any
/// remainder rather than discarding it. Commands from MCP servers and ACP
/// agents are excluded: their trailing text is a real argument the agent
/// consumes.
///
/// Native commands run a turn that produces its own thread entry, so the typed
/// command is never echoed as a user message (see `send_command_queueing_remainder`).
fn leading_native_command(
    text: &str,
    available_commands: &[acp_v2::AvailableCommand],
) -> Option<String> {
    let rest = text.trim_start().strip_prefix('/')?;
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..name_end];
    let is_native = available_commands.iter().any(|command| {
        command.name == name
            && acp_thread::command_category_from_meta(&command.meta)
                == Some(acp_thread::CommandCategory::Native)
    });
    is_native.then(|| name.to_string())
}

/// Removes a leading `/command_name` token from `text`, returning the trimmed
/// remainder. Falls back to the trimmed input if the prefix isn't present.
fn strip_leading_command(text: &str, command_name: &str) -> String {
    let trimmed = text.trim_start();
    trimmed
        .strip_prefix('/')
        .and_then(|rest| rest.strip_prefix(command_name))
        .map(|rest| rest.trim_start().to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::UpdateGlobal;
    use project::{FakeFs, Project};
    use serde_json::json;
    use settings::{SettingsStore, SplicingVec};
    use std::sync::Once;
    use util::path;
    use workspace::MultiWorkspace;

    #[test]
    fn test_reported_activity_completion_status() {
        use acp_v2::StopReason::*;
        for (reason, expected) in [
            (None, "unknown"),
            (
                Some(Other(acp_v2::OtherStopReason::new(
                    "_custom",
                    Default::default(),
                ))),
                "unknown",
            ),
            (Some(EndTurn), "success"),
            (Some(Cancelled), "cancelled"),
            (Some(Refusal), "failure"),
            (Some(MaxTokens), "failure"),
            (Some(MaxTurnRequests), "failure"),
            (Some(Error(acp_v2::ErrorStopReason::new())), "failure"),
        ] {
            assert_eq!(
                ThreadView::activity_completion_status(reason.as_ref()),
                expected
            );
        }
    }

    #[test]
    fn test_tool_call_icon_tooltip() {
        for (name, interrupted_edit, expected) in [
            (None, false, None),
            (Some(" \t\n"), false, None),
            (Some("read_file"), false, Some("Tool: read_file")),
            (
                Some("  mcp__server__**read_file**<raw>  "),
                false,
                Some("Tool:   mcp__server__**read_file**<raw>  "),
            ),
            (None, true, Some("Interrupted Edit")),
            (Some(" \t\n"), true, Some("Interrupted Edit")),
            (
                Some("edit_file"),
                true,
                Some("Interrupted Edit\nTool: edit_file"),
            ),
        ] {
            let name = name.map(SharedString::from);
            assert_eq!(
                ThreadView::tool_call_icon_tooltip(name.as_ref(), interrupted_edit).as_deref(),
                expected,
            );
        }
    }

    fn native_command(name: &str) -> acp_v2::AvailableCommand {
        acp_v2::AvailableCommand::new(name, "").meta(acp_thread::meta_with_command_category(
            acp_thread::CommandCategory::Native,
        ))
    }

    fn mcp_command(name: &str) -> acp_v2::AvailableCommand {
        acp_v2::AvailableCommand::new(name, "").meta(acp_thread::meta_with_command_category(
            acp_thread::CommandCategory::Mcp,
        ))
    }

    #[test]
    fn test_leading_native_command_matches_bare_and_with_remainder() {
        let commands = [native_command("compact"), mcp_command("deploy")];

        // Native command with trailing text.
        assert_eq!(
            leading_native_command("/compact summarize the API work", &commands),
            Some("compact".to_string())
        );
        // Leading/trailing whitespace is tolerated.
        assert_eq!(
            leading_native_command("  /compact   do x  ", &commands),
            Some("compact".to_string())
        );

        // Bare native command (no remainder) is still recognized, so it runs as
        // a command turn (without echoing a user message) rather than being sent
        // to the model as a normal prompt.
        assert_eq!(
            leading_native_command("/compact", &commands),
            Some("compact".to_string())
        );
        assert_eq!(
            leading_native_command("/compact   ", &commands),
            Some("compact".to_string())
        );

        // MCP/ACP commands are not native: their trailing text is a real
        // argument the agent consumes, and they echo as normal user messages.
        assert_eq!(leading_native_command("/deploy prod", &commands), None);
        assert_eq!(leading_native_command("/deploy", &commands), None);

        // Unknown command, or not a slash command at all.
        assert_eq!(leading_native_command("/unknown foo", &commands), None);
        assert_eq!(leading_native_command("just a message", &commands), None);
    }

    #[test]
    fn test_strip_leading_command() {
        assert_eq!(strip_leading_command("/compact do x", "compact"), "do x");
        assert_eq!(
            strip_leading_command("  /compact  do x ", "compact"),
            "do x "
        );
        // No matching prefix: returns the trimmed input unchanged.
        assert_eq!(strip_leading_command("hello", "compact"), "hello");
    }

    #[test]
    fn test_file_link_parts() {
        for (input, expected) in [
            ("src/main.rs:2", Some(("src/main.rs:2", None))),
            ("./tel:123", Some(("./tel:123", None))),
            ("main.rs:2#L3", Some(("main.rs:2", Some("L3")))),
            ("main.rs:2:4#3C2", Some(("main.rs:2:4", Some("3C2")))),
            ("main.rs:0", Some(("main.rs:0", None))),
            ("main.rs:4294967295", Some(("main.rs:4294967295", None))),
            ("main.rs:4294967296", Some(("main.rs:4294967296", None))),
            ("main.rs:2:4294967296", Some(("main.rs:2:4294967296", None))),
            ("custom.proto:123", Some(("custom.proto:123", None))),
            ("main.rs:2:3:4", None),
            ("main.rs:2:", Some(("main.rs:2:", None))),
            ("MAIN.RS:2:", Some(("MAIN.RS:2:", None))),
            ("main.rs:-2", None),
            ("tel:123", None),
            ("custom:123", None),
            ("com.example.viewer:", None),
            ("com.example.viewer::", None),
            ("file:/project/a:2", None),
            ("file:///project/a%3A2", None),
            ("https://example.com/main.rs:2#L3", None),
        ] {
            assert_eq!(file_link_parts(input, PathStyle::Unix), expected, "{input}");
        }
    }

    #[test]
    fn test_file_link_candidates_windows() {
        for (path, expected_path, expected_point) in [
            (
                r"C:\project\main.rs#L2",
                r"C:\project\main.rs",
                Point::new(1, 0),
            ),
            (
                "/C:/project/main.rs#42",
                r"C:\project\main.rs",
                Point::new(41, 0),
            ),
            (
                "/c/project/main.rs:2#L3C4",
                r"C:\project\main.rs",
                Point::new(2, 3),
            ),
            (
                "//server/share/main.rs#42:3",
                r"\\server\share\main.rs",
                Point::new(41, 2),
            ),
            (
                "/C:/project/main.rs:2:4",
                r"C:\project\main.rs",
                Point::new(1, 3),
            ),
            (
                "/c/project/main.rs:2",
                r"C:\project\main.rs",
                Point::new(1, 0),
            ),
            (
                r"C:\project\a%20b.rs:2",
                r"C:\project\a b.rs",
                Point::new(1, 0),
            ),
        ] {
            let (path, fragment) = file_link_parts(path, PathStyle::Windows).unwrap();
            let fragment_point = fragment
                .and_then(source_position_from_fragment)
                .map(|(row, column)| Point::new(row, column));
            let candidates = file_link_candidates(path, fragment_point, PathStyle::Windows);
            assert_eq!(
                candidates.first(),
                Some(&(PathBuf::from(expected_path), Some(expected_point))),
                "{path}"
            );
        }
    }

    #[gpui::test]
    async fn test_open_link_bare_path(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({"src": {"main.rs": "first\nsecond\nthird\n"}}),
        )
        .await;

        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        let workspace_weak = workspace.downgrade();

        // Relative path — call from multi_workspace so the inner workspace entity is not locked
        multi_workspace.update_in(cx, |_, window, cx| {
            open_link("src/main.rs".into(), &workspace_weak, window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            let active = workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .expect("file should be open");
            assert!(*active.path == *"src/main.rs");
        });

        multi_workspace.update_in(cx, |_, window, cx| {
            open_link("src/main.rs#L2".into(), &workspace_weak, window, cx);
        });
        cx.run_until_parked();
        let editor = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<Editor>())
                .expect("file should be open in an editor")
        });
        editor.update_in(cx, |editor, window, cx| {
            let snapshot = editor.snapshot(window, cx);
            assert_eq!(editor.selections.newest::<Point>(&snapshot).head().row, 1);
        });

        // Absolute path
        let abs_path: SharedString = path!("/project/src/main.rs").to_string().into();
        multi_workspace.update_in(cx, |_, window, cx| {
            open_link(abs_path, &workspace_weak, window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            let active = workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .expect("file should be open");
            assert!(*active.path == *"src/main.rs");
        });
    }

    #[gpui::test]
    async fn test_open_link_relative_positions(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.project.worktree.file_scan_exclusions =
                        Some(SplicingVec::from(vec!["**/excluded.rs".to_string()]));
                });
            });
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({
                "src": {"main.rs": "first\naéøbc\nthird\n", "a": "first\nsecond\n", "excluded.rs": "first\nsecond\n"},
                "main.rs": "first\naéøbc\nthird\n",
                "project": {"src": {"main.rs": "wrong file"}},
                "x.rs": {"x.rs": "first\nsecond\n"},
                "a b.rs": "first\nsecond\n",
                "a%20b.rs": "wrong file",
                "literal%20space.rs": "first\nsecond\n",
                "a%2Fb.rs": "first\nsecond\n",
            }),
        )
        .await;
        fs.insert_tree(path!("/other"), json!({"main.rs": "one\ntwo\n"}))
            .await;
        #[cfg(not(target_os = "windows"))]
        fs.insert_tree(
            path!("/project"),
            json!({"literal.rs": "wrong file", "literal.rs:2": "literal file"}),
        )
        .await;

        let project = Project::test(
            fs,
            [path!("/project").as_ref(), path!("/other").as_ref()],
            cx,
        )
        .await;
        let worktree_ids = project.read_with(cx, |project, cx| {
            project
                .worktrees(cx)
                .map(|worktree| worktree.read(cx).id())
                .collect::<Vec<_>>()
        });
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());
        let workspace_weak = workspace.downgrade();

        for (url, expected_path, expected_point) in [
            (
                "project/src/excluded.rs:2",
                path!("/project/src/excluded.rs"),
                Point::new(1, 0),
            ),
            (
                "src/excluded.rs:2",
                path!("/project/src/excluded.rs"),
                Point::new(1, 0),
            ),
            (
                path!("/project/main.rs#2"),
                path!("/project/main.rs"),
                Point::new(1, 0),
            ),
            (
                path!("/project/main.rs#L3"),
                path!("/project/main.rs"),
                Point::new(2, 0),
            ),
            (
                path!("/project/main.rs#2C4"),
                path!("/project/main.rs"),
                Point::new(1, 5),
            ),
            ("main.rs:2#L3", path!("/project/main.rs"), Point::new(2, 0)),
            (
                "main.rs:3#L2:4",
                path!("/project/main.rs"),
                Point::new(1, 5),
            ),
            ("./src/a:2", path!("/project/src/a"), Point::new(1, 0)),
            ("src/a:2:4", path!("/project/src/a"), Point::new(1, 3)),
            (
                "src/main.rs:2",
                path!("/project/src/main.rs"),
                Point::new(1, 0),
            ),
            (
                "src/main.rs:2:4",
                path!("/project/src/main.rs"),
                Point::new(1, 5),
            ),
            (
                "project/src/main.rs:3",
                path!("/project/src/main.rs"),
                Point::new(2, 0),
            ),
            (
                "./src/main.rs:1:2",
                path!("/project/src/main.rs"),
                Point::new(0, 1),
            ),
            ("main.rs:2", path!("/project/main.rs"), Point::new(1, 0)),
            ("main.rs:2:", path!("/project/main.rs"), Point::new(1, 0)),
            ("main.rs(2,4)", path!("/project/main.rs"), Point::new(1, 5)),
            ("other/main.rs:2", path!("/other/main.rs"), Point::new(1, 0)),
            ("x.rs/x.rs:2", path!("/project/x.rs/x.rs"), Point::new(1, 0)),
            ("a%20b.rs:2", path!("/project/a b.rs"), Point::new(1, 0)),
            (
                "literal%20space.rs:2",
                path!("/project/literal%20space.rs"),
                Point::new(1, 0),
            ),
            ("a%2Fb.rs:2", path!("/project/a%2Fb.rs"), Point::new(1, 0)),
            (
                "src/main.rs#L2",
                path!("/project/src/main.rs"),
                Point::new(1, 0),
            ),
            (
                "src/main.rs:2:0",
                path!("/project/src/main.rs"),
                Point::new(1, 0),
            ),
            (
                "src/main.rs:4294967295",
                path!("/project/src/main.rs"),
                Point::new(3, 0),
            ),
            #[cfg(not(target_os = "windows"))]
            (
                "file:/project/literal.rs:2",
                path!("/project/literal.rs:2"),
                Point::new(0, 0),
            ),
            #[cfg(not(target_os = "windows"))]
            (
                "literal.rs:2",
                path!("/project/literal.rs:2"),
                Point::new(0, 0),
            ),
        ] {
            multi_workspace.update_in(cx, |_, window, cx| {
                open_link(SharedString::from(url), &workspace_weak, window, cx);
            });
            cx.run_until_parked();
            assert_eq!(cx.opened_url(), None, "{url}");
            let editor = workspace.read_with(cx, |workspace, cx| {
                let item = workspace.active_item(cx).expect("file should be open");
                let project_path = item
                    .project_path(cx)
                    .expect("item should have a project path");
                assert_eq!(
                    project.read(cx).absolute_path(&project_path, cx).as_deref(),
                    Some(Path::new(expected_path)),
                    "{url}"
                );
                item.downcast::<Editor>()
                    .expect("file should be open in an editor")
            });
            editor.update_in(cx, |editor, window, cx| {
                let snapshot = editor.snapshot(window, cx);
                assert_eq!(
                    editor.selections.newest::<Point>(&snapshot).head(),
                    expected_point,
                    "{url}"
                );
            });
            project.read_with(cx, |project, cx| {
                assert_eq!(
                    project
                        .worktrees(cx)
                        .map(|worktree| worktree.read(cx).id())
                        .collect::<Vec<_>>(),
                    worktree_ids,
                    "{url}"
                );
            });
        }
    }

    #[gpui::test]
    async fn test_open_link_external_urls(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({"src": {"main.rs": ""}, "tel": "not a phone", "custom": "not a URI handler"}),
        )
        .await;
        #[cfg(not(target_os = "windows"))]
        fs.insert_tree(
            path!("/project"),
            json!({"mailto:contact@example.com": "not the email handler"}),
        )
        .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());

        for url in [
            "https://example.com/src/main.rs:2",
            "mailto:contact@example.com",
            "tel:123",
            "custom:123",
            "com.example.viewer:",
            "com.example.viewer::",
        ] {
            multi_workspace.update_in(cx, |_, window, cx| {
                open_link(SharedString::from(url), &workspace.downgrade(), window, cx);
            });
            cx.run_until_parked();
            assert_eq!(cx.opened_url().as_deref(), Some(url));
            workspace.read_with(cx, |workspace, cx| {
                assert!(workspace.active_item(cx).is_none());
            });
        }
        project.read_with(cx, |project, cx| {
            assert_eq!(project.worktrees(cx).count(), 1);
        });
    }

    #[gpui::test]
    async fn test_open_link_percent_escape_disambiguation(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({
                "a%20b.rs": "literal\nsecond\n",
                "a b.rs": "decoded\nsecond\n",
                "c d.rs": "first\nsecond\n",
                "e%20f.rs": "first\nsecond\n",
            }),
        )
        .await;

        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        let workspace_weak = workspace.downgrade();

        let open_link_and_active_path = |url: String, cx: &mut gpui::VisualTestContext| {
            multi_workspace.update_in(cx, |_, window, cx| {
                open_link(url.into(), &workspace_weak, window, cx);
            });
            cx.run_until_parked();
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .active_item(cx)
                    .and_then(|item| item.project_path(cx))
                    .expect("file should be open")
                    .path
            })
        };

        // Both interpretations exist: the decoded one wins.
        let path = open_link_and_active_path(path!("/project/a%20b.rs").to_string(), cx);
        assert_eq!(*path, *"a b.rs");

        // Only the decoded file exists.
        let path = open_link_and_active_path(path!("/project/c%20d.rs").to_string(), cx);
        assert_eq!(*path, *"c d.rs");

        // Only the literally-named file exists: fall back to it.
        let path = open_link_and_active_path(path!("/project/e%20f.rs").to_string(), cx);
        assert_eq!(*path, *"e%20f.rs");

        let path = open_link_and_active_path("a%20b.rs#L2".to_string(), cx);
        assert_eq!(*path, *"a b.rs");

        let path = open_link_and_active_path("c%20d.rs#L2".to_string(), cx);
        assert_eq!(*path, *"c d.rs");

        let path = open_link_and_active_path("e%20f.rs#L2".to_string(), cx);
        assert_eq!(*path, *"e%20f.rs");
        let editor = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<Editor>())
                .expect("file should be open in an editor")
        });
        editor.update_in(cx, |editor, window, cx| {
            let snapshot = editor.snapshot(window, cx);
            assert_eq!(editor.selections.newest::<Point>(&snapshot).head().row, 1);
        });
    }

    #[gpui::test]
    async fn test_file_link_parent_traversal_uses_selected_worktree(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/one"), json!({"two": {"src": "not a directory"}}))
            .await;
        fs.insert_tree(
            path!("/two"),
            json!({"src": {}, "file.rs": "selected file"}),
        )
        .await;
        let project = Project::test(fs, [path!("/one").as_ref(), path!("/two").as_ref()], cx).await;
        let unindexed_buffer = project
            .update(cx, |project, cx| {
                let path = project
                    .project_path_for_absolute_path(Path::new(path!("/one/file.rs")), cx)
                    .unwrap();
                project.open_buffer(path, cx)
            })
            .await
            .unwrap();
        project.read_with(cx, |project, cx| {
            let unindexed_path = project
                .project_path_for_absolute_path(Path::new(path!("/one/file.rs")), cx)
                .unwrap();
            assert!(project.entry_for_path(&unindexed_path, cx).is_none());
            assert_eq!(
                project.get_open_buffer(&unindexed_path, cx).unwrap(),
                unindexed_buffer,
            );
            let expected = project
                .project_path_for_absolute_path(Path::new(path!("/two/file.rs")), cx)
                .unwrap();
            for path in [
                "src/../file.rs",
                "two/src/../file.rs",
                "two/file.rs",
                path!("/two/src/../file.rs"),
            ] {
                assert_eq!(
                    project_path_for_file_link(project, Path::new(path), cx),
                    Some(expected.clone()),
                    "{path}",
                );
            }
        });
    }

    #[gpui::test]
    async fn test_open_link_preserves_project_symlink_buffer(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({"sub": {}, "main.rs": "other file"}),
        )
        .await;
        fs.insert_tree(path!("/outside"), json!({"target.rs": "original\n"}))
            .await;
        fs.insert_symlink(
            path!("/project/link.rs"),
            PathBuf::from(path!("/outside/target.rs")),
        )
        .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());
        let workspace_weak = workspace.downgrade();
        multi_workspace.update_in(cx, |_, window, cx| {
            open_link(SharedString::from("link.rs"), &workspace_weak, window, cx);
        });
        cx.run_until_parked();
        let original = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .unwrap()
                .downcast::<Editor>()
                .unwrap()
        });
        original.update_in(cx, |editor, window, cx| {
            editor.insert("unsaved ", window, cx)
        });

        for excluded in [false, true] {
            if excluded {
                cx.update(|_, cx| {
                    SettingsStore::update_global(cx, |store, cx| {
                        store.update_user_settings(cx, |settings| {
                            settings.project.worktree.file_scan_exclusions =
                                Some(SplicingVec::from(vec![
                                    "**/link.rs".to_string(),
                                    "**/sub".to_string(),
                                ]));
                        });
                    });
                });
                cx.run_until_parked();
                project.read_with(cx, |project, cx| {
                    let path = project
                        .project_path_for_absolute_path(Path::new(path!("/project/link.rs")), cx)
                        .unwrap();
                    assert!(project.entry_for_path(&path, cx).is_none());
                    assert!(project.get_open_buffer(&path, cx).is_some());
                });
            }
            for (url, use_absolute_helper) in [
                ("link.rs", false),
                ("project/link.rs", false),
                (path!("/project/link.rs"), false),
                (path!("/project//link.rs"), false),
                (path!("/project//link.rs"), true),
                (path!("/project/sub/../link.rs"), false),
                ("project/sub/../link.rs#L1", false),
                (path!("/project/sub/../link.rs"), true),
            ] {
                multi_workspace.update_in(cx, |_, window, cx| {
                    open_link(SharedString::from("main.rs"), &workspace_weak, window, cx);
                });
                cx.run_until_parked();
                multi_workspace.update_in(cx, |_, window, cx| {
                    if use_absolute_helper {
                        workspace.update(cx, |workspace, cx| {
                            open_abs_path_at_point(workspace, PathBuf::from(url), None, window, cx);
                        });
                    } else {
                        open_link(SharedString::from(url), &workspace_weak, window, cx);
                    }
                });
                cx.run_until_parked();
                workspace.read_with(cx, |workspace, cx| {
                    let active = workspace
                        .active_item(cx)
                        .unwrap()
                        .downcast::<Editor>()
                        .unwrap();
                    assert_eq!(active.entity_id(), original.entity_id(), "{url}");
                    assert_eq!(active.read(cx).text(cx), "unsaved original\n");
                    assert_eq!(project.read(cx).worktrees(cx).count(), 1);
                });
                assert_eq!(cx.opened_url(), None);
            }
        }
    }

    #[gpui::test]
    async fn test_open_link_does_not_retain_closed_project(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);

        let mut previous_workspace = None;
        for url in [
            format!("{}:2", path!("/outside/notes.md")),
            format!(
                "{}#L2",
                Url::from_file_path(path!("/outside/notes.md")).unwrap()
            ),
        ] {
            let fs = FakeFs::new(cx.executor());
            fs.insert_tree(path!("/project"), json!({"main.rs": ""}))
                .await;
            fs.insert_tree(path!("/outside"), json!({"notes.md": "first\nsecond\n"}))
                .await;
            let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
            let project_weak = project.downgrade();
            let (multi_workspace, window_cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let multi_workspace_weak = multi_workspace.downgrade();
            let workspace =
                multi_workspace.read_with(window_cx, |workspace, _| workspace.workspace().clone());
            let workspace_weak = workspace.downgrade();
            if let Some(previous_workspace) = previous_workspace.take() {
                window_cx.update(|window, cx| {
                    open_link(
                        SharedString::from("missing.rs:2"),
                        &previous_workspace,
                        window,
                        cx,
                    );
                });
                assert_eq!(window_cx.opened_url(), None);
            }
            drop(project);
            drop(workspace);
            drop(multi_workspace);

            window_cx.update(|window, cx| {
                open_link(SharedString::from(url.clone()), &workspace_weak, window, cx);
                window.remove_window();
            });
            assert!(multi_workspace_weak.upgrade().is_none(), "{url}");
            assert!(workspace_weak.upgrade().is_none(), "{url}");
            assert!(project_weak.upgrade().is_none(), "{url}");
            window_cx.run_until_parked();
            assert_eq!(window_cx.opened_url(), None, "{url}");
            previous_workspace = Some(workspace_weak);
        }
    }

    #[gpui::test]
    async fn test_open_link_out_of_project_path(cx: &mut gpui::TestAppContext) {
        crate::test_support::init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), json!({"src": {"main.rs": ""}}))
            .await;
        fs.insert_tree(
            path!("/outside"),
            json!({
                "notes.md": "first\naéøbc\nthird\n",
                "a b.rs": "first\nsecond\n",
                "a%20b.rs": "literal file",
                "literal%20space.rs": "first\nsecond\n",
                "x.rs": {"x.rs": "first\nsecond\n"},
            }),
        )
        .await;

        fs.insert_symlink(
            path!("/outside/notes-link.md"),
            PathBuf::from(path!("/outside/notes.md")),
        )
        .await;
        #[cfg(not(target_os = "windows"))]
        fs.insert_tree(
            path!("/outside"),
            json!({"literal.rs": "base file", "literal.rs:2": "literal file"}),
        )
        .await;

        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let visible_worktree_ids = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).id())
                .collect::<Vec<_>>()
        });
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());
        let workspace_weak = workspace.downgrade();

        let missing_file_uri = Url::from_file_path(path!("/outside/missing.md"))
            .unwrap()
            .to_string();
        for (url, attempted_paths) in [
            (
                "main.rs:2".to_string(),
                vec![
                    PathBuf::from(path!("/project/main.rs:2")),
                    PathBuf::from(path!("/project/main.rs")),
                ],
            ),
            (
                "main.rs:2#L3".to_string(),
                vec![
                    PathBuf::from(path!("/project/main.rs:2")),
                    PathBuf::from(path!("/project/main.rs")),
                ],
            ),
            (
                "crates/editor/src/inlays/inlay_hints.rs:394".to_string(),
                vec![
                    Path::new(path!("/project"))
                        .join("crates/editor/src/inlays/inlay_hints.rs:394"),
                    Path::new(path!("/project"))
                        .join("crates/editor/src/inlays")
                        .join("inlay_hints.rs"),
                ],
            ),
            (
                "crates/editor/src/element.rs:608".to_string(),
                vec![
                    Path::new(path!("/project")).join("crates/editor/src/element.rs:608"),
                    Path::new(path!("/project"))
                        .join("crates/editor/src")
                        .join("element.rs"),
                ],
            ),
            (missing_file_uri.clone(), Vec::new()),
            (
                "../outside/missing.md:2".to_string(),
                vec![
                    Path::new(path!("/project")).join("../outside/missing.md:2"),
                    Path::new(path!("/project"))
                        .join("../outside")
                        .join("missing.md"),
                ],
            ),
            (
                path!("/outside/missing.md").to_string(),
                vec![PathBuf::from(path!("/outside/missing.md"))],
            ),
            (
                path!("/project/src/missing.md").to_string(),
                vec![PathBuf::from(path!("/project/src/missing.md"))],
            ),
            (
                path!("/outside").to_string(),
                vec![PathBuf::from(path!("/outside"))],
            ),
        ] {
            let warnings = FileLinkWarningCapture::new();
            multi_workspace.update_in(cx, |_, window, cx| {
                open_link(SharedString::from(url.clone()), &workspace_weak, window, cx);
            });
            cx.run_until_parked();
            let expected_warning = if url == missing_file_uri {
                format!(
                    "Could not resolve agent file link to {:?}: no matching file",
                    Path::new(path!("/outside/missing.md"))
                )
            } else {
                let failures = attempted_paths
                    .into_iter()
                    .map(|path| format!("{path:?}: no matching file"))
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "Could not resolve agent file link {url:?} against project roots {:?}: {failures}",
                    [Path::new(path!("/project"))]
                )
            };
            assert_eq!(warnings.take(), [expected_warning], "{url}");
            workspace.read_with(cx, |workspace, cx| {
                assert!(workspace.active_item(cx).is_none());
                assert_eq!(workspace.notification_ids().len(), 0);
                assert_eq!(project.read(cx).worktrees(cx).count(), 1);
            });
            assert_eq!(cx.opened_url(), None);
            assert!(cx.pending_prompt().is_none());
        }

        let file_uri = url::Url::from_file_path(path!("/outside/notes.md")).unwrap();
        let literal_uri = Url::from_file_path(path!("/outside/a%20b.rs")).unwrap();
        let symlink_uri = Url::from_file_path(path!("/outside/notes-link.md")).unwrap();
        let mut opened_worktrees = HashMap::default();
        for (url, expected_path, expected_point) in [
            (
                literal_uri.to_string(),
                path!("/outside/a%20b.rs"),
                Point::new(0, 0),
            ),
            (
                format!("{}:2", path!("/outside/a%20b.rs")),
                path!("/outside/a b.rs"),
                Point::new(1, 0),
            ),
            #[cfg(not(target_os = "windows"))]
            (
                path!("/outside/literal.rs").to_string(),
                path!("/outside/literal.rs"),
                Point::new(0, 0),
            ),
            #[cfg(not(target_os = "windows"))]
            (
                "../outside/literal.rs:2".to_string(),
                path!("/outside/literal.rs:2"),
                Point::new(0, 0),
            ),
            (
                "../outside/notes.md:2:4".to_string(),
                path!("/outside/notes.md"),
                Point::new(1, 5),
            ),
            (
                format!("{}:3", path!("/outside/notes.md")),
                path!("/outside/notes.md"),
                Point::new(2, 0),
            ),
            (
                format!("{file_uri}#L2"),
                path!("/outside/notes.md"),
                Point::new(1, 0),
            ),
            (
                "../outside/notes.md#L3".to_string(),
                path!("/outside/notes.md"),
                Point::new(2, 0),
            ),
            (
                "../outside/a%20b.rs:2".to_string(),
                path!("/outside/a b.rs"),
                Point::new(1, 0),
            ),
            (
                format!("{}:2", path!("/outside/literal%20space.rs")),
                path!("/outside/literal%20space.rs"),
                Point::new(1, 0),
            ),
            (
                "../outside/x.rs/x.rs:2".to_string(),
                path!("/outside/x.rs/x.rs"),
                Point::new(1, 0),
            ),
            (
                format!("{symlink_uri}#L2"),
                path!("/outside/notes.md"),
                Point::new(1, 0),
            ),
            (
                "../outside/notes-link.md:3".to_string(),
                path!("/outside/notes.md"),
                Point::new(2, 0),
            ),
        ] {
            multi_workspace.update_in(cx, |_, window, cx| {
                open_link(SharedString::from(url.clone()), &workspace_weak, window, cx);
            });
            cx.run_until_parked();
            let editor = workspace.read_with(cx, |workspace, cx| {
                let item = workspace.active_item(cx).expect("file should be open");
                let project_path = item.project_path(cx).expect("item should have a path");
                let project = project.read(cx);
                assert_eq!(
                    project.absolute_path(&project_path, cx).as_deref(),
                    Some(Path::new(expected_path)),
                    "{url}"
                );
                assert!(project_path.path.is_empty());
                let worktree = project
                    .worktree_for_id(project_path.worktree_id, cx)
                    .unwrap();
                let worktree = worktree.read(cx);
                assert!(!worktree.is_visible());
                assert!(worktree.is_single_file());
                assert_eq!(worktree.abs_path().as_ref(), Path::new(expected_path));
                if let Some(previous_id) = opened_worktrees.insert(expected_path, worktree.id()) {
                    assert_eq!(previous_id, worktree.id());
                }
                assert_eq!(project.worktrees(cx).count(), 1 + opened_worktrees.len());
                assert_eq!(
                    project
                        .visible_worktrees(cx)
                        .map(|worktree| worktree.read(cx).id())
                        .collect::<Vec<_>>(),
                    visible_worktree_ids
                );
                assert_eq!(workspace.notification_ids().len(), 0);
                item.downcast::<Editor>().expect("should be an editor")
            });
            editor.update_in(cx, |editor, window, cx| {
                let snapshot = editor.snapshot(window, cx);
                assert_eq!(
                    editor.selections.newest::<Point>(&snapshot).head(),
                    expected_point,
                    "{url}"
                );
            });
            assert_eq!(cx.opened_url(), None);
            assert!(cx.pending_prompt().is_none());
        }
    }

    thread_local! {
        static FILE_LINK_WARNINGS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    struct FileLinkWarningCapture {
        previous_level: log::LevelFilter,
    }

    impl FileLinkWarningCapture {
        fn new() -> Self {
            static INSTALL_LOGGER: Once = Once::new();
            INSTALL_LOGGER.call_once(|| {
                log::set_logger(&FileLinkTestLogger)
                    .expect("failed to install file-link test logger");
            });
            let previous_level = log::max_level();
            assert!(FILE_LINK_WARNINGS.replace(Some(Vec::new())).is_none());
            log::set_max_level(previous_level.max(log::LevelFilter::Warn));
            Self { previous_level }
        }

        fn take(&self) -> Vec<String> {
            FILE_LINK_WARNINGS.with_borrow_mut(|warnings| {
                std::mem::take(warnings.as_mut().expect("warning capture should be active"))
            })
        }
    }

    impl Drop for FileLinkWarningCapture {
        fn drop(&mut self) {
            drop(FILE_LINK_WARNINGS.take());
            log::set_max_level(self.previous_level);
        }
    }

    struct FileLinkTestLogger;

    impl log::Log for FileLinkTestLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::max_level()
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let captured = FILE_LINK_WARNINGS.with_borrow_mut(|warnings| {
                if record.level() == log::Level::Warn
                    && let Some(warnings) = warnings
                {
                    warnings.push(record.args().to_string());
                    true
                } else {
                    false
                }
            });
            if !captured {
                eprintln!("{} {}: {}", record.level(), record.target(), record.args());
            }
        }

        fn flush(&self) {}
    }
}

const FAST_MODE_WARNING_NAMESPACE: &str = "fast-mode-warning-dismissed";
const FULL_ACCESS_WARNING_KEY: &str = "full-access-warning-dismissed";

fn fast_mode_warning_id(
    provider_id: &LanguageModelProviderId,
    model_id: &LanguageModelId,
) -> String {
    format!("{}:{}", provider_id.0, model_id.0)
}

fn fast_mode_warning_dismissed(
    provider_id: &LanguageModelProviderId,
    model_id: &LanguageModelId,
    cx: &App,
) -> bool {
    KeyValueStore::global(cx)
        .scoped(FAST_MODE_WARNING_NAMESPACE)
        .read(&fast_mode_warning_id(provider_id, model_id))
        .log_err()
        .flatten()
        .is_some()
}

fn set_fast_mode_warning_dismissed(
    provider_id: &LanguageModelProviderId,
    model_id: &LanguageModelId,
    cx: &mut App,
) {
    let key = fast_mode_warning_id(provider_id, model_id);
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.scoped(FAST_MODE_WARNING_NAMESPACE)
            .write(key, "1".to_string())
            .await
            .log_err();
    })
    .detach();
}

fn full_access_warning_dismissed(cx: &App) -> bool {
    KeyValueStore::global(cx)
        .read_kvp(FULL_ACCESS_WARNING_KEY)
        .log_err()
        .flatten()
        .is_some()
}

fn set_full_access_warning_dismissed(cx: &mut App) {
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.write_kvp(FULL_ACCESS_WARNING_KEY.to_string(), "1".to_string())
            .await
            .log_err();
    })
    .detach();
}

pub(crate) fn reset_full_access_warning(cx: &mut App) {
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.delete_kvp(FULL_ACCESS_WARNING_KEY.to_string())
            .await
            .log_err();
    })
    .detach();
}

pub(crate) fn reset_fast_mode_warnings(cx: &mut App) {
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.scoped(FAST_MODE_WARNING_NAMESPACE)
            .delete_all()
            .await
            .log_err();
    })
    .detach();
}
