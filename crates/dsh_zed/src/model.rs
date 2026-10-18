mod mapping;
use dsh_agent::{ModelError, ModelProvider, ModelRequest, ModelStream};
use dsh_runtime::{Cancellation, CancellationSource};
use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use gpui::{App, Task};
use language_model::{
    LanguageModel, LanguageModelCompletionError, LanguageModelCompletionStream,
    LanguageModelRegistry, LanguageModelRequest,
};
use std::{num::NonZeroUsize, sync::Arc};
use tokio::sync::{mpsc, oneshot};

type Completion =
    BoxFuture<'static, Result<LanguageModelCompletionStream, LanguageModelCompletionError>>;

struct Request {
    request: LanguageModelRequest,
    model: String,
    cancellation: Cancellation,
    reply: oneshot::Sender<Result<(Completion, String), ModelError>>,
}

pub(super) struct Bridge {
    pub adapter: Arc<Adapter>,
    stop: CancellationSource,
    task: Shared<Task<()>>,
}

pub(super) struct Adapter {
    sender: mpsc::Sender<Request>,
    stop: Cancellation,
}

pub fn model_key(model: &LanguageModel) -> String {
    format!("{}/{}", model.provider_id, model.id.0)
}

impl Bridge {
    pub fn new(capacity: NonZeroUsize, cx: &mut App) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Request>(capacity.get());
        let stop = CancellationSource::default();
        let adapter = Arc::new(Adapter {
            sender,
            stop: stop.token(),
        });
        let mut cancellation = stop.token();
        let task = cx.spawn(async move |cx| {
            loop {
                let request = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => break,
                    request = receiver.recv() => match request { Some(request) => request, None => break },
                };
                if request.reply.is_closed() || request.cancellation.is_cancelled() { continue; }
                let binding = cx.update(|cx| {
                    let registry = LanguageModelRegistry::read_global(cx);
                    let model = registry.visible_providers().iter()
                        .flat_map(|provider| provider.provided_models(cx))
                        .find(|model| model_key(model) == request.model)
                        .ok_or_else(|| ModelError("Selected model is no longer available".into()))?;
                    if model.is_disabled().is_some() { return Err(ModelError("Selected model is disabled".into())); }
                    let provider = registry.provider_for_model(&model).map_err(|error| ModelError(error.to_string()))?;
                    if !provider.is_authenticated(cx) { return Err(ModelError("Authenticate this model provider in Zed settings".into())); }
                    Ok((model, provider))
                });
                let completion = binding.map(|(model, provider)| {
                    let provider_id = model.provider_id.to_string();
                    (provider.stream_completion(&model, request.request, cx), provider_id)
                });
                if let Err(_abandoned_completion) = request.reply.send(completion) {
                    // Dropping the undelivered future cancels a request whose caller has gone away.
                }
            }
        }).shared();
        Self {
            adapter,
            stop,
            task,
        }
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.task.clone().await;
    }
}

impl ModelProvider for Adapter {
    fn stream(
        &self,
        request: ModelRequest,
        mut cancellation: Cancellation,
    ) -> BoxFuture<'_, Result<ModelStream, ModelError>> {
        Box::pin(async move {
            let model = request.model.clone();
            let request = mapping::request(request)?;
            let mut stop = self.stop.clone();
            let request_cancellation = cancellation.clone();
            let operation = async {
                let (reply, result) = oneshot::channel();
                self.sender
                    .send(Request {
                        request,
                        model,
                        cancellation: request_cancellation,
                        reply,
                    })
                    .await
                    .map_err(|_| ModelError("Zed model bridge is closed".into()))?;
                let (completion, provider) = result
                    .await
                    .map_err(|_| ModelError("Zed model bridge closed before dispatch".into()))??;
                let stream = completion
                    .await
                    .map_err(|error| ModelError(error.to_string()))?;
                Ok(mapping::response(stream, provider))
            };
            tokio::select! {
                biased;
                _ = stop.cancelled() => Err(ModelError("Zed model bridge is closed".into())),
                _ = cancellation.cancelled() => Err(ModelError("model request cancelled".into())),
                result = operation => result,
            }
        })
    }
}
