use agent_settings::{
    AgentProfileId, AgentSettings, builtin_profiles, language_model_to_selection,
};
use anyhow::{Context as _, Result};
use dsh_agent::{ModelOptions, TurnOptions};
use extension::ExtensionHostProxy;
use gpui::{App, Context, Entity, Subscription};
use language_model::{LanguageModel, LanguageModelRegistry};
use project::{Project, trusted_worktrees::TrustedWorktrees};
use settings::{LanguageModelSelection, Settings, SettingsStore, update_settings_file};

/// Zed profile and model preferences for a DSH conversation. Each turn captures a snapshot.
pub struct SessionControls {
    project: Entity<Project>,
    model: Option<LanguageModel>,
    profile: AgentProfileId,
    thinking_enabled: bool,
    thinking_effort: Option<String>,
    changes: watch::Sender<()>,
    extensions: std::sync::Arc<ExtensionHostProxy>,
    _subscriptions: Vec<Subscription>,
}

impl SessionControls {
    pub(crate) fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let profile = AgentSettings::get_global(cx).default_profile.clone();
        let preference = AgentSettings::get_global(cx)
            .profiles
            .get(&profile)
            .and_then(|profile| profile.default_model.clone())
            .or_else(|| AgentSettings::get_global(cx).default_model.clone());
        let model = preference
            .as_ref()
            .and_then(|preference| resolve_model(preference, cx))
            .or_else(|| LanguageModelRegistry::read_global(cx).default_model());
        let selection = model
            .as_ref()
            .map(|model| language_model_to_selection(model, preference.as_ref()));
        let registry = LanguageModelRegistry::global(cx);
        let subscription = cx.subscribe(&registry, |this, _, _, cx| {
            if let Some(model) = this.model.as_ref() {
                let identity = crate::model_key(model);
                if let Some(model) = acp_thread::model_catalog::catalog(cx)
                    .0
                    .values()
                    .find(|model| crate::model_key(model) == identity)
                {
                    this.model = Some(model.clone());
                }
            } else if let Some(model) = LanguageModelRegistry::read_global(cx).default_model() {
                this.select_model(model, false, cx);
            }
            this.notify(cx);
        });
        Self {
            project,
            model,
            profile,
            thinking_enabled: selection
                .as_ref()
                .is_some_and(|selection| selection.enable_thinking),
            thinking_effort: selection.and_then(|selection| selection.effort),
            changes: watch::channel(()).0,
            extensions: ExtensionHostProxy::default_global(cx),
            _subscriptions: vec![
                subscription,
                cx.observe_global::<SettingsStore>(|this, cx| this.notify(cx)),
            ],
        }
    }

    pub fn model(&self) -> Option<&LanguageModel> {
        self.model.as_ref()
    }
    pub fn thinking_enabled(&self) -> bool {
        self.thinking_enabled
    }
    pub fn thinking_effort(&self) -> Option<&String> {
        self.thinking_effort.as_ref()
    }
    pub fn is_restricted(&self, cx: &App) -> bool {
        TrustedWorktrees::has_restricted_worktrees(&self.project.read(cx).worktree_store(), cx)
    }
    pub fn profile(&self, cx: &App) -> AgentProfileId {
        if self.is_restricted(cx) {
            AgentProfileId(builtin_profiles::MINIMAL.into())
        } else {
            self.profile.clone()
        }
    }
    pub fn profile_downgraded(&self, cx: &App) -> bool {
        self.profile(cx) != self.profile
    }

    pub fn set_profile(&mut self, profile: AgentProfileId, cx: &mut Context<Self>) {
        if self.profile == profile {
            return;
        }
        self.profile = profile;
        if let Some(preference) = AgentSettings::get_global(cx)
            .profiles
            .get(&self.profile)
            .and_then(|profile| profile.default_model.clone())
        {
            if let Some(model) = resolve_model(&preference, cx) {
                let selection = language_model_to_selection(&model, Some(&preference));
                self.model = Some(model);
                self.thinking_enabled = selection.enable_thinking;
                self.thinking_effort = selection.effort;
            }
        }
        self.notify(cx);
    }

    pub(crate) fn select_model(
        &mut self,
        model: LanguageModel,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        let settings = AgentSettings::get_global(cx);
        let preference = settings.favorite_models.iter().find(|entry| {
            entry.provider.0.as_str() == model.provider_id.0.as_ref()
                && entry.model == model.id.0.as_ref()
        });
        let selection = language_model_to_selection(&model, preference);
        self.model = Some(model);
        self.thinking_enabled = selection.enable_thinking;
        self.thinking_effort = selection.effort;
        if persist {
            self.persist(cx);
        }
        self.notify(cx);
    }

    pub fn set_thinking_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(model) = &self.model else {
            return;
        };
        self.thinking_enabled = model.supports_thinking
            && (enabled
                || !model.supports_disabling_thinking_at_effort(self.thinking_effort.as_deref()));
        self.persist(cx);
        self.notify(cx);
    }

    pub fn set_thinking_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        let Some(model) = &self.model else {
            return;
        };
        self.thinking_effort = effort.filter(|value| {
            model
                .supported_effort_levels
                .iter()
                .any(|level| level.value.as_ref() == value)
        });
        self.persist(cx);
        self.notify(cx);
    }

    pub(crate) fn watch(&self) -> watch::Receiver<()> {
        self.changes.receiver()
    }
    pub(crate) fn project(&self) -> &Entity<Project> {
        &self.project
    }

    fn notify(&mut self, cx: &mut Context<Self>) {
        if let Err(_no_picker) = self.changes.send(()) {
            // A picker opened later reads the current model and catalog directly.
        }
        cx.notify();
    }

    fn persist(&self, cx: &App) {
        let Some(model) = &self.model else {
            return;
        };
        let selection = LanguageModelSelection {
            provider: model.provider_id.0.to_string().into(),
            model: model.id.0.to_string(),
            enable_thinking: self.thinking_enabled,
            effort: self.thinking_effort.clone(),
            speed: None,
        };
        update_settings_file(
            self.project.read(cx).fs().clone(),
            cx,
            move |settings, _| {
                let agent = settings.agent.get_or_insert_default();
                agent.set_model(selection.clone());
                agent.update_favorite_model(&selection.provider.0, &selection.model, |favorite| {
                    favorite.enable_thinking = selection.enable_thinking;
                    favorite.effort = selection.effort.clone();
                });
            },
        );
    }

    pub(crate) fn turn_options(&self, cx: &App) -> Result<TurnOptions> {
        let model = self.model.as_ref().context(
            "Configure a model provider in Zed settings and choose a model before sending",
        )?;
        let settings = AgentSettings::get_global(cx);
        let profile_id = self.profile(cx);
        let profile = settings
            .profiles
            .get(&profile_id)
            .context("The selected Agent profile no longer exists; choose another mode")?;
        let allowed_tools = self
            .extensions
            .agent_tools
            .snapshot()
            .into_iter()
            .filter(|(name, _, _)| {
                model.supports_tools
                    && !self.is_restricted(cx)
                    && profile
                        .tools
                        .get(name.as_str())
                        .copied()
                        .unwrap_or(profile.enable_all_context_servers)
            })
            .map(|(name, _, _)| name)
            .collect();
        let thinking_effort = self.thinking_effort.clone().filter(|value| {
            model
                .supported_effort_levels
                .iter()
                .any(|level| level.value.as_ref() == value)
        });
        Ok(TurnOptions {
            model: crate::model_key(model),
            model_options: ModelOptions {
                thinking_allowed: model.supports_thinking
                    && (self.thinking_enabled
                        || !model
                            .supports_disabling_thinking_at_effort(thinking_effort.as_deref())),
                thinking_effort,
                max_output_tokens: None,
            },
            allowed_tools,
        })
    }
}

fn resolve_model(selection: &LanguageModelSelection, cx: &App) -> Option<LanguageModel> {
    LanguageModelRegistry::read_global(cx)
        .visible_providers()
        .iter()
        .flat_map(|provider| provider.provided_models(cx))
        .find(|model| {
            model.provider_id.0.as_ref() == selection.provider.0.as_str()
                && model.id.0.as_ref() == selection.model
        })
}
