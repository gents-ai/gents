use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const NODE_READINESS_FORMAT_VERSION: u32 = 3;

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
    SubscriptionNotEntitled,
    InferenceProfileInvalid,
    ToolConfigurationInvalid,
    ToolSurfaceUnavailable,
    /// The Agent's Tools run host tools under a published WorkspaceRoot the
    /// Agent holds no `reader` relationship on, so the runtime cannot admit
    /// the root its tools read. [`Self::public_message`] names the grant.
    WorkspaceRootReaderMissing,
    ExecutorStartFailed,
}

impl AgentReadinessUnavailableReason {
    pub const fn public_message(self) -> &'static str {
        match self {
            Self::AgentDisabled => "behavior is disabled",
            Self::RuntimeConfigurationInvalid => "runtime configuration is invalid",
            Self::BackendNotConfigured => "inference backend is not configured",
            Self::BackendDisabled => "inference backend is disabled",
            Self::BackendTemporarilyUnavailable => "inference backend is temporarily unavailable",
            Self::CredentialsRequired => "inference credentials are required",
            Self::SubscriptionNotEntitled => {
                "the subscription account is not eligible for inference"
            }
            Self::InferenceProfileInvalid => "inference profile is invalid",
            Self::ToolConfigurationInvalid => "tool configuration is invalid",
            Self::ToolSurfaceUnavailable => "tool surface is unavailable",
            Self::WorkspaceRootReaderMissing => WORKSPACE_ROOT_READER_MISSING_MESSAGE,
            Self::ExecutorStartFailed => "agent executor could not start",
        }
    }

    /// Lean `Runtime.StartupDegraded.degradesNode`: a disabled Agent is off,
    /// not lacking anything, so it leaves the node ready.
    pub const fn degrades_node(self) -> bool {
        !matches!(self, Self::AgentDisabled)
    }

    pub const ALL: [Self; 12] = [
        Self::AgentDisabled,
        Self::RuntimeConfigurationInvalid,
        Self::BackendNotConfigured,
        Self::BackendDisabled,
        Self::BackendTemporarilyUnavailable,
        Self::CredentialsRequired,
        Self::SubscriptionNotEntitled,
        Self::InferenceProfileInvalid,
        Self::ToolConfigurationInvalid,
        Self::ToolSurfaceUnavailable,
        Self::WorkspaceRootReaderMissing,
        Self::ExecutorStartFailed,
    ];
}

/// The grant that makes an Agent a reader of a published root, naming the home
/// it must run against: several nodes run on one machine, and a bare `gents`
/// acts on the default home, so a grant applied elsewhere leaves this node's
/// Agent exactly as it was.
pub fn workspace_root_reader_grant(path: &str, agent_did: &str, home: &str) -> String {
    format!("gents config workspace-root grant-reader --path {path} --agent-did {agent_did} --home {home}")
}

/// The call that finds the roots on this node, where a cause names no root yet:
/// its output lists each root with its readers, so the operator sees which
/// grant is missing.
pub fn workspace_root_list_call(home: &std::path::Path) -> String {
    format!("gents config workspace-root list --home {}", home.display())
}

/// What an Agent does with a reader-grant step: hand the User its command, and
/// the separate target guidance only when the step carries one.
pub fn reader_grant_how_to_proceed(target_guidance: bool) -> &'static str {
    if target_guidance {
        "Keep these Tools and give the User user_command and target_guidance to run it from their own client; no config or policy call, and no change to your own Tools, can grant it."
    } else {
        "Keep these Tools and give the User user_command to run from their own client; no config or policy call, and no change to your own Tools, can grant it."
    }
}

/// The cause of an Agent's host tools not being admitted, when the runtime knows
/// the root and its own home: the grant, with the home it applies to.
pub fn workspace_root_reader_missing_message(agent_did: &str, path: &str, home: &str) -> String {
    format!(
        "agent has host tools under the workspace root {path}, which it is not a reader of; fix it with `{}`, then start the agent again",
        workspace_root_reader_grant(path, agent_did, home)
    )
}

/// The same cause where the root is known but the home is not: the grant omits
/// `--home` — no placeholder inside a copy-pasteable call — and the sentence
/// after it says to add one.
pub fn workspace_root_reader_missing_message_without_home(agent_did: &str, path: &str) -> String {
    format!(
        "agent has host tools under the workspace root {path}, which it is not a reader of; fix it with `gents config workspace-root grant-reader --path {path} --agent-did {agent_did}`, adding `--home` with this node's home, then start the agent again"
    )
}

