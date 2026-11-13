use std::collections::BTreeMap;
use std::time::Duration;

use crate::native::lsp::MIN_REQUESTED_ACTION_TIMEOUT_SECS;
use crate::native::BACKGROUND_COMMAND_TIMEOUT_SECS;
use gents_documents::tool_limits::{
    DEFAULT_ACTION_TIMEOUT_SECS, DEFAULT_WAIT_PROCESS_TIMEOUT_SECS, MAX_ACTION_TIMEOUT_SECS,
    MAX_WAIT_PROCESS_TIMEOUT_SECS,
};

fn secs(value: i64) -> Duration {
    Duration::from_secs(u64::try_from(value).unwrap_or(0))
}

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

pub(crate) fn effective_background_lifetime(background_timeout_secs: Option<i64>) -> Duration {
    let ceiling = Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS);
    background_timeout_secs
        .map(secs)
        .unwrap_or(ceiling)
        .min(ceiling)
}

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

    pub(crate) fn wait(wait_timeout_secs: Option<i64>, max_wait_timeout_secs: Option<i64>) -> Self {
        Self::resolve(
            wait_timeout_secs,
            max_wait_timeout_secs,
            DEFAULT_WAIT_PROCESS_TIMEOUT_SECS,
            MAX_WAIT_PROCESS_TIMEOUT_SECS,
            MAX_WAIT_PROCESS_TIMEOUT_SECS,
        )
    }

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

    pub fn wait_for(&self, requested_secs: Option<u64>) -> Duration {
        self.requested(requested_secs, 1)
    }

    pub(crate) fn action_for(&self, requested_secs: Option<u64>) -> Duration {
        self.requested(requested_secs, MIN_REQUESTED_ACTION_TIMEOUT_SECS)
    }
}

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundTimeouts {
    pub bash: BackgroundTimeoutPolicy,
    pub remote: BTreeMap<String, BackgroundTimeoutPolicy>,
}

