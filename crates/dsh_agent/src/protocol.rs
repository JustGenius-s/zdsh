use dsh_runtime::{Cancellation, Service};
use futures::{future::BoxFuture, stream::BoxStream};
use serde::Serialize;
use serde_json::Value;

/// A failure visible to the caller and recorded at the end of a started turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum AgentError {
    #[error("turn cancelled")]
    Cancelled,
    #[error("agent is stopped")]
    Stopped,
    #[error("model step limit reached")]
    StepLimit,
    #[error("model output limit reached")]
    OutputLimit,
    #[error("model refused the request")]
    Refused,
    #[error("model request failed: {0}")]
    Model(String),
    #[error("invalid model response: {0}")]
    Protocol(String),
}

/// Provider-owned failure text. Providers must redact credentials before returning it.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct ModelError(pub String);

/// Identifier assigned by the model adapter; validated before tool execution.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CallId(pub String);

/// Fully assembled tool call; adapters assemble incremental argument fragments.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    pub arguments: Value,
    pub raw_input: Option<String>,
    pub thought_signature: Option<String>,
}

/// Model-facing tool metadata. The tool implementation validates its arguments.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Tool failures are returned to the model as results, rather than ending a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum ToolError {
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("tool execution failed: {0}")]
    Execution(String),
    #[error("tool cancelled")]
    Cancelled,
}

/// Exact result returned by an executed tool, or an explicit unexecuted-call error.
pub type ToolResult = Result<Value, ToolError>;

/// Model history projected only from admitted users and settled responses/results.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System { text: String },
    User { text: String },
    Assistant { content: Vec<AssistantContent> },
    Tool { call_id: CallId, result: ToolResult },
}

/// Ordered model history, including opaque reasoning required on subsequent requests.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AssistantContent {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
    ReasoningDetails(Value),
    ToolCall(ToolCall),
}

/// Explicit inference options. `None` delegates optional values to the provider.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelOptions {
    pub thinking_allowed: bool,
    pub thinking_effort: Option<String>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// Complete model-visible request, logged before invoking the provider.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelRequest {
    pub model: String,
    pub options: ModelOptions,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    Refusal,
}

/// Ordered stream output. `Finished` is terminal; EOF without it is an error.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ModelChunk {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
    ReasoningDetails(Value),
    Usage(TokenUsage),
    ProviderEvent {
        provider: String,
        event: Value,
    },
    ToolCall(ToolCall),
    Finished(FinishReason),
}

pub type ModelStream = BoxStream<'static, Result<ModelChunk, ModelError>>;

/// Model adapter. Request and stream futures must release their resources when
/// dropped on cancellation. Adapters must document any provider-owned background
/// work whose completion is not observable through this interface.
pub trait ModelProvider: Send + Sync {
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: Cancellation,
    ) -> BoxFuture<'_, Result<ModelStream, ModelError>>;
}

pub struct ModelService;
impl Service for ModelService {
    type Value = dyn ModelProvider;
}

/// Logged identity and arguments of a tool invocation.
pub struct ToolInvocation {
    pub turn: crate::TurnId,
    pub step: usize,
    pub call: ToolCall,
}

/// Executes one tool call. Validate arguments before any side effect, observe
/// cancellation cooperatively, and finish resource use before returning.
/// The agent awaits this future even after cancellation; callbacks must not panic.
pub trait Tool: Send + Sync {
    /// Supplies the logged call identity to hosts that display permission requests.
    fn execute_call(
        &self,
        invocation: ToolInvocation,
        cancellation: Cancellation,
    ) -> BoxFuture<'_, ToolResult> {
        self.execute(invocation.call.arguments, cancellation)
    }

    fn definition(&self) -> ToolDefinition;
    fn execute(&self, arguments: Value, cancellation: Cancellation) -> BoxFuture<'_, ToolResult>;
}
