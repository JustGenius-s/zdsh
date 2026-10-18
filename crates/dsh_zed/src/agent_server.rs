mod model_selector;
mod transcript;

use crate::{Session, SessionControls, start_session};
use acp_thread::{AcpThread, AgentConnection, AgentModelSelector};
use action_log::ActionLog;
use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
use agent_servers::{AgentServer, AgentServerDelegate};
use anyhow::{Context as _, Result, anyhow, bail};
use dsh_agent::AgentConfig;
use dsh_runtime::CancellationSource;
use futures::FutureExt;
use gpui::{App, AppContext, Entity, SharedString, Task, TaskExt, WeakEntity};
use language_model::{LanguageModel, LanguageModelRegistry};
use model_selector::Selector;
use project::{AgentId, Project};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashMap,
    num::NonZeroUsize,
    rc::Rc,
};
use transcript::{apply_event, prompt_response};
use ui::IconName;
use util::path_list::PathList;
use uuid::Uuid;

pub const DSH_AGENT_ID: &str = "zdsh.dsh";

/// In-process DSH backend using Zed model providers and extension tools.
pub struct DshAgentServer;

impl AgentServer for DshAgentServer {
    fn agent_id(&self) -> AgentId {
        DSH_AGENT_ID.into()
    }
    fn logo(&self) -> IconName {
        IconName::Sparkle
    }
    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
    fn connect(
        &self,
        _: AgentServerDelegate,
        _: Entity<Project>,
        _: &mut App,
    ) -> Task<Result<Rc<dyn AgentConnection>>> {
        Task::ready(Ok(Rc::new(Connection::default())))
    }
}

#[derive(Default)]
struct Connection {
    sessions: RefCell<HashMap<acp_v2::SessionId, Rc<SessionState>>>,
}

type Startup = futures::future::Shared<
    futures::future::LocalBoxFuture<'static, std::result::Result<Rc<Session>, Rc<String>>>,
>;

struct SessionState {
    thread: WeakEntity<AcpThread>,
    controls: Entity<SessionControls>,
    active_model: RefCell<Option<LanguageModel>>,
    runtime: RefCell<Option<Rc<Session>>>,
    starting: RefCell<Option<Startup>>,
    tool_approvals: RefCell<Option<Task<()>>>,
    running: Cell<bool>,
    closed: Cell<bool>,
    cancellation: RefCell<Option<CancellationSource>>,
    _capabilities: watch::Sender<acp_v2::PromptCapabilities>,
}

/// Returns the Zed controls for a DSH session; other Agent connections return None.
pub fn session_controls(
    connection: Rc<dyn AgentConnection>,
    id: &acp_v2::SessionId,
) -> Option<Entity<SessionControls>> {
    connection
        .downcast::<Connection>()?
        .session(id)
        .ok()
        .map(|session| session.controls.clone())
}

struct PromptGuard(Rc<SessionState>);
impl Drop for PromptGuard {
    fn drop(&mut self) {
        self.0.running.set(false);
        self.0.active_model.borrow_mut().take();
        self.0.cancellation.borrow_mut().take();
    }
}

impl Connection {
    fn session(&self, id: &acp_v2::SessionId) -> Result<Rc<SessionState>> {
        self.sessions
            .borrow()
            .get(id)
            .cloned()
            .context("DSH session is closed")
    }
}

