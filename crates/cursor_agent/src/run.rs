//! Drives Cursor's bidirectional `Run` stream and keeps it open across Zed's tool rounds.
//!
//! Zed asks for a new completion after every batch of tool calls, but a Cursor Run expects the
//! tool results on the stream that requested them. A Run therefore parks after emitting its tool
//! calls and is resumed by the next request that carries their results.

use std::mem;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use bytes::Bytes;
use collections::HashMap;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::{AsyncReadExt as _, FutureExt as _, StreamExt as _, TryStreamExt as _};
use gpui::BackgroundExecutor;
use http_client::{AsyncBody, HttpClient, Method, Request as HttpRequest, Response, StatusCode};
use language_model::{
    CompletionIntent, LanguageModelCompletionError, LanguageModelCompletionEvent,
    LanguageModelCompletionStream, LanguageModelRequest, LanguageModelToolResult,
    LanguageModelToolUse, LanguageModelToolUseInput, MessageContent, ProviderErrorCategory, Role,
    StopReason,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::protocol::{self, ExecRequest, Frame, FrameDecoder, ServerMessage, ToolCall};
use crate::{CURSOR_API_URL, Credentials, CursorModel, PROVIDER_NAME, authenticated_request};

/// Cursor announces parallel tool calls one by one; waiting this long after the last one keeps
/// a batch together. Shorter windows split batches in practice.
const TOOL_BATCH_QUIET: Duration = Duration::from_millis(300);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Cursor sends heartbeats while a model thinks, so this much silence means the Run is gone.
const STALL_TIMEOUT: Duration = Duration::from_secs(90);
/// A parked Run whose tool results never arrive (the user stopped the thread) is closed.
const PARKED_RUN_TTL: Duration = Duration::from_secs(5 * 60);
/// How quickly a dropped consumer (the user stopped generation) closes the Run.
const CANCEL_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const READ_CHUNK_BYTES: usize = 64 * 1024;

type CompletionEvent = Result<LanguageModelCompletionEvent, LanguageModelCompletionError>;

enum RunCommand {
    Resume {
        results: Vec<LanguageModelToolResult>,
        events: UnboundedSender<CompletionEvent>,
    },
    Close,
}

#[derive(Clone)]
struct HeldRun {
    run_id: Uuid,
    thread_id: Option<String>,
    commands: UnboundedSender<RunCommand>,
}

/// Parked Runs, keyed by the tool use ids whose results resume them.
#[derive(Default)]
pub struct RunRegistry {
    held: Mutex<HashMap<String, HeldRun>>,
}

impl RunRegistry {
    fn hold(&self, tool_use_ids: impl IntoIterator<Item = String>, run: &HeldRun) {
        let mut held = self.held.lock();
        for tool_use_id in tool_use_ids {
            held.insert(tool_use_id, run.clone());
        }
    }

    fn take(&self, tool_use_id: &str) -> Option<HeldRun> {
        let mut held = self.held.lock();
        let run = held.get(tool_use_id)?.clone();
        held.retain(|_, candidate| candidate.run_id != run.run_id);
        Some(run)
    }

    fn release(&self, run_id: Uuid) {
        self.held
            .lock()
            .retain(|_, candidate| candidate.run_id != run_id);
    }

    /// A new user turn supersedes Runs still waiting on the same thread's tool results.
    fn close_thread(&self, thread_id: &str) {
        let mut closed = Vec::new();
        self.held.lock().retain(|_, candidate| {
            let superseded = candidate.thread_id.as_deref() == Some(thread_id);
            if superseded
                && !closed
                    .iter()
                    .any(|run: &HeldRun| run.run_id == candidate.run_id)
            {
                closed.push(candidate.clone());
            }
            !superseded
        });
        for run in closed {
            if run.commands.unbounded_send(RunCommand::Close).is_err() {
                log::debug!("Cursor run {} already finished", run.run_id);
            }
        }
    }

    #[cfg(test)]
    fn held_count(&self) -> usize {
        self.held.lock().len()
    }
}

fn trailing_tool_results(request: &LanguageModelRequest) -> Vec<LanguageModelToolResult> {
    let Some(message) = request.messages.last() else {
        return Vec::new();
    };
    if message.role != Role::User {
        return Vec::new();
    }
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::ToolResult(result) => Some(result.clone()),
            _ => None,
        })
        .collect()
}

