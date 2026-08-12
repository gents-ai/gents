use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::{builtin_catalog, CatalogServer};

const OPERATOR_SERVER_KEYS: &[&str] = &[
    "disabled",
    "priority",
    "warmup_timeout_ms",
    "capabilities",
    "workspace_ready_timings",
    "language_id",
    "settings",
    "init_options",
    "initOptions",
];
const SELF_CONFIG_SERVER_KEYS: &[&str] = &["disabled", "priority", "warmup_timeout_ms"];
const TOP_LEVEL_KEYS: &[&str] = &[
    "idle_timeout_ms",
    "format_on_write",
    "diagnostics_on_write",
    "diagnostics_on_edit",
    "diagnostics_deduplicate",
    "network_mode",
    "servers",
];

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
    pub fn parse_operator(raw: Option<&str>) -> Result<Self, String> {
        parse_strict(raw, false)
    }

    pub fn parse_self_config(raw: Option<&str>) -> Result<Self, String> {
        parse_strict(raw, true)
    }

    pub fn idle_timeout(&self) -> std::time::Duration {
        let ms = self.idle_timeout_ms.unwrap_or(300_000);
        std::time::Duration::from_millis(ms.max(1))
    }
}

fn parse_strict(raw: Option<&str>, self_config: bool) -> Result<LspConfigDocument, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(LspConfigDocument::default());
    };
    let value: Value =
        serde_json::from_str(raw).map_err(|err| format!("invalid lsp_config JSON: {err}"))?;
    let Some(obj) = value.as_object() else {
        return Err("lsp_config must be a JSON object".into());
    };
    for key in obj.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown lsp_config field {key}"));
        }
    }
    if let Some(mode) = obj.get("network_mode").and_then(Value::as_str) {
        crate::toolset::CommandNetworkMode::parse(mode).map_err(|err| err.to_string())?;
    }
    if self_config {
        allow_server_keys(&value, SELF_CONFIG_SERVER_KEYS, "self-config")?;
    } else {
        allow_server_keys(&value, OPERATOR_SERVER_KEYS, "lsp_config")?;
    }
    let parsed: LspConfigDocument =
        serde_json::from_value(value).map_err(|err| format!("invalid lsp_config JSON: {err}"))?;
    if let Some(servers) = &parsed.servers {
        let catalog = builtin_catalog();
        for name in servers.keys() {
            if !catalog.iter().any(|server| &server.name == name) {
                return Err(format!("unknown language server {name}"));
            }
        }
    }
    Ok(parsed)
}

fn allow_server_keys(value: &Value, allowed: &[&str], origin: &str) -> Result<(), String> {
    let Some(servers) = value.get("servers").and_then(Value::as_object) else {
        return Ok(());
    };
    for (name, server) in servers {
        let Some(fields) = server.as_object() else {
            return Err(format!("{origin} servers.{name} must be an object"));
        };
        for key in fields.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("{origin} cannot set servers.{name}.{key}"));
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
            if let Some(caps) = over.get("capabilities") {
                server.capabilities = Some(caps.clone());
            }
            if let Some(timings) = over.get("workspace_ready_timings") {
                server.workspace_ready_timings = Some(timings.clone());
            }
            if let Some(language_id) = over.get("language_id").and_then(Value::as_str) {
                server.language_id = Some(language_id.to_string());
            }
            if let Some(warmup) = over.get("warmup_timeout_ms").and_then(Value::as_u64) {
                server.warmup_timeout_ms = Some(warmup);
            }
        }
    }
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
        let parsed = LspConfigDocument::parse_operator(Some(raw)).expect("operator");
        let settings = parsed
            .servers
            .as_ref()
            .unwrap()
            .get("rust-analyzer")
            .unwrap()
            .get("settings");
        assert!(settings.is_some());
    }

    #[test]
    fn operator_rejects_invalid_json_and_unknown_server() {
        assert!(LspConfigDocument::parse_operator(Some("{")).is_err());
        assert!(
            LspConfigDocument::parse_operator(Some(r#"{"servers":{"not-a-server":{}}}"#))
                .unwrap_err()
                .contains("unknown language server")
        );
    }

    #[test]
    fn operator_rejects_command_override() {
        let err = LspConfigDocument::parse_operator(Some(
            r#"{"servers":{"rust-analyzer":{"command":"/tmp/evil"}}}"#,
        ))
        .unwrap_err();
        assert!(err.contains("command"), "{err}");
    }

    #[test]
    fn self_config_rejects_unknown_server_keys() {
        let err = LspConfigDocument::parse_self_config(Some(
            r#"{"servers":{"rust-analyzer":{"resolvedCommand":"/tmp/evil"}}}"#,
        ))
        .unwrap_err();
        assert!(err.contains("resolvedCommand"), "{err}");
        let err = LspConfigDocument::parse_self_config(Some(
            r#"{"servers":{"rust-analyzer":{"createClient":true}}}"#,
        ))
        .unwrap_err();
        assert!(err.contains("createClient"), "{err}");
    }
}
