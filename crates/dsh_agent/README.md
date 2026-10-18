# dsh_agent

A minimal Agent loop installed as a `dsh_runtime` plugin. It records user input, calls a streaming model, executes tools serially, and sends results back to the model until it produces a final response, fails, is cancelled, or exhausts its step budget. It does not depend on Zed or GPUI.

## Code organization

`dsh_agent.rs` defines the public Agent, Turn, and configuration interfaces. `agent_loop.rs` implements the default plugin, queue, and execution loop; `protocol.rs` defines model and tool protocols; `session.rs` manages the log; `tools.rs` manages tool registration. See [DSH architecture](../../docs/src/development/dsh.md) for component dependencies.

## Plugin composition

Activate a model plugin providing `ModelService` and a tool plugin providing `ToolsService`, then install the default loop with `install(context, config)`. The loop provides `AgentService`; consumers obtain an `Arc<dyn Agent>` through `context.require::<AgentService>()`. Replacing the model, tool collection, or Agent implementation does not require changing the runtime.

`AgentConfig` explicitly specifies the model name, `ModelOptions` (thinking permission, thinking effort, and output token limit), system prompt, maximum model steps per turn, and pending queue capacity. Budgets and capacities use `NonZeroUsize`. An empty model name or queue capacity exceeding Tokio's limit fails activation. The loop requires a host executor configured through `Runtime::with_executor`.

Each activation owns an in-memory Session and a plugin-managed worker task. When unloading the model or tool provider, the runtime cancels and waits for the Agent worker before releasing dependencies. Reactivation creates a new Session without restoring the previous conversation.

## Submission and cancellation

`agent.submit(input).await` uses a bounded queue and returns a `Turn`. Turns execute serially; submission waits when the queue is full. Dropping a pending submission does not enqueue a turn. `turn.wait().await` returns the final text or a typed error after committing the turn's terminal event.

`turn.cancel()` requests cancellation. Dropping a `Turn` or its `wait` future also requests cancellation. Queued turns cancelled before execution do not enter the Session log. Turns that have not started when the plugin unloads return `Stopped`. Started turns record `Cancelled` and close any started steps when cancellation ends execution.

Cancelling model connection or streaming drops the corresponding future or stream. Adapters must release their resources and document cancellation guarantees for provider-owned background tasks; the interface does not expose a completion notification for those tasks. Tools stop cooperatively through `Cancellation`. The Agent waits for an active tool to return rather than dropping its future on cancellation. Executed tools retain their actual results; tools not yet invoked record `Cancelled`. Every committed tool call has a result. Unresponsive tools block turn completion and plugin unloading.

## Model and tool interfaces

`ModelProvider::stream` accepts a complete `ModelRequest` and returns an ordered stream of `ModelChunk` values. Adapters assemble incremental tool arguments into complete `ToolCall` values. The loop validates nonempty call identifiers and tool names, and unique call identifiers within each response.

Each response must end with `Finished`: `Stop` cannot include tool calls, while `ToolCalls` requires them. EOF without a terminal marker, duplicate call identifiers, and inconsistent finish reasons are protocol errors; the response's tools are not executed. `MaxTokens` and `Refusal` return `OutputLimit` and `Refused`, retaining streamed log entries without committing an Assistant message or executing partial tool calls. `Finished` is terminal: the loop does not read subsequent stream items, so adapters must emit trailing usage first.

`ToolRegistry::register` assembles the tool collection before activation and rejects empty or duplicate names. Model-visible definitions are fixed at registration. Each tool's `execute` implementation validates JSON against its declared parameters before execution. The core runtime does not provide a general JSON Schema validator.

Unknown tools, invalid arguments, and tool execution failures are returned to the model as tool results so it can correct its request. Model connection or streaming failures end the current turn; the worker continues accepting later turns. Each model request consumes one step. Tool calls from the last permitted step still execute and are recorded; if another model request is needed, the turn ends with `StepLimit`.

Tools may override `Tool::execute_call` to receive the Session turn, step, and model call identifier for host authorization and result-card association. Its default implementation calls `execute`.

## Session log

`agent.session().events().await` returns a snapshot in append order. Clients subscribe through `subscribe()` and read incremental events through `events_since(cursor)`. Notifications may coalesce, but the append cursor prevents lost chunks. A cursor beyond the log length is rejected. The log contains the system prompt, user input, each complete model request, streamed chunks, Assistant messages, tool results, and step and turn outcomes. Requests include the model name, inference options, history, and tool definitions, allowing inspection of the actual model input. `AssistantContent` preserves ordered text, thinking and signatures, redacted thinking, provider reasoning details, and tool calls. Calls retain raw arguments and thought signatures. Usage and raw provider events are logged but are not replayed as extra messages.

`Session::messages` projects model history from the log, including user input, fully received and validated Assistant messages, and tool results. Chunks received before a failure or cancellation remain in `ModelOutput` events but are not replayed as completed Assistant messages. A tool result's `executed` field indicates whether its callback ran, not whether external side effects succeeded.

The log is in memory only and grows with the conversation. Disk persistence, recovery, compaction, and log size limits are not implemented. JSON serialization supports log inspection without promising compatibility with TypeScript DSH Session files or a stable Rust storage format.

## Checks

Run from the repository root:

```sh
cargo check --locked -p dsh_agent -p dsh_runtime --lib
cargo fmt -p dsh_agent -p dsh_runtime -- --check
./script/clippy --locked -p dsh_agent -p dsh_runtime
```

## Scope

[dsh_zed](../dsh_zed/README.md) reuses Zed's model providers; this crate has no HTTP provider. Built-in file and command tools, permission approval, parallel tool execution, context compaction, and waterfall extension points are not implemented here. `dsh_zed` provides the panel preview and extension-tool approval. Tool collections unload as a single plugin service, without individual tool loading or unloading during execution. Activation, model, and tool implementations must report failures through `Result`; the loop does not recover from panics.
