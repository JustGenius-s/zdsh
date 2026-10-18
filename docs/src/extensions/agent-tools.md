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

Choose DSH Preview and a tool-capable model. A chat captures installed tool definitions when its first message starts. Start a new chat after installing or updating tools. Each invocation requests explicit approval through the existing Agent panel. The tool receives validated JSON arguments; its result or error is recorded in the DSH session and displayed in the tool card.

Call `zed_extension_api::agent_tools::is_cancelled()` during long operations. Cancellation is cooperative: release resources before returning. Unloading revokes new calls, requests cancellation and waits for admitted calls before completing the reload. Old chat references never start using replacement code. A tool that ignores cancellation can block unloading or normal application shutdown.

The optional WIT interface is `zdsh:agent@0.1.0`; historical Zed WIT definitions are preserved. Existing extensions without this interface continue to load. This interface does not expose arbitrary GPUI state, project buffers, terminal access, Cordis service injection or waterfall middleware. Existing extension capabilities still govern host operations; external processes require their own confinement policy.
