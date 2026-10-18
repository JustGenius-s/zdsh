//! Zed-owned DSH sessions, sharing the editor's model registry and Tokio executor.

mod agent_server;
mod extension_tools;
mod host;
mod model;

pub use agent_server::{DSH_AGENT_ID, DshAgentServer};
pub use host::{Session, init, shutdown, start_session};
pub use model::model_key;
