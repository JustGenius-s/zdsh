# zdsh

How zdsh behaves differently from Zed.

## Window backdrop

On macOS, themes without an explicit `background.appearance` use a blurred window backdrop with the native Sidebar material. Its system tint and contrast layers are preserved beneath the workspace tint. The workspace sidebar uses an additional sidebar layer; docked panels use a stronger layer for readability. Other platforms keep the theme's existing background behavior.

The default window tint is 42% pure black for dark themes and 16% pure white for light themes. A `title_bar.background` color with alpha below 100% overrides both the tint color and its opacity. For example, use `"title_bar.background": "#00000099"` in a theme override for a 60% black tint.

The workspace sidebar adds the theme's panel color at 36% opacity in dark themes or 42% in light themes. Docked side panels use that color at 72% or 84%, respectively. Central panes use the toolbar color at 72% or 84%; nested Agent views remain transparent so that layer is painted only once. Editors and terminals paint their own opaque background, and zoom overlays stay opaque to cover the underlying panes.

Z Dark uses `#181818` for editor, terminal, and popup surfaces, `#141414` beneath conversations, and `#212121` with a `#313131` border for message and composer cards. State highlights use translucent `#f0f0f0`: top tabs use 6%, workspace icon controls use 8%, and secondary buttons use 8% normally and 14% on hover. Sidebar rows use about 6% on hover and 8% when selected. The final glass color also depends on the wallpaper and macOS material.

Text selections use a shared neutral overlay across editors, Agent responses, terminals, and GPUI Kit inputs: `#f0f0f033` (20% light gray) in Z Dark and `#17171726` (about 15% dark gray) in Z Light. Text and syntax colors remain visible over the selection. Unfocused editors retain their existing reduced selection opacity. Other themes and remote collaborators keep their own selection colors.

To return to an opaque window, add this to your settings (or set `background.appearance` in a named `theme_overrides` entry):

```json
{
  "experimental.theme_overrides": {
    "background.appearance": "opaque"
  }
}
```

## Agent tabs

Each Agent conversation opens in a central editor tab. The project file tree can stay open beside it. This applies to the Zed Agent and external ACP agents; terminal entries open in separate central terminal tabs.

Use the New Thread action, the **+** beside **Threads** in the workspace sidebar, or thread history to open a conversation. New Thread always creates an Agent conversation. Opening an already open conversation activates its tab. Drag tabs to reorder them or move them into a split pane to work beside code. Each conversation has one tab; splitting does not duplicate the conversation.

The tab displays its title and connection, generation, approval, or error status. Its toolbar uses the same row height, padding, theme background, and separator colors as file and Git toolbars and provides the agent picker and chat options. Toolbar sizing follows the global UI font size independently of the Agent conversation font size. Files can still be dragged into the conversation as context.

Use the double speech bubble button in the conversation toolbar to open **Thread History** inside the tab. The list retains project grouping, search, archive, and restore actions. It shares the existing history data across tabs. Only one tab presents the list at a time; opening it in another tab moves the list there. File tabs and the project file tree keep their full layout.

On wide panes, history sits to the right of the conversation with a draggable divider. On narrow panes, it overlays the right side of the conversation; click outside it or use the toolbar button to close it. The existing Toggle Threads Sidebar and Focus Sidebar shortcuts target this tab-local history. From a file tab, they open a conversation tab first. History starts collapsed after restarting zdsh.

The conversation uses the toolbar background. In an empty conversation the composer is centered in the tab; once the conversation has messages, it docks at the bottom. The transcript, composer and floating controls share `agent.max_content_width`, which defaults to 904px and shrinks with the available panel width. Explicit width settings and `agent.limit_content_width: false` remain effective. The composer retains model, mode, thinking, context, and send controls.

Agent response text wraps at Unicode line-break opportunities, keeping Chinese closing punctuation with the preceding text and opening punctuation with the following text. Long words can wrap between grapheme clusters without splitting combined characters or emoji. At exceptionally narrow widths, an inseparable punctuation group may extend beyond the available width. This changes visual wrapping only; authored newlines, source text, selection, and copied content are preserved.

