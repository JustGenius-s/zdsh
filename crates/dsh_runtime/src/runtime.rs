use crate::{CleanupFailure, Context, Error, Event, PluginId, PluginResult, PluginState};
use crate::{
    context::{Effect, Registration},
    task::OwnedTask,
};
use std::{
    any::{Any, TypeId, type_name},
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};
use tokio::runtime::Handle;

type Activate = Box<dyn FnMut(&mut Context<'_>) -> PluginResult + Send>;

struct Plugin {
    activate: Activate,
    state: PluginState,
    dependencies: BTreeSet<PluginId>,
    effects: Vec<Effect>,
    tasks: Vec<OwnedTask>,
}

/// Serial lifecycle owner. Service values may be thread-safe, but lifecycle
/// operations and events require exclusive access. Spawned tasks use a host-owned
/// Tokio executor; the runtime never creates or blocks an executor itself.
///
/// Call [`Self::shutdown`] explicitly: dropping this value releases Rust-owned
/// memory but does not run fallible cleanup callbacks. Activation
/// and event dispatch are blocked while any plugin has unfinished cleanup.
/// Dropping the runtime aborts remaining tasks but cannot await their exit;
/// explicit shutdown is required to preserve dependency lifetimes.
#[derive(Default)]
#[must_use = "call shutdown to complete plugin cleanup before dropping the runtime"]
pub struct Runtime {
    plugins: BTreeMap<PluginId, Plugin>,
    services: HashMap<TypeId, Registration>,
    activation_order: Vec<PluginId>,
    executor: Option<Handle>,
}

impl Runtime {
    /// Uses the host's executor for plugin-owned tasks. It must remain running
    /// until shutdown completes. `Default` supports events and cleanup but rejects spawning.
    pub fn with_executor(executor: Handle) -> Self {
        Self {
            executor: Some(executor),
            ..Self::default()
        }
    }

    /// Delivers an event to active listeners in activation and subscription order.
    /// Stops on the first error and identifies the failing plugin and event type.
    /// With no listeners this succeeds. Dropping the returned future cancels the
    /// current listener; already delivered notifications are not rolled back.
    pub async fn emit<E: Event>(&mut self, event: E) -> Result<(), Error> {
        self.ensure_ready()?;
        let event: Arc<dyn Any + Send + Sync> = Arc::new(event);
        for id in &self.activation_order {
            if let Some(plugin) = self.plugins.get_mut(id) {
                for effect in &mut plugin.effects {
                    if let Effect::Listener {
                        event: event_type,
                        run,
                    } = effect
                    {
                        if *event_type == TypeId::of::<E>() {
                            run(Arc::clone(&event))
                                .await
                                .map_err(|source| Error::Event {
                                    plugin: id.clone(),
                                    event: type_name::<E>(),
                                    source,
                                })?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn ensure_ready(&self) -> Result<(), Error> {
        if let Some((pending, _)) = self
            .plugins
            .iter()
            .find(|(_, plugin)| plugin.state == PluginState::CleanupPending)
        {
            return Err(Error::CleanupPending(pending.clone()));
        }
        Ok(())
    }

    /// Registers a reusable activation callback without running it.
    ///
    /// Activation-local resources belong in the context's services or cleanup
    /// closures, not in captures retained by this callback between activations.
    pub fn register(
        &mut self,
        id: PluginId,
        activate: impl FnMut(&mut Context<'_>) -> PluginResult + Send + 'static,
    ) -> Result<(), Error> {
        if self.plugins.contains_key(&id) {
            return Err(Error::DuplicatePlugin(id));
        }
        self.plugins.insert(
            id,
            Plugin {
                activate: Box::new(activate),
                state: PluginState::Registered,
                dependencies: BTreeSet::new(),
                effects: Vec::new(),
                tasks: Vec::new(),
            },
        );
        Ok(())
    }

    /// Returns the current state, or `None` for an unknown identity.
    pub fn state(&self, id: &PluginId) -> Option<PluginState> {
        self.plugins.get(id).map(|plugin| plugin.state)
    }

    /// Runs the synchronous activation callback, awaiting rollback on failure.
    ///
    /// Failed rollback retains the plugin in `CleanupPending`; call `unload`
    /// or `shutdown` to retry before activating any plugin again.
    pub async fn activate(&mut self, id: &PluginId) -> Result<(), Error> {
        self.ensure_ready()?;
        let plugin = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| Error::UnknownPlugin(id.clone()))?;
        if plugin.state == PluginState::Active {
            return Err(Error::AlreadyActive(id.clone()));
        }
        let result = (plugin.activate)(&mut Context {
            owner: id,
            services: &mut self.services,
            dependencies: &mut plugin.dependencies,
            effects: &mut plugin.effects,
            tasks: &mut plugin.tasks,
            executor: self.executor.as_ref(),
        });
        plugin.state = PluginState::Active;
        self.activation_order.push(id.clone());
        if let Err(source) = result {
            let rollback = self.cleanup(id).await.err();
            return Err(Error::Activation {
                plugin: id.clone(),
                source,
                rollback,
            });
        }
        Ok(())
    }

    /// Unloads all transitive consumers before the requested provider.
    ///
    /// On the first cleanup error, stops without releasing remaining effects or
    /// providers. Successfully unloaded plugins stay registered and may be
    /// explicitly activated again. Unloading a registered plugin is a no-op.
    /// Dropping this future preserves task joins and pending cleanup futures;
    /// call unload or shutdown again to finish teardown.
    pub async fn unload(&mut self, id: &PluginId) -> Result<(), Error> {
        if !self.plugins.contains_key(id) {
            return Err(Error::UnknownPlugin(id.clone()));
        }
        let mut selected = BTreeSet::from([id.clone()]);
        loop {
            let previous_length = selected.len();
            for (candidate, plugin) in &self.plugins {
                if plugin
                    .dependencies
                    .iter()
                    .any(|dependency| selected.contains(dependency))
                {
                    selected.insert(candidate.clone());
                }
            }
            if selected.len() == previous_length {
                break;
            }
        }
        let order: Vec<_> = self
            .activation_order
            .iter()
            .rev()
            .filter(|candidate| selected.contains(*candidate))
            .cloned()
            .collect();
        for candidate in order {
            self.cleanup(&candidate).await.map_err(Error::Cleanup)?;
        }
        Ok(())
    }

    /// Unloads every activation in reverse order, stopping on the first failure.
    /// Call again to retry failed cleanup; successful effects are not repeated.
    pub async fn shutdown(&mut self) -> Result<(), Error> {
        let order: Vec<_> = self.activation_order.iter().rev().cloned().collect();
        for id in order {
            self.cleanup(&id).await.map_err(Error::Cleanup)?;
        }
        Ok(())
    }

    async fn cleanup(&mut self, id: &PluginId) -> Result<(), CleanupFailure> {
        let Some(plugin) = self.plugins.get_mut(id) else {
            return Ok(());
        };
        plugin.state = PluginState::CleanupPending;
        for task in &plugin.tasks {
            task.cancel();
        }
        while let Some(task) = plugin.tasks.last_mut() {
            let result = task.join().await;
            let label = task.label.clone();
            plugin.tasks.pop();
            result.map_err(|source| CleanupFailure {
                plugin: id.clone(),
                effect: label,
                source,
            })?;
        }
        while let Some(effect) = plugin.effects.last_mut() {
            match effect {
                Effect::Listener { .. } => {}
                Effect::AsyncCleanup {
                    label,
                    make,
                    pending,
                } => {
                    let future = pending.get_or_insert_with(make);
                    let result = future.await;
                    *pending = None;
                    result.map_err(|source| CleanupFailure {
                        plugin: id.clone(),
                        effect: label.clone(),
                        source,
                    })?;
                }
                Effect::Service(key) => {
                    self.services.remove(key);
                }
                Effect::Cleanup { label, run } => {
                    run().map_err(|source| CleanupFailure {
                        plugin: id.clone(),
                        effect: label.clone(),
                        source,
                    })?;
                }
            }
            plugin.effects.pop();
        }
        plugin.dependencies.clear();
        plugin.state = PluginState::Registered;
        self.activation_order.retain(|candidate| candidate != id);
        Ok(())
    }
}
