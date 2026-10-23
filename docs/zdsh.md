# zdsh

How zdsh behaves differently from Zed.

## Window backdrop

On macOS, themes without an explicit `background.appearance` use a blurred window backdrop with the native Sidebar material. Its system tint and contrast layers are preserved beneath the workspace tint. Both rails and the right-hand tab sidebar use the same additional sidebar layer; docked panels use a stronger layer for readability. Other platforms keep the theme's existing background behavior.

The default window tint is 42% pure black for dark themes and 16% pure white for light themes. A `title_bar.background` color with alpha below 100% overrides both the tint color and its opacity. For example, use `"title_bar.background": "#00000099"` in a theme override for a 60% black tint.

Rails and the tab sidebar add the theme's panel color at 36% opacity in dark themes or 42% in light themes. Docked side panels use that color at 72% or 84%, respectively. Central panes use the toolbar color at 72% or 84%; nested Agent views remain transparent so that layer is painted only once. Editors and terminals paint their own opaque background, and zoom overlays stay opaque to cover the underlying panes.

Z Dark uses `#181818` for editor, terminal, and popup surfaces, `#141414` beneath conversations, and `#212121` with a `#313131` border for message and composer cards. State highlights use translucent `#f0f0f0`: top tabs use 6%, rail and workspace icon controls use 8%, and secondary buttons use 8% normally and 14% on hover. Sidebar rows use about 6% on hover and 8% when selected. The final glass color also depends on the wallpaper and macOS material.

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

Use the New Thread action, the **Agent** category menu in the right sidebar, or thread history to open a conversation. New Thread always creates an Agent conversation. Opening an already open conversation activates its tab. Drag tabs to reorder them or move them into a split pane to work beside code. Each conversation has one tab; splitting does not duplicate the conversation.

The tab displays its title and connection, generation, approval, or error status. Its toolbar uses the same row height, padding, theme background, and separator colors as file and Git toolbars and provides the agent picker and chat options. Toolbar sizing follows the global UI font size independently of the Agent conversation font size. Files can still be dragged into the conversation as context.

Use the double speech bubble button in the conversation toolbar to open **Thread History** inside the tab. The list retains project grouping, search, archive, and restore actions. It shares the existing history data across tabs. Only one tab presents the list at a time; opening it in another tab moves the list there. File tabs and the project file tree keep their full layout.

On wide panes, history sits to the right of the conversation with a draggable divider. On narrow panes, it overlays the right side of the conversation; click outside it or use the toolbar button to close it. The existing Toggle Threads Sidebar and Focus Sidebar shortcuts target this tab-local history. From a file tab, they open a conversation tab first. History starts collapsed after restarting zdsh.

The conversation uses the toolbar background. In an empty conversation the composer is centered in the tab; once the conversation has messages, it docks at the bottom. It follows the configured content width and retains model, mode, thinking, context, and send controls.

The `+` button in the composer picks a mode, starts a file mention, switches models, and turns MCP servers on or off. The default mode, Write, stays implicit. Picking another mode, such as Ask, shows it as a tag next to the `+` button; remove the tag to return to the default mode. User messages appear as full-width cards drawn like the composer. Editing, attachments, and checkpoint actions remain available. The agent's thinking shows as a single "Thinking" line while it streams and "Thought" afterwards, with how long it took, such as "Thought 16s"; click it to read the reasoning. Reading a file shows as a single line with the file's name; click it to open the file. Other tool calls that only look things up, such as searching, show as single lines you can click to see their input and output. Edits, commands, subagents, and tool calls waiting for permission show as cards. An edit's header shows how many lines it added and removed, a command's header shows its first line, and the buttons that answer a permission request sit at the bottom of the card. Consecutive tool calls, with the thinking between them, fold into one line that sums them up, such as "Edited 2 files, explored 3 files, ran 1 command +12 −4"; click it to see each one. Subagents and tool calls waiting for permission stay in view. Once a turn is done, everything the agent did before its last reply folds into a "Worked for 2m 15s" line above the reply. Completed turns start folded immediately, including the turn you just watched. Opening a Worked row keeps that turn expanded until you close it. Errors and warnings about the conversation show as cards above the composer, with buttons to act on them, such as retrying or copying the error.

Switching tabs keeps work running. Closing a conversation tab requests cancellation and releases its session. Saved conversations remain available through the agent's history support. Central Agent tab positions are not restored when restarting zdsh.

While a thread is still empty, the agent selector above the message editor switches it to **Zed Agent** or any installed external agent. Once the thread has messages, its agent stays fixed. To open a terminal, use **New Terminal** from the right sidebar's **Terminal** category menu. The agent selector only lists agents.

