//! Zed-owned DSH sessions, sharing the editor's model registry and Tokio executor.

mod agent_server;
mod controls;
mod extension_tools;
mod host;
mod model;

pub use agent_server::{DSH_AGENT_ID, DshAgentServer, session_controls};
pub use controls::SessionControls;
pub use host::{Session, init, shutdown, start_session};
pub use model::model_key;
