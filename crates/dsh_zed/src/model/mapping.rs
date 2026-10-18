use dsh_agent::{
    AssistantContent, CallId, FinishReason, Message, ModelChunk, ModelError, ModelRequest,
    ModelStream, TokenUsage, ToolCall,
};
use futures::{StreamExt, stream};
use language_model::{
    LanguageModelCompletionEvent as Event, LanguageModelCompletionStream, LanguageModelRequest,
    LanguageModelRequestMessage, LanguageModelRequestTool, LanguageModelToolResult,
    LanguageModelToolResultContent, LanguageModelToolUse, LanguageModelToolUseInput,
    MessageContent, Role, StopReason,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

pub(super) fn request(request: ModelRequest) -> Result<LanguageModelRequest, ModelError> {
    let mut messages = Vec::new();
    let mut pending = BTreeMap::new();
    for message in request.messages {
        let mut reasoning_details = None;
        let (role, content) = match message {
            Message::System { text } => (Role::System, vec![MessageContent::Text(text)]),
            Message::User { text } => (Role::User, vec![MessageContent::Text(text)]),
            Message::Assistant { content } => {
                let mut mapped = Vec::new();
                for part in content {
                    mapped.push(match part {
                        AssistantContent::Text(text) => MessageContent::Text(text),
                        AssistantContent::Thinking { text, signature } => MessageContent::Thinking { text, signature },
                        AssistantContent::RedactedThinking(data) => MessageContent::RedactedThinking(data),
                        AssistantContent::ReasoningDetails(details) => {
                            // Zed providers send replacement reasoning details, including an empty early placeholder.
                            if !matches!(&details, serde_json::Value::Array(items) if items.is_empty()) {
                                reasoning_details = Some(Arc::new(details));
                            }
                            continue;
                        }
                        AssistantContent::ToolCall(call) => {
                            if pending.insert(call.id.clone(), call.name.clone()).is_some() {
                                return Err(ModelError("duplicate unsettled tool call".into()));
                            }
                            MessageContent::ToolUse(LanguageModelToolUse {
                                id: call.id.0.into(), name: call.name.into(),
                                raw_input: call.raw_input.unwrap_or_else(|| call.arguments.to_string()),
                                input: LanguageModelToolUseInput::Json(call.arguments),
                                is_input_complete: true, thought_signature: call.thought_signature,
                            })
                        }
                    });
                }
                (Role::Assistant, mapped)
            }
            Message::Tool { call_id, result } => {
                let name = pending
                    .remove(&call_id)
                    .ok_or_else(|| ModelError("tool result has no matching call".into()))?;
                let is_error = result.is_err();
                let output = match result {
                    Ok(value) => value,
                    Err(error) => serde_json::to_value(error)
                        .map_err(|error| ModelError(error.to_string()))?,
                };
                (
                    Role::User,
                    vec![MessageContent::ToolResult(LanguageModelToolResult {
                        tool_use_id: call_id.0.into(),
                        tool_name: name.into(),
                        is_error,
                        content: vec![LanguageModelToolResultContent::Text(
                            output.to_string().into(),
                        )],
                        output: Some(output),
                    })],
                )
            }
        };
        messages.push(LanguageModelRequestMessage {
            role,
            content,
            cache: false,
            reasoning_details,
        });
    }
    if !pending.is_empty() {
        return Err(ModelError(
            "model history contains unsettled tool calls".into(),
        ));
    }
    Ok(LanguageModelRequest {
        thread_id: None,
        prompt_cache_key: None,
        prompt_id: None,
        intent: None,
        messages,
        tools: request
            .tools
            .into_iter()
            .map(|tool| {
                LanguageModelRequestTool::function(
                    tool.name,
                    tool.description,
                    tool.parameters,
                    false,
                )
            })
            .collect(),
        tool_choice: None,
        stop: Vec::new(),
        temperature: None,
        thinking_allowed: request.options.thinking_allowed,
        thinking_effort: request.options.thinking_effort,
        max_output_tokens: request.options.max_output_tokens,
        speed: None,
        compact_at_tokens: None,
    })
}

struct Response {
    stream: LanguageModelCompletionStream,
    provider: String,
    stop: Option<FinishReason>,
    pending_calls: BTreeSet<String>,
    completed_calls: BTreeSet<String>,
    buffered: VecDeque<Result<ModelChunk, ModelError>>,
    ended: bool,
}

pub(super) fn response(stream: LanguageModelCompletionStream, provider: String) -> ModelStream {
    stream::unfold(
        Response {
            stream,
            provider,
            stop: None,
            pending_calls: BTreeSet::new(),
            completed_calls: BTreeSet::new(),
            buffered: VecDeque::new(),
            ended: false,
        },
        |mut state| async move {
            loop {
                if let Some(chunk) = state.buffered.pop_front() {
                    return Some((chunk, state));
                }
                if state.ended {
                    return None;
                }
                match state.stream.next().await {
                    Some(Ok(event)) => {
                        if let Err(error) = state.event(event) {
                            state.buffered.push_back(Err(error));
                            state.ended = true;
                        }
                    }
                    Some(Err(error)) => {
                        state.buffered.push_back(Err(ModelError(error.to_string())));
                        state.ended = true;
                    }
                    None => {
                        state.ended = true;
                        let terminal = match state.stop.take() {
                            Some(reason @ (FinishReason::MaxTokens | FinishReason::Refusal)) => {
                                Ok(ModelChunk::Finished(reason))
                            }
                            _ if !state.pending_calls.is_empty() => Err(ModelError(
                                "model ended with incomplete tool arguments".into(),
                            )),
                            Some(reason) => Ok(ModelChunk::Finished(reason)),
                            None => Err(ModelError(
                                "model stream ended without a stop reason".into(),
                            )),
                        };
                        state.buffered.push_back(terminal);
                    }
                }
            }
        },
    )
    .boxed()
}

impl Response {
    fn event(&mut self, event: Event) -> Result<(), ModelError> {
        // Retain provider progress and malformed/unsupported events even when translation fails.
        self.buffered.push_back(Ok(ModelChunk::ProviderEvent {
            provider: self.provider.clone(),
            event: serde_json::to_value(&event).map_err(|error| ModelError(error.to_string()))?,
        }));
        if self.stop.is_some() && !matches!(event, Event::UsageUpdate(_)) {
            return Err(ModelError(
                "model emitted content after its stop reason".into(),
            ));
        }
        let chunk = match event {
            Event::Text(text) => Some(ModelChunk::Text(text)),
            Event::Thinking { text, signature } => Some(ModelChunk::Thinking { text, signature }),
            Event::RedactedThinking { data } => Some(ModelChunk::RedactedThinking(data)),
            Event::ReasoningDetails(details) => Some(ModelChunk::ReasoningDetails(details)),
            Event::UsageUpdate(usage) => Some(ModelChunk::Usage(TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
            })),
            Event::Stop(reason) => {
                // Some providers send final usage after Stop; expose Finished only after EOF.
                self.stop = Some(match reason {
                    StopReason::EndTurn => FinishReason::Stop,
                    StopReason::ToolUse => FinishReason::ToolCalls,
                    StopReason::MaxTokens => FinishReason::MaxTokens,
                    StopReason::Refusal => FinishReason::Refusal,
                });
                None
            }
            Event::ToolUse(call) => {
                let id = call.id.to_string();
                if self.completed_calls.contains(&id) {
                    return Err(ModelError("model repeated a completed tool call".into()));
                }
                if !call.is_input_complete {
                    self.pending_calls.insert(id);
                    return Ok(());
                }
                self.pending_calls.remove(&id);
                self.completed_calls.insert(id.clone());
                let LanguageModelToolUseInput::Json(arguments) = call.input else {
                    return Err(ModelError("DSH supports JSON function tools only".into()));
                };
                Some(ModelChunk::ToolCall(ToolCall {
                    id: CallId(id),
                    name: call.name.to_string(),
                    arguments,
                    raw_input: Some(call.raw_input),
                    thought_signature: call.thought_signature,
                }))
            }
            Event::ToolUseJsonParseError { .. } => {
                return Err(ModelError("model returned invalid tool JSON".into()));
            }
            Event::Compaction(_) => {
                return Err(ModelError(
                    "DSH does not yet support provider context compaction".into(),
                ));
            }
            Event::Queued { .. } | Event::Started | Event::StartMessage { .. } => None,
        };
        if let Some(chunk) = chunk {
            self.buffered.push_back(Ok(chunk));
        }
        Ok(())
    }
}
