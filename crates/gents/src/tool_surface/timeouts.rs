//! Effective tool timeouts: admitted Tools-document values under host ceilings.
//!
//! `ToolPolicy.effectiveBashForeground`, `effectiveCliTimeout`,
//! `effectiveBackgroundLifetime`, `underCeiling`, `waitFor` and `lspActionFor`
//! own these semantics; `generated_tool_timeout_cases_bind_native_resolution`
//! binds the functions below to them. Inputs are already admitted by
//! `Tools::validate`.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::background_tools::{DEFAULT_WAIT_PROCESS_TIMEOUT_SECS, MAX_WAIT_PROCESS_TIMEOUT_SECS};
use crate::toolset::lsp::{
    DEFAULT_ACTION_TIMEOUT_SECS, MAX_ACTION_TIMEOUT_SECS, MIN_REQUESTED_ACTION_TIMEOUT_SECS,
};
use crate::toolset::BACKGROUND_COMMAND_TIMEOUT_SECS;

fn secs(value: i64) -> Duration {
    Duration::from_secs(u64::try_from(value).unwrap_or(0))
}

/// Bash foreground `(default, maximum)`. An authored default without an
/// authored maximum fixes both; with neither authored the host pair applies.
pub(crate) fn effective_bash_foreground(
    host_default: Duration,
    host_maximum: Duration,
    timeout_secs: Option<i64>,
    max_timeout_secs: Option<i64>,
) -> (Duration, Duration) {
    let cap = host_default.max(host_maximum);
    let authored_default = timeout_secs.map(secs);
    let maximum = max_timeout_secs
        .map(secs)
        .or(authored_default)
        .unwrap_or(cap)
        .min(cap);
    (
        authored_default.unwrap_or(host_default).min(maximum),
        maximum,
    )
}

/// An authored CLI timeout replaces the host registration's, clamped to the
/// host foreground cap. Unauthored keeps the registration unchanged.
pub(crate) fn effective_cli_timeout_secs(
    host_cap: Duration,
    registration_secs: u64,
    timeout_secs: Option<i64>,
) -> u64 {
    match timeout_secs {
        None => registration_secs,
        Some(value) => secs(value).min(host_cap).as_secs(),
    }
}

/// Lifetime of one `spawn_process` execution.
pub(crate) fn effective_background_lifetime(background_timeout_secs: Option<i64>) -> Duration {
    let ceiling = Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS);
    background_timeout_secs
        .map(secs)
        .unwrap_or(ceiling)
        .min(ceiling)
}

/// An admitted `(default, maximum)` pair narrowed under a fixed ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedTimeout {
    pub default: Duration,
    pub maximum: Duration,
}

impl BoundedTimeout {
    fn resolve(
        authored: Option<i64>,
        maximum: Option<i64>,
        fallback: u64,
        maximum_fallback: u64,
        ceiling: u64,
    ) -> Self {
        let ceiling = Duration::from_secs(ceiling);
        let maximum = maximum
            .map(secs)
            .unwrap_or(Duration::from_secs(maximum_fallback))
            .min(ceiling);
        Self {
            default: authored
                .map(secs)
                .unwrap_or(Duration::from_secs(fallback))
                .min(maximum),
            maximum,
        }
    }

    /// Observation wait policy for `wait_process`.
    pub(crate) fn wait(wait_timeout_secs: Option<i64>, max_wait_timeout_secs: Option<i64>) -> Self {
        Self::resolve(
            wait_timeout_secs,
            max_wait_timeout_secs,
            DEFAULT_WAIT_PROCESS_TIMEOUT_SECS,
            MAX_WAIT_PROCESS_TIMEOUT_SECS,
            MAX_WAIT_PROCESS_TIMEOUT_SECS,
        )
    }

    /// LSP action timeout policy.
    pub(crate) fn lsp_action(timeout_secs: Option<i64>, max_timeout_secs: Option<i64>) -> Self {
        Self::resolve(
            timeout_secs,
            max_timeout_secs,
            DEFAULT_ACTION_TIMEOUT_SECS,
            MAX_ACTION_TIMEOUT_SECS,
            MAX_ACTION_TIMEOUT_SECS,
        )
    }

