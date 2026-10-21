---
title: Agent Tool Extensions
description: "Contribute Agent tools from the same ZDSH extension package."
---

# Agent Tool Extensions

ZDSH development builds accept Agent tools from the existing extension installer. A package can provide both editor features and Agent tools through one `extension.toml` and one Wasm component. Upstream Zed does not implement this optional interface.

## Declare and implement a tool

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

## Invocation and lifecycle

Extension tools are available to the Zed Agent and to DSH. Use a tool-capable model and the Write mode, or a profile that enables the tool. A profile's `tools` entry for the model-facing name (for example `"my-extension_echo": true`) enables or disables a tool explicitly; without an entry, `enable_all_context_servers` decides, as it does for MCP tools. Extension tools are not offered in restricted workspaces. A tool whose name matches a built-in Zed Agent tool is skipped.

The tool receives validated JSON arguments. Its result, or its error, is returned to the model and shown in the tool card.

- **Zed Agent**: the tool list is read again for each model request, so newly installed or updated tools appear without starting a new chat. Approval follows [tool permissions](../ai/tool-permissions.md) under the ID `extension:<extension-id>:<tool-name>`, where `<tool-name>` is the manifest key. Choosing "Always for … extension tool" in the prompt saves that ID's `default`.
- **DSH**: a chat captures installed tool definitions when its first message starts, so start a new chat after installing or updating tools. Every invocation asks for approval once.

Call `zed_extension_api::agent_tools::is_cancelled()` during long operations. Cancellation is cooperative: release resources before returning. Unloading revokes new calls, requests cancellation and waits for admitted calls before completing the reload. Old chat references never start using replacement code. A tool that ignores cancellation can block unloading or normal application shutdown.

The optional WIT interface is `zdsh:agent@0.1.0`; historical Zed WIT definitions are preserved. Existing extensions without this interface continue to load. This interface does not expose arbitrary GPUI state, project buffers, terminal access, Cordis service injection or waterfall middleware. Existing extension capabilities still govern host operations; external processes require their own confinement policy.
