use crate::{PluginError, PluginId};
use std::{error::Error as StdError, fmt};

/// The first cleanup that failed. Earlier successful cleanups are not repeated.
#[derive(Debug)]
pub struct CleanupFailure {
    pub plugin: PluginId,
    pub effect: String,
    pub source: PluginError,
}

impl fmt::Display for CleanupFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plugin {} cleanup {} failed: {}",
            self.plugin, self.effect, self.source
        )
    }
}

impl StdError for CleanupFailure {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Lifecycle and service-resolution failures that callers can distinguish.
#[derive(Debug)]
pub enum Error {
    DuplicatePlugin(PluginId),
    UnknownPlugin(PluginId),
    AlreadyActive(PluginId),
    CleanupPending(PluginId),
    MissingService(&'static str),
    DuplicateService(&'static str),
    MissingExecutor,
    /// Indicates an internal mismatch in erased event dispatch.
    EventTypeMismatch(&'static str),
    Event {
        plugin: PluginId,
        event: &'static str,
        source: PluginError,
    },
    Activation {
        plugin: PluginId,
        source: PluginError,
        rollback: Option<CleanupFailure>,
    },
    Cleanup(CleanupFailure),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePlugin(plugin) => {
                write!(formatter, "plugin {plugin} is already registered")
            }
            Self::UnknownPlugin(plugin) => write!(formatter, "plugin {plugin} is not registered"),
            Self::AlreadyActive(plugin) => write!(formatter, "plugin {plugin} is already active"),
            Self::CleanupPending(plugin) => write!(
                formatter,
                "plugin {plugin} must finish cleanup before activation"
            ),
            Self::MissingService(service) => {
                write!(formatter, "service {service} is not available")
            }
            Self::DuplicateService(service) => {
                write!(formatter, "service {service} already has a provider")
            }
            Self::MissingExecutor => write!(
                formatter,
                "task spawning requires an explicit Tokio executor"
            ),
            Self::EventTypeMismatch(event) => {
                write!(formatter, "event dispatch type mismatch for {event}")
            }
            Self::Event {
                plugin,
                event,
                source,
            } => write!(
                formatter,
                "plugin {plugin} listener for {event} failed: {source}"
            ),
            Self::Activation {
                plugin,
                source,
                rollback,
            } => {
                write!(formatter, "plugin {plugin} activation failed: {source}")?;
                if let Some(failure) = rollback {
                    write!(formatter, "; rollback failed: {failure}")?;
                }
                Ok(())
            }
            Self::Cleanup(failure) => failure.fmt(formatter),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Activation { source, .. } | Self::Event { source, .. } => Some(source.as_ref()),
            Self::Cleanup(failure) => Some(failure),
            _ => None,
        }
    }
}
