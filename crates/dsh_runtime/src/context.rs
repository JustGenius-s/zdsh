use crate::{
    Cancellation, Error, Event, PluginFuture, PluginId, PluginResult, Service, task::OwnedTask,
};
use std::{
    any::{Any, TypeId, type_name},
    collections::{BTreeSet, HashMap},
    future::Future,
    sync::Arc,
};
use tokio::runtime::Handle;

type Cleanup = Box<dyn FnMut() -> PluginResult + Send>;
type AsyncCleanup = Box<dyn FnMut() -> PluginFuture + Send>;
type Listener = Box<dyn FnMut(Arc<dyn Any + Send + Sync>) -> PluginFuture + Send>;

pub(super) enum Effect {
    Service(TypeId),
    Cleanup {
        label: String,
        run: Cleanup,
    },
    AsyncCleanup {
        label: String,
        make: AsyncCleanup,
        pending: Option<PluginFuture>,
    },
    Listener {
        event: TypeId,
        run: Listener,
    },
}

pub(super) struct Registration {
    pub(super) owner: PluginId,
    pub(super) value: Box<dyn Any + Send + Sync>,
}

/// Activation-scoped access to services and reversible registrations.
///
/// Register cleanup immediately after acquiring a resource. Effects are unwound
/// in reverse registration order. Service handles must stay inside the owning
/// plugin's lifetime: cloning an `Arc` does not extend a provider's active lifetime.
pub struct Context<'a> {
    pub(super) owner: &'a PluginId,
    pub(super) services: &'a mut HashMap<TypeId, Registration>,
    pub(super) dependencies: &'a mut BTreeSet<PluginId>,
    pub(super) effects: &'a mut Vec<Effect>,
    pub(super) tasks: &'a mut Vec<OwnedTask>,
    pub(super) executor: Option<&'a Handle>,
}

impl Context<'_> {
    /// Registers a service until this activation's effects are unwound.
    ///
    /// A duplicate key returns an error without replacing the existing provider.
    pub fn provide<S: Service>(&mut self, value: Arc<S::Value>) -> Result<(), Error> {
        let key = TypeId::of::<S>();
        if self.services.contains_key(&key) {
            return Err(Error::DuplicateService(type_name::<S>()));
        }
        self.services.insert(
            key,
            Registration {
                owner: self.owner.clone(),
                value: Box::new(value),
            },
        );
        self.effects.push(Effect::Service(key));
        Ok(())
    }

    /// Resolves a service and records its provider as a teardown dependency.
    ///
    /// Providers must already be active, or the service must have been provided
    /// by this activation. Missing services fail immediately; the runtime does
    /// not activate providers automatically.
    pub fn require<S: Service>(&mut self) -> Result<Arc<S::Value>, Error> {
        let registration = self
            .services
            .get(&TypeId::of::<S>())
            .ok_or(Error::MissingService(type_name::<S>()))?;
        let value = registration
            .value
            .downcast_ref::<Arc<S::Value>>()
            .ok_or(Error::MissingService(type_name::<S>()))?;
        if registration.owner != *self.owner {
            self.dependencies.insert(registration.owner.clone());
        }
        Ok(Arc::clone(value))
    }

    /// Registers a labeled, synchronous cleanup callback.
    ///
    /// On failure, this callback and older effects remain pending. A later unload
    /// retries this callback, so it must tolerate partial work from a failed call.
    /// Callbacks must finish all resource use before returning success.
    pub fn on_cleanup(
        &mut self,
        label: impl Into<String>,
        cleanup: impl FnMut() -> PluginResult + Send + 'static,
    ) {
        self.effects.push(Effect::Cleanup {
            label: label.into(),
            run: Box::new(cleanup),
        });
    }

    /// Registers an asynchronous cleanup factory. Failed attempts are retried
    /// with a new future; interrupted attempts retain and resume the same future.
    pub fn on_cleanup_async<F>(
        &mut self,
        label: impl Into<String>,
        mut cleanup: impl FnMut() -> F + Send + 'static,
    ) where
        F: Future<Output = PluginResult> + Send + 'static,
    {
        self.effects.push(Effect::AsyncCleanup {
            label: label.into(),
            make: Box::new(move || Box::pin(cleanup())),
            pending: None,
        });
    }

    /// Subscribes until this activation is unloaded. Listeners are awaited in
    /// registration order, stopping on the first error. A dropped dispatch drops
    /// the current listener future; listeners must tolerate that cancellation.
    pub fn on<E, F>(&mut self, mut listener: impl FnMut(Arc<E>) -> F + Send + 'static)
    where
        E: Event,
        F: Future<Output = PluginResult> + Send + 'static,
    {
        self.effects.push(Effect::Listener {
            event: TypeId::of::<E>(),
            run: Box::new(move |payload| match payload.downcast::<E>() {
                Ok(payload) => Box::pin(listener(payload)),
                Err(_) => {
                    Box::pin(async { Err(Error::EventTypeMismatch(type_name::<E>()).into()) })
                }
            }),
        });
    }

    /// Starts a task on the explicitly configured executor and owns its join.
    /// All tasks of this plugin receive cancellation before waiting for any;
    /// all must exit before services, listeners or cleanup effects are released.
    /// Task errors and panics are reported when teardown joins the task.
    pub fn spawn<F>(
        &mut self,
        label: impl Into<String>,
        task: impl FnOnce(Cancellation) -> F,
    ) -> Result<(), Error>
    where
        F: Future<Output = PluginResult> + Send + 'static,
    {
        let executor = self.executor.ok_or(Error::MissingExecutor)?;
        self.tasks.push(OwnedTask::spawn(
            executor,
            label.into(),
            move |cancellation| Box::pin(task(cancellation)),
        ));
        Ok(())
    }
}
