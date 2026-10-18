# dsh_zed

The adapter between Zed and the DSH Rust core. It reuses providers, authentication state, and credentials from `LanguageModelRegistry` and passes `gpui_tokio::Tokio::handle(cx)` to DSH without creating another Tokio runtime. `dsh_agent` and `dsh_runtime` do not depend on GPUI.

## Code organization

`dsh_zed.rs` exports the host API. `host.rs` manages sessions and shutdown; `model.rs` and `model/mapping.rs` dispatch and translate model requests and responses; `agent_server.rs` implements Agent connections, with `model_selector.rs` and `transcript.rs` handling model selection and ACP event projection. `extension_tools.rs` adapts extension tools and authorization. See [DSH architecture](../../docs/src/development/dsh.md) for component dependencies.

## Panel preview

Choose **DSH Preview** from the Agent panel's new-chat menu, select a configured Zed model through the existing model picker, and send text. Responses and thinking stream into the panel, and trailing token usage updates its statistics. The stop button requests cancellation; closing a chat waits for DSH cleanup. Zed Agent remains the default.

The preview accepts text and loads Agent tools declared by installed extensions. Every invocation requires panel approval. Built-in file and command tools are not available; attachments are rejected before submission. The model is fixed after the first successful DSH runtime creation, so changing models requires a new chat. Authentication uses the selected Zed provider. Conversations, sidebar entries, and drafts remain in memory and cannot be restored after closing or quitting. The preview is unavailable in collaborative projects.

`DshAgentServer` implements Zed's in-process `AgentServer` and `AgentConnection` interfaces without launching an ACP subprocess. Log subscriptions use append cursors to catch up after coalesced notifications. Shutdown retains ownership of pending startup tasks so abandoning a UI wait does not leave an unmanaged session.

## Invocation and ownership

The application calls `init(cx)` after initializing model providers. Initialization neither creates Agent sessions nor sends model requests. Callers select an existing `LanguageModel`, set `AgentConfig.model` to `model_key(&model)`, explicitly supply `ModelOptions`, budgets, queue capacity, and a `ToolRegistry`, then await `start_session(model, config, tools, cx)?.await`. Missing authentication, a mismatched model key, or supplying tools to a model without tool support fails before session creation.

`Session::agent()` returns the DSH Agent service for submitting turns and reading the log. `Session::close(cx).await` cancels and waits for the Agent and tools before stopping GPUI model dispatch. Dropping the Session handle does not unload it; the host retains ownership until explicit close or application shutdown. Failed closes can be retried, and interrupting a close wait preserves pending runtime cleanup.

Model dispatch begins on the GPUI foreground executor. DSH's Tokio tasks await the sendable future returned by the Zed provider; GPUI state and provider objects are not sent to Tokio. The bridge uses the session's explicit queue capacity. Connection and stream cancellation follow provider behavior: the adapter drops the future or stream, and some Zed implementations cancel internal producer tasks by dropping GPUI tasks without exposing a completion notification. The host can therefore wait for DSH tools and bridge dispatch, but does not guarantee that all provider-internal tasks or remote requests have ended.

## Model translation

The adapter translates system, user, Assistant, and tool-result messages while preserving ordered thinking, signatures, redacted thinking, reasoning details, raw tool arguments, and thought signatures. Incremental tool output is logged as progress; complete JSON arguments reach the loop once. Reasoning details follow Zed's last-nonempty-update semantics.

Raw provider events and normalized output are recorded in the in-memory Session. Usage retains input, output, and cache token fields. The adapter continues reading trailing usage after Stop and emits DSH's Finished at EOF. Missing Stop, new content after Stop, incomplete or duplicate tool calls, and argument parsing failures are reported explicitly. Truncation and refusal reach the Agent as distinct finish reasons. Free-text tools and provider context compaction are unsupported; such events end the current step while retaining the raw event.

## Application shutdown

Normal Quit and quitting after closing the last window await `shutdown(cx)` before calling `App::quit`. Once shutdown begins, new sessions are rejected. All sessions request closure concurrently, each waiting for tool cleanup. Unresponsive tools block normal exit. Cleanup failures return to the caller; the exit task logs the error and keeps the process alive so quitting can be retried.

`on_app_quit` provides a fallback, but GPUI currently waits only 200 milliseconds for quit callbacks, which cannot guarantee arbitrary tool cleanup. Direct exit, restart, and forced termination lack normal Quit's complete waiting guarantee. Session `close` and normal `shutdown` must run while GPUI can still dispatch work.

## Checks and scope

Run from the repository root:

```sh
cargo check --locked -p zed
cargo fmt -p dsh_zed -p dsh_agent -p dsh_runtime -p zed -- --check
./script/clippy --locked -p dsh_zed -p dsh_agent -p dsh_runtime
```

Built-in file and command tools, model changes during a conversation, and disk session recovery are not available. Streaming, conversation continuation, cancellation, and tool approval require manual UI verification. Real model API calls have not been verified.

## Unified extension tools

Agent tools share Zed's `extension.toml`, development extension installer, and Wasm instance with language, theme, and other editor extensions. The `extension` crate owns tool contributions; `dsh_zed` adapts them for DSH without a separate installer or plugin directory. See [Agent tools](../../docs/src/extensions/agent-tools.md) for the extension interface.

The tool list is captured when the session runtime is first created. Start a new chat after changing the model or installing or updating extensions. Tool names include the extension identifier, and arguments are validated against the manifest's JSON Schema. Each execution requests approval or denial for that invocation only. Arguments, results, and failures enter the DSH log and appear in native tool cards. The preview permits at most eight model requests per turn.

Unloading synchronously revokes new calls and requests cancellation of active tools. The host waits for those calls to return before completing reload and working-directory cleanup. Cancellation is cooperative: extensions must inspect the cancellation flag and finish using resources before returning. Unresponsive extensions can block unloading and normal application exit. Existing sessions hold revocable tool references; they cannot invoke old code after unloading or silently switch to a replacement version.

Wasm and existing host capability checks still apply. External processes launched by extensions do not automatically receive additional sandboxing. The tool interface accepts JSON without adding project-file, buffer, or terminal access APIs.
