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
}
