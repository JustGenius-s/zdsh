use std::sync::{Arc, LazyLock};

use anyhow::Result;
use codebuddy::CODEBUDDY_API_URL;
use credentials_provider::CredentialsProvider;
use futures::{FutureExt, StreamExt, future::BoxFuture};
use gpui::{App, AppContext, AsyncApp, Context, Entity, SharedString, Task};
use http_client::{CustomHeaders, HttpClient};
use language_model::chat_completion::ChatCompletionEventMapper;
use language_model::{
    ApiKeyConfiguration, ApiKeyState, AuthenticateError, EnvVar, IconOrSvg, LanguageModel,
    LanguageModelClient, LanguageModelCompletionError, LanguageModelCompletionStream,
    LanguageModelEffortLevel, LanguageModelId, LanguageModelName, LanguageModelProvider,
    LanguageModelProviderId, LanguageModelProviderName, LanguageModelProviderState,
    LanguageModelRequest, LanguageModelToolChoiceSupport, ModelRateLimiters, ProviderSettingsView,
    env_var, unavailable_error,
};
use open_ai::ReasoningEffort;
use settings::{Settings, SettingsStore};
use ui::IconName;
use util::ResultExt as _;

use crate::provider::open_ai::{ChatCompletionMaxTokensParameter, into_open_ai};

const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("codebuddy");
const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("CodeBuddy");

const API_KEY_ENV_VAR_NAME: &str = "CODEBUDDY_API_KEY";
static API_KEY_ENV_VAR: LazyLock<EnvVar> = env_var!(API_KEY_ENV_VAR_NAME);

/// These identify a single request or thread; a fixed value from settings would merge every
/// conversation into one.
pub(crate) const RESERVED_HEADER_NAMES: &[&str] = &[
    "X-Conversation-Id",
    "X-Conversation-Request-Id",
    "X-Conversation-Message-Id",
    "X-Request-Id",
];

/// Used when the catalog omits a model's context window.
const DEFAULT_MAX_INPUT_TOKENS: u64 = 128_000;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct CodeBuddySettings {
    pub api_url: String,
    pub custom_headers: CustomHeaders,
}

pub struct CodeBuddyLanguageModelProvider {
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
    request_limiters: ModelRateLimiters,
}

pub struct State {
    api_key_state: ApiKeyState,
    credentials_provider: Arc<dyn CredentialsProvider>,
    http_client: Arc<dyn HttpClient>,
    models: Vec<codebuddy::Model>,
    fetch_models_task: Option<Task<()>>,
}

impl State {
    fn is_authenticated(&self) -> bool {
        self.api_key_state.has_key()
    }

    fn set_api_key(&mut self, api_key: Option<String>, cx: &mut Context<Self>) -> Task<Result<()>> {
        let credentials_provider = self.credentials_provider.clone();
        let api_url = CodeBuddyLanguageModelProvider::api_url(cx);
        let task = self.api_key_state.store(
            api_url,
            api_key,
            |this| &mut this.api_key_state,
            credentials_provider,
            cx,
        );
        self.models.clear();
        self.fetch_models_task = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            task.await?;
            this.update(cx, |this, cx| this.refresh_models(cx))
        })
    }

    fn authenticate(&mut self, cx: &mut Context<Self>) -> Task<Result<(), AuthenticateError>> {
        let credentials_provider = self.credentials_provider.clone();
        let api_url = CodeBuddyLanguageModelProvider::api_url(cx);
        let task = self.api_key_state.load_if_needed(
            api_url,
            |this| &mut this.api_key_state,
            credentials_provider,
            cx,
        );
        cx.spawn(async move |this, cx| {
            task.await?;
            this.update(cx, |this, cx| {
                if this.models.is_empty() && this.fetch_models_task.is_none() {
                    this.refresh_models(cx);
                }
            })?;
            Ok(())
        })
    }

    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        let api_url = CodeBuddyLanguageModelProvider::api_url(cx);
        let Some(api_key) = self.api_key_state.key(&api_url) else {
            return;
        };
        let http_client = self.http_client.clone();
        self.fetch_models_task = Some(cx.spawn(async move |this, cx| {
            let result = codebuddy::fetch_models(http_client.as_ref(), &api_url, &api_key).await;
            this.update(cx, |this, cx| {
                this.fetch_models_task = None;
                match result {
                    Ok(models) => {
                        this.models = models;
                        cx.notify();
                    }
                    // The previous catalog stays in place so a transient failure does not empty
                    // the model picker; the next authentication retries when nothing was loaded.
                    Err(error) => log::error!("Failed to load CodeBuddy models: {error:#}"),
                }
            })
            .log_err();
        }));
    }
}

