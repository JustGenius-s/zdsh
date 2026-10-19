use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use credentials_provider::CredentialsProvider;
use cursor_agent::{CursorModel, PROVIDER_ID, PROVIDER_NAME, RunRegistry, State, language_model};
use futures::{FutureExt as _, future::BoxFuture};
use gpui::{App, AsyncApp, Context, Entity, SharedString, Task, Window};
use http_client::HttpClient;
use language_model::{
    AuthenticateError, IconOrSvg, InlineDescription, LanguageModel, LanguageModelClient,
    LanguageModelCompletionError, LanguageModelCompletionStream, LanguageModelProvider,
    LanguageModelProviderId, LanguageModelProviderName, LanguageModelProviderState,
    LanguageModelRequest, ProviderErrorCategory, ProviderSettingsView, unavailable_error,
};
use ui::{ConfiguredApiCard, prelude::*};

const DESCRIPTION: &str = "Sign in with your Cursor account to use Cursor's models in Zed's agent.";

pub struct CursorLanguageModelProvider {
    state: Entity<State>,
    runs: Arc<RunRegistry>,
}

impl CursorLanguageModelProvider {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        let state = cx.new(|cx| State::new(http_client, credentials_provider, cx));
        Self {
            state,
            runs: Arc::new(RunRegistry::default()),
        }
    }

    /// The current catalog entry for `model`, if Cursor still offers it.
    fn config(
        &self,
        model: &LanguageModel,
        cx: &App,
    ) -> Result<CursorModel, LanguageModelCompletionError> {
        self.state
            .read(cx)
            .models()
            .iter()
            .find(|candidate| candidate.id == model.id.0.as_ref())
            .cloned()
            .ok_or_else(|| unavailable_error(model))
    }
}

impl LanguageModelProviderState for CursorLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for CursorLanguageModelProvider {
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
        self.state.read(cx).models().first().map(language_model)
    }

    fn default_fast_model(&self, _cx: &App) -> Option<LanguageModel> {
        None
    }

    fn provided_models(&self, cx: &App) -> Vec<LanguageModel> {
        let state = self.state.read(cx);
        if !state.is_authenticated() {
            return Vec::new();
        }
        state.models().iter().map(language_model).collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        if self.is_authenticated(cx) {
            return Task::ready(Ok(()));
        }
        let Some(load_task) = self.state.read(cx).load_task() else {
            return Task::ready(Err(AuthenticateError::CredentialsNotFound));
        };
        let state = self.state.downgrade();
        cx.spawn(async move |cx| {
            load_task.await;
            let is_authenticated = state
                .read_with(&*cx, |state, _| state.is_authenticated())
                .map_err(AuthenticateError::Other)?;
            if is_authenticated {
                Ok(())
            } else {
                Err(AuthenticateError::CredentialsNotFound)
            }
        })
    }

    fn settings_view(&self, cx: &mut App) -> Option<ProviderSettingsView> {
        let is_authenticated = self.state.read(cx).is_authenticated();
        Some(ProviderSettingsView::Inline(
            language_model::InlineProviderSettings {
                title: (!is_authenticated).then(|| "Configure Cursor".into()),
                description: (!is_authenticated)
                    .then(|| InlineDescription::Text(DESCRIPTION.into())),
                create_view: Arc::new({
                    let state = self.state.clone();
                    move |_window, cx| {
                        cx.new(|_cx| ConfigurationView {
                            state: state.clone(),
                        })
                        .into()
                    }
                }),
            },
        ))
    }

    fn authentication_error_message(&self) -> SharedString {
        "Your Cursor session is invalid or has expired. \
        Sign in again via Settings > AI > LLM Providers to continue."
            .into()
    }

    fn missing_credentials_error_message(&self) -> SharedString {
        "You are not signed in to Cursor. \
        Sign in via Settings > AI > LLM Providers to continue."
            .into()
    }
}

impl LanguageModelClient for CursorLanguageModelProvider {
    fn stream_completion(
        &self,
        model: &LanguageModel,
        request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<LanguageModelCompletionStream, LanguageModelCompletionError>>
    {
        let prepared = cx.update(|cx| {
            let config = self.config(model, cx)?;
            let state = self.state.read(cx);
            let credentials = state.credentials().ok_or_else(|| {
                LanguageModelCompletionError::from_provider_response(
                    PROVIDER_NAME,
                    None,
                    None,
                    self.missing_credentials_error_message().to_string(),
                    None,
                    ProviderErrorCategory::Authentication,
                )
            })?;
            Ok((config, credentials, state.http_client()))
        });
        let (config, credentials, http_client) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return async move { Err(error) }.boxed(),
        };
        let stream = cursor_agent::stream_completion(
            &self.runs,
            http_client,
            credentials,
            &config,
            request,
            cx.background_executor().clone(),
        );
        async move { Ok(stream) }.boxed()
    }
}

fn expiry_label(expires_at: SystemTime) -> String {
    let days = expires_at
        .duration_since(SystemTime::now())
        .map(|remaining| remaining.as_secs() / 86_400)
        .unwrap_or(0);
    match days {
        0 => "expires today".to_string(),
        1 => "expires in 1 day".to_string(),
        days => format!("expires in {days} days"),
    }
}

struct ConfigurationView {
    state: Entity<State>,
}

impl Render for ConfigurationView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);

        if state.is_authenticated() {
            let label = match state.expires_at() {
                Some(expires_at) => format!("Signed in · {}", expiry_label(expires_at)),
                None => "Signed in".to_string(),
            };
            let models_error = state.models_error();
            let state_entity = self.state.clone();
            return v_flex()
                .gap_2()
                .child(
                    ConfiguredApiCard::new("cursor-sign-out", SharedString::from(label))
                        .button_label("Sign Out")
                        .on_click(cx.listener(move |_this, _, _window, cx| {
                            state_entity
                                .update(cx, |state, cx| state.sign_out(cx))
                                .detach_and_log_err(cx);
                        })),
                )
                .when_some(models_error, |this, error| this.child(render_error(error)))
                .into_any_element();
        }

        let is_signing_in = state.is_signing_in();
        let error = state.last_auth_error().or_else(|| {
            state
                .is_session_expired()
                .then(|| "Your Cursor session expired. Sign in again.".into())
        });
        let sign_in_state = self.state.clone();
        let cancel_state = self.state.clone();
        let button_label = if is_signing_in {
            "Waiting for Browser…"
        } else {
            "Sign In"
        };

        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cursor-sign-in", button_label)
                            .style(ButtonStyle::Outlined)
                            .size(ButtonSize::Medium)
                            .loading(is_signing_in)
                            .disabled(is_signing_in)
                            .on_click(move |_, _window, cx| {
                                sign_in_state.update(cx, |state, cx| state.sign_in(cx));
                            }),
                    )
                    .when(is_signing_in, |this| {
                        this.child(
                            Button::new("cursor-cancel-sign-in", "Cancel")
                                .style(ButtonStyle::Subtle)
                                .size(ButtonSize::Medium)
                                .on_click(move |_, _window, cx| {
                                    cancel_state.update(cx, |state, cx| state.cancel_sign_in(cx));
                                }),
                        )
                    }),
            )
            .when_some(error, |this, error| this.child(render_error(error)))
            .into_any_element()
    }
}

fn render_error(error: SharedString) -> impl IntoElement {
    h_flex()
        .gap_1()
        .child(
            Icon::new(IconName::XCircle)
                .color(Color::Error)
                .size(IconSize::Small),
        )
        .child(Label::new(error).color(Color::Muted))
}