/// Side requests such as titles and summaries share the thread id but must not end its Run.
fn is_conversation_turn(request: &LanguageModelRequest) -> bool {
    !matches!(
        request.intent,
        Some(
            CompletionIntent::ThreadSummarization
                | CompletionIntent::ThreadContextSummarization
                | CompletionIntent::GenerateGitCommitMessage
        )
    )
}

pub fn stream_completion(
    registry: &Arc<RunRegistry>,
    http_client: Arc<dyn HttpClient>,
    credentials: Credentials,
    model: &CursorModel,
    request: LanguageModelRequest,
    executor: BackgroundExecutor,
) -> LanguageModelCompletionStream {
    let (events_tx, events_rx) = mpsc::unbounded();

    let results = trailing_tool_results(&request);
    if let Some(first) = results.first()
        && let Some(run) = registry.take(&first.tool_use_id.to_string())
    {
        let resume = RunCommand::Resume {
            results,
            events: events_tx.clone(),
        };
        if run.commands.unbounded_send(resume).is_ok() {
            return events_rx.boxed();
        }
        log::debug!("Cursor run ended while parked; starting a new run from the transcript");
    }

    if is_conversation_turn(&request)
        && let Some(thread_id) = &request.thread_id
    {
        registry.close_thread(thread_id);
    }

    let effort = if request.thinking_allowed {
        request.thinking_effort.as_deref()
    } else {
        None
    };
    let tools = protocol::tool_definitions(&request.tools);
    let first_frame = protocol::run_request(
        protocol::transcript(&request),
        &tools,
        model.wire_model_id(effort),
        &Uuid::new_v4().to_string(),
    );
    let (commands_tx, commands_rx) = mpsc::unbounded();
    let held = HeldRun {
        run_id: Uuid::new_v4(),
        thread_id: request.thread_id.clone(),
        commands: commands_tx,
    };
    let run = Run::new(registry.clone(), held, executor.clone(), tools, events_tx);
    executor
        .spawn(run.drive(first_frame, http_client, credentials, commands_rx))
        .detach();
    events_rx.boxed()
}

struct PendingTool {
    tool_use_id: String,
    id: u64,
    exec_id: String,
}

enum Step {
    Continue,
    Done,
}

struct Run {
    registry: Arc<RunRegistry>,
    held: HeldRun,
    executor: BackgroundExecutor,
    tools: Vec<Value>,
    body: Option<UnboundedSender<std::io::Result<Bytes>>>,
    /// The consumer of the current round; `None` while parked on tool results.
    events: Option<UnboundedSender<CompletionEvent>>,
    /// Output produced while parked, delivered when the Run resumes.
    deferred: Vec<CompletionEvent>,
    pending: Vec<PendingTool>,
    /// Tool calls emitted in this round that have not yet ended it.
    unflushed_tools: usize,
    last_tool_at: Option<Instant>,
    last_frame_at: Instant,
    parked_at: Option<Instant>,
    next_heartbeat: Instant,
    blobs: HashMap<String, String>,
    turn_ended: bool,
}

impl Run {
    fn new(
        registry: Arc<RunRegistry>,
        held: HeldRun,
        executor: BackgroundExecutor,
        tools: Vec<Value>,
        events: UnboundedSender<CompletionEvent>,
    ) -> Self {
        let now = Instant::now();
        Self {
            registry,
            held,
            executor,
            tools,
            body: None,
            events: Some(events),
            deferred: Vec::new(),
            pending: Vec::new(),
            unflushed_tools: 0,
            last_tool_at: None,
            last_frame_at: now,
            parked_at: None,
            next_heartbeat: now + HEARTBEAT_INTERVAL,
            blobs: HashMap::default(),
            turn_ended: false,
        }
    }