impl BackgroundTimeouts {
    pub fn for_execution(
        &self,
        tool_name: &str,
        selected_service_id: Option<&str>,
    ) -> BackgroundTimeoutPolicy {
        if crate::native::is_bash_tool_name(tool_name) {
            return self.bash;
        }
        selected_service_id
            .and_then(|service| self.remote.get(service))
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTimeouts {
    pub bash_timeout_secs: Option<i64>,
    pub bash_max_timeout_secs: Option<i64>,
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
    pub fn from_document(tools: &gents_documents::Tools) -> Self {
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

    fn document(groups: Value) -> gents_documents::Tools {
        let mut value = groups;
        value["tools_id"] = "tools".into();
        value["node_did"] = "owner".into();
        serde_json::from_value(value).unwrap()
    }

    fn admitted(groups: Value) -> bool {
        document(groups).validate().is_ok()
    }

    type BoundedObservation = Option<((u64, u64), Vec<u64>)>;

    fn bounded(
        groups: Value,
        authored: (Option<i64>, Option<i64>),
        requests: &[Option<u64>],
        resolve: fn(Option<i64>, Option<i64>) -> BoundedTimeout,
        effective: fn(&BoundedTimeout, Option<u64>) -> Duration,
    ) -> BoundedObservation {
        admitted(groups).then(|| {
            let policy = resolve(authored.0, authored.1);
            (
                (policy.default.as_secs(), policy.maximum.as_secs()),
                requests
                    .iter()
                    .map(|requested| effective(&policy, *requested).as_secs())
                    .collect(),
            )
        })
    }

    #[test]
    fn generated_foreground_timeouts_bind_native_resolution() {
        use gents_lean_vocab::families::tool_timeouts_foreground_pair::Pair;
        gents_lean_vocab::families::tool_timeouts_foreground_pair::replay(|i| {
            let (timeout, maximum) = (i.foreground.timeout_secs, i.foreground.max_timeout_secs);
            admitted(json!({"host": {"bash": {
                "timeout_secs": timeout, "max_timeout_secs": maximum}}}))
            .then(|| {
                let (default, maximum) = effective_bash_foreground(
                    Duration::from_secs(i.host_default),
                    Duration::from_secs(i.host_maximum),
                    timeout,
                    maximum,
                );
                Pair {
                    default: default.as_secs(),
                    maximum: maximum.as_secs(),
                }
            })
        });
    }

    #[test]
    fn generated_cli_timeouts_bind_native_resolution() {
        use gents_lean_vocab::families::tool_timeouts_cli_timeout::CliOutput;
        gents_lean_vocab::families::tool_timeouts_cli_timeout::replay(|i| CliOutput {
            cli: admitted(json!({"host": {"cli": [{
                "name": "tool", "timeout_secs": i.timeout_secs}]}}))
            .then(|| {
                effective_cli_timeout_secs(
                    Duration::from_secs(i.host_default.max(i.host_maximum)),
                    i.registration_secs,
                    i.timeout_secs,
                )
            }),
        });
    }

    #[test]
    fn generated_background_lifetimes_bind_native_resolution() {
        use gents_lean_vocab::families::tool_timeouts_background_lifetime::BackgroundOutput;
        gents_lean_vocab::families::tool_timeouts_background_lifetime::replay(|i| {
            let authored = i.background_timeout_secs;
            let [host, remote] = [
                json!({"host": {"bash": {"background_timeout_secs": authored}}}),
                json!({"remote": {"services": [{"mcp_service_id": "svc",
                    "background_timeout_secs": authored}]}}),
            ]
            .map(|groups| {
                admitted(groups).then(|| effective_background_lifetime(authored).as_secs())
            });
            assert_eq!(
                host, remote,
                "{}: host bash and remote service lifetimes disagree",
                i.name
            );
            BackgroundOutput { background: host }
        });
    }

    #[test]
    fn generated_wait_timeouts_bind_native_resolution() {
        use gents_lean_vocab::families::tool_timeouts_wait_policy::{BoundedOutput, Pair, Request};
        gents_lean_vocab::families::tool_timeouts_wait_policy::replay(|i| {
            let authored = (i.wait.timeout_secs, i.wait.max_timeout_secs);
            let fields = json!({
                "wait_timeout_secs": authored.0,
                "max_wait_timeout_secs": authored.1,
            });
            let mut service = fields.clone();
            service["mcp_service_id"] = "svc".into();
            let host = bounded(
                json!({"host": {"bash": fields}}),
                authored,
                &i.requests,
                BoundedTimeout::wait,
                BoundedTimeout::wait_for,
            );
            let remote = bounded(
                json!({"remote": {"services": [service]}}),
                authored,
                &i.requests,
                BoundedTimeout::wait,
                BoundedTimeout::wait_for,
            );
            assert_eq!(
                host, remote,
                "{}: host bash and remote service waits disagree",
                i.name
            );
            let (expected, effective) = host.unzip();
            BoundedOutput {
                expected: expected.map(|(default, maximum)| Pair { default, maximum }),
                requests: i
                    .requests
                    .iter()
                    .enumerate()
                    .map(|(index, requested)| Request {
                        requested: *requested,
                        effective: effective.as_ref().map(|effective| effective[index]),
                    })
                    .collect(),
            }
        });
    }

    #[test]
    fn generated_lsp_timeouts_bind_native_resolution() {
        use gents_lean_vocab::families::tool_timeouts_lsp_policy::{BoundedOutput, Pair, Request};
        gents_lean_vocab::families::tool_timeouts_lsp_policy::replay(|i| {
            let authored = (i.lsp.timeout_secs, i.lsp.max_timeout_secs);
            let (expected, effective) = bounded(
                json!({"integrations": {"lsp": {
                    "timeout_secs": authored.0, "max_timeout_secs": authored.1}}}),
                authored,
                &i.requests,
                BoundedTimeout::lsp_action,
                BoundedTimeout::action_for,
            )
            .unzip();
            BoundedOutput {
                expected: expected.map(|(default, maximum)| Pair { default, maximum }),
                requests: i
                    .requests
                    .iter()
                    .enumerate()
                    .map(|(index, requested)| Request {
                        requested: *requested,
                        effective: effective.as_ref().map(|effective| effective[index]),
                    })
                    .collect(),
            }
        });
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