Context, provider usage, completion, and composer menus float above an ongoing conversation's composer or below an empty one. Opening them does not shrink the transcript or its navigation. The context indicator remains visible before the first request: native conversations show an estimate from their context and model limit; an external agent that has not reported usage shows an unavailable value. Completed turns fold their final thinking block together with earlier work while keeping the answer visible. A new user message has 32 px of space above it.

The composer has a changed-files chip when the conversation edited files; Git commits remain in the Changes panel. Copying an answer briefly changes its copy icon to a checkmark and shows a success notification. The response footer has more space between its action buttons and no longer has scroll-to-user-message or scroll-to-top buttons. Select response text and choose **Add to Chat** to append it as a quote to the draft, without sending. This works with native and external ACP conversations.

For an idle native conversation, **Fork Conversation Here** beneath an answer opens a new conversation containing the history through that answer, including its saved thinking and work durations. Later turns, the current draft, temporary terminal state, and session sandbox grants are not copied. Forking leaves project files and the original conversation unchanged. Historical forking is not exposed for external ACP agents, whose support varies.

Native conversations show the last model request's output token rate and input cache hit rate when the provider reports token counts. The rate divides output tokens by the full request duration, including connection, prefill, and reasoning time but excluding subsequent tool execution. The cache percentage divides cache-read tokens by total input tokens. These measurements are saved with the assistant message and survive reopening a conversation; old messages and providers without usage reports have no inferred measurements.

After a native reply completes, **Composer Predictions** can suggest a short follow-up in an empty composer. **Tab** accepts it as an editable draft; sending is a separate action. Typing, adding context, editing an earlier message, or starting another request discards the suggestion. **Escape** dismisses it. Predictions use the current conversation model, the latest user request, the completed final reply, and earlier user requests for context, with normal provider usage. They must cite a supporting passage from the final reply internally; unrelated, malformed, or unsupported results are not displayed. A completed task without a clear continuation produces no suggestion. Disable them with `agent.show_composer_predictions: false` or the corresponding Agent setting. They are disabled in Debug mode and are not generated for external ACP conversations.

## Debug mode

The native Agent's **Debug** mode provides a runtime-evidence workflow for trusted local projects. Entering Debug supplies full investigation guidance; later turns receive a shorter reminder with the current session, run ID, collector address, and log path. Mode changes take effect before the next model request, and compaction restores the full guidance. These internal reminders persist with the conversation without appearing as user messages. The guidance asks the agent to identify hypotheses, add temporary probes, cite observations when confirming or rejecting hypotheses, compare the same reproduction before and after a fix, and retain instrumentation until verification or an explicit cleanup request. These are model instructions, not an automatic guarantee that a diagnosis is correct.

The **Debug** pill opens a bounded **Reproduction Steps** tray with Markdown content and a pending indicator when user action is needed. Steps can arrive while the response streams; **Proceed** and **Mark Fixed** become available after generation stops. Protocol tags are hidden from the transcript while the original message remains in history. Ordinary queued messages wait while reproduction is pending; leaving Debug resumes normal queue dispatch. Failed responses suppress reproduction actions. Cancelled or interrupted Debug follow-ups can retry the same user request, including after reopening the conversation, instead of creating duplicate follow-up messages.

**Proceed** sends a short reproduction confirmation; the agent reads the actual log file with its tools. **Mark Fixed** requests removal of temporary instrumentation while preserving the fix and unrelated changes, and keeps the conversation in Debug mode. The user controls reproduction and confirmation. The **Logs** view shows a live count, timestamps and messages, expandable raw events, **Open File**, and **Clear Logs**. Visible logs refresh in the background every 500 ms, with incremental reading and a bounded view of the latest 100 entries / 64 KiB. Clear Logs affects only the current session's file; incomplete trailing lines are not counted until completed.

A session-local collector accepts JSON events on a loopback address. Its configuration and NDJSON log are stored under `.zed/debug/<session-id>/` and reused when the conversation is reopened; generated files are Git-ignored. Individual HTTP events are limited to 64 KiB and the log to 16 MiB. If the saved port is occupied, the collector keeps the session token, saves a new address, and informs the user and agent that probes need updating. Collector health, startup and file errors are exposed with a retry action. Subagents share their parent investigation's collector. External ACP agents retain their own modes; this workflow does not provide a remote or cloud collector.

## Sidebar and file navigation

