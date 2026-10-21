use crate::{AgentToolOutput, AnyAgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema::v2 as acp;
use anyhow::{Result, anyhow};
use extension::{AgentToolDefinition, ExtensionAgentTool, ExtensionHostProxy};
use futures::FutureExt as _;
use gpui::{App, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use std::sync::{Arc, Weak};

/// Generates a tool ID for an extension tool that can be used in settings.
///
/// The format is `extension:<extension_id>:<tool_name>`, where `tool_name` is
/// the manifest key, to avoid collisions with built-in and MCP tools.
pub fn extension_tool_id(extension_id: &str, tool_name: &str) -> String {
    format!("extension:{}:{}", extension_id, tool_name)
}

/// Returns the Agent tools contributed by currently loaded extensions, keyed by
/// their model-facing name (`<extension_id>_<tool_name>`).
pub(crate) fn extension_tools(cx: &App) -> Vec<(SharedString, Arc<dyn AnyAgentTool>)> {
    let Some(proxy) = ExtensionHostProxy::try_global(cx) else {
        return Vec::new();
    };
    proxy
        .agent_tools
        .snapshot()
        .into_iter()
        .filter_map(|(name, definition, tool)| {
            // A tool whose extension unloaded after the snapshot was taken is skipped.
            let permission_id = {
                let tool = tool.upgrade()?;
                extension_tool_id(&tool.extension_id, &tool.local_name)
            };
            let name = SharedString::from(name);
            let tool: Arc<dyn AnyAgentTool> = Arc::new(ExtensionTool {
                name: name.clone(),
                permission_id,
                definition,
                tool,
            });
            Some((name, tool))
        })
        .collect()
}

struct ExtensionTool {
    name: SharedString,
    permission_id: String,
    definition: AgentToolDefinition,
    // Weak so a thread cannot keep an unloaded Wasm instance alive or call its replacement.
    tool: Weak<ExtensionAgentTool>,
}

impl AnyAgentTool for ExtensionTool {
    fn name(&self) -> SharedString {
        self.name.clone()
    }

    fn description(&self) -> SharedString {
        self.definition.description.clone().into()
    }

    fn kind(&self) -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(&self, _input: serde_json::Value, _cx: &mut App) -> SharedString {
        format!("Run extension tool `{}`", self.name).into()
    }

    fn input_schema(&self) -> serde_json::Value {
        let mut schema = self.definition.input_schema.clone();
        language_model::tool_schema::normalize_tool_schema(&mut schema);
        schema
    }

    // Extension code runs with the extension's host capabilities, so it is
    // withheld from untrusted workspaces like the other tools that can act
    // outside the project.
    fn allow_in_restricted_mode(&self) -> bool {
        false
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<serde_json::Value>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<AgentToolOutput, AgentToolOutput>> {
        let initial_title = self.initial_title(serde_json::Value::Null, cx);
        let authorize = event_stream.authorize_third_party_tool(
            initial_title,
            self.permission_id.clone(),
            self.name.to_string(),
            "extension",
            cx,
        );

        cx.spawn(async move |_cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| anyhow!(error.to_string()))?;

            authorize
                .await
                .map_err(|error| anyhow!(error.to_string()))?;

            let tool = self
                .tool
                .upgrade()
                .ok_or_else(|| anyhow!("The extension providing `{}` was unloaded", self.name))?;
            let cancellation = tool.cancellation();
            let execution = tool.execute(input, cancellation.clone()).fuse();
            futures::pin_mut!(execution);

            let output = futures::select! {
                output = execution => output?,
                _ = event_stream.cancelled_by_user().fuse() => {
                    cancellation.cancel();
                    // Extensions stop cooperatively; wait for the call to release
                    // its resources so unloading never races an active call.
                    if let Err(error) = execution.await {
                        log::debug!(
                            "Extension tool `{}` failed after cancellation: {error:#}",
                            self.name
                        );
                    }
                    return Err(anyhow!("Extension tool cancelled by user").into());
                }
            };

            let text = match &output {
                serde_json::Value::String(text) => text.clone(),
                output => output.to_string(),
            };
            event_stream.update_fields(|update| {
                update.content(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                    acp::TextContent::new(text.clone()),
                ))])
            });
            Ok(AgentToolOutput {
                llm_output: vec![LanguageModelToolResultContent::Text(text.into())],
                raw_output: output,
            })
        })
    }

    fn replay(
        &self,
        _input: serde_json::Value,
        _output: serde_json::Value,
        _event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Result<()> {
        Ok(())
    }
}