impl CodeBuddyLanguageModelProvider {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        let state = cx.new(|cx| {
            cx.observe_global::<SettingsStore>({
                let mut last_api_url = Self::api_url(cx);
                move |this: &mut State, cx| {
                    let api_url = Self::api_url(cx);
                    if api_url == last_api_url {
                        return;
                    }
                    last_api_url = api_url.clone();
                    let credentials_provider = this.credentials_provider.clone();
                    this.api_key_state.handle_url_change(
                        api_url,
                        |this| &mut this.api_key_state,
                        credentials_provider,
                        cx,
                    );
                    this.models.clear();
                    this.fetch_models_task = None;
                    this.authenticate(cx).detach();
                    cx.notify();
                }
            })
            .detach();
            State {
                api_key_state: ApiKeyState::new(Self::api_url(cx), (*API_KEY_ENV_VAR).clone()),
                credentials_provider,
                http_client: http_client.clone(),
                models: Vec::new(),
                fetch_models_task: None,
            }
        });

        Self {
            http_client,
            state,
            request_limiters: ModelRateLimiters::default(),
        }
    }

    fn settings(cx: &App) -> &CodeBuddySettings {
        &crate::AllLanguageModelSettings::get_global(cx).codebuddy
    }

    fn api_url(cx: &App) -> SharedString {
        let api_url = &Self::settings(cx).api_url;
        if api_url.trim().is_empty() {
            CODEBUDDY_API_URL.into()
        } else {
            codebuddy::chat_api_url(api_url).into()
        }
    }

    /// The current catalog entry for `model`, if this provider still offers it.
    fn config(
        &self,
        model: &LanguageModel,
        cx: &App,
    ) -> Result<codebuddy::Model, LanguageModelCompletionError> {
        self.state
            .read(cx)
            .models
            .iter()
            .find(|candidate| candidate.id == model.id.0.as_ref())
            .cloned()
            .ok_or_else(|| unavailable_error(model))
    }
}

impl LanguageModelProviderState for CodeBuddyLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for CodeBuddyLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }

    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }

    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiOpenAiCompat)
    }

    fn default_model(&self, cx: &App) -> Option<LanguageModel> {
        self.state.read(cx).models.first().map(language_model)
    }

    fn default_fast_model(&self, cx: &App) -> Option<LanguageModel> {
        self.state
            .read(cx)
            .models
            .iter()
            .find(|model| model.is_lite())
            .map(language_model)
    }

    fn provided_models(&self, cx: &App) -> Vec<LanguageModel> {
        self.state
            .read(cx)
            .models
            .iter()
            .map(language_model)
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        self.state.update(cx, |state, cx| state.authenticate(cx))
    }

    fn settings_view(&self, cx: &mut App) -> Option<ProviderSettingsView> {
        let api_key_url = codebuddy::product_origin(&Self::api_url(cx));
        let state = self.state.read(cx);
        Some(ProviderSettingsView::ApiKey(ApiKeyConfiguration::new(
            state.api_key_state.has_key(),
            state.api_key_state.is_from_env_var(),
            state.api_key_state.env_var_name().clone(),
            api_key_url.into(),
        )))
    }

    fn set_api_key(&self, api_key: Option<String>, cx: &mut App) -> Task<Result<()>> {
        self.state
            .update(cx, |state, cx| state.set_api_key(api_key, cx))
    }
}

