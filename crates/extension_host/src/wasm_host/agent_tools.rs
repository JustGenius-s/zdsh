use super::WasmState;

wasmtime::component::bindgen!({
    path: "../extension_api/wit/agent-tools",
    imports: { default: async | trappable },
    exports: { default: async },
});

impl zdsh::agent::cancellation::Host for WasmState {
    async fn is_cancelled(&mut self) -> wasmtime::Result<bool> {
        Ok(self
            .agent_tool_cancellation
            .as_ref()
            .is_none_or(|cancellation| cancellation.is_cancelled()))
    }
}