/// The rejection text of [`AgentReadinessUnavailableReason::WorkspaceRootReaderMissing`]
/// when the runtime knows neither the root nor the home: it names the failing
/// step and the call that lists the roots and their readers.
pub const WORKSPACE_ROOT_READER_MISSING_MESSAGE: &str =
    "agent has host tools under a workspace root it is not a reader of; find it with `gents config workspace-root list` and grant the reader it names, adding `--home` with this node's home, then start the agent again";

/// What a full rejection text of
/// [`AgentReadinessUnavailableReason::WorkspaceRootReaderMissing`] starts with:
/// the fixed wording, followed by the root, the Agent and the node home of the
/// grant that fixes it. A reader compares the opening, since the tail carries
/// this node's paths ([`workspace_root_reader_missing_message`]).
pub const WORKSPACE_ROOT_READER_MISSING_PREFIX: &str =
    "agent has host tools under the workspace root";

pub const BEHAVIOR_NOT_ASSIGNED_MESSAGE: &str = "behavior is not assigned to this runtime";

/// External premise: a client sees why the runtime rejected a request only in
/// the AgentRequest's replicated `failure_reason`; the schema carries no
/// rejection kind, so the readiness messages the runtime writes are matched
/// exactly.
pub fn is_behavior_unavailable_rejection(failure_reason: &str) -> bool {
    failure_reason == BEHAVIOR_NOT_ASSIGNED_MESSAGE
        || unavailable_rejection_reason(failure_reason).is_some()
}

