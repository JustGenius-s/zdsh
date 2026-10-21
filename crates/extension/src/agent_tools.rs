use crate::Extension;
use anyhow::{Context as _, Result, bail, ensure};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolDefinition {
    pub description: String,
    pub input_schema: Value,
}

/// Cancellation is cooperative; the caller must await completion after requesting it.
#[derive(Clone, Default)]
pub struct AgentToolCancellation {
    requested: Arc<AtomicBool>,
    revoked: Arc<AtomicBool>,
}
impl AgentToolCancellation {
    pub fn cancel(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::SeqCst) || self.revoked.load(Ordering::SeqCst)
    }
}

trait AgentToolExecutor: Send + Sync {
    fn execute(
        &self,
        name: String,
        arguments: Value,
        cancellation: AgentToolCancellation,
    ) -> futures::future::BoxFuture<'_, Result<Value>>;
}
struct WasmToolExecutor(Arc<dyn Extension>);
impl AgentToolExecutor for WasmToolExecutor {
    fn execute(
        &self,
        name: String,
        arguments: Value,
        cancellation: AgentToolCancellation,
    ) -> futures::future::BoxFuture<'_, Result<Value>> {
        self.0.run_agent_tool(name, arguments, cancellation)
    }
}

pub struct ExtensionAgentTool {
    pub extension_id: Arc<str>,
    pub name: String,
    pub local_name: String,
    pub definition: AgentToolDefinition,
    executor: Arc<dyn AgentToolExecutor>,
    revoked: Arc<AtomicBool>,
    validator: jsonschema::Validator,
    execution: futures::lock::Mutex<()>,
}
impl ExtensionAgentTool {
    pub fn cancellation(&self) -> AgentToolCancellation {
        AgentToolCancellation {
            requested: Arc::default(),
            revoked: self.revoked.clone(),
        }
    }
    pub async fn execute(
        &self,
        arguments: Value,
        cancellation: AgentToolCancellation,
    ) -> Result<Value> {
        let _execution = self.execution.lock().await;
        ensure!(
            !self.revoked.load(Ordering::SeqCst),
            "extension tool is unloaded; start a new chat to refresh tools"
        );
        ensure!(!cancellation.is_cancelled(), "extension tool cancelled");
        if let Some(error) = self.validator.iter_errors(&arguments).next() {
            bail!("invalid tool arguments: {error}");
        }
        self.executor
            .execute(self.local_name.clone(), arguments, cancellation)
            .await
    }
}

/// Contributions owned by Zed's existing extension host. Snapshots hold weak references
/// so an old chat cannot keep an unloaded Wasm instance alive or call a replacement.
#[derive(Default)]
pub struct ExtensionAgentTools {
    entries: RwLock<BTreeMap<Arc<str>, Vec<Arc<ExtensionAgentTool>>>>,
}
impl ExtensionAgentTools {
    pub fn register(&self, extension: Arc<dyn Extension>) -> Result<()> {
        let manifest = extension.manifest();
        self.register_tools(
            manifest.id.clone(),
            &manifest.agent_tools,
            Arc::new(WasmToolExecutor(extension)),
        )
    }
    fn register_tools(
        &self,
        id: Arc<str>,
        definitions: &BTreeMap<String, AgentToolDefinition>,
        executor: Arc<dyn AgentToolExecutor>,
    ) -> Result<()> {
        let revoked = Arc::new(AtomicBool::new(false));
        let mut tools = Vec::new();
        for (local_name, definition) in definitions {
            let name = format!("{id}_{local_name}");
            ensure!(
                name.len() <= 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
                "Agent tool name {name:?} must contain at most 64 ASCII letters, digits, underscores or hyphens"
            );
            ensure!(
                !local_name.is_empty() && !definition.description.trim().is_empty(),
                "Agent tool name and description are required"
            );
            ensure!(
                definition.input_schema.get("type").and_then(Value::as_str) == Some("object"),
                "Agent tool {name} must accept an object"
            );
            let validator = jsonschema::options()
                .with_retriever(LocalSchemasOnly)
                .build(&definition.input_schema)
                .context("invalid Agent tool input schema")?;
            tools.push(Arc::new(ExtensionAgentTool {
                extension_id: id.clone(),
                name,
                local_name: local_name.clone(),
                definition: definition.clone(),
                executor: executor.clone(),
                revoked: revoked.clone(),
                validator,
                execution: futures::lock::Mutex::new(()),
            }));
        }
        let mut entries = self.entries.write();
        ensure!(
            !entries.contains_key(&id),
            "Agent tools for {} are already registered",
            id
        );
        for existing in entries.values().flatten() {
            ensure!(
                !tools.iter().any(|tool| tool.name == existing.name),
                "duplicate Agent tool name {}",
                existing.name
            );
        }
        entries.insert(id, tools);
        Ok(())
    }
    /// Revokes admission synchronously, then waits for admitted tools to finish cleanup.
    pub fn unregister(&self, extension_id: &str) -> impl Future<Output = ()> + Send + use<> {
        let tools = self
            .entries
            .write()
            .remove(extension_id)
            .unwrap_or_default();
        for tool in &tools {
            tool.revoked.store(true, Ordering::SeqCst);
        }
        async move {
            for tool in tools {
                let _finished = tool.execution.lock().await;
            }
        }
    }
    pub fn snapshot(&self) -> Vec<(String, AgentToolDefinition, Weak<ExtensionAgentTool>)> {
        self.entries
            .read()
            .values()
            .flatten()
            .map(|tool| {
                (
                    tool.name.clone(),
                    tool.definition.clone(),
                    Arc::downgrade(tool),
                )
            })
            .collect()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ExtensionAgentTools {
    /// Registers tools for extension `id` whose calls run `execute` instead of a Wasm instance.
    pub fn register_fake(
        &self,
        id: Arc<str>,
        definitions: &BTreeMap<String, AgentToolDefinition>,
        execute: impl Fn(
            String,
            Value,
            AgentToolCancellation,
        ) -> futures::future::BoxFuture<'static, Result<Value>>
        + Send
        + Sync
        + 'static,
    ) -> Result<()> {
        struct FakeToolExecutor<F>(F);
        impl<F> AgentToolExecutor for FakeToolExecutor<F>
        where
            F: Fn(
                    String,
                    Value,
                    AgentToolCancellation,
                ) -> futures::future::BoxFuture<'static, Result<Value>>
                + Send
                + Sync,
        {
            fn execute(
                &self,
                name: String,
                arguments: Value,
                cancellation: AgentToolCancellation,
            ) -> futures::future::BoxFuture<'_, Result<Value>> {
                (self.0)(name, arguments, cancellation)
            }
        }
        self.register_tools(id, definitions, Arc::new(FakeToolExecutor(execute)))
    }
}

struct LocalSchemasOnly;
impl jsonschema::Retrieve for LocalSchemasOnly {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("external Agent tool schema references are not supported: {uri}").into())
    }
}

impl Drop for ExtensionAgentTools {
    fn drop(&mut self) {
        for tool in self.entries.get_mut().values().flatten() {
            tool.revoked.store(true, Ordering::SeqCst);
        }
    }
}
