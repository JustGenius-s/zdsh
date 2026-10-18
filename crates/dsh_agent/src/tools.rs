use crate::{Tool, ToolDefinition};
use dsh_runtime::Service;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("tool name cannot be empty")]
    EmptyName,
    #[error("tool {0} is already registered")]
    Duplicate(String),
}

/// Tool collection assembled before activation and provided as one plugin service.
/// Metadata is captured at registration so requests and execution use the same names.
/// This version does not support mutating a mounted registry or per-tool unloading.
#[derive(Default)]
pub struct ToolRegistry {
    entries: BTreeMap<String, (ToolDefinition, Arc<dyn Tool>)>,
}

impl ToolRegistry {
    /// Registers a tool; invalid or duplicate names leave the registry unchanged.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), RegistrationError> {
        let definition = tool.definition();
        if definition.name.trim().is_empty() {
            return Err(RegistrationError::EmptyName);
        }
        if self.entries.contains_key(&definition.name) {
            return Err(RegistrationError::Duplicate(definition.name));
        }
        self.entries
            .insert(definition.name.clone(), (definition, tool));
        Ok(())
    }

    /// Returns model-visible definitions in deterministic name order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.entries
            .values()
            .map(|(definition, _)| definition.clone())
            .collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.entries.get(name).map(|(_, tool)| tool)
    }
}

pub struct ToolsService;
impl Service for ToolsService {
    type Value = ToolRegistry;
}
