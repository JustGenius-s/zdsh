use crate::{
    Agent, AgentConfig, AgentError, AgentService, AssistantContent, EmptyModelName, FinishReason,
    ModelChunk, ModelProvider, ModelRequest, ModelService, QueueCapacityExceeded, Session,
    SessionEvent, ToolCall, ToolError, ToolInvocation, ToolRegistry, ToolsService, Turn, TurnId,
};
use dsh_runtime::{Cancellation, CancellationSource, Context, PluginError};
use futures::{StreamExt, future::BoxFuture};
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::{mpsc, oneshot};

use crate::TurnOptions;

struct Client {
    options: TurnOptions,
    sender: mpsc::Sender<Command>,
    session: Arc<Session>,
}

impl Agent for Client {
    fn submit(&self, input: String) -> BoxFuture<'_, Result<Turn, AgentError>> {
        self.submit_with_options(input, self.options.clone())
    }

    fn submit_with_options(
        &self,
        input: String,
        options: TurnOptions,
    ) -> BoxFuture<'_, Result<Turn, AgentError>> {
        Box::pin(async move {
            if options.model.trim().is_empty() {
                return Err(AgentError::Protocol(
                    "turn model name cannot be empty".into(),
                ));
            }
            let cancellation = CancellationSource::default();
            let (reply, result) = oneshot::channel();
            self.sender
                .send(Command {
                    input,
                    options,
                    cancellation: cancellation.clone(),
                    reply,
                })
                .await
                .map_err(|_| AgentError::Stopped)?;
            Ok(Turn {
                cancellation,
                result,
            })
        })
    }

    fn session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }
}

struct Command {
    options: TurnOptions,
    input: String,
    cancellation: CancellationSource,
    reply: oneshot::Sender<Result<String, AgentError>>,
}

/// Installs one default loop as a plugin activation. Requires `ModelService`,
/// `ToolsService` and a runtime executor. Unload cancels and joins the loop before
/// releasing those dependencies. Register a different `AgentService` to replace it.
pub fn install(context: &mut Context<'_>, config: AgentConfig) -> Result<(), PluginError> {
    if config.model.trim().is_empty() {
        return Err(EmptyModelName.into());
    }
    if config.queue_capacity.get() > tokio::sync::Semaphore::MAX_PERMITS {
        return Err(QueueCapacityExceeded.into());
    }
    let model = context.require::<ModelService>()?;
    let tools = context.require::<ToolsService>()?;
    let session = Arc::new(Session::new(config.system_prompt.clone()));
    let (sender, receiver) = mpsc::channel(config.queue_capacity.get());
    let client = Arc::new(Client {
        options: TurnOptions {
            model: config.model.clone(),
            model_options: config.model_options.clone(),
            allowed_tools: tools
                .definitions()
                .into_iter()
                .map(|tool| tool.name)
                .collect(),
        },
        sender,
        session: Arc::clone(&session),
    });
    let agent = Loop {
        config,
        model,
        tools,
        session,
    };
    context.spawn("agent loop", move |stop| async move {
        agent.run(receiver, stop).await;
        Ok(())
    })?;
    context.provide::<AgentService>(client)?;
    Ok(())
}

struct Loop {
    config: AgentConfig,
    model: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
    session: Arc<Session>,
}

