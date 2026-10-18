//! Plugin-provided agent loop with in-memory session logging and serial tool calls.

mod agent_loop;
mod protocol;
mod session;
mod tools;
pub use agent_loop::install;
pub use protocol::*;
pub use session::{Session, SessionEvent, TurnId};
pub use tools::{RegistrationError, ToolRegistry, ToolsService};

use dsh_runtime::{CancellationSource, Service};
use futures::future::BoxFuture;
use std::{collections::BTreeSet, num::NonZeroUsize, sync::Arc};
use tokio::sync::oneshot;

/// Explicit deployment choices for one agent instance. No default model or budget.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub model: String,
    pub model_options: ModelOptions,
    pub system_prompt: String,
    pub max_steps: NonZeroUsize,
    pub queue_capacity: NonZeroUsize,
}

/// Immutable settings for one queued turn. Tool names are an explicit allowlist.
#[derive(Clone, Debug)]
pub struct TurnOptions {
    pub model: String,
    pub model_options: ModelOptions,
    pub allowed_tools: BTreeSet<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("model name cannot be empty")]
pub struct EmptyModelName;

#[derive(Debug, thiserror::Error)]
#[error("queue capacity exceeds Tokio channel limit")]
pub struct QueueCapacityExceeded;

/// An accepted queued turn. Dropping it requests cancellation, including when
/// dropping the future returned by `wait`. Completion means the terminal log is committed.
pub struct Turn {
    cancellation: CancellationSource,
    result: oneshot::Receiver<Result<String, AgentError>>,
}

impl Turn {
    /// Requests cancellation without dropping the result receiver.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// A cloneable cancellation handle for hosts awaiting completion concurrently.
    pub fn cancellation_handle(&self) -> CancellationSource {
        self.cancellation.clone()
    }

    /// Waits for a final response or failure. Cancellation waits for active tools
    /// to finish cleanup; an uncooperative tool can prevent completion.
    pub async fn wait(mut self) -> Result<String, AgentError> {
        (&mut self.result).await.map_err(|_| AgentError::Stopped)?
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Replaceable agent service. A host submits work and inspects the session;
/// the loop implementation owns execution and shutdown.
pub trait Agent: Send + Sync {
    /// Enqueues input with backpressure. Cancelling this future before enqueue
    /// completes does not submit a turn. Turns execute serially.
    fn submit(&self, input: String) -> BoxFuture<'_, Result<Turn, AgentError>>;
    /// Enqueues a turn with settings captured at submission; later selections cannot alter it.
    fn submit_with_options(
        &self,
        input: String,
        options: TurnOptions,
    ) -> BoxFuture<'_, Result<Turn, AgentError>>;
    fn session(&self) -> Arc<Session>;
}

pub struct AgentService;
impl Service for AgentService {
    type Value = dyn Agent;
}
