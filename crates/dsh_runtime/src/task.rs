use crate::{PluginFuture, PluginResult};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

/// Cooperative stop signal for one plugin-owned task.
///
/// Once cancellation is requested it stays requested. Tasks must finish their
/// resource use before returning; the runtime does not impose a timeout.
#[derive(Clone)]
pub struct Cancellation {
    receiver: watch::Receiver<bool>,
}

impl Cancellation {
    /// Returns whether the owning plugin has requested shutdown.
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow() || self.receiver.has_changed().is_err()
    }

    /// Waits for cancellation. Closing the signal also requests cancellation.
    pub async fn cancelled(&mut self) {
        while !*self.receiver.borrow_and_update() {
            if self.receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Cloneable authority to request cooperative cancellation. Dropping all sources
/// also cancels their tokens. A fresh source is required for each new operation.
#[derive(Clone)]
pub struct CancellationSource {
    sender: watch::Sender<bool>,
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self {
            sender: watch::channel(false).0,
        }
    }
}

impl CancellationSource {
    /// Creates a token observing this source, including prior cancellation.
    pub fn token(&self) -> Cancellation {
        Cancellation {
            receiver: self.sender.subscribe(),
        }
    }

    /// Permanently requests cancellation for every token from this source.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

pub(crate) struct OwnedTask {
    pub label: String,
    cancel: CancellationSource,
    handle: Option<JoinHandle<PluginResult>>,
}

impl OwnedTask {
    pub fn spawn(
        executor: &Handle,
        label: String,
        make: impl FnOnce(Cancellation) -> PluginFuture,
    ) -> Self {
        let cancel = CancellationSource::default();
        let future = make(cancel.token());
        Self {
            label,
            cancel,
            handle: Some(executor.spawn(future)),
        }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn join(&mut self) -> PluginResult {
        let Some(handle) = self.handle.as_mut() else {
            return Ok(());
        };
        let result = handle.await;
        self.handle = None;
        result?
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.cancel();
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}