The `+` button in the composer picks a mode, starts a file mention, switches models, and turns MCP servers on or off. The default mode, Write, stays implicit. Picking another mode, such as Ask, shows it as a tag next to the `+` button; remove the tag to return to the default mode. User messages appear as full-width cards drawn like the composer. Editing, attachments, and checkpoint actions remain available. The agent's thinking shows as a single "Thinking" line while it streams and "Thought" afterwards, with how long it took, such as "Thought 16s"; click it to read the reasoning. Reading a file shows as a single line with the file's name; click it to open the file. Other tool calls that only look things up, such as searching, show as single lines you can click to see their input and output. Edits, commands, subagents, and tool calls waiting for permission show as cards. An edit's header shows how many lines it added and removed, a command's header shows its first line, and the buttons that answer a permission request sit at the bottom of the card. Consecutive tool calls, with the thinking between them, fold into one line that sums them up, such as "Edited 2 files, explored 3 files, ran 1 command +12 −4"; click it to see each one. Subagents and tool calls waiting for permission stay in view. Once a turn is done, everything the agent did before its last reply folds into a "Worked for 2m 15s" line above the reply. Completed turns start folded immediately, including the turn you just watched. Opening a Worked row keeps that turn expanded until you close it. Errors and warnings about the conversation show as cards above the composer, with buttons to act on them, such as retrying or copying the error.

Switching tabs keeps work running. Closing a conversation tab requests cancellation and releases its session, except when native background tasks or their completion delivery still own it. Those conversations continue without an open Agent tab. Saved conversations remain available through the agent's history support. Central Agent tab positions are not restored when restarting zdsh.

While a thread is still empty, the agent selector above the message editor switches it to **Zed Agent** or any installed external agent. Once the thread has messages, its agent stays fixed. To open a terminal, use the **+** beside **Terminals** in the workspace sidebar. The agent selector only lists agents.

Context, provider usage, completion, and composer menus float above an ongoing conversation's composer or below an empty one. Opening them does not shrink the transcript or its navigation. The context indicator remains visible before the first request: native conversations show an estimate from their context and model limit; an external agent that has not reported usage shows an unavailable value. Completed turns fold their final thinking block together with earlier work while keeping the answer visible. A new user message has 32 px of space above it.

The composer has a changed-files chip when the conversation edited files; Git commits remain in the Changes panel. Copying an answer briefly changes its copy icon to a checkmark and shows a success notification. The response footer has more space between its action buttons and no longer has scroll-to-user-message or scroll-to-top buttons. Select response text and choose **Add to Chat** to append it as a quote to the draft, without sending. This works with native and external ACP conversations.

For an idle native conversation, **Fork Conversation Here** beneath an answer opens a new conversation containing the history through that answer, including its saved thinking and work durations. Later turns, the current draft, temporary terminal state, and session sandbox grants are not copied. Forking leaves project files and the original conversation unchanged. Historical forking is not exposed for external ACP agents, whose support varies.

Native conversations show the last model request's output token rate and input cache hit rate when the provider reports token counts. The rate divides output tokens by the full request duration, including connection, prefill, and reasoning time but excluding subsequent tool execution. The cache percentage divides cache-read tokens by total input tokens. These measurements are saved with the assistant message and survive reopening a conversation; old messages and providers without usage reports have no inferred measurements.

After a native reply completes, **Composer Predictions** can suggest a short follow-up in an empty composer. **Tab** accepts it as an editable draft; sending is a separate action. Typing, adding context, editing an earlier message, or starting another request discards the suggestion. **Escape** dismisses it. Predictions use the current conversation model, the latest user request, the completed final reply, and earlier user requests for context, with normal provider usage. They must cite a supporting passage from the final reply internally; unrelated, malformed, or unsupported results are not displayed. A completed task without a clear continuation produces no suggestion. Disable them with `agent.show_composer_predictions: false` or the corresponding Agent setting. They are disabled in Debug mode and are not generated for external ACP conversations.

## Background tasks

Native Agent terminal commands can continue in a new **Terminal** tab while the Agent works on other steps. They use the same approved process and sandbox; moving a command to the background does not execute it again or change its permissions. The new tab appears in the current workspace without taking focus and remains available after completion.

