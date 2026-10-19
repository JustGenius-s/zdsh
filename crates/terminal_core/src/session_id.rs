use serde::{Deserialize, Serialize};

/// Unique identifier for a terminal session.
///
/// Each bootstrapped subshell (including SSH sessions) gets its own `SessionId`.
#[derive(Copy, Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SessionId(u64);

impl SessionId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl From<u64> for SessionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<SessionId> for u64 {
    fn from(session_id: SessionId) -> Self {
        session_id.as_u64()
    }
}

/// Generates a nonzero identifier for a bootstrapped shell session.
pub fn generate_session_id() -> SessionId {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    loop {
        let id = rng.r#gen::<u64>();
        if id != 0 {
            return SessionId::from(id);
        }
    }
}
