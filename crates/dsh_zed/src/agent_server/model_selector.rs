use super::SessionState;
use acp_thread::{AgentModelId, AgentModelInfo, AgentModelList, AgentModelSelector, model_catalog};
use agent_settings::{AgentSettings, language_model_to_selection};
use anyhow::{Context as _, Result, bail};
use collections::HashSet;
use gpui::{App, Task};
use language_model::LanguageModelRegistry;
use settings::{Settings, update_settings_file};
use std::rc::Rc;

pub(super) struct Selector(pub(super) Rc<SessionState>);
impl AgentModelSelector for Selector {
    fn list_models(&self, cx: &mut App) -> Task<Result<AgentModelList>> {
        Task::ready(Ok(model_catalog::catalog(cx).1))
    }
    fn selected_model(&self, cx: &mut App) -> Task<Result<AgentModelInfo>> {
        Task::ready((|| {
            let model = self
                .0
                .controls
                .read(cx)
                .model()
                .context("Choose a configured Zed model")?;
            let provider = LanguageModelRegistry::read_global(cx).provider_for_model(model)?;
            Ok(model_catalog::model_info(model, &provider))
        })())
    }
    fn select_model(&self, id: AgentModelId, cx: &mut App) -> Task<Result<()>> {
        Task::ready((|| {
            if self.0.closed.get() {
                bail!("DSH session is closed");
            }
            let model = model_catalog::catalog(cx)
                .0
                .remove(&id)
                .context("Model is unavailable; configure its provider in Zed settings")?;
            anyhow::ensure!(
                model.is_disabled().is_none(),
                "This model is currently unavailable"
            );
            self.0
                .controls
                .update(cx, |controls, cx| controls.select_model(model, true, cx));
            Ok(())
        })())
    }
    fn watch(&self, cx: &mut App) -> Option<watch::Receiver<()>> {
        Some(self.0.controls.read(cx).watch())
    }
    fn favorite_model_ids(&self, cx: &mut App) -> HashSet<AgentModelId> {
        AgentSettings::get_global(cx)
            .favorite_model_ids()
            .into_iter()
            .map(AgentModelId::from)
            .collect()
    }
    fn toggle_favorite_model(&self, id: AgentModelId, favorite: bool, cx: &App) {
        let Some(model) = model_catalog::catalog(cx).0.remove(&id) else {
            return;
        };
        let controls = self.0.controls.read(cx);
        let mut selection = language_model_to_selection(&model, None);
        if controls
            .model()
            .is_some_and(|selected| crate::model_key(selected) == crate::model_key(&model))
        {
            selection.enable_thinking = controls.thinking_enabled();
            selection.effort = controls.thinking_effort().cloned();
        }
        update_settings_file(
            controls.project().read(cx).fs().clone(),
            cx,
            move |settings, _| {
                let agent = settings.agent.get_or_insert_default();
                if favorite {
                    agent.add_favorite_model(selection.clone());
                } else {
                    agent.remove_favorite_model(&selection);
                }
            },
        );
    }
    fn should_render_footer(&self) -> bool {
        true
    }
}
