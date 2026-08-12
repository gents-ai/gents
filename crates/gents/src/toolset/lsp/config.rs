use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::CatalogServer;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LspConfigDocument {
    #[serde(default)]
    pub idle_timeout_ms: Option<u64>,
    #[serde(default)]
    pub format_on_write: Option<bool>,
    #[serde(default)]
    pub diagnostics_on_write: Option<bool>,
    #[serde(default)]
    pub diagnostics_on_edit: Option<bool>,
    #[serde(default)]
    pub diagnostics_deduplicate: Option<bool>,
    #[serde(default)]
    pub network_mode: Option<String>,
    #[serde(default)]
    pub servers: Option<serde_json::Map<String, Value>>,
}

impl LspConfigDocument {
    pub fn parse_operator(raw: Option<&str>) -> Self {
        parse_with_allowlist(raw, false)
    }

    pub fn parse_self_config(raw: Option<&str>) -> Result<Self, String> {
        if let Some(raw) = raw {
            if let Ok(value) = serde_json::from_str::<Value>(raw) {
                reject_self_config_keys(&value)?;
            }
        }
        Ok(parse_with_allowlist(raw, true))
    }

    pub fn idle_timeout(&self) -> std::time::Duration {
        let ms = self.idle_timeout_ms.unwrap_or(300_000);
        std::time::Duration::from_millis(ms.max(1))
    }
}

fn parse_with_allowlist(raw: Option<&str>, self_config: bool) -> LspConfigDocument {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return LspConfigDocument::default();
    };
    let Ok(mut parsed) = serde_json::from_str::<LspConfigDocument>(raw) else {
        return LspConfigDocument::default();
    };
    if self_config {
        if let Some(servers) = parsed.servers.as_mut() {
            for value in servers.values_mut() {
                if let Some(obj) = value.as_object_mut() {
                    obj.remove("settings");
                    obj.remove("init_options");
                    obj.remove("initOptions");
                    obj.remove("command");
                    obj.remove("args");
                    obj.remove("capabilities");
                    obj.remove("workspace_ready_timings");
                    obj.remove("language_id");
                }
            }
        }
    }
    if let Some(servers) = parsed.servers.as_mut() {
        for value in servers.values_mut() {
            if let Some(obj) = value.as_object_mut() {
                obj.remove("command");
                obj.remove("args");
                obj.remove("resolvedCommand");
                obj.remove("createClient");
            }
        }
    }
    parsed
}

fn reject_self_config_keys(value: &Value) -> Result<(), String> {
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    if let Some(servers) = obj.get("servers").and_then(Value::as_object) {
        for (name, server) in servers {
            if let Some(fields) = server.as_object() {
                for forbidden in [
                    "command",
                    "args",
                    "settings",
                    "init_options",
                    "initOptions",
                    "capabilities",
                    "workspace_ready_timings",
                    "language_id",
                ] {
                    if fields.contains_key(forbidden) {
                        return Err(format!(
                            "self-config cannot patch servers.{name}.{forbidden}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn apply_overrides(
    mut catalog: Vec<CatalogServer>,
    doc: &LspConfigDocument,
) -> Vec<CatalogServer> {
    let Some(overrides) = &doc.servers else {
        return catalog;
    };
    catalog.retain(|server| {
        overrides
            .get(&server.name)
            .and_then(|v| v.get("disabled"))
            .and_then(Value::as_bool)
            != Some(true)
    });
    for server in &mut catalog {
        if let Some(over) = overrides.get(&server.name).and_then(Value::as_object) {
            if let Some(priority) = over.get("priority").and_then(Value::as_u64) {
                server.priority = priority as u16;
            }
            if let Some(settings) = over.get("settings") {
                server.settings = Some(settings.clone());
            }
            if let Some(init) = over.get("init_options").or_else(|| over.get("initOptions")) {
                server.init_options = Some(init.clone());
            }
        }
    }
    catalog.retain(|server| overrides.keys().any(|k| k == &server.name) || true);
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_config_rejects_settings() {
        let raw = r#"{"servers":{"rust-analyzer":{"settings":{"x":1}}}}"#;
        let err = LspConfigDocument::parse_self_config(Some(raw)).unwrap_err();
        assert!(err.contains("settings"), "{err}");
    }

    #[test]
    fn operator_config_may_include_settings() {
        let raw = r#"{"servers":{"rust-analyzer":{"settings":{"x":1}}}}"#;
        let parsed = LspConfigDocument::parse_operator(Some(raw));
        let settings = parsed
            .servers
            .as_ref()
            .unwrap()
            .get("rust-analyzer")
            .unwrap()
            .get("settings");
        assert!(settings.is_some());
    }
}
