use crate::model::{self, model_key};
use anyhow::{Context as _, anyhow, bail};
use dsh_agent::{Agent, AgentConfig, AgentService, ModelService, ToolRegistry, ToolsService};
use dsh_runtime::{CancellationSource, PluginId, Runtime};
use futures::future::LocalBoxFuture;
use gpui::{App, Global, Task};
use language_model::{LanguageModel, LanguageModelRegistry};
use std::{cell::Cell, rc::Rc, sync::Arc};
use tokio::sync::{Mutex, oneshot};

struct Host {
    executor: tokio::runtime::Handle,
    sessions: Vec<Rc<Owner>>,
    closing: bool,
}
impl Global for Host {}

struct Owner {
    runtime: Mutex<Option<Runtime>>,
    bridge: model::Bridge,
    closing: CancellationSource,
    closed: Cell<bool>,
}

impl Owner {
    async fn shutdown(&self) -> anyhow::Result<()> {
        self.closing.cancel();
        if self.closed.get() {
            return Ok(());
        }
        let mut runtime = self.runtime.lock().await;
        if let Some(runtime) = runtime.as_mut() {
            runtime.shutdown().await?;
        }
        // Release activation factories and their tool handles after all consumers stop.
        runtime.take();
        drop(runtime);
        self.bridge.shutdown().await;
        self.closed.set(true);
        Ok(())
    }
}

/// A host-owned session. Dropping this handle does not unload its plugins;
/// call `close` to await cleanup, or let application shutdown close it.
pub struct Session {
    agent: Arc<dyn Agent>,
    owner: Rc<Owner>,
}

impl Session {
    pub fn agent(&self) -> Arc<dyn Agent> {
        Arc::clone(&self.agent)
    }

    /// Stops the agent and awaits its tools before releasing model dispatch.
    /// A failed close remains owned by the host and can be retried.
    pub fn close(&self, cx: &mut App) -> Task<anyhow::Result<()>> {
        self.owner.closing.cancel();
        let owner = Rc::clone(&self.owner);
        cx.spawn(async move |cx| {
            owner.shutdown().await?;
            cx.update(|cx| {
                cx.global_mut::<Host>()
                    .sessions
                    .retain(|entry| !Rc::ptr_eq(entry, &owner))
            });
            Ok(())
        })
    }
}

/// Call after `gpui_tokio::init` and the language-model registry initialization.
/// Initialization does not create sessions, authenticate, or send model requests.
pub fn init(cx: &mut App) {
    cx.set_global(Host {
        executor: gpui_tokio::Tokio::handle(cx),
        sessions: Vec::new(),
        closing: false,
    });
    cx.on_app_quit(|cx| {
        let shutdown = shutdown(cx);
        async move {
            if let Err(error) = shutdown.await {
                log::error!("DSH shutdown failed: {error:#}");
            }
        }
    })
    .detach();
}

/// Uses an already authenticated Zed provider. The caller selects the model and
/// initial inference options; `config.model` must equal `model_key(model)`.
/// Later turns can select another authenticated model through `submit_with_options`.
/// Await the returned task; dropping it leaves cleanup owned by the host.
pub fn start_session(
    model: LanguageModel,
    config: AgentConfig,
    tools: ToolRegistry,
    cx: &mut App,
) -> anyhow::Result<Task<anyhow::Result<Session>>> {
    let host = cx
        .try_global::<Host>()
        .context("DSH host is not initialized")?;
    if host.closing {
        bail!("DSH host is shutting down");
    }
    if config.model != model_key(&model) {
        bail!("DSH model key does not match the selected Zed model");
    }
    if config.queue_capacity.get() > tokio::sync::Semaphore::MAX_PERMITS {
        bail!("DSH queue capacity exceeds Tokio channel limit");
    }
    let executor = host.executor.clone();
    let provider = LanguageModelRegistry::read_global(cx).provider_for_model(&model)?;
    if !provider.is_authenticated(cx) {
        bail!("authenticate the selected model provider in Zed settings first");
    }
    let bridge = model::Bridge::new(config.queue_capacity, cx);
    let owner = Rc::new(Owner {
        runtime: Mutex::new(Some(Runtime::with_executor(executor))),
        bridge,
        closing: CancellationSource::default(),
        closed: Cell::new(false),
    });
    cx.global_mut::<Host>().sessions.push(Rc::clone(&owner));
    Ok(cx.spawn(async move |cx| {
        let result = async {
            let mut runtime = owner.runtime.lock().await;
            let runtime = runtime.as_mut().context("DSH session is closed")?;
            if owner.closing.token().is_cancelled() {
                bail!("DSH session closed before startup");
            }
            let model_id = PluginId::new("zed-model");
            let tools_id = PluginId::new("tools");
            let agent_id = PluginId::new("agent");
            let client_id = PluginId::new("zed-client");
            runtime.register(model_id.clone(), {
                let adapter = owner.bridge.adapter.clone();
                move |context| {
                    context.provide::<ModelService>(adapter.clone())?;
                    Ok(())
                }
            })?;
            runtime.register(tools_id.clone(), {
                let tools = Arc::new(tools);
                move |context| {
                    context.provide::<ToolsService>(tools.clone())?;
                    Ok(())
                }
            })?;
            runtime.register(agent_id.clone(), move |context| {
                dsh_agent::install(context, config.clone())
            })?;
            let (sender, receiver) = oneshot::channel();
            let mut sender = Some(sender);
            runtime.register(client_id.clone(), move |context| {
                let agent = context.require::<AgentService>()?;
                let sender = sender.take().ok_or("session client already activated")?;
                sender.send(agent).map_err(|_| "session caller closed")?;
                Ok(())
            })?;
            for id in [&model_id, &tools_id, &agent_id, &client_id] {
                runtime.activate(id).await?;
            }
            let agent = receiver.await.context("DSH agent was not provided")?;
            anyhow::Ok(agent)
        }
        .await;
        match result {
            Ok(agent) => Ok(Session { agent, owner }),
            Err(error) => {
                if let Err(cleanup) = owner.shutdown().await {
                    return Err(anyhow!(
                        "DSH startup failed: {error:#}; cleanup failed: {cleanup:#}"
                    ));
                }
                cx.update(|cx| {
                    cx.global_mut::<Host>()
                        .sessions
                        .retain(|entry| !Rc::ptr_eq(entry, &owner))
                });
                Err(error)
            }
        }
    }))
}

/// Call before `App::quit` while GPUI can still dispatch work. GPUI's quit
/// observers have a bounded wait and cannot guarantee arbitrary tool cleanup.
/// New sessions remain disabled after shutdown starts; failures are retryable.
pub fn shutdown(cx: &mut App) -> LocalBoxFuture<'static, anyhow::Result<()>> {
    let sessions = if cx.try_global::<Host>().is_some() {
        let host = cx.global_mut::<Host>();
        host.closing = true;
        for owner in &host.sessions {
            owner.closing.cancel();
        }
        host.sessions.clone()
    } else {
        Vec::new()
    };
    Box::pin(async move {
        let mut failures = Vec::new();
        let results =
            futures::future::join_all(sessions.iter().map(|owner| owner.shutdown())).await;
        for result in results {
            if let Err(error) = result {
                failures.push(format!("{error:#}"));
            }
        }
        if !failures.is_empty() {
            bail!("DSH cleanup failed: {}", failures.join("; "));
        }
        Ok(())
    })
}