/// The readiness reason a rejection's `failure_reason` states.
/// External premise: replicated AgentRequest.failure_reason contains only
/// persisted rejection text, not a typed rejection-kind field. Match the
/// runtime-owned readiness messages/prefix to recover their public reason.
pub fn unavailable_rejection_reason(
    failure_reason: &str,
) -> Option<AgentReadinessUnavailableReason> {
    if failure_reason.starts_with(WORKSPACE_ROOT_READER_MISSING_PREFIX) {
        return Some(AgentReadinessUnavailableReason::WorkspaceRootReaderMissing);
    }
    AgentReadinessUnavailableReason::ALL
        .into_iter()
        .find(|reason| reason.public_message() == failure_reason)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceTierBinding {
    Unset,
    Selected { tier: String },
    NotSupportedByProvider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReasoningEffortBinding {
    Unset,
    Sent {
        effort: String,
    },
    DroppedUnsupported {
        effort: String,
        advertised: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveInferenceSettings {
    pub profile_id: Option<String>,
    pub backend_id: Option<String>,
    pub model_name: String,
    pub reasoning_effort: Option<String>,
    pub service_tier: ServiceTierBinding,
    pub reasoning_effort_dispatch: ReasoningEffortBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReadinessEntry {
    pub agent_did: String,
    pub state: AgentReadinessState,
    pub reason: Option<AgentReadinessUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_inference: Option<EffectiveInferenceSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeReadinessSnapshot {
    pub format_version: u32,
    pub process_state: NodeReadinessProcessState,
    pub active_generation: u64,
    pub router_generation: u64,
    pub default_agent_did: String,
    pub agents: Vec<AgentReadinessEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReadinessSourceEntry {
    pub agent_did: String,
    pub dispatcher_present: bool,
    pub unavailable_reason: Option<AgentReadinessUnavailableReason>,
    pub startup_demoted: bool,
    pub effective_inference: Option<EffectiveInferenceSettings>,
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

pub fn project_node_readiness_source(
    process_state: NodeReadinessProcessState,
    active_generation: u64,
    router_generation: u64,
    default_agent_did: impl Into<String>,
    sources: impl IntoIterator<Item = AgentReadinessSourceEntry>,
) -> Result<NodeReadinessSnapshot, String> {
    let default_agent_did = default_agent_did.into();
    if !is_canonical_id(&default_agent_did) {
        return Err(format!(
            "default agent {default_agent_did:?} is not canonical"
        ));
    }

    let mut agents = BTreeMap::new();
    for source in sources {
        if !is_canonical_id(&source.agent_did) {
            return Err(format!(
                "agent identifier {:?} is not canonical",
                source.agent_did
            ));
        }
        let entry = match effective_agent_readiness_admission(
            source.dispatcher_present,
            source.unavailable_reason,
            source.startup_demoted,
        ) {
            EffectiveAgentReadinessAdmission::Ready => Some(AgentReadinessEntry {
                agent_did: source.agent_did.clone(),
                state: AgentReadinessState::Ready,
                reason: None,
                effective_inference: source.effective_inference,
            }),
            EffectiveAgentReadinessAdmission::Unavailable(reason) => Some(AgentReadinessEntry {
                agent_did: source.agent_did.clone(),
                state: AgentReadinessState::Unavailable,
                reason: Some(reason),
                effective_inference: None,
            }),
            EffectiveAgentReadinessAdmission::Unassigned => None,
        };
        if agents.insert(source.agent_did, entry).is_some() {
            return Err("duplicate agent readiness source".to_string());
        }
    }
    let agents = agents.into_values().flatten().collect::<Vec<_>>();
    if !agents
        .iter()
        .any(|entry| entry.agent_did == default_agent_did)
    {
        return Err(format!(
            "default agent {default_agent_did:?} is not assigned"
        ));
    }

    Ok(NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state,
        active_generation,
        router_generation,
        default_agent_did,
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
    StorageUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeStorageObservation {
    #[default]
    Writable,
    Unavailable,
}

impl RuntimeStorageObservation {
    pub fn from_unavailable(unavailable: bool) -> Self {
        if unavailable {
            Self::Unavailable
        } else {
            Self::Writable
        }
    }

    pub fn is_unavailable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
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
    pub default_agent_did: Option<String>,
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

impl NodeReadinessSummary {
    /// Lean `Runtime.StartupDegraded.nodeReady`.
    pub fn is_ready(&self) -> bool {
        self.unavailable_agents
            .values()
            .all(|reason| !reason.degrades_node())
    }
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
    agent_dids: BTreeSet<String>,
    reason: AgentReadinessUnknownReason,
) -> NodeReadinessProjection {
    NodeReadinessProjection {
        active_generation: None,
        router_generation: None,
        default_agent_did: None,
        updated_at: None,
        unknown_reason: Some(reason),
        agents: agent_dids
            .into_iter()
            .map(|agent_did| (agent_did, ProjectedAgentReadiness::Unknown(reason)))
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
    let version = serde_json::from_str::<Version>(&row.snapshot_json)
        .map_err(|_| AgentReadinessUnknownReason::ReadinessMalformed)?;
    if version.format_version != NODE_READINESS_FORMAT_VERSION {
        return Err(AgentReadinessUnknownReason::ReadinessVersionUnsupported);
    }
    let snapshot = serde_json::from_str::<NodeReadinessSnapshot>(&row.snapshot_json)
        .map_err(|_| AgentReadinessUnknownReason::ReadinessMalformed)?;
    let entries_are_canonical = snapshot.agents.iter().all(|entry| {
        is_canonical_id(&entry.agent_did)
            && match entry.state {
                AgentReadinessState::Ready => entry.reason.is_none(),
                AgentReadinessState::Unavailable => entry.reason.is_some(),
            }
    }) && snapshot
        .agents
        .windows(2)
        .all(|pair| pair[0].agent_did < pair[1].agent_did);
    if !entries_are_canonical || !is_canonical_id(&snapshot.default_agent_did) {
        return Err(AgentReadinessUnknownReason::ReadinessMalformed);
    }
    if !snapshot
        .agents
        .iter()
        .any(|entry| entry.agent_did == snapshot.default_agent_did)
    {
        return Err(AgentReadinessUnknownReason::AgentNotAssigned);
    }
    Ok(snapshot)
}

pub fn project_node_readiness_summary_with_storage(
    row: Option<&NodeReadinessRow>,
    expected_node_did: &str,
    storage: RuntimeStorageObservation,
) -> ProjectedNodeReadinessSummary {
    if storage.is_unavailable() {
        return ProjectedNodeReadinessSummary::Unknown(
            AgentReadinessUnknownReason::StorageUnavailable,
        );
    }
    project_node_readiness_summary(row, expected_node_did)
}

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
                    entry.agent_did.clone(),
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

pub fn project_node_readiness_with_storage<'a>(
    row: Option<&NodeReadinessRow>,
    expected_node_did: &str,
    configured_agent_dids: impl IntoIterator<Item = &'a str>,
    configured_default_agent_did: Option<&str>,
    storage: RuntimeStorageObservation,
) -> NodeReadinessProjection {
    if storage.is_unavailable() {
        let mut agent_dids = BTreeSet::new();
        for agent_did in configured_agent_dids {
            if is_canonical_id(agent_did) {
                agent_dids.insert(agent_did.to_owned());
            }
        }
        if let Some(default_agent_did) = configured_default_agent_did {
            if is_canonical_id(default_agent_did) {
                agent_dids.insert(default_agent_did.to_owned());
            }
        }
        return unknown_projection(agent_dids, AgentReadinessUnknownReason::StorageUnavailable);
    }
    project_node_readiness(
        row,
        expected_node_did,
        configured_agent_dids,
        configured_default_agent_did,
    )
}

pub fn project_node_readiness<'a>(
    row: Option<&NodeReadinessRow>,
    expected_node_did: &str,
    configured_agent_dids: impl IntoIterator<Item = &'a str>,
    configured_default_agent_did: Option<&str>,
) -> NodeReadinessProjection {
    let mut agent_dids = BTreeSet::new();
    let mut configured_ids_malformed = false;
    for agent_did in configured_agent_dids {
        if !is_canonical_id(agent_did) {
            configured_ids_malformed = true;
        } else {
            agent_dids.insert(agent_did.to_owned());
        }
    }
    if let Some(default_agent_did) = configured_default_agent_did {
        if !is_canonical_id(default_agent_did) {
            configured_ids_malformed = true;
        } else {
            agent_dids.insert(default_agent_did.to_owned());
        }
    }
    if configured_ids_malformed {
        return unknown_projection(agent_dids, AgentReadinessUnknownReason::ReadinessMalformed);
    }

    let Some(row) = row else {
        return unknown_projection(agent_dids, AgentReadinessUnknownReason::ReadinessMissing);
    };
    let snapshot = match decode_node_readiness_snapshot(row, expected_node_did) {
        Ok(snapshot) => snapshot,
        Err(reason) => return unknown_projection(agent_dids, reason),
    };

    let entries = snapshot
        .agents
        .into_iter()
        .map(|entry| (entry.agent_did.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    agent_dids.extend(entries.keys().cloned());
    debug_assert!(entries.contains_key(&snapshot.default_agent_did));
    let global_unknown = if !snapshot.process_state.accepts_work() {
        Some(AgentReadinessUnknownReason::ProcessNotReady)
    } else if snapshot.active_generation == 0
        || snapshot.router_generation != snapshot.active_generation
    {
        Some(AgentReadinessUnknownReason::RouterGenerationStale)
    } else {
        None
    };
    let agents = agent_dids
        .into_iter()
        .map(|agent_did| {
            let state = if let Some(reason) = global_unknown {
                ProjectedAgentReadiness::Unknown(reason)
            } else {
                match entries.get(&agent_did) {
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
            (agent_did, state)
        })
        .collect();
    NodeReadinessProjection {
        active_generation: Some(snapshot.active_generation),
        router_generation: Some(snapshot.router_generation),
        default_agent_did: Some(snapshot.default_agent_did),
        updated_at: Some(row.updated_at.clone()),
        unknown_reason: global_unknown,
        agents,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcilePhase {
    Idle,
    Debouncing,
    Resolving,
    Diffing,
    Applying,
}

impl ReconcilePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Debouncing => "debouncing",
            Self::Resolving => "resolving",
            Self::Diffing => "diffing",
            Self::Applying => "applying",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileResult {
    Startup,
    Noop,
    Applied,
    Error,
}

impl ReconcileResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Noop => "noop",
            Self::Applied => "applied",
            Self::Error => "error",
        }
    }
}

#[cfg(test)]
#[path = "node_readiness/tests.rs"]
mod tests;

#[cfg(test)]
mod conformance {
    use super::ReconcilePhase;

    #[test]
    fn reconcile_phases_are_the_lean_reconcile_phases() {
        gents_lean_vocab::families::reconcile_phase_vocabulary::replay(&[
            ReconcilePhase::Idle,
            ReconcilePhase::Debouncing,
            ReconcilePhase::Resolving,
            ReconcilePhase::Diffing,
            ReconcilePhase::Applying,
        ]);
    }
}
