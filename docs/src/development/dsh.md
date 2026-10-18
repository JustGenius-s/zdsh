# DSH Architecture

DSH implements the Agent loop and built-in service lifecycle in zdsh's root Cargo workspace. It preserves typed services, reversible effects, and model context reconstructed from Session logs. All three crates live under `crates/` and share the toolchain and Cargo.lock.

## Component responsibilities

| Component | Responsibility |
| --- | --- |
| `dsh_runtime` | Built-in plugin services, events, task cancellation, dependency unloading, and asynchronous cleanup |
| `dsh_agent` | Agent interfaces, default loop, model and tool protocols, and in-memory Session logs |
| `dsh_zed` | Zed model bridge, host session management, ACP connections, and extension-tool approval |
| `language_model` / `language_models` | Zed model registration, configuration, authentication, and provider implementations |
| `extension` / `extension_host` / `extension_api` | Extension declarations, contribution registration, installation, Wasm loading, and SDK |
| `acp_thread` / `agent_ui` | Shared conversation UI, tool cards, and permission interactions |

Dependencies flow from `dsh_zed` to `dsh_agent` to `dsh_runtime`; the Zed adapter also uses the runtime directly. `dsh_runtime` and `dsh_agent` do not depend on Zed or GPUI. Standalone hosts may create their own Tokio runtime; the Zed host uses the existing `gpui_tokio` handle.

## Sessions and extensions

`dsh_zed::host` owns the runtime and model bridge, including startup, failure cleanup, and shutdown. `agent_server` implements Zed's AgentServer and AgentConnection, with separate child modules for model selection and log-to-ACP translation. `model` dispatches model requests between GPUI and Tokio; its `mapping` child module translates requests and responses. `extension_tools` adapts extension contributions as DSH tools and requests approval through ACP.

Third-party tools and editor features share `extension.toml`, the installer, and the Wasm instance; see [Agent tools](../extensions/agent-tools.md). `dsh_runtime` manages built-in Agent services and tasks without providing a separate third-party installer. Shutdown cancels and waits for the Agent and tools before releasing model dispatch and dependent resources.

## Current scope

The Agent picker offers DSH; Zed Agent remains the default. Conversations open as central workspace items, with `ConversationItem` owning each view and `AgentPanel` routing existing actions and history. Closing a tab cancels its conversation and removes the controller's retained references; moving a tab between panes preserves its session. See [Agent tabs](../ai/agent-panel.md#agent-tabs-in-zdsh) for behavior and restoration limits. DSH supports text conversations and extension-tool approval. Built-in file and command tools, attachments, disk session recovery, and context compaction are not available. Real model API calls have not been verified. DSH shares the native Agent model catalog and favorites, profile selector, and model-specific thinking controls. Model, profile, and inference choices are captured per turn without resetting the in-memory conversation.

TypeScript Cordis plugins and historical DSH Session files are not compatible. Third-party extensions do not expose Cordis service injection, plugin trees, or waterfall middleware. Future editor tools should reuse Zed's project, buffer, and terminal capabilities, with adaptations for each tool's execution context.

### Thread history placement

`ConversationItem` presents the existing Threads sidebar inside its tab. `MultiWorkspace` keeps one shared list controller and records its owning item, so project observers, metadata writes, and restore operations are not duplicated for each tab. The global window layout and status bar do not render a Threads sidebar. History switches between an inline column and an overlay based on the conversation pane width. Closing its owning item releases the presentation; the shared history store remains available.