The `terminal` tool accepts `is_background: true` or `block_until_ms: 0` for immediate background execution. Otherwise it waits up to `block_until_ms` (30 seconds by default) before transferring a running command. The command card also offers **Run in background**. `timeout_ms` remains a separate limit on the command's total runtime, including time spent in the foreground. Environments without a native Terminal tab host keep foreground execution and reject explicit background requests.

The tool returns a task ID. `background_task` lists this conversation's task states, reads a specific task's output, or stops it with `stop: true`. Completion is delivered automatically to the originating conversation once it is idle. Results wait during generation, approvals, questions, and Debug reproduction. Reading a specific completed result consumes its pending notification when the query result enters conversation history; listing states does not. Several pending completions can share one follow-up. Output is marked as tool data rather than a human request.

Stopping Agent generation pauses automatic background follow-ups until another message or retry; it does not terminate commands already in background tabs. Stop a command from its card, with `background_task`, or by closing its Terminal tab. Closing an Agent tab preserves background work and its conversation. Deleting the conversation stops its tasks and removes pending delivery.

Task metadata, completion delivery state, and the latest 64 KiB terminal-output tail are saved with the conversation. Running snapshots refresh about every five seconds; querying a running task reads its current terminal contents. Completion events include up to 4 KiB per task, with more available through `background_task`. These are bounded terminal snapshots, not full raw log files. On reopening a conversation after an IDE restart, previously running tasks become **Interrupted** and are not relaunched; saved results and pending delivery remain available. Terminal tabs themselves are not restored as new shell commands. Failed or cancelled delivery is paused for user-directed retry; this does not promise exactly-once model execution across crashes.

## Debug mode

The native Agent's **Debug** mode provides a runtime-evidence workflow for trusted local projects. Entering Debug supplies full investigation guidance; later turns receive a shorter reminder with the current session, run ID, collector address, and log path. Mode changes take effect before the next model request, and compaction restores the full guidance. These internal reminders persist with the conversation without appearing as user messages. The guidance asks the agent to identify hypotheses, add temporary probes, cite observations when confirming or rejecting hypotheses, compare the same reproduction before and after a fix, and retain instrumentation until verification or an explicit cleanup request. These are model instructions, not an automatic guarantee that a diagnosis is correct.

The **Debug** pill opens a bounded **Reproduction Steps** tray with Markdown content and a pending indicator when user action is needed. Steps can arrive while the response streams; **Proceed** and **Mark Fixed** become available after generation stops. Protocol tags are hidden from the transcript while the original message remains in history. Ordinary queued messages wait while reproduction is pending; leaving Debug resumes normal queue dispatch. Failed responses suppress reproduction actions. Cancelled or interrupted Debug follow-ups can retry the same user request, including after reopening the conversation, instead of creating duplicate follow-up messages.

**Proceed** sends a short reproduction confirmation; the agent reads the actual log file with its tools. **Mark Fixed** requests removal of temporary instrumentation while preserving the fix and unrelated changes, and keeps the conversation in Debug mode. The user controls reproduction and confirmation. The **Logs** view shows a live count, timestamps and messages, expandable raw events, **Open File**, and **Clear Logs**. Visible logs refresh in the background every 500 ms, with incremental reading and a bounded view of the latest 100 entries / 64 KiB. Clear Logs affects only the current session's file; incomplete trailing lines are not counted until completed.

A session-local collector accepts JSON events on a loopback address. Its configuration and NDJSON log are stored under `.zed/debug/<session-id>/` and reused when the conversation is reopened; generated files are Git-ignored. Individual HTTP events are limited to 64 KiB and the log to 16 MiB. If the saved port is occupied, the collector keeps the session token, saves a new address, and informs the user and agent that probes need updating. Collector health, startup and file errors are exposed with a retry action. Subagents share their parent investigation's collector. External ACP agents retain their own modes; this workflow does not provide a remote or cloud collector.

## Layout

On macOS, text defaults to grayscale antialiasing without additional CoreGraphics stroke thickening, following Cursor's default rendering policy. `text_rendering_mode` applies at startup and when changed. Explicit user settings remain effective: `platform_default` keeps the existing platform smoothing behavior. This affects glyph rendering throughout the application, while editor, terminal, and UI fonts retain their separate settings. Other platforms retain their existing defaults. See the [UI style standard](ui-style.md) for the rendering and typography rules.