    fn requested(&self, requested_secs: Option<u64>, floor: u64) -> Duration {
        match requested_secs {
            None => self.default,
            Some(requested) => Duration::from_secs(requested.max(floor)).min(self.maximum),
        }
    }

    /// Wait for one `wait_process` call; waiting never cancels the work.
    pub(crate) fn wait_for(&self, requested_secs: Option<u64>) -> Duration {
        self.requested(requested_secs, 1)
    }

    /// Action timeout for one LSP call.
    pub(crate) fn action_for(&self, requested_secs: Option<u64>) -> Duration {
        self.requested(requested_secs, MIN_REQUESTED_ACTION_TIMEOUT_SECS)
    }
}

/// Lifetime and observation wait for one kind of background execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundTimeoutPolicy {
    pub lifetime: Duration,
    pub wait: BoundedTimeout,
}

impl Default for BackgroundTimeoutPolicy {
    fn default() -> Self {
        Self {
            lifetime: effective_background_lifetime(None),
            wait: BoundedTimeout::wait(None, None),
        }
    }
}

impl BackgroundTimeoutPolicy {
    fn from_fields(
        background_timeout_secs: Option<i64>,
        wait_timeout_secs: Option<i64>,
        max_wait_timeout_secs: Option<i64>,
    ) -> Self {
        Self {
            lifetime: effective_background_lifetime(background_timeout_secs),
            wait: BoundedTimeout::wait(wait_timeout_secs, max_wait_timeout_secs),
        }
    }
}

/// Background policies from `host.bash` and each `remote.services[]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundTimeouts {
    pub bash: BackgroundTimeoutPolicy,
    /// Keyed by MCP service id.
    pub remote: BTreeMap<String, BackgroundTimeoutPolicy>,
}

impl BackgroundTimeouts {
    /// Policy for an execution of `tool_name`, dispatched to
    /// `selected_service_id` when it is a remote call.
    pub(crate) fn for_execution(
        &self,
        tool_name: &str,
        selected_service_id: Option<&str>,
    ) -> BackgroundTimeoutPolicy {
        if crate::toolset::is_bash_tool_name(tool_name) {
            return self.bash;
        }
        selected_service_id
            .and_then(|service| self.remote.get(service))
            .copied()
            .unwrap_or_default()
    }
}

/// Every Tools-document timeout, projected once for the capability owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTimeouts {
    pub bash_timeout_secs: Option<i64>,
    pub bash_max_timeout_secs: Option<i64>,
    /// Authored `host.cli[].timeout_secs`, keyed by CLI tool name.
    pub cli_timeout_secs: BTreeMap<String, i64>,
    pub background: BackgroundTimeouts,
    pub lsp_action: BoundedTimeout,
}

impl Default for ToolTimeouts {
    fn default() -> Self {
        Self {
            bash_timeout_secs: None,
            bash_max_timeout_secs: None,
            cli_timeout_secs: BTreeMap::new(),
            background: BackgroundTimeouts::default(),
            lsp_action: BoundedTimeout::lsp_action(None, None),
        }
    }
}