    async fn drive(
        mut self,
        first_frame: Value,
        http_client: Arc<dyn HttpClient>,
        credentials: Credentials,
        commands: UnboundedReceiver<RunCommand>,
    ) {
        let result = self
            .run(first_frame, http_client, credentials, commands)
            .await;
        if let Err(error) = result {
            self.emit(Err(error));
        }
        self.close_body();
        self.registry.release(self.held.run_id);
    }

    async fn run(
        &mut self,
        first_frame: Value,
        http_client: Arc<dyn HttpClient>,
        credentials: Credentials,
        mut commands: UnboundedReceiver<RunCommand>,
    ) -> Result<(), LanguageModelCompletionError> {
        let response = self
            .connect(&first_frame, http_client.as_ref(), &credentials)
            .await?;
        let mut frames = self.read_frames(response);

        loop {
            if self.consumer_gone() {
                return Ok(());
            }
            let now = Instant::now();
            if let Some(parked_at) = self.parked_at
                && now.duration_since(parked_at) >= PARKED_RUN_TTL
            {
                return Ok(());
            }
            if self.parked_at.is_none() && now.duration_since(self.last_frame_at) >= STALL_TIMEOUT {
                return Err(LanguageModelCompletionError::from_provider_response(
                    PROVIDER_NAME,
                    None,
                    None,
                    "Cursor stopped responding".to_string(),
                    None,
                    ProviderErrorCategory::Timeout,
                ));
            }
            if now >= self.next_heartbeat {
                self.write(&protocol::heartbeat())?;
                self.next_heartbeat = now + HEARTBEAT_INTERVAL;
            }
            if self.round_complete(now) {
                self.park();
            }

            let timer = self.executor.timer(self.next_wake(now));
            futures::select_biased! {
                frame = frames.next() => match frame {
                    Some(Ok(Frame::Message(message))) => {
                        self.last_frame_at = Instant::now();
                        if let Step::Done = self.handle(&message)? {
                            return Ok(());
                        }
                    }
                    Some(Ok(Frame::End(end))) => return self.finish(&end),
                    Some(Err(error)) => {
                        return Err(LanguageModelCompletionError::Other(
                            error.context("Failed to read the Cursor response"),
                        ));
                    }
                    None => {
                        return if self.turn_ended {
                            Ok(())
                        } else {
                            Err(LanguageModelCompletionError::StreamEndedUnexpectedly {
                                provider: PROVIDER_NAME,
                            })
                        };
                    }
                },
                command = commands.next() => match command {
                    Some(RunCommand::Resume { results, events }) => {
                        if let Step::Done = self.resume(results, events)? {
                            return Ok(());
                        }
                    }
                    Some(RunCommand::Close) | None => return Ok(()),
                },
                () = timer.fuse() => {}
            }
        }
    }

    /// Opens the Run, retrying once when the connection fails before Cursor answered: nothing
    /// has reached the model yet, so the retry cannot duplicate work.
    async fn connect(
        &mut self,
        first_frame: &Value,
        http_client: &dyn HttpClient,
        credentials: &Credentials,
    ) -> Result<Response<AsyncBody>, LanguageModelCompletionError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let (body_tx, body_rx) = mpsc::unbounded::<std::io::Result<Bytes>>();
            self.body = Some(body_tx);
            self.write(first_frame)?;

            let builder = HttpRequest::builder()
                .method(Method::POST)
                .uri(format!("{CURSOR_API_URL}/agent.v1.AgentService/Run"))
                .header("content-type", "application/connect+json")
                .header("connect-protocol-version", "1")
                .header("connect-accept-encoding", "identity")
                .header("user-agent", "connect-es/1.6.1")
                .header("x-cursor-client-type", "ide")
                .header("x-cursor-client-layout", "unifiedAgent")
                .header("x-cursor-streaming", "true");
            let request = authenticated_request(builder, credentials)
                .body(AsyncBody::from_reader(body_rx.into_async_read()))
                .map_err(|error| LanguageModelCompletionError::BuildRequestBody {
                    provider: PROVIDER_NAME,
                    error,
                })?;