The title bar holds a pill for each project open in the window. Click a pill or press `ctrl-1` through `ctrl-9` to switch projects; the window slides the project in from the side of its pill. A dot on a pill marks a project with a thread waiting for your approval. Hover a pill and click its **×** to close that project; unsaved files are asked about first. **New** opens a folder, clones a repository, connects to an SSH host or opens a dev container. The gear menu opens Settings, Extensions and Keymap and picks the appearance (System, Light or Dark), color theme and icon theme. Empty title-bar space drags the window; on macOS, double-clicking it follows the system's title-bar action setting. Each pane shows its own tabs above its content.

Each project has a sidebar at its left edge, initially 272px wide at the default UI font size. Drag its right edge to resize it; the width is saved per project and restored on the next launch:

- The header shows the project's name with two buttons: **Customize Sidebar** and Search, separated by 8px. **Customize Sidebar** orders threads by **Updated** or **Status**, hides their times (**Show** › **Updated**), shows archived threads (**Show** › **Archived**), filters them by status (**Filters** › **Status**: Needs Attention, Error, Unread, Working, Draft, Done) and marks them all as read. These choices last until the window closes.
- **Pinned** is the first category when it has content. Hover a file, thread or terminal and use **Pin** to move it here; **Unpin** returns it to its original category. Files and terminals share their tabs' pin state and workspace persistence. Thread pins are saved per project. Pinning keeps the current tab active.
- **Threads** lists the project's unpinned threads, newest first, with their status: Cursor's 3×3 animated dot grid while running, a warning dot and **Approve** while waiting for permission, a colored unread dot, a subdued read dot, or a small hollow circle for drafts. The first 8 are listed, then **More** lists the rest. Click a thread to open its tab. Hover for its full title, Pin, and (for a saved thread) Archive. Buttons overlay the title lane to the left of the timestamp and reserve no space when hidden; the title fades earlier while they are visible and the time stays fixed. Right-click for Pin / Unpin, Rename, Mark as Read / Unread, Fork, Copy and Archive. Copy offers the title, thread ID and conversation content. Fork supports saved, idle native conversations; unsupported operations are disabled. Rename commits with Enter or blur and cancels with Escape. **+** starts a thread.
- **Files** lists the files open in tabs. Each row shows the file name; its tooltip shows the path. **+** creates or opens a file.
- **Changes** lists all changed files across the project's repositories and open conversations, without a file-count limit. Duplicate paths appear once, using Git statistics when available. Counts and M, A or D use Cursor's Git palette, with extra space at the right edge. Switch between a flat list and a directory tree using the view button beside **Changes**; tree folders can collapse. Tree content starts 14px inside the section header, with another 14px for each nested level. Click a file to open its single-file diff; its hover button opens the source file while keeping line counts visible. The other header button opens the full Changes panel. A file can be listed under both **Files** and **Changes**.
- **Terminals** lists the terminal tabs by folder, with the same dot-grid animation while a command runs and an error dot when it fails; the tooltip shows the command and its status. **+** opens a terminal or runs a quick action.
- **Archived** appears after Terminals when **Show** › **Archived** is checked. Click a thread or choose **Restore** in its context menu to restore and open it.
- The footer shows the branch, without a changed-file count or a divider above it. Its GPUI Kit button opens a matching searchable branch list: filter local/remote branches, switch branches, or type a new name to create one; the current branch has a trailing check. Hover another branch for Delete (Alt: force). The diagnostic pulse icon beside it opens **Workspace Tools**, with Diagnostics and the Debugger. The Changes panel remains available through `ctrl-shift-g`.

Sections follow Cursor's group labels: every row starts in the same icon column, with the file's icon from the icon theme under **Files** and **Changes** and a dot elsewhere. Built-in and extension file icon themes are supported, including multicolor icons. A section title has its icon in the rows' icon column (an agent for Threads, a folder that opens and closes for Files, `diff-multiple` for Changes, a bordered terminal for Terminals), which turns into a chevron while hovered (down when the section is expanded, right when folded), the title in the rows' title column, and buttons that show on hover. Click a section's title to fold it. An expanded section with no rows says so in muted text, like Cursor's "No agents yet". Row highlighting follows the active tab in the active pane; a file shown in both Files and Changes is highlighted in both places while its tab is active. Sidebar text follows the [UI style standard](ui-style.md): 14px text with a 20px line height at the default UI scale, a secondary foreground for ordinary items, a primary foreground for current items, and muted group labels and metadata. The default UI font is the system font; explicit font preferences and UI scaling remain available. Row heights, icon slots, and sidebar widths keep their own dimensions. Icons come from the same icon set as Cursor's sidebar. The Cursor measurements are recorded in [the Threads layout research](cursor-thread-layout-research.md).