impl ToolTimeouts {
    pub(crate) fn from_document(tools: &crate::document_config::Tools) -> Self {
        let host = tools.host.as_ref();
        let bash = host.and_then(|host| host.bash.as_ref());
        let lsp = tools
            .integrations
            .as_ref()
            .and_then(|integrations| integrations.lsp.as_ref());
        Self {
            bash_timeout_secs: bash.and_then(|bash| bash.timeout_secs),
            bash_max_timeout_secs: bash.and_then(|bash| bash.max_timeout_secs),
            cli_timeout_secs: host
                .into_iter()
                .flat_map(|host| host.cli.iter())
                .filter_map(|cli| cli.timeout_secs.map(|secs| (cli.name.clone(), secs)))
                .collect(),
            background: BackgroundTimeouts {
                bash: bash
                    .map(|bash| {
                        BackgroundTimeoutPolicy::from_fields(
                            bash.background_timeout_secs,
                            bash.wait_timeout_secs,
                            bash.max_wait_timeout_secs,
                        )
                    })
                    .unwrap_or_default(),
                remote: tools
                    .remote
                    .iter()
                    .flat_map(|remote| remote.services.iter())
                    .map(|service| {
                        (
                            service.mcp_service_id.clone(),
                            BackgroundTimeoutPolicy::from_fields(
                                service.background_timeout_secs,
                                service.wait_timeout_secs,
                                service.max_wait_timeout_secs,
                            ),
                        )
                    })
                    .collect(),
            },
            lsp_action: lsp
                .map(|lsp| BoundedTimeout::lsp_action(lsp.timeout_secs, lsp.max_timeout_secs))
                .unwrap_or_else(|| BoundedTimeout::lsp_action(None, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn opt_i64(value: &Value) -> Option<i64> {
        value.as_i64()
    }

    fn opt_u64(value: &Value) -> Option<u64> {
        value.as_u64()
    }

    fn document(groups: Value) -> crate::document_config::Tools {
        let mut value = groups;
        value["tools_id"] = "tools".into();
        value["agent_did"] = "owner".into();
        serde_json::from_value(value).unwrap()
    }

    /// A `null` expectation is a document the model rejects; native validation
    /// must reject the same document.
    fn assert_admission(name: &str, expected: &Value, groups: Value) -> bool {
        let admitted = document(groups).validate();
        assert_eq!(
            admitted.is_ok(),
            !expected.is_null(),
            "{name}: native admission disagrees with the model: {admitted:?}"
        );
        !expected.is_null()
    }

    fn pair(value: &Value) -> (Duration, Duration) {
        (
            Duration::from_secs(value["default"].as_u64().unwrap()),
            Duration::from_secs(value["maximum"].as_u64().unwrap()),
        )
    }

    fn bounded_cases(
        cases: &Value,
        groups: impl Fn(Value) -> Value,
        resolve: impl Fn(Option<i64>, Option<i64>) -> BoundedTimeout,
        effective: impl Fn(&BoundedTimeout, Option<u64>) -> Duration,
    ) {
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let (authored, maximum) = (
                opt_i64(&case["timeout_secs"]),
                opt_i64(&case["max_timeout_secs"]),
            );
            let fields = json!({"timeout_secs": authored, "max_timeout_secs": maximum});
            if !assert_admission(name, &case["expected"], groups(fields)) {
                continue;
            }
            let policy = resolve(authored, maximum);
            let (default, max) = pair(&case["expected"]);
            assert_eq!((policy.default, policy.maximum), (default, max), "{name}");
            for request in case["requests"].as_array().unwrap() {
                assert_eq!(
                    effective(&policy, opt_u64(&request["requested"])),
                    Duration::from_secs(request["effective"].as_u64().unwrap()),
                    "{name}: {request}"
                );
            }
        }
    }

    #[test]
    fn generated_tool_timeout_cases_bind_native_resolution() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().tool_timeout_cases;

        let foreground = cases["foreground"].as_array().unwrap();
        assert_eq!(foreground.len(), 11);
        for case in foreground {
            let name = case["name"].as_str().unwrap();
            let (timeout, maximum) = (
                opt_i64(&case["timeout_secs"]),
                opt_i64(&case["max_timeout_secs"]),
            );
            let groups = json!({"host": {"bash": {
                "timeout_secs": timeout, "max_timeout_secs": maximum}}});
            if !assert_admission(name, &case["expected"], groups) {
                continue;
            }
            let host_default = Duration::from_secs(case["host_default"].as_u64().unwrap());
            let host_maximum = Duration::from_secs(case["host_maximum"].as_u64().unwrap());
            assert_eq!(
                effective_bash_foreground(host_default, host_maximum, timeout, maximum),
                pair(&case["expected"]),
                "{name}"
            );
        }

        for case in cases["cli"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let authored = opt_i64(&case["timeout_secs"]);
            let groups = json!({"host": {"cli": [{"name": "tool", "timeout_secs": authored}]}});
            if !assert_admission(name, &case["expected"], groups) {
                continue;
            }
            let cap = Duration::from_secs(
                case["host_default"]
                    .as_u64()
                    .unwrap()
                    .max(case["host_maximum"].as_u64().unwrap()),
            );
            assert_eq!(
                effective_cli_timeout_secs(
                    cap,
                    case["registration_secs"].as_u64().unwrap(),
                    authored
                ),
                case["expected"].as_u64().unwrap(),
                "{name}"
            );
        }

        for case in cases["background"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let authored = opt_i64(&case["background_timeout_secs"]);
            for groups in [
                json!({"host": {"bash": {"background_timeout_secs": authored}}}),
                json!({"remote": {"services": [{"mcp_service_id": "svc",
                    "background_timeout_secs": authored}]}}),
            ] {
                if !assert_admission(name, &case["expected"], groups) {
                    continue;
                }
                assert_eq!(
                    effective_background_lifetime(authored),
                    Duration::from_secs(case["expected"].as_u64().unwrap()),
                    "{name}"
                );
            }
        }

        let wait_fields = |fields: Value| {
            json!({
                "wait_timeout_secs": fields["timeout_secs"],
                "max_wait_timeout_secs": fields["max_timeout_secs"],
            })
        };
        bounded_cases(
            &cases["wait"],
            |fields| json!({"host": {"bash": wait_fields(fields)}}),
            BoundedTimeout::wait,
            BoundedTimeout::wait_for,
        );
        bounded_cases(
            &cases["wait"],
            |fields| {
                let mut service = wait_fields(fields);
                service["mcp_service_id"] = "svc".into();
                json!({"remote": {"services": [service]}})
            },
            BoundedTimeout::wait,
            BoundedTimeout::wait_for,
        );
        bounded_cases(
            &cases["lsp"],
            |fields| json!({"integrations": {"lsp": fields}}),
            BoundedTimeout::lsp_action,
            BoundedTimeout::action_for,
        );
    }

    #[test]
    fn unconfigured_timeouts_keep_each_owner_default() {
        let timeouts = ToolTimeouts::default();
        assert_eq!(timeouts, ToolTimeouts::from_document(&document(json!({}))));
        assert_eq!(
            (timeouts.lsp_action.default, timeouts.lsp_action.maximum),
            (Duration::from_secs(20), Duration::from_secs(300))
        );
        let bash = timeouts.background.bash;
        assert_eq!(bash.lifetime, Duration::from_secs(36_000));
        assert_eq!(
            (bash.wait.default, bash.wait.maximum),
            (Duration::from_secs(30), Duration::from_secs(600))
        );
    }

    #[test]
    fn background_policy_follows_the_execution_kind() {
        let timeouts = ToolTimeouts::from_document(&document(json!({
            "host": {"bash": {"mode": "ReadOnly", "background_enabled": true,
                "background_timeout_secs": 60, "wait_timeout_secs": 5}},
            "remote": {"services": [{"mcp_service_id": "search",
                "background_timeout_secs": 120, "max_wait_timeout_secs": 900}]}
        })))
        .background;
        for bash in ["bash", "bash_unrestricted"] {
            let policy = timeouts.for_execution(bash, None);
            assert_eq!(policy.lifetime, Duration::from_secs(60));
            assert_eq!(policy.wait.default, Duration::from_secs(5));
            assert_eq!(policy.wait.maximum, Duration::from_secs(600));
        }
        let remote = timeouts.for_execution("call_tool", Some("search"));
        assert_eq!(remote.lifetime, Duration::from_secs(120));
        assert_eq!(remote.wait.default, Duration::from_secs(30));
        assert_eq!(remote.wait.maximum, Duration::from_secs(600));
        assert_eq!(
            timeouts.for_execution("call_tool", Some("unselected")),
            BackgroundTimeoutPolicy::default()
        );
        assert_eq!(
            timeouts.for_execution("call_tool", None),
            BackgroundTimeoutPolicy::default()
        );
    }
}
