//! Typed services and serial events with cancellation-safe asynchronous teardown.
//!
//! The host must call [`Runtime::shutdown`] before dropping the runtime. Callbacks
//! return errors instead of panicking. Lifecycle calls require exclusive runtime access.

mod context;
mod error;
mod runtime;
mod task;

pub use context::Context;
pub use error::{CleanupFailure, Error};
pub use runtime::Runtime;
pub use task::{Cancellation, CancellationSource};

use std::{error::Error as StdError, fmt, future::Future, pin::Pin};

/// A plugin's activation or cleanup failure, retaining its original error type.
pub type PluginError = Box<dyn StdError + Send + Sync>;
/// Result returned by activation and cleanup callbacks.
pub type PluginResult = Result<(), PluginError>;
/// Owned future used by asynchronous listeners and cleanup callbacks.
pub type PluginFuture = Pin<Box<dyn Future<Output = PluginResult> + Send + 'static>>;

/// A typed event payload. Each implementing Rust type identifies one event.
pub trait Event: Send + Sync + 'static {}

/// Identity of one registered plugin instance within a runtime.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    /// Creates an identity; registering the same identity twice is rejected.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A service key independent of its provider's concrete implementation.
///
/// `Value` may be a trait object, such as `dyn ModelProvider`. One provider per
/// key is permitted in a runtime. Different keys may share the same value type.
pub trait Service: 'static {
    type Value: ?Sized + Send + Sync + 'static;
}

/// State exposed to the host for activation and cleanup recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginState {
    Registered,
    Active,
    /// Teardown is in progress, interrupted or failed; resources are retained.
    CleanupPending,
}