impl Loop {
    async fn run(self, mut receiver: mpsc::Receiver<Command>, mut stop: Cancellation) {
        let mut next_turn = Some(1_u64);
        loop {
            let command = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                command = receiver.recv() => match command { Some(command) => command, None => break },
            };
            let Some(number) = next_turn else {
                break;
            };
            let result = if command.cancellation.token().is_cancelled() {
                Err(AgentError::Cancelled)
            } else {
                next_turn = number.checked_add(1);
                self.turn(
                    TurnId(number),
                    command.input,
                    &command.options,
                    &command.cancellation,
                    &mut stop,
                )
                .await
            };
            if let Err(_abandoned) = command.reply.send(result) {
                // The caller dropped its turn; the terminal outcome remains in the session.
            }
        }
        // Dropping queued commands closes their result channels without starting a turn.
    }

    async fn turn(
        &self,
        turn: TurnId,
        input: String,
        options: &TurnOptions,
        cancellation: &CancellationSource,
        stop: &mut Cancellation,
    ) -> Result<String, AgentError> {
        self.session
            .append(SessionEvent::TurnStarted { turn, input })
            .await;
        let mut result = Err(AgentError::StepLimit);
        for step in 1..=self.config.max_steps.get() {
            if stop.is_cancelled() || cancellation.token().is_cancelled() {
                cancellation.cancel();
                result = Err(AgentError::Cancelled);
                break;
            }
            self.session
                .append(SessionEvent::StepStarted { turn, step })
                .await;
            let response = self.step(turn, step, options, cancellation, stop).await;
            self.session
                .append(SessionEvent::StepFinished {
                    turn,
                    step,
                    result: response.as_ref().map(|_| ()).map_err(Clone::clone),
                })
                .await;
            match response {
                Ok(Some(text)) => {
                    result = Ok(text);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        self.session
            .append(SessionEvent::TurnFinished {
                turn,
                result: result.clone(),
            })
            .await;
        result
    }

    async fn step(
        &self,
        turn: TurnId,
        step: usize,
        options: &TurnOptions,
        cancellation: &CancellationSource,
        stop: &mut Cancellation,
    ) -> Result<Option<String>, AgentError> {
        let request = ModelRequest {
            model: options.model.clone(),
            options: options.model_options.clone(),
            messages: self.session.messages_for_model(&options.model).await,
            tools: self
                .tools
                .definitions()
                .into_iter()
                .filter(|tool| options.allowed_tools.contains(&tool.name))
                .collect(),
        };
        self.session
            .append(SessionEvent::Request {
                turn,
                step,
                request: request.clone(),
            })
            .await;
        let (text, calls, content) = self
            .response(turn, step, request, cancellation, stop)
            .await?;
        self.session
            .append(SessionEvent::Assistant {
                turn,
                step,
                content,
            })
            .await;
        for call in &calls {
            let (executed, result) = if stop.is_cancelled() || cancellation.token().is_cancelled() {
                cancellation.cancel();
                (false, Err(ToolError::Cancelled))
            } else if !options.allowed_tools.contains(&call.name) {
                (false, Err(ToolError::UnknownTool(call.name.clone())))
            } else if let Some(tool) = self.tools.get(&call.name) {
                let mut signal = cancellation.token();
                let execution = tool.execute_call(
                    ToolInvocation {
                        turn,
                        step,
                        call: call.clone(),
                    },
                    signal.clone(),
                );
                tokio::pin!(execution);
                let result = tokio::select! {
                    biased;
                    _ = stop.cancelled() => { cancellation.cancel(); execution.await }
                    _ = signal.cancelled() => execution.await,
                    result = &mut execution => result,
                };
                (true, result)
            } else {
                (false, Err(ToolError::UnknownTool(call.name.clone())))
            };
            self.session
                .append(SessionEvent::ToolFinished {
                    turn,
                    step,
                    call_id: call.id.clone(),
                    executed,
                    result,
                })
                .await;
        }
        if stop.is_cancelled() || cancellation.token().is_cancelled() {
            cancellation.cancel();
            return Err(AgentError::Cancelled);
        }
        Ok(calls.is_empty().then_some(text))
    }

    async fn response(
        &self,
        turn: TurnId,
        step: usize,
        request: ModelRequest,
        cancellation: &CancellationSource,
        stop: &mut Cancellation,
    ) -> Result<(String, Vec<ToolCall>, Vec<AssistantContent>), AgentError> {
        let mut signal = cancellation.token();
        let provider_signal = signal.clone();
        let mut stream = tokio::select! {
            biased;
            _ = stop.cancelled() => { cancellation.cancel(); return Err(AgentError::Cancelled); }
            _ = signal.cancelled() => return Err(AgentError::Cancelled),
            result = self.model.stream(request, provider_signal) => result.map_err(|error| AgentError::Model(error.0))?,
        };
        let mut content = Vec::new();
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut call_ids = BTreeSet::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = stop.cancelled() => { cancellation.cancel(); return Err(AgentError::Cancelled); }
                _ = signal.cancelled() => return Err(AgentError::Cancelled),
                chunk = stream.next() => chunk.ok_or_else(|| AgentError::Protocol("stream ended without Finished".into()))?.map_err(|error| AgentError::Model(error.0))?,
            };
            self.session
                .append(SessionEvent::ModelOutput {
                    turn,
                    step,
                    chunk: chunk.clone(),
                })
                .await;
            match chunk {
                ModelChunk::Text(delta) => {
                    text.push_str(&delta);
                    if let Some(AssistantContent::Text(previous)) = content.last_mut() {
                        previous.push_str(&delta);
                    } else {
                        content.push(AssistantContent::Text(delta));
                    }
                }
                ModelChunk::Thinking { text, signature } => {
                    if let Some(AssistantContent::Thinking {
                        text: previous,
                        signature: previous_signature,
                    }) = content.last_mut()
                    {
                        previous.push_str(&text);
                        *previous_signature = signature.or(previous_signature.take());
                    } else {
                        content.push(AssistantContent::Thinking { text, signature });
                    }
                }
                ModelChunk::RedactedThinking(data) => {
                    content.push(AssistantContent::RedactedThinking(data))
                }
                ModelChunk::ReasoningDetails(details) => {
                    content.push(AssistantContent::ReasoningDetails(details))
                }
                ModelChunk::Usage(_) | ModelChunk::ProviderEvent { .. } => {}
                ModelChunk::ToolCall(call) => {
                    if call.id.0.trim().is_empty()
                        || call.name.trim().is_empty()
                        || !call_ids.insert(call.id.clone())
                    {
                        return Err(AgentError::Protocol(
                            "empty tool identity or duplicate call id in response".into(),
                        ));
                    }
                    content.push(AssistantContent::ToolCall(call.clone()));
                    calls.push(call);
                }
                ModelChunk::Finished(reason) => {
                    match reason {
                        FinishReason::MaxTokens => return Err(AgentError::OutputLimit),
                        FinishReason::Refusal => return Err(AgentError::Refused),
                        FinishReason::Stop | FinishReason::ToolCalls => {}
                    }
                    if (reason == FinishReason::ToolCalls) != !calls.is_empty() {
                        return Err(AgentError::Protocol(
                            "finish reason does not match tool calls".into(),
                        ));
                    }
                    return Ok((text, calls, content));
                }
            }
        }
    }
}
