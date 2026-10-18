# dsh_runtime

The plugin runtime for the DSH Rust port provides typed services, serial events, and asynchronous unloading without depending on Zed or GPUI. Background tasks use a Tokio executor supplied explicitly by the host.

## Code organization

`dsh_runtime.rs` contains public types and exports. `context.rs` implements activation scopes and reversible registrations; `runtime.rs` manages dependencies, activation, and cleanup; `error.rs` defines failure types; `task.rs` manages cancellation and task completion. See [DSH architecture](../../docs/src/development/dsh.md) for component dependencies.

## Lifecycle

`Runtime::register` stores a synchronous activation callback under a `PluginId`. `activate(...).await` runs activation and waits for rollback on failure. `unload(...).await` and `shutdown().await` wait for tasks and cleanup actions to finish.

`Service` uses marker types as service keys. Its associated `Value` may be a trait object; each key has at most one provider per runtime. `Context::provide` registers a service, and `require` resolves it while recording a dependency on its provider. Dependencies must already be active. Missing services fail immediately, without automatic scheduling or waiting. A plugin may also resolve services it has registered during its own activation.

`unload` stops all direct and transitive consumers before their provider. Unrelated plugins remain active. Each plugin first requests cancellation of all its tasks and waits for them to exit, then reverses its service registrations, event subscriptions, and cleanup actions. After unloading, the plugin returns to `Registered` and may be explicitly activated again. Consumers are not reactivated automatically.

## Typed events

Event payloads implement `Event`. Plugins register asynchronous listeners with `Context::on::<E, _>`, and hosts dispatch events with `Runtime::emit(event).await`. Listeners are awaited in plugin activation order and then registration order. Dispatch succeeds when there are no listeners. The first failure stops dispatch and reports the plugin, event type, and original error.

Subscriptions are removed during rollback or unloading. Dispatch holds exclusive access to the runtime, preventing concurrent lifecycle operations. Cancelling dispatch drops the current listener future; it neither reverses completed listeners nor resends the event. Listeners must tolerate cancellation and must not start background tasks outside plugin ownership.

Events currently provide serial notifications, without parallel broadcasting, return-value aggregation, or waterfall middleware.

## Tasks and cleanup

`Runtime::with_executor(handle)` accepts the host's Tokio handle, and `Context::spawn` creates plugin-owned tasks. `Runtime::default()` supports events and cleanup but rejects background task creation. The host must keep the executor running until unloading completes.

Independent operations can create a cancellation source with `CancellationSource::default()`, obtain a token with `token()`, and request cancellation with `cancel()`. Dropping all copies of a source also cancels its tokens. The Agent uses this mechanism to cancel individual turns.

Tasks receive a `Cancellation` token and may inspect `is_cancelled()` or await `cancelled()`. Cancellation remains requested once signalled. Tasks must finish using their resources before returning. Normal unloading neither forcibly terminates tasks nor applies an implicit timeout; unresponsive tasks block unloading. Task errors and panics are reported when unloading joins tasks, without immediate notification during execution. Completed task failures are reported once; a subsequent unload continues with remaining tasks and resources.

Register synchronous cleanup with `on_cleanup` and asynchronous cleanup with `on_cleanup_async`. Asynchronous cleanup uses a future factory: retrying after failure creates a new future, while interrupting only the outer unload preserves the pending future for resumption. Cleanup actions must support retries after partial execution.

After cleanup fails or its wait is interrupted, the plugin remains `CleanupPending`, retaining unreleased resources and dependencies and blocking activation and event dispatch. Calling `unload` or `shutdown` again resumes cleanup without repeating successful actions. If activation and rollback both fail, both errors are retained. Dropping an activation future during rollback discards that call's activation error but preserves pending cleanup, which still requires explicit unloading.

## Resource ownership

- Register cleanup immediately after acquiring a resource. Tasks must finish using resources before returning and must not wait for their own plugin's cleanup callbacks to stop them: those callbacks run only after tasks exit.
- Service registrations, tasks, or cleanup closures own activation resources. Activation callbacks remain registered after unloading and must not retain activation resources in their captures.
- `Arc` guarantees memory lifetime only. Service handles must not escape their plugin's lifetime; cloning a handle does not keep an unloaded provider operational.
- Hosts must explicitly call `shutdown` and handle its result. Dropping `Runtime` requests cancellation and aborts remaining tasks, but cannot wait for them or run fallible cleanup actions, so it cannot guarantee resource release order.
- Activation, event, and cleanup callbacks must return failures through `Result` rather than panic. Only panics in tasks created through `spawn` are reported as Tokio join errors.

## Checks

Run from the repository root:

```sh
cargo check --locked -p dsh_runtime --lib
cargo fmt -p dsh_runtime -- --check
./script/clippy --locked -p dsh_runtime
```

## Scope

Plugin trees, child scopes, waterfall middleware, configuration loading, and dynamic plugin distribution are not implemented. This crate does not promise Cordis API or behavioral compatibility. `dsh_agent` implements the Agent loop, and `dsh_zed` integrates it with Zed.
