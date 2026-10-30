use super::*;
use serde_json::{Map, Value};
use std::path::Path;

impl Scanner {
    pub(super) fn parse_config(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        format: ConfigFormat,
        content: &str,
    ) {
        let value: Result<Value, String> = match format {
            ConfigFormat::ClaudeJson | ConfigFormat::McpJson => {
                serde_json::from_str(content).map_err(|error| error.to_string())
            }
            ConfigFormat::CodexToml | ConfigFormat::GrokToml => {
                toml::from_str::<toml::Value>(content)
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    })
            }
        };
        let value = match value {
            Ok(value) => value,
            Err(_) => {
                source.partial = true;
                self.warning(
                    DiscoveryNoticeCode::MalformedFile,
                    Some(&source.input.source_id),
                    Some(&path_string(relative)),
                    "configuration file was malformed and was discarded",
                    None,
                    None,
                );
                return;
            }
        };
        match format {
            ConfigFormat::ClaudeJson => self.parse_claude(source, relative, &value),
            ConfigFormat::CodexToml => self.parse_codex(source, relative, &value),
            ConfigFormat::GrokToml => self.parse_grok(source, relative, &value),
            ConfigFormat::McpJson => self.parse_mcp_object(source, relative, &value),
        }
    }

    pub(super) fn parse_claude(&mut self, source: &mut SourceScan, relative: &Path, value: &Value) {
        let Some(object) = value.as_object() else {
            self.malformed_object(source, relative);
            return;
        };
        if let Some(model) = object.get("model").and_then(Value::as_str) {
            self.model_item(source, relative, "default", model, None, None);
        }
        if let Some(permissions) = object.get("permissions").and_then(Value::as_object) {
            for effect in ["allow", "deny", "ask"] {
                if let Some(rules) = permissions.get(effect).and_then(Value::as_array) {
                    self.permission_item(source, relative, effect, rules);
                }
            }
        }
        if let Some(hooks) = object.get("hooks").and_then(Value::as_object) {
            self.hook_items(source, relative, hooks);
        }
        if object.contains_key("mcpServers") {
            self.parse_mcp_object(source, relative, value);
        }
        for key in object.keys() {
            if !["model", "permissions", "hooks", "mcpServers"].contains(&key.as_str()) {
                self.unsupported_setting(source, relative, key, &object[key]);
            }
        }
    }

    pub(super) fn parse_codex(&mut self, source: &mut SourceScan, relative: &Path, value: &Value) {
        let Some(object) = value.as_object() else {
            self.malformed_object(source, relative);
            return;
        };
        if let Some(model) = object.get("model").and_then(Value::as_str) {
            self.model_item(
                source,
                relative,
                "default",
                model,
                object.get("model_provider").and_then(Value::as_str),
                None,
            );
        }
        if let Some(servers) = object.get("mcp_servers").and_then(Value::as_object) {
            self.remote_tool_items(source, relative, servers);
        }
        for key in ["approval_policy", "sandbox_mode"] {
            if let Some(setting) = object.get(key) {
                let rules = vec![setting.clone()];
                self.permission_item(source, relative, key, &rules);
            }
        }
        if let Some(hooks) = object.get("hooks").and_then(Value::as_object) {
            self.hook_items(source, relative, hooks);
        }
        if let Some(reasoning) = object.get("model_reasoning_effort") {
            self.unsupported_setting(source, relative, "model_reasoning_effort", reasoning);
        }
        collect_disabled_skills(object, &mut source.disabled_skills);
        self.unsupported_nested_settings(source, relative, object, "skills", &["disabled"]);
        for key in object.keys() {
            if ![
                "model",
                "model_provider",
                "model_reasoning_effort",
                "mcp_servers",
                "approval_policy",
                "sandbox_mode",
                "hooks",
                "skills",
            ]
            .contains(&key.as_str())
            {
                self.unsupported_setting(source, relative, key, &object[key]);
            }
        }
    }

    pub(super) fn parse_grok(&mut self, source: &mut SourceScan, relative: &Path, value: &Value) {
        let Some(object) = value.as_object() else {
            self.malformed_object(source, relative);
            return;
        };
        if let Some(models) = object.get("models").and_then(Value::as_object) {
            for role in ["default", "web_search"] {
                if let Some(model) = models.get(role).and_then(Value::as_str) {
                    self.model_item(source, relative, role, model, None, None);
                }
            }
        }
        self.unsupported_nested_settings(
            source,
            relative,
            object,
            "models",
            &["default", "web_search"],
        );
        if let Some(custom_models) = object.get("model").and_then(Value::as_object) {
            for (name, config) in custom_models {
                let config = config.as_object();
                let model = config
                    .and_then(|value| value.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or(name);
                let endpoint = config
                    .and_then(|value| value.get("base_url"))
                    .and_then(Value::as_str);
                self.model_item(source, relative, name, model, None, endpoint);
            }
        }
        if let Some(servers) = object.get("mcp_servers").and_then(Value::as_object) {
            self.remote_tool_items(source, relative, servers);
        }
        if let Some(hooks) = object.get("hooks").and_then(Value::as_object) {
            self.hook_items(source, relative, hooks);
        }
        if let Some(ui) = object.get("ui").and_then(Value::as_object) {
            if let Some(permission) = ui.get("default_selected_permission") {
                self.permission_item(
                    source,
                    relative,
                    "default_selected_permission",
                    std::slice::from_ref(permission),
                );
            }
        }
        collect_disabled_skills(object, &mut source.disabled_skills);
        self.unsupported_nested_settings(source, relative, object, "skills", &["disabled"]);
        for key in object.keys() {
            if !["models", "model", "mcp_servers", "hooks", "ui", "skills"].contains(&key.as_str())
            {
                self.unsupported_setting(source, relative, key, &object[key]);
            }
        }
    }

    pub(super) fn parse_mcp_object(&mut self, source: &mut SourceScan, relative: &Path, value: &Value) {
        let Some(object) = value.as_object() else {
            self.malformed_object(source, relative);
            return;
        };
        let servers = object
            .get("mcpServers")
            .or_else(|| object.get("mcp_servers"))
            .and_then(Value::as_object);
        if let Some(servers) = servers {
            self.remote_tool_items(source, relative, servers);
        }
    }

    pub(super) fn remote_tool_items(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        servers: &Map<String, Value>,
    ) {
        let mut names: Vec<_> = servers.keys().collect();
        names.sort();
        for name in names {
            let Some(config) = servers[name].as_object() else {
                self.emit(
                    source,
                    relative,
                    DiscoveryCategory::RemoteTool,
                    name,
                    DiscoveryItemState::Unknown,
                    DiscoveryFacts::UnsupportedSetting {
                        key: sanitize_text(name),
                        value_type: value_type(&servers[name]).to_owned(),
                    },
                    MappingSupport::unsupported("remote tool entry was not an object"),
                );
                continue;
            };
            let enabled = config.get("enabled").and_then(Value::as_bool);
            let state = match enabled {
                Some(true) => DiscoveryItemState::Enabled,
                Some(false) => DiscoveryItemState::Disabled,
                None => DiscoveryItemState::Unknown,
            };
            let url = config.get("url").and_then(Value::as_str);
            let command = config.get("command").and_then(Value::as_str);
            let args = config.get("args").and_then(Value::as_array);
            let env = config.get("env").and_then(Value::as_object);
            let headers = config
                .get("headers")
                .or_else(|| config.get("http_headers"))
                .and_then(Value::as_object);
            let mut environment_keys = env
                .into_iter()
                .flat_map(|values| values.keys())
                .map(|key| sanitize_text(key))
                .collect::<Vec<_>>();
            environment_keys.extend(
                config
                    .get("env_vars")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(sanitize_text),
            );
            environment_keys.sort();
            environment_keys.dedup();
            let mut header_keys = headers
                .into_iter()
                .flat_map(|values| values.keys())
                .map(|key| sanitize_text(key))
                .collect::<Vec<_>>();
            header_keys.extend(
                config
                    .get("env_http_headers")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|values| values.keys())
                    .map(|key| sanitize_text(key)),
            );
            if config.contains_key("bearer_token_env_var") {
                header_keys.push("Authorization (from named environment variable)".to_owned());
            }
            header_keys.sort();
            header_keys.dedup();
            self.emit(
                source,
                relative,
                DiscoveryCategory::RemoteTool,
                name,
                state,
                DiscoveryFacts::RemoteTool {
                    transport: config
                        .get("type")
                        .and_then(Value::as_str)
                        .map(sanitize_text)
                        .or_else(|| url.map(|_| "http".to_owned()))
                        .or_else(|| command.map(|_| "stdio".to_owned())),
                    endpoint_origin: url.and_then(safe_endpoint_origin),
                    executable: command.and_then(executable_name),
                    argument_count: args.map_or(0, Vec::len),
                    environment_keys,
                    header_keys,
                },
                MappingSupport::partial(
                    "discovery preserves disabled state and inert metadata but does not translate or launch the server",
                ),
            );
        }
    }

    pub(super) fn hook_items(&mut self, source: &mut SourceScan, relative: &Path, hooks: &Map<String, Value>) {
        let mut events: Vec<_> = hooks.keys().collect();
        events.sort();
        for event in events {
            let mut commands = Vec::new();
            collect_command_strings(&hooks[event], &mut commands);
            let mut executables: Vec<_> = commands
                .iter()
                .filter_map(|command| executable_name(command))
                .collect();
            executables.sort();
            executables.dedup();
            self.emit(
                source,
                relative,
                DiscoveryCategory::Hook,
                event,
                DiscoveryItemState::Unknown,
                DiscoveryFacts::Hook {
                    event: sanitize_text(event),
                    command_count: commands.len(),
                    executables,
                },
                MappingSupport::unsupported(
                    "foreign hook ordering, authority, matching, and lifecycle semantics are not imported",
                ),
            );
        }
    }

    pub(super) fn permission_item(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        effect: &str,
        rules: &[Value],
    ) {
        let sanitized_rules = rules
            .iter()
            .filter_map(scalar_summary)
            .map(|value| bounded_sanitized_text(&value))
            .collect::<Vec<_>>();
        self.emit(
            source,
            relative,
            DiscoveryCategory::Permission,
            effect,
            DiscoveryItemState::Enabled,
            DiscoveryFacts::Permission {
                effect: sanitize_text(effect),
                rule_count: rules.len(),
                rules: sanitized_rules,
            },
            MappingSupport::unsupported(
                "foreign permission rules are retained for review but cannot be claimed equivalent to Gents authority",
            ),
        );
    }

    pub(super) fn model_item(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        role: &str,
        model: &str,
        provider: Option<&str>,
        endpoint: Option<&str>,
    ) {
        self.emit(
            source,
            relative,
            DiscoveryCategory::ModelPreference,
            role,
            DiscoveryItemState::Enabled,
            DiscoveryFacts::ModelPreference {
                role: sanitize_text(role),
                model: sanitize_text(model),
                provider: provider.map(sanitize_text),
                endpoint_origin: endpoint.and_then(safe_endpoint_origin),
            },
            MappingSupport::partial(
                "model names and provider catalogs differ; this is a preference hint, not a compatible binding",
            ),
        );
    }

    pub(super) fn unsupported_setting(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        key: &str,
        value: &Value,
    ) {
        self.emit(
            source,
            relative,
            DiscoveryCategory::Permission,
            &format!("unsupported setting: {key}"),
            DiscoveryItemState::Unknown,
            DiscoveryFacts::UnsupportedSetting {
                key: sanitize_text(key),
                value_type: value_type(value).to_owned(),
            },
            MappingSupport::unsupported(
                "setting is retained by name and type only; its value and semantics were not imported",
            ),
        );
    }

    pub(super) fn unsupported_nested_settings(
        &mut self,
        source: &mut SourceScan,
        relative: &Path,
        object: &Map<String, Value>,
        table: &str,
        recognized: &[&str],
    ) {
        let Some(settings) = object.get(table).and_then(Value::as_object) else {
            return;
        };
        for (key, value) in settings {
            if !recognized.contains(&key.as_str()) {
                self.unsupported_setting(source, relative, &format!("{table}.{key}"), value);
            }
        }
    }

    pub(super) fn malformed_object(&mut self, source: &mut SourceScan, relative: &Path) {
        source.partial = true;
        self.warning(
            DiscoveryNoticeCode::MalformedFile,
            Some(&source.input.source_id),
            Some(&path_string(relative)),
            "configuration root was not an object and was discarded",
            None,
            None,
        );
    }
}
