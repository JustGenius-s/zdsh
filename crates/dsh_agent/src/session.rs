use crate::{AgentError, AssistantContent, CallId, Message, ModelChunk, ModelRequest, ToolResult};
use serde::Serialize;
use tokio::sync::{Mutex, watch};

/// Monotonic identity within one in-memory session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TurnId(pub u64);

/// Append-only facts for the experimental Rust session format.
/// Partial model chunks are retained for inspection but excluded from history
/// unless an `Assistant` settlement is committed for that step.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    Started {
        system_prompt: String,
    },
    TurnStarted {
        turn: TurnId,
        input: String,
    },
    StepStarted {
        turn: TurnId,
        step: usize,
    },
    Request {
        turn: TurnId,
        step: usize,
        request: ModelRequest,
    },
    ModelOutput {
        turn: TurnId,
        step: usize,
        chunk: ModelChunk,
    },
    Assistant {
        turn: TurnId,
        step: usize,
        content: Vec<AssistantContent>,
    },
    ToolFinished {
        turn: TurnId,
        step: usize,
        call_id: CallId,
        executed: bool,
        result: ToolResult,
    },
    StepFinished {
        turn: TurnId,
        step: usize,
        result: Result<(), AgentError>,
    },
    TurnFinished {
        turn: TurnId,
        result: Result<String, AgentError>,
    },
}

/// In-memory session with a single writer owned by the agent worker.
/// Snapshots may be serialized for inspection; no disk recovery or compatibility
/// with TypeScript DSH's persisted session generations is implied.
pub struct Session {
    events: Mutex<Vec<SessionEvent>>,
    changes: watch::Sender<usize>,
}

impl Session {
    pub(crate) fn new(system_prompt: String) -> Self {
        Self {
            events: Mutex::new(vec![SessionEvent::Started { system_prompt }]),
            changes: watch::channel(1).0,
        }
    }

    pub(crate) async fn append(&self, event: SessionEvent) {
        let mut events = self.events.lock().await;
        events.push(event);
        self.changes.send_replace(events.len());
    }

    /// Subscribe before reading history. Notifications may coalesce; use the
    /// append cursor with `events_since` to retrieve every event without loss.
    pub fn subscribe(&self) -> watch::Receiver<usize> {
        self.changes.subscribe()
    }

    /// Reads events after an append cursor. The returned cursor is the new log
    /// length. A cursor beyond the log is rejected instead of silently skipping.
    pub async fn events_since(
        &self,
        cursor: usize,
    ) -> Result<(usize, Vec<SessionEvent>), crate::AgentError> {
        let events = self.events.lock().await;
        let tail = events.get(cursor..).ok_or_else(|| {
            crate::AgentError::Protocol("session cursor exceeds log length".into())
        })?;
        Ok((events.len(), tail.to_vec()))
    }

    /// Returns all committed events in append order, including partial streams.
    pub async fn events(&self) -> Vec<SessionEvent> {
        self.events.lock().await.clone()
    }

    /// Reconstructs model history from committed events, excluding incomplete responses.
    pub async fn messages(&self) -> Vec<Message> {
        self.events
            .lock()
            .await
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Started { system_prompt } if !system_prompt.is_empty() => {
                    Some(Message::System {
                        text: system_prompt.clone(),
                    })
                }
                SessionEvent::TurnStarted { input, .. } => Some(Message::User {
                    text: input.clone(),
                }),
                SessionEvent::Assistant { content, .. } => Some(Message::Assistant {
                    content: content.clone(),
                }),
                SessionEvent::ToolFinished {
                    call_id, result, ..
                } => Some(Message::Tool {
                    call_id: call_id.clone(),
                    result: result.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}
