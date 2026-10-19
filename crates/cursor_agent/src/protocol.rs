//! Cursor's agent protocol: Connect streaming frames carrying protobuf-JSON messages.

use anyhow::{Context as _, Result, anyhow};
use bytes::Bytes;
use http_client::StatusCode;
use language_model::{
    LanguageModelCompletionError, LanguageModelRequest, LanguageModelRequestTool,
    LanguageModelRequestToolInput, LanguageModelToolResult, LanguageModelToolResultContent,
    MessageContent, ProviderErrorCategory, Role, TokenUsage,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::PROVIDER_NAME;

/// Cursor reports our tools under this MCP server.
pub(crate) const PROVIDER_IDENTIFIER: &str = "zed";

const END_STREAM_FLAG: u8 = 0b10;
const COMPRESSED_FLAG: u8 = 0b01;
/// Larger tool results are cut so a single transcript cannot exceed Cursor's prompt limit.
const TOOL_RESULT_MAX_CHARS: usize = 10_000;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn encode_frame(message: &Value) -> Result<Bytes> {
    let body = serde_json::to_vec(message)?;
    let length = u32::try_from(body.len()).context("Cursor request frame is too large")?;
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.push(0);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(Bytes::from(frame))
}

#[derive(Debug, PartialEq)]
pub(crate) enum Frame {
    Message(Value),
    /// The Connect end-of-stream frame; carries an error when the stream failed.
    End(Value),
}

#[derive(Default)]
pub(crate) struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub(crate) fn next_frame(&mut self) -> Result<Option<Frame>> {
        let Some(header) = self.buffer.get(..5) else {
            return Ok(None);
        };
        let flags = header[0];
        let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(anyhow!("Cursor sent a {length}-byte frame"));
        }
        if self.buffer.len() < 5 + length {
            return Ok(None);
        }
        if flags & COMPRESSED_FLAG != 0 {
            return Err(anyhow!("Cursor sent a compressed frame"));
        }
        let payload: Vec<u8> = self.buffer.drain(..5 + length).skip(5).collect();
        let value = if payload.is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_slice(&payload).context("Cursor sent a frame that is not JSON")?
        };
        Ok(Some(if flags & END_STREAM_FLAG != 0 {
            Frame::End(value)
        } else {
            Frame::Message(value)
        }))
    }
}

/// Keeps JSON Schema keywords Cursor accepts; others make it reject the whole tool list.
pub(crate) fn sanitize_schema(node: &Value) -> Value {
    let Value::Object(object) = node else {
        return node.clone();
    };
    let mut out = Map::new();
    let schema_type = match object.get("type") {
        Some(Value::String(schema_type)) => Some(schema_type.clone()),
        Some(Value::Array(types)) => types.iter().find_map(Value::as_str).map(str::to_string),
        _ => None,
    };
    if let Some(schema_type) = schema_type {
        out.insert("type".into(), schema_type.into());
    }
    for key in ["description", "title"] {
        if let Some(Value::String(text)) = object.get(key)
            && !text.is_empty()
        {
            out.insert(key.into(), text.clone().into());
        }
    }
    for key in [
        "enum",
        "const",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minProperties",
        "maxProperties",
        "format",
        "default",
    ] {
        if let Some(value) = object.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(Value::Array(required)) = object.get("required") {
        let required: Vec<Value> = required
            .iter()
            .filter(|key| key.is_string())
            .cloned()
            .collect();
        out.insert("required".into(), required.into());
    }
    if let Some(Value::Object(properties)) = object.get("properties") {
        out.entry("type").or_insert_with(|| "object".into());
        let properties: Map<String, Value> = properties
            .iter()
            .map(|(key, value)| (key.clone(), sanitize_schema(value)))
            .collect();
        out.insert("properties".into(), properties.into());
    }
    match object.get("additionalProperties") {
        Some(Value::Bool(allowed)) => {
            out.insert("additionalProperties".into(), (*allowed).into());
        }
        Some(schema @ Value::Object(_)) => {
            out.insert("additionalProperties".into(), sanitize_schema(schema));
        }
        _ => {}
    }
    if let Some(items) = object.get("items") {
        out.entry("type").or_insert_with(|| "array".into());
        let items = match items {
            Value::Array(items) => items.iter().map(sanitize_schema).collect(),
            items => sanitize_schema(items),
        };
        out.insert("items".into(), items);
    }
    for combiner in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(schemas)) = object.get(combiner) {
            out.insert(
                combiner.into(),
                schemas
                    .iter()
                    .map(sanitize_schema)
                    .collect::<Vec<_>>()
                    .into(),
            );
        }
    }
    if let Some(schema) = object.get("not") {
        out.insert("not".into(), sanitize_schema(schema));
    }
    let has_shape = ["type", "enum", "anyOf", "oneOf", "allOf"]
        .iter()
        .any(|key| out.contains_key(*key));
    if !has_shape {
        out.insert("type".into(), "object".into());
    }
    Value::Object(out)
}