            let error = match http_client.send(request).await {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => response_error(response).await,
                Err(error) => LanguageModelCompletionError::HttpSend {
                    provider: PROVIDER_NAME,
                    host: "api2.cursor.sh".to_string(),
                    error,
                },
            };
            if attempt > 1 || !is_retryable(&error) {
                return Err(error);
            }
            log::warn!("Retrying Cursor run after: {error}");
            self.close_body();
            self.executor.timer(RETRY_DELAY).await;
        }
    }

    fn read_frames(&self, response: Response<AsyncBody>) -> UnboundedReceiver<Result<Frame>> {
        let (frames_tx, frames_rx) = mpsc::unbounded();
        let mut body = response.into_body();
        self.executor
            .spawn(async move {
                let mut decoder = FrameDecoder::default();
                let mut chunk = vec![0u8; READ_CHUNK_BYTES];
                loop {
                    let read = match body.read(&mut chunk).await {
                        Ok(0) => return,
                        Ok(read) => read,
                        Err(error) => {
                            if frames_tx.unbounded_send(Err(error.into())).is_err() {
                                log::debug!("Cursor run closed before a read error was delivered");
                            }
                            return;
                        }
                    };
                    decoder.push(&chunk[..read]);
                    loop {
                        let frame = match decoder.next_frame() {
                            Ok(Some(frame)) => Ok(frame),
                            Ok(None) => break,
                            Err(error) => Err(error),
                        };
                        let is_error = frame.is_err();
                        if frames_tx.unbounded_send(frame).is_err() || is_error {
                            return;
                        }
                    }
                }
            })
            .detach();
        frames_rx
    }

    fn handle(&mut self, message: &Value) -> Result<Step, LanguageModelCompletionError> {
        match protocol::parse_server_message(message) {
            ServerMessage::TextDelta(text) => {
                if !text.is_empty() {
                    self.emit(Ok(LanguageModelCompletionEvent::Text(text)));
                }
            }
            ServerMessage::ThinkingDelta(text) => {
                if !text.is_empty() {
                    self.emit(Ok(LanguageModelCompletionEvent::Thinking {
                        text,
                        signature: None,
                    }));
                }
            }
            ServerMessage::TurnEnded(usage) => {
                self.emit(Ok(LanguageModelCompletionEvent::UsageUpdate(usage)));
                self.turn_ended = true;
                if self.pending.is_empty() {
                    self.emit(Ok(LanguageModelCompletionEvent::Stop(StopReason::EndTurn)));
                    self.close_body();
                    return Ok(Step::Done);
                }
            }
            ServerMessage::Exec {
                id,
                exec_id,
                request,
            } => match request {
                ExecRequest::RequestContext => {
                    let reply = protocol::request_context_result(id, &exec_id, &self.tools);
                    self.write(&reply)?;
                    self.write(&protocol::stream_close(id))?;
                }
                ExecRequest::McpState => {
                    let reply = protocol::mcp_state_result(id, &exec_id, &self.tools);
                    self.write(&reply)?;
                    self.write(&protocol::stream_close(id))?;
                }
                ExecRequest::Precheck(field) => {
                    let reply = protocol::exec_result(
                        id,
                        &exec_id,
                        &field,
                        json!({ "allowlisted": false }),
                    );
                    self.write(&reply)?;
                    self.write(&protocol::stream_close(id))?;
                }
                ExecRequest::ToolCall(call) => self.tool_call(id, exec_id, call),
                ExecRequest::Unsupported(kind) => {
                    self.write(&protocol::unsupported_exec(id, &kind))?;
                }
            },
            ServerMessage::SetBlob { id, blob_id, data } => {
                self.blobs.insert(blob_id, data);
                self.write(&protocol::set_blob_result(id))?;
            }
            ServerMessage::GetBlob { id, blob_id } => {
                let reply =
                    protocol::get_blob_result(id, self.blobs.get(&blob_id).map(String::as_str));
                self.write(&reply)?;
            }
            ServerMessage::Query { id, kind } => match protocol::query_refusal(id, &kind) {
                Some(reply) => self.write(&reply)?,
                None => {
                    return Err(LanguageModelCompletionError::Other(anyhow!(
                        "Cursor asked for {kind}, which Zed cannot provide"
                    )));
                }
            },
            ServerMessage::Error(error) => return Err(error),
            ServerMessage::Other => {}
        }
        Ok(Step::Continue)
    }

    fn tool_call(&mut self, id: u64, exec_id: String, call: ToolCall) {
        // Cursor can announce the same call more than once while its arguments settle.
        let duplicate = self.pending.iter().any(|pending| {
            pending.tool_use_id == call.id || (!exec_id.is_empty() && pending.exec_id == exec_id)
        });
        if duplicate {
            return;
        }
        self.emit(Ok(LanguageModelCompletionEvent::ToolUse(
            LanguageModelToolUse {
                id: call.id.clone().into(),
                name: call.name.into(),
                raw_input: call.input.to_string(),
                input: LanguageModelToolUseInput::Json(call.input),
                is_input_complete: true,
                thought_signature: None,
            },
        )));
        self.pending.push(PendingTool {
            tool_use_id: call.id,
            id,
            exec_id,
        });
        self.unflushed_tools += 1;
        self.last_tool_at = Some(Instant::now());
    }

    fn round_complete(&self, now: Instant) -> bool {
        self.events.is_some()
            && self.unflushed_tools > 0
            && self
                .last_tool_at
                .is_some_and(|last| now.duration_since(last) >= TOOL_BATCH_QUIET)
    }

    /// Ends Zed's round with the batched tool calls and waits for their results.
    fn park(&mut self) {
        self.emit(Ok(LanguageModelCompletionEvent::Stop(StopReason::ToolUse)));
        self.events = None;
        self.unflushed_tools = 0;
        self.parked_at = Some(Instant::now());
        let tool_use_ids = self
            .pending
            .iter()
            .map(|pending| pending.tool_use_id.clone());
        self.registry.hold(tool_use_ids, &self.held);
    }

    fn resume(
        &mut self,
        results: Vec<LanguageModelToolResult>,
        events: UnboundedSender<CompletionEvent>,
    ) -> Result<Step, LanguageModelCompletionError> {
        self.events = Some(events);
        self.parked_at = None;
        self.last_frame_at = Instant::now();
        for result in results {
            let tool_use_id = result.tool_use_id.to_string();
            let Some(index) = self
                .pending
                .iter()
                .position(|pending| pending.tool_use_id == tool_use_id)
            else {
                continue;
            };
            let pending = self.pending.remove(index);
            let text = protocol::tool_result_text(&result);
            self.write(&protocol::tool_result(
                pending.id,
                &pending.exec_id,
                &text,
                result.is_error,
            ))?;
            self.write(&protocol::stream_close(pending.id))?;
        }
        for event in mem::take(&mut self.deferred) {
            self.emit(event);
        }
        if self.turn_ended && self.pending.is_empty() {
            self.emit(Ok(LanguageModelCompletionEvent::Stop(StopReason::EndTurn)));
            self.close_body();
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }

    fn finish(&mut self, end: &Value) -> Result<(), LanguageModelCompletionError> {
        if let Some(error) = end.get("error") {
            return Err(protocol::connect_error(error, None));
        }
        if !self.turn_ended {
            self.emit(Ok(LanguageModelCompletionEvent::Stop(StopReason::EndTurn)));
        }
        Ok(())
    }

    fn next_wake(&self, now: Instant) -> Duration {
        let mut wake = self.next_heartbeat.saturating_duration_since(now);
        if self.events.is_some() {
            wake = wake.min(CANCEL_CHECK_INTERVAL);
            if self.unflushed_tools > 0
                && let Some(last) = self.last_tool_at
            {
                wake = wake.min((last + TOOL_BATCH_QUIET).saturating_duration_since(now));
            }
        }
        wake.max(Duration::from_millis(10))
    }

    fn consumer_gone(&self) -> bool {
        self.events
            .as_ref()
            .is_some_and(|events| events.is_closed())
    }

    fn emit(&mut self, event: CompletionEvent) {
        match &self.events {
            Some(events) => {
                if events.unbounded_send(event).is_err() {
                    log::debug!("Cursor completion was dropped by its consumer");
                }
            }
            None => self.deferred.push(event),
        }
    }

    fn write(&self, message: &Value) -> Result<(), LanguageModelCompletionError> {
        let frame = protocol::encode_frame(message)?;
        self.body
            .as_ref()
            .ok_or_else(|| anyhow!("Cursor request stream is closed"))?
            .unbounded_send(Ok(frame))
            .map_err(|_| anyhow!("Cursor request stream is closed"))?;
        Ok(())
    }

    fn close_body(&mut self) {
        if let Some(body) = self.body.take() {
            body.close_channel();
        }
    }
}

