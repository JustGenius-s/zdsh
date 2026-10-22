use anyhow::Result;
use serde_json::{Map, Value};

use crate::migrations::migrate_settings;

/// The sandbox no longer restricts network access, so its host allowlist
/// settings have no effect.
pub fn remove_sandbox_network_permissions(value: &mut Value) -> Result<()> {
    migrate_settings(value, &mut migrate_one)
}

fn migrate_one(settings: &mut Map<String, Value>) -> Result<()> {
    let Some(agent) = settings.get_mut("agent").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let Some(sandbox_permissions) = agent
        .get_mut("sandbox_permissions")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };

    let removed_all_hosts = sandbox_permissions.remove("allow_all_hosts").is_some();
    let removed_network_hosts = sandbox_permissions.remove("network_hosts").is_some();
    if (removed_all_hosts || removed_network_hosts) && sandbox_permissions.is_empty() {
        agent.remove("sandbox_permissions");
    }

    Ok(())
}

/// Thread permissions replaced both the `allow_unsandboxed` sandbox switch,
/// which becomes full access, and the global tool permission default.
pub fn migrate_agent_permission_defaults(value: &mut Value) -> Result<()> {
    migrate_settings(value, &mut migrate_permission_defaults_one)
}

fn migrate_permission_defaults_one(settings: &mut Map<String, Value>) -> Result<()> {
    let Some(agent) = settings.get_mut("agent").and_then(Value::as_object_mut) else {
        return Ok(());
    };

    let mut allow_unsandboxed = None;
    let mut sandbox_permissions_emptied = false;
    if let Some(sandbox_permissions) = agent
        .get_mut("sandbox_permissions")
        .and_then(Value::as_object_mut)
    {
        allow_unsandboxed = sandbox_permissions.remove("allow_unsandboxed");
        sandbox_permissions_emptied = allow_unsandboxed.is_some() && sandbox_permissions.is_empty();
    }
    if sandbox_permissions_emptied {
        agent.remove("sandbox_permissions");
    }
    if allow_unsandboxed == Some(Value::Bool(true)) {
        agent
            .entry("default_permissions")
            .or_insert_with(|| Value::String("full_access".to_string()));
    }

    let mut tool_permissions_emptied = false;
    if let Some(tool_permissions) = agent
        .get_mut("tool_permissions")
        .and_then(Value::as_object_mut)
    {
        let removed_default = tool_permissions.remove("default").is_some();
        let removed_default_mode = tool_permissions.remove("default_mode").is_some();
        tool_permissions_emptied =
            (removed_default || removed_default_mode) && tool_permissions.is_empty();
    }
    if tool_permissions_emptied {
        agent.remove("tool_permissions");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn removes_network_permissions_in_all_settings_scopes() {
        let mut value = json!({
            "agent": {
                "sandbox_permissions": {
                    "allow_all_hosts": true,
                    "network_hosts": ["github.com"],
                    "allow_fs_write_all": true
                }
            },
            "macos": {
                "agent": {
                    "sandbox_permissions": { "network_hosts": ["*.npmjs.org"] }
                }
            },
            "profiles": {
                "work": {
                    "settings": {
                        "agent": {
                            "sandbox_permissions": { "allow_all_hosts": false }
                        }
                    }
                }
            }
        });

        remove_sandbox_network_permissions(&mut value).expect("migration should succeed");

        assert_eq!(
            value,
            json!({
                "agent": {
                    "sandbox_permissions": { "allow_fs_write_all": true }
                },
                "macos": { "agent": {} },
                "profiles": { "work": { "settings": { "agent": {} } } }
            })
        );
    }

    #[test]
    fn leaves_settings_without_network_permissions_untouched() {
        let original = json!({
            "agent": {
                "sandbox_permissions": {},
                "default_model": { "provider": "anthropic", "model": "claude" }
            }
        });
        let mut value = original.clone();

        remove_sandbox_network_permissions(&mut value).expect("migration should succeed");

        assert_eq!(value, original);
    }

    #[test]
    fn allow_unsandboxed_becomes_full_access_and_global_default_is_removed() {
        let mut value = json!({
            "agent": {
                "sandbox_permissions": { "allow_unsandboxed": true },
                "tool_permissions": {
                    "default": "allow",
                    "tools": { "terminal": { "default": "confirm" } }
                }
            },
            "profiles": {
                "work": {
                    "settings": {
                        "agent": {
                            "sandbox_permissions": {
                                "allow_unsandboxed": false,
                                "allow_fs_write_all": true
                            },
                            "tool_permissions": { "default_mode": "deny" }
                        }
                    }
                }
            }
        });

        migrate_agent_permission_defaults(&mut value).expect("migration should succeed");

        assert_eq!(
            value,
            json!({
                "agent": {
                    "default_permissions": "full_access",
                    "tool_permissions": {
                        "tools": { "terminal": { "default": "confirm" } }
                    }
                },
                "profiles": {
                    "work": {
                        "settings": {
                            "agent": {
                                "sandbox_permissions": { "allow_fs_write_all": true }
                            }
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn allow_unsandboxed_keeps_an_explicit_default_permissions() {
        let mut value = json!({
            "agent": {
                "default_permissions": "read_only",
                "sandbox_permissions": { "allow_unsandboxed": true }
            }
        });

        migrate_agent_permission_defaults(&mut value).expect("migration should succeed");

        assert_eq!(
            value,
            json!({ "agent": { "default_permissions": "read_only" } })
        );
    }
}