pub(crate) fn tool_definitions(tools: &[LanguageModelRequestTool]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let LanguageModelRequestToolInput::Function { input_schema, .. } = &tool.input else {
                return None;
            };
            Some(json!({
                "name": tool.name,
                "description": tool.description,
                "toolName": tool.name,
                "providerIdentifier": PROVIDER_IDENTIFIER,
                "inputSchema": sanitize_schema(input_schema),
            }))
        })
        .collect()
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}\n… [truncated]")
}

pub(crate) fn tool_result_text(result: &LanguageModelToolResult) -> String {
    result
        .content
        .iter()
        .map(|content| match content {
            LanguageModelToolResultContent::Text(text) => text.to_string(),
            LanguageModelToolResultContent::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A Run starts from one user message, so a fresh Run carries the whole Zed transcript
/// flattened into text, plus every image it contains.
pub(crate) struct Transcript {
    pub prompt: String,
    pub images: Vec<String>,
}

pub(crate) fn transcript(request: &LanguageModelRequest) -> Transcript {
    let mut parts = Vec::new();
    let mut images = Vec::new();
    for message in &request.messages {
        for content in &message.content {
            match content {
                MessageContent::Text(text) if !text.trim().is_empty() => {
                    parts.push(text.clone());
                }
                MessageContent::Image(image) => images.push(image.source.to_string()),
                MessageContent::ToolUse(tool_use) => parts.push(format!(
                    "Previous assistant tool call {} ({}):\n{}",
                    tool_use.name, tool_use.id, tool_use.raw_input
                )),
                MessageContent::ToolResult(result) => {
                    for content in &result.content {
                        if let LanguageModelToolResultContent::Image(image) = content {
                            images.push(image.source.to_string());
                        }
                    }
                    parts.push(format!(
                        "Result of {} ({}):\n{}",
                        result.tool_name,
                        result.tool_use_id,
                        truncate_chars(&tool_result_text(result), TOOL_RESULT_MAX_CHARS)
                    ));
                }
                MessageContent::Compaction(_)
                | MessageContent::Text(_)
                | MessageContent::Thinking { .. }
                | MessageContent::RedactedThinking(_) => {}
            }
        }
    }
    let ends_with_tool_results = request.messages.last().is_some_and(|message| {
        message.role == Role::User
            && message
                .content
                .iter()
                .any(|content| matches!(content, MessageContent::ToolResult(_)))
    });
    if ends_with_tool_results {
        parts.push("Continue from the tool results above. Do not repeat them.".to_string());
    }
    Transcript {
        prompt: parts.join("\n\n"),
        images,
    }
}

pub(crate) fn run_request(
    transcript: Transcript,
    tools: &[Value],
    wire_model_id: &str,
    conversation_id: &str,
) -> Value {
    let mut user_message = json!({
        "text": transcript.prompt,
        "messageId": Uuid::new_v4().to_string(),
    });
    if !transcript.images.is_empty() {
        let images: Vec<Value> = transcript
            .images
            .into_iter()
            .map(|data| {
                json!({ "uuid": Uuid::new_v4().to_string(), "mimeType": "image/png", "data": data })
            })
            .collect();
        user_message["selectedContext"] = json!({ "selectedImages": images });
    }
    let mut run_request = json!({
        "conversationState": {},
        "action": { "userMessageAction": { "userMessage": user_message } },
        "modelDetails": {
            "modelId": wire_model_id,
            "displayName": wire_model_id,
            "displayNameShort": wire_model_id,
        },
        "requestedModel": { "modelId": wire_model_id },
        "conversationId": conversation_id,
    });
    if !tools.is_empty() {
        run_request["mcpTools"] = json!({ "tools": tools });
    }
    json!({ "runRequest": run_request })
}

#[derive(Debug, PartialEq)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ExecRequest {
    RequestContext,
    McpState,
    /// Allowlist prechecks; the field is the result field to reply with.
    Precheck(String),
    ToolCall(ToolCall),
    /// A native IDE capability Zed does not provide.
    Unsupported(String),
}

#[derive(Debug)]
pub(crate) enum ServerMessage {
    TextDelta(String),
    ThinkingDelta(String),
    TurnEnded(TokenUsage),
    Exec {
        id: u64,
        exec_id: String,
        request: ExecRequest,
    },
    SetBlob {
        id: u64,
        blob_id: String,
        data: String,
    },
    GetBlob {
        id: u64,
        blob_id: String,
    },
    Query {
        id: u64,
        kind: String,
    },
    Error(LanguageModelCompletionError),
    /// Heartbeats, checkpoints and progress updates that need no action.
    Other,
}

/// ProtoJSON omits zero-valued integers, including the first message id.
fn wire_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn text_field(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => ["text", "delta", "content"]
            .iter()
            .find_map(|key| object.get(*key).map(text_field))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// MCP arguments arrive as protobuf `Value` wrappers or JSON-encoded strings.
fn decode_protojson(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            let looks_like_json = (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'));
            if looks_like_json && let Ok(parsed) = serde_json::from_str(trimmed) {
                return parsed;
            }
            value.clone()
        }
        Value::Array(items) => items.iter().map(decode_protojson).collect(),
        Value::Object(object) => {
            if let Some(text) = object.get("stringValue") {
                return text.clone();
            }
            if let Some(number) = object.get("numberValue") {
                return number.clone();
            }
            if let Some(flag) = object.get("boolValue") {
                return flag.clone();
            }
            if object.contains_key("nullValue") {
                return Value::Null;
            }
            if let Some(list) = object.get("listValue") {
                return list
                    .get("values")
                    .and_then(Value::as_array)
                    .map(|values| values.iter().map(decode_protojson).collect())
                    .unwrap_or_else(|| Value::Array(Vec::new()));
            }
            if let Some(nested) = object.get("structValue") {
                return decode_protojson(nested);
            }
            if let Some(Value::Object(fields)) = object.get("fields") {
                return fields
                    .iter()
                    .map(|(key, value)| (key.clone(), decode_protojson(value)))
                    .collect::<Map<_, _>>()
                    .into();
            }
            object
                .iter()
                .map(|(key, value)| (key.clone(), decode_protojson(value)))
                .collect::<Map<_, _>>()
                .into()
        }
        other => other.clone(),
    }
}

fn parse_tool_call(args: &Value) -> Option<ToolCall> {
    // `name` carries the MCP server prefix; `toolName` is the name the tool was declared with.
    let name = args
        .get("toolName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let name = args.get("name")?.as_str()?;
            let prefix = format!("{PROVIDER_IDENTIFIER}-");
            Some(name.strip_prefix(&prefix).unwrap_or(name).to_string())
        })?;
    let input = match args.get("args").or_else(|| args.get("arguments")) {
        Some(raw) => match decode_protojson(raw) {
            object @ Value::Object(_) => object,
            Value::Null => json!({}),
            other => json!({ "value": other }),
        },
        None => json!({}),
    };
    let id = args
        .get("toolCallId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
    Some(ToolCall { id, name, input })
}

fn exec_request(exec: &Value) -> ExecRequest {
    let Some(object) = exec.as_object() else {
        return ExecRequest::Unsupported("unknown".into());
    };
    if object.contains_key("requestContextArgs") {
        return ExecRequest::RequestContext;
    }
    if object.contains_key("mcpStateExecArgs") {
        return ExecRequest::McpState;
    }
    for precheck in [
        "shellAllowlistPrecheckArgs",
        "mcpAllowlistPrecheckArgs",
        "webFetchAllowlistPrecheckArgs",
    ] {
        if object.contains_key(precheck) {
            return ExecRequest::Precheck(precheck.replace("Args", "Result"));
        }
    }
    for field in ["mcpArgs", "cursor"] {
        if let Some(call) = object.get(field).and_then(parse_tool_call) {
            return ExecRequest::ToolCall(call);
        }
    }
    let kind = object
        .keys()
        .find(|key| key.ends_with("Args") || key.ends_with("Request"))
        .cloned()
        .unwrap_or_else(|| "unknown".into());
    ExecRequest::Unsupported(kind)
}

fn usage_number(bags: &[&Value], keys: &[&str]) -> u64 {
    bags.iter()
        .find_map(|bag| {
            keys.iter()
                .map(|key| wire_u64(bag.get(*key)))
                .find(|count| *count > 0)
        })
        .unwrap_or(0)
}

pub(crate) fn turn_usage(ended: &Value) -> TokenUsage {
    let mut bags = vec![ended];
    for key in [
        "usage",
        "tokenDetails",
        "promptTokensDetails",
        "inputTokensDetails",
    ] {
        if let Some(nested) = ended.get(key) {
            bags.push(nested);
        }
    }
    TokenUsage {
        input_tokens: usage_number(&bags, &["inputTokens", "promptTokens"]),
        output_tokens: usage_number(&bags, &["outputTokens", "completionTokens"]),
        cache_read_input_tokens: usage_number(
            &bags,
            &["cacheReadTokens", "cachedTokens", "cacheReadInputTokens"],
        ),
        cache_creation_input_tokens: usage_number(
            &bags,
            &["cacheWriteTokens", "cacheCreationInputTokens"],
        ),
    }
}

pub(crate) fn connect_error(
    error: &Value,
    status: Option<StatusCode>,
) -> LanguageModelCompletionError {
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Cursor returned an error: {error}"));
    let category = match code.as_deref() {
        Some("unauthenticated") => ProviderErrorCategory::Authentication,
        Some("permission_denied") => ProviderErrorCategory::Permission,
        Some("resource_exhausted") => ProviderErrorCategory::RateLimit,
        Some("unavailable") => ProviderErrorCategory::Overloaded,
        Some("invalid_argument" | "failed_precondition" | "out_of_range") => {
            ProviderErrorCategory::InvalidRequest
        }
        Some("deadline_exceeded") => ProviderErrorCategory::Timeout,
        Some("internal" | "unknown" | "data_loss") => ProviderErrorCategory::InternalServer,
        _ => match status {
            Some(status) => ProviderErrorCategory::from_http_status(status, &message),
            None => ProviderErrorCategory::Other,
        },
    };
    LanguageModelCompletionError::from_provider_response(
        PROVIDER_NAME,
        status,
        code,
        message,
        None,
        category,
    )
}

pub(crate) fn parse_server_message(message: &Value) -> ServerMessage {
    if let Some(error) = message.get("error") {
        return ServerMessage::Error(connect_error(error, None));
    }
    if let Some(update) = message.get("interactionUpdate") {
        if let Some(delta) = update.get("textDelta") {
            return ServerMessage::TextDelta(text_field(delta));
        }
        if let Some(delta) = update.get("thinkingDelta") {
            // Cursor pads some thinking chunks with zero-width spaces.
            return ServerMessage::ThinkingDelta(text_field(delta).replace('\u{200b}', ""));
        }
        if let Some(ended) = update.get("turnEnded") {
            return ServerMessage::TurnEnded(turn_usage(ended));
        }
        return ServerMessage::Other;
    }
    if let Some(exec) = message.get("execServerMessage") {
        return ServerMessage::Exec {
            id: wire_u64(exec.get("id")),
            exec_id: exec
                .get("execId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            request: exec_request(exec),
        };
    }
    if let Some(kv) = message.get("kvServerMessage") {
        let id = wire_u64(kv.get("id"));
        if let Some(set) = kv.get("setBlobArgs") {
            return ServerMessage::SetBlob {
                id,
                blob_id: set
                    .get("blobId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                data: set
                    .get("blobData")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            };
        }
        if let Some(get) = kv.get("getBlobArgs") {
            return ServerMessage::GetBlob {
                id,
                blob_id: get
                    .get("blobId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            };
        }
        return ServerMessage::Other;
    }
    if let Some(query) = message.get("interactionQuery") {
        let kind = query
            .as_object()
            .and_then(|object| object.keys().find(|key| key.as_str() != "id").cloned())
            .unwrap_or_else(|| "unknown".into());
        return ServerMessage::Query {
            id: wire_u64(query.get("id")),
            kind,
        };
    }
    ServerMessage::Other
}

pub(crate) fn heartbeat() -> Value {
    json!({ "clientHeartbeat": {} })
}

pub(crate) fn stream_close(id: u64) -> Value {
    json!({ "execClientControlMessage": { "streamClose": { "id": id } } })
}

pub(crate) fn exec_result(id: u64, exec_id: &str, field: &str, result: Value) -> Value {
    json!({ "execClientMessage": { "id": id, "execId": exec_id, field: result } })
}

pub(crate) fn request_context_result(id: u64, exec_id: &str, tools: &[Value]) -> Value {
    let operating_system = match crate::client_os() {
        "mac" => "darwin",
        "windows" => "win32",
        _ => "linux",
    };
    exec_result(
        id,
        exec_id,
        "requestContextResult",
        json!({ "success": { "requestContext": {
            "env": { "operatingSystem": operating_system, "defaultShell": "bash" },
            "tools": tools,
        }}}),
    )
}

pub(crate) fn mcp_state_result(id: u64, exec_id: &str, tools: &[Value]) -> Value {
    exec_result(
        id,
        exec_id,
        "mcpStateExecResult",
        json!({ "success": { "servers": [{
            "serverName": PROVIDER_IDENTIFIER,
            "serverIdentifier": PROVIDER_IDENTIFIER,
            "status": "connected",
            "tools": tools,
            "instructions": [],
        }]}}),
    )
}

pub(crate) fn tool_result(id: u64, exec_id: &str, text: &str, is_error: bool) -> Value {
    exec_result(
        id,
        exec_id,
        "mcpResult",
        json!({ "success": { "content": [{ "text": { "text": text } }], "isError": is_error } }),
    )
}

/// Refuses a native IDE capability so the model falls back to the declared MCP tools.
pub(crate) fn unsupported_exec(id: u64, kind: &str) -> Value {
    json!({ "execClientControlMessage": { "throw": {
        "id": id,
        "error": format!("Native IDE capability {kind} is unavailable. Use the provided MCP tools."),
        "errorCode": "UNSUPPORTED_CAPABILITY",
    }}})
}

pub(crate) fn set_blob_result(id: u64) -> Value {
    json!({ "kvClientMessage": { "id": id, "setBlobResult": {} } })
}

pub(crate) fn get_blob_result(id: u64, data: Option<&str>) -> Value {
    let result = match data {
        Some(data) => json!({ "blobData": data }),
        None => json!({}),
    };
    json!({ "kvClientMessage": { "id": id, "getBlobResult": result } })
}

/// Response field paths for interaction queries, ending in the field that carries the reason.
fn query_refusal_path(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "webSearchRequestQuery" => &["webSearchRequestResponse", "rejected", "reason"],
        "webFetchRequestQuery" => &["webFetchRequestResponse", "rejected", "reason"],
        "askQuestionInteractionQuery" => &[
            "askQuestionInteractionResponse",
            "result",
            "rejected",
            "reason",
        ],
        "switchModeRequestQuery" => &["switchModeRequestResponse", "rejected", "reason"],
        "createPlanRequestQuery" => &["createPlanRequestResponse", "result", "error", "error"],
        "prManagementRequestQuery" => &["prManagementResult", "rejected", "reason"],
        "mcpAuthRequestQuery" => &["mcpAuthRequestResponse", "rejected", "reason"],
        "generateImageRequestQuery" => &["generateImageRequestResponse", "rejected", "reason"],
        "replaceEnvArgs" => &["replaceEnvResult", "failure", "errorMessage"],
        "connectScmRequestQuery" => &["connectScmRequestResponse", "rejected", "reason"],
        _ => return None,
    })
}

/// Declines an interaction Cursor asks the client to perform; `None` when there is no
/// rejection shape for it and the Run cannot continue.
pub(crate) fn query_refusal(id: u64, kind: &str) -> Option<Value> {
    let path = query_refusal_path(kind)?;
    let mut result = Value::String(
        "This capability is not available in Zed. Use the provided MCP tools.".to_string(),
    );
    for field in path.iter().rev() {
        result = json!({ *field: result });
    }
    let Value::Object(mut response) = result else {
        return None;
    };
    response.insert("id".into(), id.into());
    Some(json!({ "interactionResponse": response }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use language_model::{
        LanguageModelImage, LanguageModelRequestMessage, LanguageModelToolUse,
        LanguageModelToolUseInput,
    };
    use std::sync::Arc;

    #[test]
    fn decodes_frames_across_chunks() -> Result<()> {
        let first = encode_frame(&json!({ "a": 1 }))?;
        let mut end = vec![END_STREAM_FLAG];
        let end_body = br#"{"error":{"code":"unauthenticated","message":"expired"}}"#;
        end.extend_from_slice(&(end_body.len() as u32).to_be_bytes());
        end.extend_from_slice(end_body);

        let mut decoder = FrameDecoder::default();
        decoder.push(&first[..3]);
        assert_eq!(decoder.next_frame()?, None);
        decoder.push(&first[3..]);
        decoder.push(&end);
        assert_eq!(
            decoder.next_frame()?,
            Some(Frame::Message(json!({ "a": 1 })))
        );
        assert!(
            matches!(decoder.next_frame()?, Some(Frame::End(value)) if value["error"]["code"] == "unauthenticated")
        );
        assert_eq!(decoder.next_frame()?, None);
        Ok(())
    }

    #[test]
    fn sanitizes_schemas() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": ["string", "null"],
            "properties": { "path": { "type": "string", "description": "File", "examples": ["a"] } },
            "required": ["path", 3],
            "definitions": {}
        });
        assert_eq!(
            sanitize_schema(&schema),
            json!({
                "type": "string",
                "required": ["path"],
                "properties": { "path": { "type": "string", "description": "File" } }
            })
        );
        assert_eq!(sanitize_schema(&json!({})), json!({ "type": "object" }));
    }

    #[test]
    fn flattens_transcript() {
        let request = LanguageModelRequest {
            messages: vec![
                LanguageModelRequestMessage {
                    role: Role::System,
                    content: vec![MessageContent::Text("You are helpful.".into())],
                    cache: false,
                    reasoning_details: None,
                },
                LanguageModelRequestMessage {
                    role: Role::User,
                    content: vec![
                        MessageContent::Text("Read main.rs".into()),
                        MessageContent::Image(LanguageModelImage {
                            source: "aW1n".into(),
                        }),
                    ],
                    cache: false,
                    reasoning_details: None,
                },
                LanguageModelRequestMessage {
                    role: Role::Assistant,
                    content: vec![
                        MessageContent::Thinking {
                            text: "hidden".into(),
                            signature: None,
                        },
                        MessageContent::ToolUse(LanguageModelToolUse {
                            id: "call_1".into(),
                            name: "read_file".into(),
                            raw_input: r#"{"path":"main.rs"}"#.into(),
                            input: LanguageModelToolUseInput::Json(json!({ "path": "main.rs" })),
                            is_input_complete: true,
                            thought_signature: None,
                        }),
                    ],
                    cache: false,
                    reasoning_details: None,
                },
                LanguageModelRequestMessage {
                    role: Role::User,
                    content: vec![MessageContent::ToolResult(LanguageModelToolResult {
                        tool_use_id: "call_1".into(),
                        tool_name: Arc::from("read_file"),
                        is_error: false,
                        content: vec![LanguageModelToolResultContent::Text("fn main() {}".into())],
                        output: None,
                    })],
                    cache: false,
                    reasoning_details: None,
                },
            ],
            ..Default::default()
        };
        let transcript = transcript(&request);
        assert_eq!(
            transcript.prompt,
            "You are helpful.\n\nRead main.rs\n\nPrevious assistant tool call read_file (call_1):\n{\"path\":\"main.rs\"}\n\nResult of read_file (call_1):\nfn main() {}\n\nContinue from the tool results above. Do not repeat them."
        );
        assert_eq!(transcript.images, ["aW1n"]);
    }

    #[test]
    fn parses_server_messages() {
        assert!(matches!(
            parse_server_message(&json!({ "interactionUpdate": { "textDelta": { "text": "hi" } } })),
            ServerMessage::TextDelta(text) if text == "hi"
        ));
        assert!(matches!(
            parse_server_message(
                &json!({ "interactionUpdate": { "thinkingDelta": { "text": "\u{200b}hmm" } } })
            ),
            ServerMessage::ThinkingDelta(text) if text == "hmm"
        ));
        assert!(matches!(
            parse_server_message(&json!({ "interactionUpdate": { "turnEnded": {
                "inputTokens": "120", "outputTokens": 7, "usage": { "cacheReadTokens": 100 }
            } } })),
            ServerMessage::TurnEnded(TokenUsage {
                input_tokens: 120,
                output_tokens: 7,
                cache_read_input_tokens: 100,
                cache_creation_input_tokens: 0,
            })
        ));

        let ServerMessage::Exec {
            id,
            exec_id,
            request,
        } = parse_server_message(&json!({
            "execServerMessage": {
                "execId": "e1",
                "mcpArgs": {
                    "name": "zed-read_file",
                    "toolName": "read_file",
                    "toolCallId": "tool_9",
                    "args": { "path": { "stringValue": "main.rs" }, "range": "[1,2]" }
                }
            }
        }))
        else {
            panic!("expected an exec message");
        };
        assert_eq!((id, exec_id.as_str()), (0, "e1"));
        assert_eq!(
            request,
            ExecRequest::ToolCall(ToolCall {
                id: "tool_9".into(),
                name: "read_file".into(),
                input: json!({ "path": "main.rs", "range": [1, 2] }),
            })
        );

        let ServerMessage::Exec {
            request: ExecRequest::ToolCall(call),
            ..
        } = parse_server_message(
            &json!({ "execServerMessage": { "id": 3, "mcpArgs": { "name": "zed-grep" } } }),
        )
        else {
            panic!("expected a tool call");
        };
        assert_eq!(call.name, "grep");
        assert_eq!(call.input, json!({}));
        assert!(call.id.starts_with("call_"));

        assert!(matches!(
            parse_server_message(
                &json!({ "execServerMessage": { "id": 2, "readArgs": { "path": "a" } } })
            ),
            ServerMessage::Exec { id: 2, request: ExecRequest::Unsupported(kind), .. } if kind == "readArgs"
        ));
        assert!(matches!(
            parse_server_message(&json!({ "kvServerMessage": {
                "id": 4, "setBlobArgs": { "blobId": "b", "blobData": "AA==" }
            } })),
            ServerMessage::SetBlob { id: 4, blob_id, data } if blob_id == "b" && data == "AA=="
        ));
        assert!(matches!(
            parse_server_message(
                &json!({ "interactionQuery": { "id": 5, "webSearchRequestQuery": {} } })
            ),
            ServerMessage::Query { id: 5, kind } if kind == "webSearchRequestQuery"
        ));
        assert!(matches!(
            parse_server_message(
                &json!({ "error": { "code": "resource_exhausted", "message": "slow down" } })
            ),
            ServerMessage::Error(LanguageModelCompletionError::ProviderRejection {
                category: ProviderErrorCategory::RateLimit,
                ..
            })
        ));
    }

    #[test]
    fn builds_query_refusals() {
        assert_eq!(
            query_refusal(5, "webSearchRequestQuery"),
            Some(json!({ "interactionResponse": {
                "id": 5,
                "webSearchRequestResponse": { "rejected": { "reason":
                    "This capability is not available in Zed. Use the provided MCP tools." } }
            } }))
        );
        assert_eq!(query_refusal(5, "setupVmEnvironmentArgs"), None);
    }

    #[test]
    fn maps_connect_errors() {
        let error = connect_error(
            &json!({ "code": "unauthenticated", "message": "expired" }),
            None,
        );
        assert!(matches!(
            error,
            LanguageModelCompletionError::ProviderRejection {
                category: ProviderErrorCategory::Authentication,
                ..
            }
        ));
        assert_eq!(error.to_string(), "expired");
    }
}