The right sidebar keeps **Agent**, **Terminal**, **File**, and **Action** categories available. With no tabs it becomes an icon rail; category menus create the corresponding tabs. In the expanded sidebar, category menu indicators appear on hover. Agent tabs show connection, generation, approval, and error status; terminal tabs show command or task execution status. File tabs use their file icons, respecting icon settings and falling back to a category icon when needed.

The Threads panel provides history, search, and archive actions. It has no New Thread, New Terminal, or project-switch dropdown controls. Use the right category menus to create tabs and the file panel header to switch projects or worktrees.

The file panel's project and worktree dropdowns use GPUI Kit search lists, and its context menu uses the Kit popup menu. Project selection includes other projects already open in the window. Worktree search retains creation, switching, opening in another window, removal from the window, and deletion with confirmation. Indentation guides stay hidden until an entry is selected.

Choose **Add to Chat** on a file or folder to attach it to the active conversation draft, creating a draft conversation if needed. A changed file's **Open Diff** action opens its single-file diff directly. The diff toolbar uses Kit buttons for navigating changes, staging, unstaging, restoring, and whole-file actions, while retaining the existing diff-style and full-file controls.

## Accounts

zdsh runs without a Zed account. Configure a provider in the Settings Editor to use the Zed Agent with your own API access, provider subscription, or local model. External agents retain their own authorization flows.

Zed-hosted models, Zed cloud search, subscriptions, contacts, channels, calls, and shared editing are unavailable. The desktop does not restore Zed login sessions or read or delete existing Zed account credentials. The titlebar menu still provides settings, themes, and extensions.

Existing explicit Zed-hosted model selections require you to choose another model; zdsh does not replace them with a different paid provider. SSH, local development tools, extension downloads, and telemetry are independent of Zed account access.

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

Command approval cards keep the full command visible after a `$` prompt, with a stronger pending border and compact Kit buttons. **Run** allows this execution and **Skip** rejects only this execution, including their keyboard shortcuts; neither inherits a permanent scope selected earlier. The separate **Always Run** split button applies an explicitly selected set of command patterns or a single named persistent scope; its arrow opens the available rules. Selecting patterns does not run a command, and the action stays disabled until a scope is known. Narrow cards hide the permanent action before shrinking Run and Skip. Requested capabilities and sandbox warnings remain visible, with the sandbox documentation link inline. An agent-supplied reason and any review reason appear on a tinted strip below the command card. Existing permission and sandbox checks remain in force.

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

All standalone terminals open as central tabs beside your files. Choose **New Terminal** from the right sidebar's **Terminal** category menu or use `workspace::NewTerminal`. **Open in Terminal** uses the selected folder or the selected file's parent directory. Task execution, debugger terminals, and external-agent login commands also open central tabs.

Use `terminal::Focus` to focus the most recent terminal, creating one if necessary. Existing bindings for `workspace::NewCenterTerminal` remain supported. Replace custom bindings for `terminal_panel::Toggle` or `terminal_panel::ToggleFocus` with `terminal::Focus`.

Use `workspace::NewTerminal` to create additional terminal tabs. Move or split them with the same tab and pane controls used for files.

Command suggestions and history navigation load the current shell's `HISTFILE`, including custom paths configured by Zsh or Bash startup files. The history is refreshed in the background at each shell prompt and combined with commands from the current tab. Commands submitted through the input area or quick actions are added to shell history and flushed to the configured file; internal shell-integration commands and commands beginning with whitespace are not saved. With no `HISTFILE` configured, suggestions still use the current tab's in-memory history.

The **Terminal** category menu in the right sidebar lists saved command sequences. Choose **Manage quick actions…** to open their management tab, or use `terminal::OpenTerminalQuickActions`. Add, edit, and delete actions there; saved actions are shared across workspaces in the current user data directory. Each command targets the current terminal or a new terminal using its working directory. The current target starts as the most recently active terminal and follows any terminal created by an earlier step; when no terminal is open, one is created. Commands require shell integration and wait for the target shell to be ready. **Stop remaining steps** cancels queued steps without interrupting commands already submitted. Switching tabs preserves an editing draft; closing the management tab discards unsaved edits.

Terminal tabs restore through the workspace layout. Existing terminal-panel sessions are imported as central tabs, preserving working directories and custom titles; panel splits become tabs. Running processes and command output are not restored.

The terminal panel and its settings controls are removed. Legacy `terminal.button`, `dock`, `starts_open`, `flexible`, `default_width`, `default_height`, and `show_count_badge` values are accepted but have no effect. Tasks use `reveal_target: "center"`; legacy `"dock"` values are accepted as an alias. `reveal: "never"` keeps the current tab active, while `"no_focus"` displays the terminal without taking keyboard focus.
