use crate::{AgentModelId, AgentModelList};
use collections::{HashMap, IndexMap};
use gpui::App;
use language_model::{IconOrSvg, LanguageModel, LanguageModelProvider, LanguageModelRegistry};
use std::sync::Arc;

/// Zed's authenticated model catalog, including recommended and provider groups.
pub fn catalog(cx: &App) -> (HashMap<AgentModelId, LanguageModel>, AgentModelList) {
    let providers = LanguageModelRegistry::global(cx)
        .read(cx)
        .visible_providers()
        .into_iter()
        .filter(|provider| provider.is_authenticated(cx))
        .collect::<Vec<_>>();

    let mut language_model_list = IndexMap::default();

    let mut recommended = Vec::new();
    for provider in &providers {
        for model in provider.recommended_models(cx) {
            recommended.push(model_info(&model, provider));
        }
    }
    if !recommended.is_empty() {
        language_model_list.insert(
            crate::AgentModelGroupName("Recommended".into()),
            recommended,
        );
    }

    let mut models = HashMap::default();
    for provider in providers {
        let mut provider_models = Vec::new();
        for model in provider.provided_models(cx) {
            let model_info = model_info(&model, &provider);
            let model_id = model_info.id.clone();
            provider_models.push(model_info);
            models.insert(model_id, model);
        }
        if !provider_models.is_empty() {
            language_model_list.insert(
                crate::AgentModelGroupName(provider.name().0.clone()),
                provider_models,
            );
        }
    }

    (models, AgentModelList::Grouped(language_model_list))
}

/// The common model label, provider icon, pricing and availability used in Agent pickers.
pub fn model_info(
    model: &LanguageModel,
    provider: &Arc<dyn LanguageModelProvider>,
) -> crate::AgentModelInfo {
    crate::AgentModelInfo {
        id: AgentModelId::new(format!("{}/{}", model.provider_id().0, model.id().0)),
        name: model.name().0,
        description: None,
        icon: Some(match provider.icon() {
            IconOrSvg::Svg(path) => crate::AgentModelIcon::Path(path),
            IconOrSvg::Icon(name) => crate::AgentModelIcon::Named(name),
        }),
        is_latest: model.is_latest(),
        cost: model.model_cost_info().map(|cost| cost.to_shared_string()),
        disabled: model.is_disabled(),
    }
}
