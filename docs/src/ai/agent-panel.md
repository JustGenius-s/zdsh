---
title: AI Coding Agent - Zed Agent Panel
description: Use Zed's AI coding agent to generate, refactor, and debug code with tool calling, checkpoints, and multi-model support.
---

# Agent Panel

The Agent Panel is where you interact with AI agents that can read, write, and run code in your project.
It's the core of Zed's AI code editing experience — use it for code generation, refactoring, debugging, documentation, and general questions.

Open it with {#action agent::NewThread} from [the Command Palette](../command-palette.md) or click the **New Agent Tab** icon in the top-right pane toolbar.

## Agent tabs in zdsh

In zdsh, each Agent conversation opens in a central editor tab. The project file tree can stay open beside it. This applies to the Zed Agent, DSH, and external ACP agents; terminal entries open in separate central terminal tabs.

Use the New Thread action, the **New Agent Tab** button in the top-right pane toolbar, or thread history to open a conversation. New Thread always creates an Agent conversation. The pane toolbar stays visible when focus moves elsewhere and when all tabs are closed; each toolbar opens new tabs in its own pane. Opening an already open conversation activates its tab. Drag tabs to reorder them or move them into a split pane to work beside code. Each conversation has one tab; splitting does not duplicate the conversation.

The tab displays its title and connection, generation, approval, or error status. Its toolbar uses the same row height, padding, theme background, and separator colors as file and Git toolbars and provides the agent picker and chat options. Toolbar sizing follows the global UI font size independently of the Agent conversation font size. Files can still be dragged into the conversation as context.

Use the double speech bubble button in the conversation toolbar to open **Thread History** inside the tab. The list retains project grouping, search, archive, and restore actions. It shares the existing history data across tabs. Only one tab presents the list at a time; opening it in another tab moves the list there. File tabs and the project file tree keep their full layout.

On wide panes, history sits to the right of the conversation with a draggable divider. On narrow panes, it overlays the right side of the conversation; click outside it or use the toolbar button to close it. The existing Toggle Threads Sidebar and Focus Sidebar shortcuts target this tab-local history. From a file tab, they open a conversation tab first. History starts collapsed after restarting zdsh.

The conversation uses the toolbar background. In an empty conversation the composer is centered in the tab; once the conversation has messages, it docks at the bottom. It follows the configured content width and retains model, mode, thinking, context, and send controls.

The `+` button in the composer picks a mode, starts a file mention, switches models, and turns MCP servers on or off. The default mode, Write, stays implicit. Picking another mode, such as Ask, shows it as a tag next to the `+` button; remove the tag to return to the default mode. User messages appear as full-width cards drawn like the composer. Editing, attachments, and checkpoint actions remain available. The agent's thinking shows as a single "Thinking" line while it streams and "Thought" afterwards, with how long it took, such as "Thought 16s"; click it to read the reasoning. Reading a file shows as a single line with the file's name; click it to open the file. Other tool calls that only look things up, such as searching, show as single lines you can click to see their input and output. Edits, commands, subagents, and tool calls waiting for permission show as cards. An edit's header shows how many lines it added and removed, a command's header shows its first line, and the buttons that answer a permission request sit at the bottom of the card. Consecutive tool calls, with the thinking between them, fold into one line that sums them up, such as "Edited 2 files, explored 3 files, ran 1 command +12 −4"; click it to see each one. Subagents and tool calls waiting for permission stay in view. Once a turn is done, everything the agent did before its last reply folds into a "Worked for 2m 15s" line above the reply. The turn you just watched stays open until you send the next message, and turns from earlier or from a reopened conversation start folded. Errors and warnings about the conversation show as cards above the composer, with buttons to act on them, such as retrying or copying the error.

Switching tabs keeps work running. Closing a conversation tab requests cancellation and releases its session. Saved conversations remain available through the agent's history support; DSH conversations are currently in memory and cannot be restored after closing. Central Agent tab positions are not restored when restarting zdsh.

## Accounts in zdsh {#zdsh-accounts}

zdsh runs without a Zed account. Configure a provider in the Settings Editor to use
DSH or Zed Agent with your own API access, provider subscription, or local model.
External agents and Copilot retain their own authorization flows.

Zed-hosted models, Zed cloud search, Zed cloud edit predictions, subscriptions,
contacts, channels, calls, and shared editing are unavailable. The desktop does
not restore Zed login sessions or read or delete existing Zed account credentials.
The titlebar menu still provides settings, themes, extensions, and updates.

Existing explicit Zed-hosted model selections require you to choose another model;
zdsh does not replace them with a different paid provider. A legacy
`edit_predictions.provider` value of `zed` disables predictions until you select
an available provider. SSH, local development tools, extension downloads, updates,
and telemetry are independent of Zed account access.

## Getting Started {#getting-started}

If you're using the Agent Panel for the first time, configure either a model for the [Zed Agent](./zed-agent.md) or an [External Agent](./external-agents.md).

- Use [LLM Providers](./llm-providers.md) for API access, provider subscriptions, gateways, and local models.
- Use [External Agents](./external-agents.md) for ACP-integrated agents.
- Use [AI Quick Start](./quick-start.md) if you are not sure which path to choose.

## Overview {#overview}

With an LLM provider or External Agent configured, type in the message editor and press `enter` to submit.
Expand the editor with {#kb agent::ExpandMessageEditor} if you need more room.

Responses stream in with indicators showing [which tools](./tools.md) the model is using.
The sections below cover what you can do from here.

> Note that some Agent Panel features may not be available for every External Agent. Restoring threads from history, checkpoints, token usage display, and similar features depend on the agent integration.
> Their availability varies depending on the agent.

### Creating New Threads {#new-thread}

By default, the Agent Panel uses Zed's first-party agent.

Start a new thread with {#kb agent::NewThread}. While a thread is still empty, the agent selector above the message editor switches it to **Zed Agent** or any installed [External Agent](./external-agents.md). Once the thread has messages, its agent stays fixed.

To open a terminal, use **New Terminal** from the pane toolbar’s `+` menu. The agent selector only lists agents.

{#action agent::NewExternalAgentThread} creates a new thread with the specified External Agent id.

You can also start a new thread from the [Threads Sidebar](./parallel-agents.md#threads-sidebar), scoped to a specific project — see [Running Multiple Threads](./parallel-agents.md#running-multiple-threads).

### Managing Multiple Threads {#multiple-threads}

You can run multiple agent threads at once, each working independently with its own agent, context window, and conversation history. Open the Threads Sidebar with {#kb multi_workspace::ToggleWorkspaceSidebar} to see all your threads grouped by project. Click any thread to switch to it, or use the thread switcher ({#kb agents_sidebar::ToggleThreadSwitcher}) to cycle between recent threads without opening the sidebar.

Threads you're no longer working on can be archived by hovering over them in the sidebar and clicking the archive icon, or selecting them and pressing {#kb agent::ArchiveSelectedThread}. The Thread History holds all your threads across all projects, sorted chronologically, and you can restore them at any time.

If two threads might edit the same files, you can isolate one in a new Git worktree. Use the worktree picker in the title bar to pick which worktree the agent runs in, or create a new one. See [Worktree Isolation](./parallel-agents.md#worktree-isolation) for details.

For more details on the Threads Sidebar and managing multiple projects, see [Parallel Agents](./parallel-agents.md).

### Editing Messages {#editing-messages}

Any message that you send to the model is editable.
You can click on the card that contains your message and re-submit it with an adjusted prompt and/or new pieces of context.

### Queueing Messages

Messages sent while the agent is in the generating state get, by default, queued.

By default, queued messages get sent once the agent finishes generating. If you want a queued message to reach the Zed Agent sooner—interrupting it at its next step (usually between a tool call and a response) rather than waiting for it to finish—toggle "Steer" on that message. Steering is only available for the Zed Agent, since Zed can't detect turn boundaries for external agents.

You can edit or remove (an individual or all) queued messages.
You can also still interrupt the agent immediately if you want by either clicking on the stop button or by clicking the "Send Now" (double-enter) on a queued message.

### Checkpoints {#checkpoints}

Every time the model performs an edit, hovering your message shows a "Restore Checkpoint" button on its top edge, allowing you to return your code base to the state it was in prior to that message.

The checkpoint button appears even if you interrupt the thread midway through an edit, as this is likely a moment when you've identified that the agent is not heading in the right direction and you want to revert back.

### Context Menu {#context-menu}

Right-click on any agent response in the thread view to access a context menu with the following actions:

- **Copy**: Copies the currently selected text as plain text (available when text is selected).
- **Copy as Markdown**: Copies the currently selected text as Markdown (available when text is selected).
- **Copy This Agent Response**: Copies the full text of the agent response you right-clicked on.
- **Scroll to Top / Scroll to Bottom**: Scrolls to the beginning or end of the thread, depending on your current position.
- **Open Thread as Markdown**: Opens the entire thread as a Markdown file in a new tab.

### Navigating the Thread {#navigating-the-thread}

In long conversations, use the scroll arrow buttons at the bottom of the panel to jump to your most recent prompt or to the very beginning of the thread. Once your prompt scrolls out of view, it stays pinned at the top of the thread while you read the agent's response; click it to scroll back to the prompt. You can also scroll the thread using arrow keys, Page Up/Down, Home/End, and Shift+Page Up/Down to jump between messages, when the thread pane is focused.

When focus is in the message editor, you can also use {#kb agent::ScrollOutputPageUp}, {#kb agent::ScrollOutputPageDown}, {#kb agent::ScrollOutputToTop}, {#kb agent::ScrollOutputToBottom}, {#kb agent::ScrollOutputLineUp}, and {#kb agent::ScrollOutputLineDown} to navigate the thread, or {#kb agent::ScrollOutputToPreviousMessage} and {#kb agent::ScrollOutputToNextMessage} to jump between your prompts.

### Thread titles {#thread-titles}

Thread titles are auto-generated based on the content of the conversation.
You can rename a thread from the [Threads Sidebar](./parallel-agents.md#threads-sidebar).

### Following the Agent {#following-the-agent}

Follow the agent as it reads and edits files by clicking the crosshair icon at the bottom left of the panel.
Your editor will jump to each file the agent touches.

You can also hold `cmd`/`ctrl` when submitting a message to automatically follow.

### Keeping the System Awake {#prevent-idle-sleep}

While an agent thread is running, Zed asks the operating system not to idle-sleep, so long-running turns are not interrupted when you step away.
The display can still turn off and the screen can still lock; only system sleep is held back, and only while a thread is generating, not while it waits for your confirmation.

Turn this off with the `agent.prevent_idle_sleep` setting.
If the operating system rejects the request, Zed logs the reason and continues without it.

### Get Notified {#get-notified}

If you send a prompt to the Agent and then put Zed in the background, you can choose to be notified when its generation wraps up via:

- a visual desktop notification from your operating system
- a sound notification

These notifications can be used together or individually, and you can use the `agent.notify_when_agent_waiting` and `agent.play_sound_when_agent_done` settings keys to customize that, including turning both off entirely.

### Reviewing Changes {#reviewing-changes}

Once the agent has made changes to your project, the panel will surface which files, how many of them, and how many lines have been edited.

To see which files specifically have been edited, click the "Changes" button above the message editor, which lists them in a card over the composer, or click the `Review Changes` button ({#kb agent::OpenAgentDiff}), which opens a special multi-buffer tab with all changes.

You can accept or reject each individual change hunk, or the whole set of changes made by the agent.

Edit diffs can also appear inline in individual files with the same
keep/reject hunk controls as the multi-buffer review pane. This temporarily overrides the buffer's git diff while review is active. Enable it by setting `agent.single_file_review` to `true` in your settings.

## Terminal Threads {#terminal-threads}

In zdsh, Terminal Threads open in central terminal tabs alongside Agent conversation tabs. Closing a terminal tab also removes its Terminal Thread entry. For opening, closing, notifications, terminal titles, and CLI/TUI-specific setup, see [Terminal Threads](./terminal-threads.md).

## Adding Context {#adding-context}

The agent can search your codebase to find relevant context, but providing it explicitly improves response quality and reduces latency.

Add context by typing `@` in the message editor.
You can mention files, directories, symbols, previous threads, [skills](./skills.md), diagnostics, branch diffs, and URLs to fetch.

When you paste multi-line code selections copied from a buffer, Zed automatically formats them as @-mentions with the file context.
To paste content without this automatic formatting, use {#kb agent::PasteRaw} to paste raw text directly.

### Selection as Context

Additionally, you can also select text in a buffer or terminal and add it as context by using the {#kb agent::AddSelectionToThread} keybinding, running the {#action agent::AddSelectionToThread} action, or choosing **Selection** in the `@` menu.

### Images as Context

It's also possible to attach images in your prompt for providers that support vision models.
OpenAI GPT-4o and later, Anthropic Claude 3 and later, Google Gemini 1.5 and 2.0, and Bedrock vision models (Claude 3+, Amazon Nova Pro and Lite, Meta Llama 3.2 Vision, Mistral Pixtral) all support image inputs.

To add an image, choose **Image** in the `@` menu to pick image files, @-mention it from your project's folder, or drag it from your file system directly into the Agent Panel message editor.
Copying an image and pasting it is also supported.

## Token Usage and Compaction {#token-usage}

Zed surfaces how many tokens you are consuming for your currently active thread near the profile selector in the panel's message editor.

Zed automatically compacts long Zed Agent threads as they approach the configured token threshold. Compaction summarizes earlier messages and replaces them in the model context with that summary, leaving more room for the next turn. The thread shows a **Context Compacted** entry that you can expand to inspect the summary. You can compact manually by typing `/compact` in the message editor.

If the selected model's context window is too small for automatic compaction (less than 80000 tokens), a banner appears above the message editor as you approach the token limit. Use **Start New Thread** from that banner to continue in a new thread seeded with a summary. You can also @-mention a past thread in a new one.

Configure automatic compaction with `agent.auto_compact`. See [Agent Settings](./agent-settings.md#automatic-compaction) for options.

## Changing Models {#changing-models}

After you've configured your LLM providers—either via [API access](./use-api-access.md) or through [Zed-hosted models](../account/zed-hosted-models.md)—you can switch between their models by clicking on the model selector on the message editor or by using the {#kb agent::ToggleModelSelector} keybinding.

> The same model can be offered via multiple providers - for example, Claude Sonnet 4.5 is available via Zed Pro, OpenRouter, Anthropic directly, and more.
> Make sure you've selected the correct model **_provider_** for the model you'd like to use, delineated by the logo to the left of the model in the model selector.

### Favoriting Models

You can mark specific models as favorites either through the model selector, by clicking on the star icon button that appears as you hover the model, or through your settings via the `agent.favorite_models` settings key.

Cycle through your favorites with {#kb agent::CycleFavoriteModels} without opening the model selector.

## Using Tools and Profiles {#using-tools}

The Agent Panel supports tool calling, which enables agentic editing. Zed includes [built-in tools](./tools.md) for searching your codebase, editing files, running terminal commands, and more.

Use [Agent Profiles](./agent-profiles.md) to choose which built-in tools and MCP tools are available in a Zed Agent thread. Use [Tool Permissions](./tool-permissions.md) to control whether permission-gated tool calls are allowed, denied, or confirmed.

You can add external tools with [MCP Servers](./mcp.md).

### Model Support {#model-support}

Tool calling needs to be individually supported by each model and model provider.
Therefore, despite the presence of built-in tools, some models may not have the ability to pick them up.
You should see a "No tools" label if you select a model that falls into this case.

All [Zed-hosted models](../account/zed-hosted-models.md) support tool calling out-of-the-box.

### MCP Servers {#mcp-servers}

Similarly to the built-in tools, some models may not support all tools included in a given MCP Server.
Zed's UI will inform you about this via a warning icon that appears close to the model selector.

## Errors and Debugging {#errors-and-debugging}

If you hit an error or unusual LLM behavior, open the thread as Markdown with {#action agent::OpenActiveThreadAsMarkdown} and attach it to your GitHub issue.

You can also open threads as Markdown by clicking on the file icon button, to the right of the thumbs down button, when focused on the panel's editor.

## Feedback {#feedback}

You can rate agent responses to help improve Zed's system prompt and tools.

> **Warning:** Rating an AI response sends the conversation thread to Zed. The
> conversation thread includes your messages, AI responses, and thread metadata.
> See [Feedback and Training Data](./ai-improvement.md) and
> [AI Privacy](./privacy-and-security.md) for more information.
> **_If you don't want data persisted on Zed's servers, don't rate_**.
> We will not collect data for improving Zed's agent experience without you
> explicitly rating responses.

To help improve Zed's system prompt and tools, rate responses with the thumbs up/down controls at the end of each response.
In case of a thumbs down, a new text area will show up where you can add more specifics about what happened.

You can provide feedback on the thread at any point after the agent responds, and multiple times within the same thread.
