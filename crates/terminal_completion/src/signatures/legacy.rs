use std::sync::{Arc, OnceLock};

mod miss_cache;
pub mod registry;

pub use registry::CommandRegistry;

static GLOBAL_REGISTRY: OnceLock<Arc<CommandRegistry>> = OnceLock::new();

impl CommandRegistry {
    /// Returns a reference to a single global instance of the command registry.
    ///
    /// The registry is a read-only store of information used to provide smart
    /// suggestions and completions, and as such, only one instance is required
    /// across the application.  The registry itself can be quite large, so use
    /// of a single global instance avoids unnecessary memory allocations and
    /// usage.
    pub fn global_instance() -> Arc<Self> {
        GLOBAL_REGISTRY
            .get_or_init(|| {
                // TODO(wasm): Determine how to asynchronously load command signatures on wasm.
                Arc::new(CommandRegistry::new_with_embedded_signatures())
            })
            .clone()
    }

    /// Returns a new [`CommandRegistry`] that looks up commands in the embedded
    /// set of command signatures.
    fn new_with_embedded_signatures() -> Self {
        let registry = CommandRegistry::new(
            |command| {
                let start = instant::Instant::now();
                let signature = crate::catalog::signature_by_name(command);
                log::debug!(
                    "Lazily loaded command signature for {command} in {}s",
                    start.elapsed().as_secs_f32()
                );
                signature
            },
            crate::catalog::dynamic_command_signature_data(),
        );

        registry
    }

    /// Returns an empty [`CommandRegistry`] that contains no signatures nor
    /// generators.
    pub fn empty() -> Self {
        CommandRegistry::new(|_| None, std::collections::HashMap::new())
    }
}