impl AgentConnection for Connection {
    fn agent_id(&self) -> AgentId {
        DSH_AGENT_ID.into()
    }
    fn telemetry_id(&self) -> SharedString {
        "dsh".into()
    }
    fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
    fn auth_methods(&self) -> &[acp_v2::AuthMethod] {
        &[]
    }
    fn authenticate(&self, _: acp_v2::AuthMethodId, _: &mut App) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }
    fn supports_close_session(&self) -> bool {
        true
    }

    fn new_session(
        self: Rc<Self>,
        project: Entity<Project>,
        work_dirs: PathList,
        cx: &mut App,
    ) -> Task<Result<Entity<AcpThread>>> {
        if project.read(cx).is_via_collab() {
            return Task::ready(Err(anyhow!(
                "DSH is not available in collaborative projects"
            )));
        }
        let id = acp_v2::SessionId::new(Uuid::new_v4().to_string());
        let controls = cx.new(|cx| SessionControls::new(project.clone(), cx));
        let (capabilities, capabilities_rx) = watch::channel(acp_v2::PromptCapabilities::default());
        let action_log = cx.new(|_| ActionLog::new(project.clone()));
        let thread = cx.new(|cx| {
            let thread = AcpThread::new(
                None,
                None,
                Some(work_dirs),
                self.clone(),
                project,
                action_log,
                id.clone(),
                capabilities_rx,
                cx,
            );
            cx.on_release({
                let connection = self.clone();
                let id = id.clone();
                move |_, cx| {
                    connection.close_session(&id, cx).detach_and_log_err(cx);
                }
            })
            .detach();
            thread
        });
        self.sessions.borrow_mut().insert(
            id,
            Rc::new(SessionState {
                thread: thread.downgrade(),
                controls,
                active_model: RefCell::new(None),
                runtime: RefCell::new(None),
                starting: RefCell::new(None),
                tool_approvals: RefCell::new(None),
                running: Cell::new(false),
                closed: Cell::new(false),
                cancellation: RefCell::new(None),
                _capabilities: capabilities,
            }),
        );
        Task::ready(Ok(thread))
    }

    fn close_session(self: Rc<Self>, id: &acp_v2::SessionId, cx: &mut App) -> Task<Result<()>> {
        let Some(session) = self.sessions.borrow().get(id).cloned() else {
            return Task::ready(Ok(()));
        };
        session.closed.set(true);
        if let Some(cancellation) = session.cancellation.borrow().as_ref() {
            cancellation.cancel();
        }
        let id = id.clone();
        cx.spawn(async move |cx| {
            let mut runtime = session.runtime.borrow().clone();
            let starting = session.starting.borrow().clone();
            if runtime.is_none()
                && let Some(starting) = starting
            {
                match starting.await {
                    Ok(started) => runtime = Some(started),
                    Err(error) => {
                        self.sessions.borrow_mut().remove(&id);
                        return Err(anyhow!("{error}"));
                    }
                }
            }
            if let Some(runtime) = runtime {
                cx.update(|cx| runtime.close(cx)).await?;
            }
            session.tool_approvals.borrow_mut().take();
            self.sessions.borrow_mut().remove(&id);
            Ok(())
        })
    }

    fn validate_prompt_content(&self, content: &[acp_v2::ContentBlock]) -> Result<()> {
        text_prompt(content).map(|_| ())
    }

    fn prompt(
        &self,
        params: acp_v2::PromptRequest,
        cx: &mut App,
    ) -> Task<Result<acp_v1::PromptResponse>> {
        let prepared = (|| {
            let input = text_prompt(&params.prompt)?;
            let session = self.session(&params.session_id)?;
            if session.closed.get() {
                bail!("DSH session is closed");
            }
            if session.running.get() {
                bail!("DSH is still finishing the previous turn");
            }
            let options = session.controls.read(cx).turn_options(cx)?;
            let model = session
                .controls
                .read(cx)
                .model()
                .cloned()
                .context("Choose a model for DSH")?;
            session.active_model.replace(Some(model.clone()));
            session.running.set(true);
            let cancellation = CancellationSource::default();
            session.cancellation.replace(Some(cancellation.clone()));
            Ok((input, session, model, options, cancellation))
        })();
        let (input, session, model, options, cancellation) = match prepared {
            Ok(value) => value,
            Err(error) => return Task::ready(Err(error)),
        };
        // Construct outside the task so dropping an unpolled task also releases admission.
        let guard = PromptGuard(session.clone());
        cx.spawn(async move |cx| {
            let _guard = guard;
            let existing = session.runtime.borrow().clone();
            let runtime = if let Some(runtime) = existing { runtime } else {
                let config = AgentConfig {
                    model: options.model.clone(),
                    model_options: options.model_options.clone(),
                    system_prompt: "You are DSH, an assistant in a code editor. Use only the tools supplied in this request. Extension tools require user approval. Do not claim to inspect or modify the project without a tool result supporting that claim.".into(),
                    max_steps: NonZeroUsize::new(8).context("invalid step limit")?, queue_capacity: NonZeroUsize::MIN,
                };
                let authentication = cx.update(|cx| {
                    let provider = LanguageModelRegistry::read_global(cx).provider_for_model(&model)?;
                    Ok::<_, anyhow::Error>((!provider.is_authenticated(cx)).then(|| provider.authenticate(cx)))
                })?;
                if let Some(authentication) = authentication {
                    let mut signal = cancellation.token();
                    tokio::select! {
                        biased;
                        _ = signal.cancelled() => return Ok(acp_v1::PromptResponse::new(acp_v1::StopReason::Cancelled)),
                        result = authentication => { result.context("Configure this model provider in Zed settings")?; }
                    }
                }
                if session.closed.get() || cancellation.token().is_cancelled() {
                    return Ok(acp_v1::PromptResponse::new(acp_v1::StopReason::Cancelled));
                }
                let start = cx.update(|cx| {
                    let (tools, approvals) = crate::extension_tools::registry(session.thread.clone(), cx)?;
                    session.tool_approvals.replace(Some(approvals));
                    start_session(model, config, tools, cx)
                })?;
                let starting = async move { start.await.map(Rc::new).map_err(|error| Rc::new(format!("{error:#}"))) }.boxed_local().shared();
                session.starting.replace(Some(starting.clone()));
                let result = starting.await;
                session.starting.borrow_mut().take();
                let runtime = result.map_err(|error| anyhow!("{error}"))?;
                if session.closed.get() || cancellation.token().is_cancelled() {
                    cx.update(|cx| runtime.close(cx)).await?;
                    return Ok(acp_v1::PromptResponse::new(acp_v1::StopReason::Cancelled));
                }
                session.runtime.replace(Some(runtime.clone()));
                runtime
            };
            if cancellation.token().is_cancelled() { return Ok(acp_v1::PromptResponse::new(acp_v1::StopReason::Cancelled)); }
            let agent = runtime.agent();
            let log = agent.session();
            let mut changes = log.subscribe();
            let mut cursor = *changes.borrow_and_update();
            let turn = agent.submit_with_options(input, options).await?;
            let cancel_turn = turn.cancellation_handle();
            let completion = turn.wait();
            tokio::pin!(completion);
            let mut signal = cancellation.token();
            let mut cancelled = false;
            let mut visible_turn = None;
            let result = loop {
                let (next, events) = log.events_since(cursor).await?;
                cursor = next;
                for event in events {
                    apply_event(&session, &mut visible_turn, event, cx)?;
                }
                tokio::select! {
                    biased;
                    _ = signal.cancelled(), if !cancelled => { cancel_turn.cancel(); cancelled = true; }
                    result = &mut completion => break result,
                    changed = changes.changed() => { changed.context("DSH session stream closed")?; }
                }
            };
            let (_, events) = log.events_since(cursor).await?;
            for event in events {
                apply_event(&session, &mut visible_turn, event, cx)?;
            }
            prompt_response(result)
        })
    }

    fn cancel(&self, id: &acp_v2::SessionId, _: &mut App) -> Result<()> {
        let session = self.session(id)?;
        if let Some(cancellation) = session.cancellation.borrow().as_ref() {
            cancellation.cancel();
        }
        Ok(())
    }

    fn model_selector(&self, id: &acp_v2::SessionId) -> Option<Rc<dyn AgentModelSelector>> {
        self.session(id)
            .ok()
            .map(|session| Rc::new(Selector(session)) as Rc<dyn AgentModelSelector>)
    }
}

fn text_prompt(content: &[acp_v2::ContentBlock]) -> Result<String> {
    let mut parts = Vec::new();
    for block in content {
        let acp_v2::ContentBlock::Text(text) = block else {
            bail!("DSH accepts text only; remove attachments or use Zed Agent");
        };
        parts.push(text.text.as_str());
    }
    let text = parts.join("\n");
    if text.trim().is_empty() {
        bail!("Enter a message for DSH");
    }
    Ok(text)
}