async fn response_error(mut response: Response<AsyncBody>) -> LanguageModelCompletionError {
    let status = response.status();
    let mut body = String::new();
    if let Err(error) = response.body_mut().read_to_string(&mut body).await {
        log::debug!("Failed to read Cursor error response: {error}");
    }
    match serde_json::from_str::<Value>(&body) {
        Ok(error) if error.get("code").is_some() || error.get("message").is_some() => {
            protocol::connect_error(&error, Some(status))
        }
        _ => {
            let message = if body.trim().is_empty() {
                format!("Cursor returned HTTP {status}")
            } else {
                body.trim().chars().take(500).collect()
            };
            LanguageModelCompletionError::from_provider_response(
                PROVIDER_NAME,
                Some(status),
                None,
                message.clone(),
                None,
                ProviderErrorCategory::from_http_status(status, &message),
            )
        }
    }
}

fn is_retryable(error: &LanguageModelCompletionError) -> bool {
    match error {
        LanguageModelCompletionError::HttpSend { .. } => true,
        LanguageModelCompletionError::ProviderRejection { status, .. } => matches!(
            *status,
            Some(
                StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            )
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use language_model::{LanguageModelRequestMessage, LanguageModelToolResultContent};

    fn tool_result_request(tool_use_id: &str, thread_id: &str) -> LanguageModelRequest {
        LanguageModelRequest {
            thread_id: Some(thread_id.into()),
            messages: vec![LanguageModelRequestMessage {
                role: Role::User,
                content: vec![MessageContent::ToolResult(LanguageModelToolResult {
                    tool_use_id: tool_use_id.into(),
                    tool_name: Arc::from("read_file"),
                    is_error: false,
                    content: vec![LanguageModelToolResultContent::Text("done".into())],
                    output: None,
                })],
                cache: false,
                reasoning_details: None,
            }],
            ..Default::default()
        }
    }

    fn held_run(thread_id: &str) -> (HeldRun, UnboundedReceiver<RunCommand>) {
        let (commands, receiver) = mpsc::unbounded();
        (
            HeldRun {
                run_id: Uuid::new_v4(),
                thread_id: Some(thread_id.into()),
                commands,
            },
            receiver,
        )
    }

    #[test]
    fn registry_takes_and_closes_runs() {
        let registry = RunRegistry::default();
        let (first, _first_commands) = held_run("thread-a");
        let (second, mut second_commands) = held_run("thread-b");
        registry.hold(["call_1".to_string(), "call_2".to_string()], &first);
        registry.hold(["call_3".to_string()], &second);
        assert_eq!(registry.held_count(), 3);

        let taken = registry.take("call_2").map(|run| run.run_id);
        assert_eq!(taken, Some(first.run_id));
        assert_eq!(registry.held_count(), 1);
        assert!(registry.take("call_1").is_none());

        registry.close_thread("thread-b");
        assert_eq!(registry.held_count(), 0);
        assert!(matches!(second_commands.try_recv(), Ok(RunCommand::Close)));
    }

    #[test]
    fn side_requests_do_not_end_runs() {
        let mut request = tool_result_request("call_1", "thread");
        assert!(is_conversation_turn(&request));
        request.intent = Some(CompletionIntent::ThreadSummarization);
        assert!(!is_conversation_turn(&request));
        assert_eq!(trailing_tool_results(&request).len(), 1);
    }

    #[gpui::test]
    async fn resumes_parked_run_with_tool_results(cx: &mut TestAppContext) {
        let registry = Arc::new(RunRegistry::default());
        let (held, commands) = held_run("thread");
        let (body_tx, mut body_rx) = mpsc::unbounded();
        let (events_tx, mut events_rx) = mpsc::unbounded();
        let mut run = Run::new(registry.clone(), held, cx.executor(), Vec::new(), events_tx);
        run.body = Some(body_tx);

        run.tool_call(
            7,
            "exec-7".into(),
            ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                input: json!({ "path": "a" }),
            },
        );
        assert!(!run.round_complete(Instant::now()));
        assert!(run.round_complete(Instant::now() + TOOL_BATCH_QUIET));
        run.park();
        assert!(matches!(
            events_rx.next().await,
            Some(Ok(LanguageModelCompletionEvent::ToolUse(tool_use))) if tool_use.id.to_string() == "call_1"
        ));
        assert!(matches!(
            events_rx.next().await,
            Some(Ok(LanguageModelCompletionEvent::Stop(StopReason::ToolUse)))
        ));
        assert!(events_rx.next().await.is_none());

        // Output that arrives while parked waits for the next round.
        assert!(matches!(
            run.handle(&json!({ "interactionUpdate": { "textDelta": { "text": "late" } } })),
            Ok(Step::Continue)
        ));
        assert_eq!(run.deferred.len(), 1);

        let stream = stream_completion(
            &registry,
            http_client::FakeHttpClient::with_404_response(),
            crate::tests::test_credentials(),
            &CursorModel {
                id: "default".into(),
                display_name: "Auto".into(),
                variants: Vec::new(),
                context_window: 200_000,
                max_output_tokens: 8_000,
            },
            tool_result_request("call_1", "thread"),
            cx.executor(),
        );
        let mut commands = commands;
        let Some(RunCommand::Resume { results, events }) = commands.next().await else {
            panic!("expected the parked run to resume");
        };
        assert!(matches!(run.resume(results, events), Ok(Step::Continue)));
        assert!(run.pending.is_empty());

        let written: Vec<Value> = std::iter::from_fn(|| body_rx.try_recv().ok())
            .filter_map(Result::ok)
            .filter_map(|frame| serde_json::from_slice(&frame[5..]).ok())
            .collect();
        assert_eq!(
            written,
            [
                protocol::tool_result(7, "exec-7", "done", false),
                protocol::stream_close(7)
            ]
        );

        assert!(matches!(
            run.handle(&json!({ "interactionUpdate": { "turnEnded": { "outputTokens": 3 } } })),
            Ok(Step::Done)
        ));
        drop(run);
        let events: Vec<_> = stream.collect().await;
        assert!(matches!(
            events.as_slice(),
            [
                Ok(LanguageModelCompletionEvent::Text(text)),
                Ok(LanguageModelCompletionEvent::UsageUpdate(_)),
                Ok(LanguageModelCompletionEvent::Stop(StopReason::EndTurn)),
            ] if text == "late"
        ));
    }
}