The tree button beside **Files**, `cmd-shift-e`, and actions that reveal a file open the project's file tree at the right edge of the content area, inside its rounded corners. It floats over the center and closes when a file opens or on Escape; pin it to keep it in a column of its own. Drag its left edge to resize it in either mode. Both modes share the saved width, initially 264px; reopening the panel or project restores it. Both side panels shrink temporarily in a narrow window to preserve room for the center, without replacing the saved widths. **Find a File…** opens file search. The left dock no longer shows the file tree.

The **Changes** tab shows the git panel's change lists, history and commit box beside the diff of the picked file. Opening the git panel from any action opens this tab instead of a dock.

A workspace without a project shows a start page with buttons to open a folder, clone a repository, connect to an SSH host or open a dev container, and up to five recent projects, marking those already open in the window. This fallback also applies to restored center panes when they contain no active tab, including after the last scratch tab closes. You can also drag a folder into the window.

When startup has no session or workspace to restore, it shows this start page. An explicit `restore_on_startup: "empty_tab"` still opens a blank editor. Restoring an older projectless window containing only one unmodified, empty, unpinned untitled tab returns to the start page; notes with content, dirty buffers, saved files, pinned tabs and split layouts retain their restored state. Creating a window from the CLI follows `on_new_window`, just like the New Window action.

The thread switcher (`agents_sidebar::ToggleThreadSwitcher`) and thread history keep working without the former Threads panel.

The file panel has no header: the title bar's pills switch projects, and **git: worktree** in the command palette picks a worktree. Its context menu uses the Kit popup menu. Indentation guides stay hidden until an entry is selected.

Choose **Add to Chat** on a file or folder to attach it to the active conversation draft, creating a draft conversation if needed. A changed file's **Open Diff** action opens its single-file diff directly. The diff toolbar uses Kit buttons for navigating changes, staging, unstaging, restoring, and whole-file actions, while retaining the existing diff-style and full-file controls.

## Accounts

zdsh runs without a Zed account. Configure a provider in the Settings Editor to use the Zed Agent with your own API access, provider subscription, or local model. External agents retain their own authorization flows.

Zed-hosted models, Zed cloud search, subscriptions, contacts, channels, calls, and shared editing are unavailable. The desktop does not restore Zed login sessions or read or delete existing Zed account credentials. The titlebar menu still provides settings, themes, and extensions.

Existing explicit Zed-hosted model selections require you to choose another model; zdsh does not replace them with a different paid provider. SSH, local development tools, extension downloads, and telemetry are independent of Zed account access.

Completed conversation turns offer **Copy Message** and, for native conversations, **Fork Chat**, followed by the reply's timestamp. Copy takes the final nonempty reply without thinking or tool output and briefly shows a check. Fork creates a new conversation through that reply and shows a spinner while saving. Timestamps prefer the final message's start time, falling back to turn completion, and refresh every minute; hover for the full local date and recorded duration. The latest time remains visible, while earlier times appear on hover. Times persist through reopening and Fork; older conversations without a recorded time leave it absent. Rating buttons are omitted.

## Profiles

- `Minimal`: uses no project tools. Zed switches to it in restricted workspaces, so it isn't offered as a mode in the Agent Panel.

A profile can add its own text to the Zed Agent's system prompt. `prompt_prefix` is placed right after the agent's opening description, which suits a persona or a role for the profile. `prompt_suffix` is placed at the end of the prompt, after your `AGENTS.md` and project rules.

```json
{
  "agent": {
    "profiles": {
      "reviewer": {
        "name": "Reviewer",
        "tools": { "read_file": true, "grep": true },
        "prompt_prefix": "You are reviewing changes, not making them. Report problems with file and line references.",
        "prompt_suffix": "End every answer with a one-line verdict."
      }
    }
  }
}
```

