use acp_thread::{AcpThread, AuthorizationKind, PermissionOptions, RequestPermissionOutcome};
use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
use anyhow::Result;
use dsh_agent::{Tool, ToolDefinition, ToolError, ToolInvocation, ToolRegistry, ToolResult};
use dsh_runtime::Cancellation;
use extension::{ExtensionAgentTool, ExtensionHostProxy};
use futures::{FutureExt, future::BoxFuture};
use gpui::{App, AsyncApp, Task, WeakEntity};
use serde_json::Value;
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, oneshot};

struct Approval {
    id: String,
    name: String,
    arguments: Value,
    cancellation: Cancellation,
    response: oneshot::Sender<ToolResult>,
}

struct ExtensionTool {
    definition: ToolDefinition,
    tool: Weak<ExtensionAgentTool>,
    approvals: mpsc::UnboundedSender<Approval>,
}

pub fn call_id(turn: dsh_agent::TurnId, step: usize, id: &dsh_agent::CallId) -> String {
    format!("dsh-{}-{step}-{}", turn.0, id.0)
}

pub fn registry(thread: WeakEntity<AcpThread>, cx: &mut App) -> Result<(ToolRegistry, Task<()>)> {
    let mut registry = ToolRegistry::default();
    let (approvals, mut requests) = mpsc::unbounded_channel::<Approval>();
    for (name, definition, tool) in ExtensionHostProxy::default_global(cx)
        .agent_tools
        .snapshot()
    {
        registry.register(Arc::new(ExtensionTool {
            definition: ToolDefinition {
                name,
                description: definition.description,
                parameters: definition.input_schema,
            },
            tool,
            approvals: approvals.clone(),
        }))?;
    }
    let task = cx.spawn(async move |cx| {
        while let Some(request) = requests.recv().await {
            let result = approve(&thread, &request, cx)
                .await
                .map_err(|error| ToolError::Execution(format!("{error:#}")));
            if let Err(_caller_gone) = request.response.send(result) {
                // The session can close while a permission request is being resolved.
            }
        }
    });
    Ok((registry, task))
}

async fn approve(
    thread: &WeakEntity<AcpThread>,
    request: &Approval,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let mut cancellation = request.cancellation.clone();
    anyhow::ensure!(
        !cancellation.is_cancelled(),
        "tool cancelled before permission request"
    );
    let id = acp_v1::ToolCallId::new(request.id.clone());
    let permission = thread.update(cx, |thread, cx| {
        thread.request_tool_call_authorization(
            acp_v1::ToolCallUpdate::new(
                id,
                acp_v1::ToolCallUpdateFields::new()
                    .title(format!("Extension tool: {}", request.name))
                    .raw_input(request.arguments.clone())
                    .status(acp_v1::ToolCallStatus::Pending),
            ),
            PermissionOptions::Flat(vec![
                acp_v2::PermissionOption::new(
                    "allow",
                    "Allow once",
                    acp_v2::PermissionOptionKind::AllowOnce,
                ),
                acp_v2::PermissionOption::new(
                    "reject",
                    "Reject",
                    acp_v2::PermissionOptionKind::RejectOnce,
                ),
            ]),
            AuthorizationKind::PermissionGrant,
            cx,
        )
    })??;
    let outcome = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            thread.update(cx, |thread, cx| thread.cancel_tool_call_authorization(&acp_v2::ToolCallId::new(request.id.clone()), cx))?;
            RequestPermissionOutcome::Cancelled
        },
        outcome = permission => outcome,
    };
    match outcome {
        RequestPermissionOutcome::Selected(selected)
            if selected.option_id.0.as_ref() == "allow"
                && selected.option_kind == acp_v2::PermissionOptionKind::AllowOnce =>
        {
            Ok(Value::Null)
        }
        _ => anyhow::bail!("extension tool permission was not granted"),
    }
}

impl ExtensionTool {
    async fn run(
        &self,
        id: String,
        arguments: Value,
        mut cancellation: Cancellation,
    ) -> ToolResult {
        let tool = self.tool.upgrade().ok_or_else(|| {
            ToolError::Execution("extension unloaded; start a new chat to refresh tools".into())
        })?;
        let extension_cancellation = tool.cancellation();
        if cancellation.is_cancelled() || extension_cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let (response, approved) = oneshot::channel();
        self.approvals
            .send(Approval {
                id,
                name: self.definition.name.clone(),
                arguments: arguments.clone(),
                cancellation: cancellation.clone(),
                response,
            })
            .map_err(|_| {
                ToolError::Execution("Agent panel closed before permission request".into())
            })?;
        approved.await.map_err(|_| {
            ToolError::Execution("Agent panel closed while awaiting permission".into())
        })??;
        if cancellation.is_cancelled() || extension_cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let execution = tool.execute(arguments, extension_cancellation.clone());
        tokio::pin!(execution);
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => { extension_cancellation.cancel(); execution.await },
            result = &mut execution => result,
        };
        if cancellation.is_cancelled() || extension_cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        result.map_err(|error| ToolError::Execution(format!("{error:#}")))
    }
}

impl Tool for ExtensionTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn execute_call(
        &self,
        invocation: ToolInvocation,
        cancellation: Cancellation,
    ) -> BoxFuture<'_, ToolResult> {
        self.run(
            call_id(invocation.turn, invocation.step, &invocation.call.id),
            invocation.call.arguments,
            cancellation,
        )
        .boxed()
    }
    fn execute(&self, arguments: Value, cancellation: Cancellation) -> BoxFuture<'_, ToolResult> {
        self.run(uuid::Uuid::new_v4().to_string(), arguments, cancellation)
            .boxed()
    }
}
