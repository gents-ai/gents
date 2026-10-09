//! Canonical runtime-authored node-readiness wire contract.
//!
//! Runtime configuration is never treated as proof that an agent can accept
//! work. The source projector admits only installed dispatchers not vetoed by
//! explicit unavailability or a generation-owned startup demotion. The client
//! projector then fails closed on missing, malformed, non-ready, or generation-
//! skewed observations. This is last-known application state, not connectivity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const NODE_READINESS_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeReadinessProcessState {
    #[serde(rename = "uninitialized")]
    Uninitialized,
    #[serde(rename = "recovering")]
    Recovering,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "shuttingDown")]
    ShuttingDown,
    #[serde(rename = "shutdown")]
    Shutdown,
}

impl NodeReadinessProcessState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Recovering => "recovering",
            Self::Ready => "ready",
            Self::ShuttingDown => "shuttingDown",
            Self::Shutdown => "shutdown",
        }
    }

    pub const fn accepts_work(self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReadinessState {
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReadinessUnavailableReason {
    AgentDisabled,
    RuntimeConfigurationInvalid,
    BackendNotConfigured,
    BackendDisabled,
    BackendTemporarilyUnavailable,
    CredentialsRequired,
    InferenceProfileInvalid,
    ToolConfigurationInvalid,
    ToolSurfaceUnavailable,
    ExecutorStartFailed,
}

impl AgentReadinessUnavailableReason {
    /// Stable presentation-safe admission message. Resolver diagnostics are
    /// deliberately excluded from durable request state and client views.
    pub const fn public_message(self) -> &'static str {
        match self {
            Self::AgentDisabled => "behavior is disabled",
            Self::RuntimeConfigurationInvalid => "runtime configuration is invalid",
            Self::BackendNotConfigured => "inference backend is not configured",
            Self::BackendDisabled => "inference backend is disabled",
            Self::BackendTemporarilyUnavailable => "inference backend is temporarily unavailable",
            Self::CredentialsRequired => "inference credentials are required",
            Self::InferenceProfileInvalid => "inference profile is invalid",
            Self::ToolConfigurationInvalid => "tool configuration is invalid",
            Self::ToolSurfaceUnavailable => "tool surface is unavailable",
            Self::ExecutorStartFailed => "agent executor could not start",
        }
    }

    pub const ALL: [Self; 10] = [
        Self::AgentDisabled,
        Self::RuntimeConfigurationInvalid,
        Self::BackendNotConfigured,
        Self::BackendDisabled,
        Self::BackendTemporarilyUnavailable,
        Self::CredentialsRequired,
        Self::InferenceProfileInvalid,
        Self::ToolConfigurationInvalid,
        Self::ToolSurfaceUnavailable,
        Self::ExecutorStartFailed,
    ];
}

/// Routing's admission message for an agent the active runtime does not assign.
pub const BEHAVIOR_NOT_ASSIGNED_MESSAGE: &str = "behavior is not assigned to this runtime";

/// Whether `failure_reason` is exactly one of routing's pre-dispatch
/// behavior-unavailability rejections. Routing writes these stable messages,
/// never resolver diagnostics, so exact comparison identifies the cause.
pub fn is_behavior_unavailable_rejection(failure_reason: &str) -> bool {
    failure_reason == BEHAVIOR_NOT_ASSIGNED_MESSAGE
        || AgentReadinessUnavailableReason::ALL
            .iter()
            .any(|reason| reason.public_message() == failure_reason)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReadinessEntry {
    pub agent_id: String,
    pub state: AgentReadinessState,
    pub reason: Option<AgentReadinessUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeReadinessSnapshot {
    pub format_version: u32,
    pub process_state: NodeReadinessProcessState,
    pub active_generation: u64,
    pub router_generation: u64,
    pub default_agent_id: String,
    pub agents: Vec<AgentReadinessEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReadinessSourceEntry {
    pub agent_id: String,
    pub dispatcher_present: bool,
    pub unavailable_reason: Option<AgentReadinessUnavailableReason>,
    pub startup_demoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveAgentReadinessAdmission {
    Ready,
    Unavailable(AgentReadinessUnavailableReason),
    Unassigned,
}

pub fn effective_agent_readiness_admission(
    dispatcher_present: bool,
    unavailable_reason: Option<AgentReadinessUnavailableReason>,
    startup_demoted: bool,
) -> EffectiveAgentReadinessAdmission {
    if startup_demoted {
        EffectiveAgentReadinessAdmission::Unavailable(
            AgentReadinessUnavailableReason::ExecutorStartFailed,
        )
    } else if let Some(reason) = unavailable_reason {
        EffectiveAgentReadinessAdmission::Unavailable(reason)
    } else if dispatcher_present {
        EffectiveAgentReadinessAdmission::Ready
    } else {
        EffectiveAgentReadinessAdmission::Unassigned
    }
}

/// Pure source projector shared by the runtime publisher and Lean-generated
/// conformance harness. Unavailability and startup demotion veto a dispatcher.
pub fn project_node_readiness_source(
    process_state: NodeReadinessProcessState,
    active_generation: u64,
    router_generation: u64,
    default_agent_id: impl Into<String>,
    sources: impl IntoIterator<Item = AgentReadinessSourceEntry>,
) -> Result<NodeReadinessSnapshot, String> {
    let default_agent_id = default_agent_id.into();
    if !is_canonical_id(&default_agent_id) {
        return Err(format!(
            "default agent {default_agent_id:?} is not canonical"
        ));
    }

    let mut agents = BTreeMap::new();
    for source in sources {
        if !is_canonical_id(&source.agent_id) {
            return Err(format!(
                "agent identifier {:?} is not canonical",
                source.agent_id
            ));
        }
        let entry = match effective_agent_readiness_admission(
            source.dispatcher_present,
            source.unavailable_reason,
            source.startup_demoted,
        ) {
            EffectiveAgentReadinessAdmission::Ready => Some(AgentReadinessEntry {
                agent_id: source.agent_id.clone(),
                state: AgentReadinessState::Ready,
                reason: None,
            }),
            EffectiveAgentReadinessAdmission::Unavailable(reason) => Some(AgentReadinessEntry {
                agent_id: source.agent_id.clone(),
                state: AgentReadinessState::Unavailable,
                reason: Some(reason),
            }),
            EffectiveAgentReadinessAdmission::Unassigned => None,
        };
        if agents.insert(source.agent_id, entry).is_some() {
            return Err("duplicate agent readiness source".to_string());
        }
    }
    let agents = agents.into_values().flatten().collect::<Vec<_>>();
    if !agents
        .iter()
        .any(|entry| entry.agent_id == default_agent_id)
    {
        return Err(format!(
            "default agent {default_agent_id:?} is not assigned"
        ));
    }

    Ok(NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state,
        active_generation,
        router_generation,
        default_agent_id,
        agents,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeReadinessRow {
    pub node_did: String,
    pub snapshot_json: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReadinessUnknownReason {
    ReadinessMissing,
    ReadinessMalformed,
    ReadinessVersionUnsupported,
    ProcessNotReady,
    RouterGenerationStale,
    AgentNotAssigned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedAgentReadiness {
    Ready,
    Unavailable(AgentReadinessUnavailableReason),
    Unknown(AgentReadinessUnknownReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeReadinessProjection {
    pub active_generation: Option<u64>,
    pub router_generation: Option<u64>,
    pub default_agent_id: Option<String>,
    pub updated_at: Option<String>,
    pub unknown_reason: Option<AgentReadinessUnknownReason>,
    pub agents: BTreeMap<String, ProjectedAgentReadiness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeReadinessSummary {
    pub snapshot: NodeReadinessSnapshot,
    pub ready_count: usize,
    pub unavailable_agents: BTreeMap<String, AgentReadinessUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedNodeReadinessSummary {
    Observed(NodeReadinessSummary),
    Unknown(AgentReadinessUnknownReason),
}

fn is_canonical_id(value: &str) -> bool {
    !value.is_empty() && value == value.trim()
}

fn unknown_projection(
    agent_ids: BTreeSet<String>,
    reason: AgentReadinessUnknownReason,
) -> NodeReadinessProjection {
    NodeReadinessProjection {
        active_generation: None,
        router_generation: None,
        default_agent_id: None,
        updated_at: None,
        unknown_reason: Some(reason),
        agents: agent_ids
            .into_iter()
            .map(|agent_id| (agent_id, ProjectedAgentReadiness::Unknown(reason)))
            .collect(),
    }
}

/// Decode and validate the runtime-authored row without imposing admission.
/// Operational views use this for lifecycle/generation observability while
/// `project_node_readiness_summary` additionally requires Ready and an
/// aligned router generation.
pub fn decode_node_readiness_snapshot(
    row: &NodeReadinessRow,
    expected_node_did: &str,
) -> Result<NodeReadinessSnapshot, AgentReadinessUnknownReason> {
    if !is_canonical_id(expected_node_did)
        || row.node_did != expected_node_did
        || !is_canonical_id(&row.node_did)
    {
        return Err(AgentReadinessUnknownReason::ReadinessMalformed);
    }
    #[derive(Deserialize)]
    struct Version {
        format_version: u32,
    }
    // An unsupported wire version can have a different payload shape, so
    // classify its version before applying the current strict schema.
    let version = serde_json::from_str::<Version>(&row.snapshot_json)
        .map_err(|_| AgentReadinessUnknownReason::ReadinessMalformed)?;
    if version.format_version != NODE_READINESS_FORMAT_VERSION {
        return Err(AgentReadinessUnknownReason::ReadinessVersionUnsupported);
    }
    let snapshot = serde_json::from_str::<NodeReadinessSnapshot>(&row.snapshot_json)
        .map_err(|_| AgentReadinessUnknownReason::ReadinessMalformed)?;
    let entries_are_canonical = snapshot.agents.iter().all(|entry| {
        is_canonical_id(&entry.agent_id)
            && match entry.state {
                AgentReadinessState::Ready => entry.reason.is_none(),
                AgentReadinessState::Unavailable => entry.reason.is_some(),
            }
    }) && snapshot
        .agents
        .windows(2)
        .all(|pair| pair[0].agent_id < pair[1].agent_id);
    if !entries_are_canonical || !is_canonical_id(&snapshot.default_agent_id) {
        return Err(AgentReadinessUnknownReason::ReadinessMalformed);
    }
    if !snapshot
        .agents
        .iter()
        .any(|entry| entry.agent_id == snapshot.default_agent_id)
    {
        return Err(AgentReadinessUnknownReason::AgentNotAssigned);
    }
    Ok(snapshot)
}

/// Strict operational summary from the sole durable readiness authority.
/// Missing, malformed, non-ready, or generation-skewed observations fail
/// closed and never manufacture agent counts from configuration rows.
/// Document age does not establish runtime liveness. Transport health has its
/// own database owner.
pub fn project_node_readiness_summary(
    row: Option<&NodeReadinessRow>,
    expected_node_did: &str,
) -> ProjectedNodeReadinessSummary {
    let Some(row) = row else {
        return ProjectedNodeReadinessSummary::Unknown(
            AgentReadinessUnknownReason::ReadinessMissing,
        );
    };
    let snapshot = match decode_node_readiness_snapshot(row, expected_node_did) {
        Ok(snapshot) => snapshot,
        Err(reason) => return ProjectedNodeReadinessSummary::Unknown(reason),
    };
    if !snapshot.process_state.accepts_work() {
        return ProjectedNodeReadinessSummary::Unknown(
            AgentReadinessUnknownReason::ProcessNotReady,
        );
    }
    if snapshot.active_generation == 0 || snapshot.router_generation != snapshot.active_generation {
        return ProjectedNodeReadinessSummary::Unknown(
            AgentReadinessUnknownReason::RouterGenerationStale,
        );
    }
    let mut ready_count = 0;
    let mut unavailable_agents = BTreeMap::new();
    for entry in &snapshot.agents {
        match entry.state {
            AgentReadinessState::Ready => ready_count += 1,
            AgentReadinessState::Unavailable => {
                unavailable_agents.insert(
                    entry.agent_id.clone(),
                    entry
                        .reason
                        .expect("canonical unavailable entry has reason"),
                );
            }
        }
    }
    ProjectedNodeReadinessSummary::Observed(NodeReadinessSummary {
        snapshot,
        ready_count,
        unavailable_agents,
    })
}

/// Project the runtime-authored row into the only legal client readiness
/// states. Configured identifiers are validated exactly, never normalized.
/// Readiness changes only when the runtime publishes a semantic change.
pub fn project_node_readiness<'a>(
    row: Option<&NodeReadinessRow>,
    expected_node_did: &str,
    configured_agent_ids: impl IntoIterator<Item = &'a str>,
    configured_default_agent_id: Option<&str>,
) -> NodeReadinessProjection {
    let mut agent_ids = BTreeSet::new();
    let mut configured_ids_malformed = false;
    for agent_id in configured_agent_ids {
        if !is_canonical_id(agent_id) {
            configured_ids_malformed = true;
        } else {
            agent_ids.insert(agent_id.to_owned());
        }
    }
    if let Some(default_agent_id) = configured_default_agent_id {
        if !is_canonical_id(default_agent_id) {
            configured_ids_malformed = true;
        } else {
            agent_ids.insert(default_agent_id.to_owned());
        }
    }
    if configured_ids_malformed {
        return unknown_projection(agent_ids, AgentReadinessUnknownReason::ReadinessMalformed);
    }

    let Some(row) = row else {
        return unknown_projection(agent_ids, AgentReadinessUnknownReason::ReadinessMissing);
    };
    let snapshot = match decode_node_readiness_snapshot(row, expected_node_did) {
        Ok(snapshot) => snapshot,
        Err(reason) => return unknown_projection(agent_ids, reason),
    };

    let entries = snapshot
        .agents
        .into_iter()
        .map(|entry| (entry.agent_id.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    agent_ids.extend(entries.keys().cloned());
    debug_assert!(entries.contains_key(&snapshot.default_agent_id));
    let global_unknown = if !snapshot.process_state.accepts_work() {
        Some(AgentReadinessUnknownReason::ProcessNotReady)
    } else if snapshot.active_generation == 0
        || snapshot.router_generation != snapshot.active_generation
    {
        Some(AgentReadinessUnknownReason::RouterGenerationStale)
    } else {
        None
    };
    let agents = agent_ids
        .into_iter()
        .map(|agent_id| {
            let state = if let Some(reason) = global_unknown {
                ProjectedAgentReadiness::Unknown(reason)
            } else {
                match entries.get(&agent_id) {
                    Some(AgentReadinessEntry {
                        state: AgentReadinessState::Ready,
                        reason: None,
                        ..
                    }) => ProjectedAgentReadiness::Ready,
                    Some(AgentReadinessEntry {
                        state: AgentReadinessState::Unavailable,
                        reason: Some(reason),
                        ..
                    }) => ProjectedAgentReadiness::Unavailable(*reason),
                    Some(_) => unreachable!("canonical readiness entry checked above"),
                    None => ProjectedAgentReadiness::Unknown(
                        AgentReadinessUnknownReason::AgentNotAssigned,
                    ),
                }
            };
            (agent_id, state)
        })
        .collect();
    NodeReadinessProjection {
        active_generation: Some(snapshot.active_generation),
        router_generation: Some(snapshot.router_generation),
        default_agent_id: Some(snapshot.default_agent_id),
        updated_at: Some(row.updated_at.clone()),
        unknown_reason: global_unknown,
        agents,
    }
}

#[cfg(test)]
#[path = "node_readiness/tests.rs"]
mod tests;