A profile you create from another one copies its prompts.

## Tool results and safeguards

The Zed Agent adjusts what the model sees from its tools. The tool cards in the conversation always show each tool's complete output.

Write, Ask, and Debug enable the `ask_user` tool by default. Related questions appear in a dedicated Kit tray above the composer, with lettered choices, single or multiple selection, and an Other input. Its body scrolls while the title and footer remain visible. Single selection advances to the next unanswered question; Continue submits the complete group. Arrow and letter keys navigate choices, and Enter advances or submits. Escape collapses the tray without losing its draft, while Skip explicitly declines the group. Completed questions appear as compact question-and-answer summaries in history. Old single-question calls remain supported; other ACP and MCP elicitation forms keep their generic presentation. Custom profiles can disable the tool.

Command approval cards keep the full command visible after a `$` prompt, with a stronger pending border and compact Kit buttons. **Run** allows this execution and **Skip** rejects only this execution, including their keyboard shortcuts; neither inherits a permanent scope selected earlier. The separate **Always Run** split button applies an explicitly selected set of command patterns or a single named persistent scope; its arrow opens the available rules. Selecting patterns does not run a command, and the action stays disabled until a scope is known. Narrow cards hide the permanent action before shrinking Run and Skip. Command approval cards omit the Write Access summary and generic sandbox documentation link. Dedicated sandbox warnings remain visible. The `$` prompt and command share font metrics and align on their first-line baseline. An agent-supplied reason and any review reason appear on a tinted strip below the command card. Existing permission and sandbox checks remain in force.

