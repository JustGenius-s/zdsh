use super::SessionState;
use crate::model_key;
use acp_thread::{AgentModelId, AgentModelInfo, AgentModelList, AgentModelSelector};
use anyhow::{Context as _, Result, bail};
use gpui::{App, Task};
use language_model::{LanguageModel, LanguageModelRegistry};
use std::rc::Rc;

pub(super) struct Selector(pub(super) Rc<SessionState>);
fn model_info(model: &LanguageModel) -> AgentModelInfo {
    AgentModelInfo {
        id: model_key(model).into(),
        name: model.name.0.clone(),
        description: Some(model.provider_name.0.clone()),
        icon: None,
        is_latest: false,
        cost: None,
        disabled: model.is_disabled(),
    }
}

impl AgentModelSelector for Selector {
    fn list_models(&self, cx: &mut App) -> Task<Result<AgentModelList>> {
        let models = if self.0.runtime.borrow().is_some() {
            self.0.selected.borrow().iter().map(model_info).collect()
        } else {
            LanguageModelRegistry::read_global(cx)
                .visible_providers()
                .iter()
                .flat_map(|provider| provider.provided_models(cx))
                .map(|model| model_info(&model))
                .collect()
        };
        Task::ready(Ok(AgentModelList::Flat(models)))
    }
    fn selected_model(&self, _: &mut App) -> Task<Result<AgentModelInfo>> {
        Task::ready(
            self.0
                .selected
                .borrow()
                .as_ref()
                .map(model_info)
                .context("Choose a configured Zed model"),
        )
    }
    fn select_model(&self, id: AgentModelId, cx: &mut App) -> Task<Result<()>> {
        let result = (|| {
            if self.0.closed.get() {
                bail!("DSH session is closed");
            }
            if self
                .0
                .selected
                .borrow()
                .as_ref()
                .is_some_and(|model| model_key(model) == id.as_str())
            {
                return Ok(());
            }
            if self.0.running.get() || self.0.runtime.borrow().is_some() {
                bail!("Start a new DSH chat to change its model");
            }
            let model = LanguageModelRegistry::read_global(cx)
                .visible_providers()
                .iter()
                .flat_map(|provider| provider.provided_models(cx))
                .find(|model| model_key(model) == id.as_str())
                .context("Model is unavailable; configure its provider in Zed settings")?;
            self.0.selected.replace(Some(model));
            self.0.notify_model();
            Ok(())
        })();
        Task::ready(result)
    }
    fn watch(&self, _: &mut App) -> Option<watch::Receiver<()>> {
        Some(self.0.model_changes.borrow().receiver())
    }
}
