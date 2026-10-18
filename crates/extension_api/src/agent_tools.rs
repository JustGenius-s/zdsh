//! Optional Agent exports on the same Zed extension instance.
//! Requires a ZDSH host; ordinary Zed extensions need not enable this feature.

mod bindings {
    wit_bindgen::generate!({ path: "./wit/agent-tools" });
}

/// Returns true after the user cancels the call or the extension is unloaded.
/// Finish resource cleanup before returning from a cancelled call.
pub fn is_cancelled() -> bool {
    bindings::zdsh::agent::cancellation::is_cancelled()
}

struct Component;
bindings::export!(Component with_types_in bindings);

impl bindings::exports::zdsh::agent::tools::Guest for Component {
    fn execute(name: String, arguments: String) -> Result<String, String> {
        let arguments = serde_json::from_str(&arguments).map_err(|error| error.to_string())?;
        let result = super::extension().run_agent_tool(name, arguments)?;
        serde_json::to_string(&result).map_err(|error| error.to_string())
    }
}