Pending commands before terminal execution use the [shared approval style](ui-style.md#command-approval). Full approval cards remain in the transcript. The main thread shows a floating copy above the composer only after its next pending request moves entirely outside the viewport, and hides it when the original returns. The floating header can scroll back to the original; with multiple requests, it also shows a stacked-card silhouette and pending count, advancing after each decision. The footer puts the native permission-mode menu on the left and one-shot and permanent actions on the right; wide layouts name the selected command rules directly in the Always Run button. Native policy names and confirmation flows remain truthful to zdsh, and external agents do not get a fabricated permission policy. One 8px card border encloses the command and actions, with no duplicate title bar or action divider. Floating cards let the command body and reason scroll independently while actions stay visible; inline cards show their full content. Keyboard decisions and warning checks target the displayed request. Other activity sections retain their own cards.

Native prompts distinguish progress updates from final results and ask for outcome-first explanations grounded in evidence and actual verification. Detailed summaries and compaction preserve the latest constraints, decisions, validation, and unfinished work. Summary generation retains all streamed text, including newlines within a chunk. This first alignment pass was informed by static analysis of Cursor 3.26.16's shipped prompt generators. The active server instructions and model-specific branches were not observed, and output quality has not been compared with the same model and task. The streamed-text fix applies to generated conversation summaries.

- **Oversized results**: when a single tool result has more than about 50 KB of text, the model receives its start and end with a note about the omitted middle, and is told to call the tool again with narrower arguments if it needs that part.
- **Repeated calls**: when the model sends the same call, with the same arguments, in 3, 5 and 8 responses in a row, the result carries a reminder to check the previous result and change approach. Calls made side by side in one response don't count against each other, and a new message from you starts over.
- **Overwriting files**: `write_file` won't replace an existing file the agent hasn't read, created or edited in the conversation, or one that changed on disk since. The agent is asked to read the file first. Targeted edits with `edit_file` only apply where the text they replace still matches the file.

## Extension Agent tools

zdsh development builds accept Agent tools from the existing extension installer. A package can provide both editor features and Agent tools through one `extension.toml` and one Wasm component. Upstream Zed does not implement this optional interface.

Enable `features = ["agent-tools"]` on the local `zed_extension_api` dependency. Declare tools in the existing manifest:

```toml
[agent_tools.echo]
description = "Return the supplied message."
[agent_tools.echo.input_schema]
type = "object"
required = ["message"]
additionalProperties = false
[agent_tools.echo.input_schema.properties.message]
type = "string"
```

Implement `Extension::run_agent_tool(&mut self, name: String, arguments: serde_json::Value) -> Result<serde_json::Value, String>`. The name passed to the extension is the local manifest key; the model sees `<extension-id>_<tool-name>`. Model-facing names must fit 64 ASCII letters, digits, underscores or hyphens and must not collide with another installed tool. Input schemas must describe objects; external schema retrieval is disabled. Invalid schemas and missing Wasm exports produce extension-load errors.

Build with `wasm32-wasip2` and install using **zed: install dev extension**.

Extension tools are available to the Zed Agent. Use a tool-capable model and the Write mode, or a profile that enables the tool. A profile's `tools` entry for the model-facing name (for example `"my-extension_echo": true`) enables or disables a tool explicitly; without an entry, `enable_all_context_servers` decides, as it does for MCP tools. Extension tools are not offered in restricted workspaces. A tool whose name matches a built-in Zed Agent tool is skipped.

The tool list is read again for each model request, so newly installed or updated tools appear without starting a new chat. Approval follows the Agent's tool permissions under the ID `extension:<extension-id>:<tool-name>`, where `<tool-name>` is the manifest key. MCP and extension tools only support the tool-level option. Choosing "Always for … extension tool" in the prompt saves that ID's `default`. The tool receives validated JSON arguments; its result, or its error, is returned to the model and shown in the tool card.

Call `zed_extension_api::agent_tools::is_cancelled()` during long operations. Cancellation is cooperative: release resources before returning. Unloading revokes new calls, requests cancellation and waits for admitted calls before completing the reload. A call that started before a reload never runs the replacement code; later requests use the reloaded tools. A tool that ignores cancellation can block unloading or normal application shutdown.

The optional WIT interface is `zdsh:agent@0.1.0`; historical Zed WIT definitions are preserved. Existing extensions without this interface continue to load. This interface does not expose arbitrary GPUI state, project buffers, terminal access, Cordis service injection or waterfall middleware. Existing extension capabilities still govern host operations; external processes require their own confinement policy.

## Terminal tabs

All standalone terminals open as central tabs beside your files. Choose **New Terminal** from the **+** beside **Terminals** in the workspace sidebar or use `workspace::NewTerminal`. **Open in Terminal** uses the selected folder or the selected file's parent directory. Task execution, debugger terminals, and external-agent login commands also open central tabs.

Use `terminal::Focus` to focus the most recent terminal, creating one if necessary. Existing bindings for `workspace::NewCenterTerminal` remain supported. Replace custom bindings for `terminal_panel::Toggle` or `terminal_panel::ToggleFocus` with `terminal::Focus`.

Use `workspace::NewTerminal` to create additional terminal tabs. Move or split them with the same tab and pane controls used for files.

Command suggestions and history navigation load the current shell's `HISTFILE`, including custom paths configured by Zsh or Bash startup files. The history is refreshed in the background at each shell prompt and combined with commands from the current tab. Commands submitted through the input area or quick actions are added to shell history and flushed to the configured file; internal shell-integration commands and commands beginning with whitespace are not saved. With no `HISTFILE` configured, suggestions still use the current tab's in-memory history.

The **+** beside **Terminals** in the workspace sidebar lists saved command sequences. Choose **Manage quick actions…** to open their management tab, or use `terminal::OpenTerminalQuickActions`. Add, edit, and delete actions there; saved actions are shared across workspaces in the current user data directory. Each command targets the current terminal or a new terminal using its working directory. The current target starts as the most recently active terminal and follows any terminal created by an earlier step; when no terminal is open, one is created. Commands require shell integration and wait for the target shell to be ready. **Stop remaining steps** cancels queued steps without interrupting commands already submitted. Switching tabs preserves an editing draft; closing the management tab discards unsaved edits.

Terminal tabs restore through the workspace layout. Existing terminal-panel sessions are imported as central tabs, preserving working directories and custom titles; panel splits become tabs. Running processes and command output are not restored.

The terminal panel and its settings controls are removed. Legacy `terminal.button`, `dock`, `starts_open`, `flexible`, `default_width`, `default_height`, and `show_count_badge` values are accepted but have no effect. Tasks use `reveal_target: "center"`; legacy `"dock"` values are accepted as an alias. `reveal: "never"` keeps the current tab active, while `"no_focus"` displays the terminal without taking keyboard focus.