impl LanguageModelClient for CodeBuddyLanguageModelProvider {
    fn stream_completion(
        &self,
        model: &LanguageModel,
        mut request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<LanguageModelCompletionStream, LanguageModelCompletionError>>
    {
        let config = match cx.update(|cx| self.config(model, cx)) {
            Ok(config) => config,
            Err(error) => return async move { Err(error) }.boxed(),
        };
        // `speed` can leak in from a parent thread's model; CodeBuddy has no fast mode.
        request.speed = None;
        if request.temperature.is_none() {
            request.temperature = config.temperature;
        }
        let reasoning_effort = reasoning_effort(&request, &config);
        let thread_id = request.thread_id.clone();
        let request = match into_open_ai(
            request,
            &config.id,
            false,
            false,
            config.max_output_tokens,
            ChatCompletionMaxTokensParameter::MaxTokens,
            reasoning_effort,
            false,
        ) {
            Ok(request) => request,
            Err(error) => return async move { Err(error.into()) }.boxed(),
        };

        let request_limiter = self.request_limiters.for_model(&model.id);
        let http_client = self.http_client.clone();
        let (api_key, api_url, custom_headers) = self.state.read_with(cx, |state, cx| {
            let api_url = Self::api_url(cx);
            (
                state.api_key_state.key(&api_url),
                api_url,
                Self::settings(cx).custom_headers.clone(),
            )
        });
        let executor = cx.background_executor().clone();

        let events = request_limiter.stream(async move {
            let Some(api_key) = api_key else {
                return Err(LanguageModelCompletionError::NoApiKey {
                    provider: PROVIDER_NAME,
                });
            };
            let headers = codebuddy::chat_headers(thread_id.as_deref(), &custom_headers)?;
            let events = open_ai::stream_completion(
                http_client.as_ref(),
                PROVIDER_NAME.0.as_ref(),
                &api_url,
                &api_key,
                request,
                &headers,
            )
            .await?;
            Ok(events.map(|event| {
                event.map(|mut event| {
                    codebuddy::normalize_stream_event(&mut event);
                    event
                })
            }))
        });

        async move {
            let events = events.await?;
            let mapper = ChatCompletionEventMapper::new();
            Ok(language_model::stream_in_background(
                mapper.map_stream(events.boxed()).boxed(),
                executor,
            ))
        }
        .boxed()
    }
}

fn language_model(model: &codebuddy::Model) -> LanguageModel {
    let max_input_tokens = if model.max_input_tokens > 0 {
        model.max_input_tokens
    } else {
        DEFAULT_MAX_INPUT_TOKENS
    };
    LanguageModel {
        supports_tools: model.supports_tools,
        supports_images: model.supports_images,
        tool_choice_support: LanguageModelToolChoiceSupport {
            auto: model.supports_tools,
            any: model.supports_tools,
            none: true,
        },
        supports_streaming_tools: true,
        supports_thinking: model.supports_reasoning(),
        supported_effort_levels: model
            .reasoning_efforts
            .iter()
            .map(|effort| LanguageModelEffortLevel {
                name: effort.label().into(),
                value: effort.value().into(),
                is_default: Some(*effort) == model.default_reasoning_effort,
                supports_disabling_thinking: None,
            })
            .collect(),
        supports_split_token_display: true,
        max_output_tokens: model.max_output_tokens,
        ..LanguageModel::new(
            LanguageModelId::from(model.id.clone()),
            LanguageModelName::from(model.display_name.clone()),
            PROVIDER_ID,
            PROVIDER_NAME,
            format!("codebuddy/{}", model.id),
            max_input_tokens,
        )
    }
}

/// Without an explicit selection the catalog default applies, and when it has none the effort
/// is omitted so CodeBuddy picks its own.
fn reasoning_effort(
    request: &LanguageModelRequest,
    model: &codebuddy::Model,
) -> Option<ReasoningEffort> {
    if !request.thinking_allowed || !model.supports_reasoning() {
        return None;
    }
    request
        .thinking_effort
        .as_deref()
        .and_then(|effort| effort.parse::<ReasoningEffort>().ok())
        .filter(|effort| model.reasoning_efforts.contains(effort))
        .or(model.default_reasoning_effort)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(reasoning_efforts: Vec<ReasoningEffort>) -> codebuddy::Model {
        codebuddy::Model {
            id: "glm-5".into(),
            display_name: "GLM-5".into(),
            max_input_tokens: 0,
            max_output_tokens: Some(32_000),
            supports_tools: true,
            supports_images: false,
            temperature: None,
            default_reasoning_effort: reasoning_efforts.last().copied(),
            reasoning_efforts,
        }
    }

    #[test]
    fn reasoning_effort_respects_catalog() {
        let reasoning_model = model(vec![ReasoningEffort::Low, ReasoningEffort::High]);
        let mut request = LanguageModelRequest {
            thinking_allowed: true,
            thinking_effort: Some("low".into()),
            ..Default::default()
        };
        assert_eq!(
            reasoning_effort(&request, &reasoning_model),
            Some(ReasoningEffort::Low)
        );

        request.thinking_effort = Some("max".into());
        assert_eq!(
            reasoning_effort(&request, &reasoning_model),
            Some(ReasoningEffort::High)
        );

        request.thinking_allowed = false;
        assert_eq!(reasoning_effort(&request, &reasoning_model), None);

        request.thinking_allowed = true;
        assert_eq!(reasoning_effort(&request, &model(Vec::new())), None);
    }

    #[test]
    fn language_model_reflects_catalog() {
        let model = language_model(&model(vec![ReasoningEffort::Low, ReasoningEffort::High]));
        assert_eq!(model.max_token_count, DEFAULT_MAX_INPUT_TOKENS);
        assert!(model.supports_thinking);
        let defaults: Vec<_> = model
            .supported_effort_levels
            .iter()
            .map(|level| (level.value.as_ref(), level.is_default))
            .collect();
        assert_eq!(defaults, [("low", false), ("high", true)]);
    }
}
