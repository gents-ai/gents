//! Canonical runtime-authored behavior-readiness wire contract.
//!
//! Runtime configuration is never treated as proof that a behavior can accept
//! work. The source projector admits only installed dispatchers not vetoed by
//! explicit unavailability or a generation-owned startup demotion. The client
//! projector then fails closed on missing, malformed, non-ready, or stale
//! observations.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const BEHAVIOR_READINESS_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BehaviorReadinessProcessState {
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

impl BehaviorReadinessProcessState {
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
pub enum BehaviorReadinessState {
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorReadinessUnavailableReason {
    BehaviorDisabled,
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

impl BehaviorReadinessUnavailableReason {
    /// Stable presentation-safe admission message. Resolver diagnostics are
    /// deliberately excluded from durable request state and client views.
    pub const fn public_message(self) -> &'static str {
        match self {
            Self::BehaviorDisabled => "behavior is disabled",
            Self::RuntimeConfigurationInvalid => "runtime configuration is invalid",
            Self::BackendNotConfigured => "inference backend is not configured",
            Self::BackendDisabled => "inference backend is disabled",
            Self::BackendTemporarilyUnavailable => "inference backend is temporarily unavailable",
            Self::CredentialsRequired => "inference credentials are required",
            Self::InferenceProfileInvalid => "inference profile is invalid",
            Self::ToolConfigurationInvalid => "tool configuration is invalid",
            Self::ToolSurfaceUnavailable => "tool surface is unavailable",
            Self::ExecutorStartFailed => "behavior executor could not start",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorReadinessEntry {
    pub behavior_id: String,
    pub state: BehaviorReadinessState,
    pub reason: Option<BehaviorReadinessUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorReadinessSnapshot {
    pub format_version: u32,
    pub process_state: BehaviorReadinessProcessState,
    pub active_generation: u64,
    pub router_generation: u64,
    pub default_behavior_id: String,
    pub behaviors: Vec<BehaviorReadinessEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorReadinessSourceEntry {
    pub behavior_id: String,
    pub dispatcher_present: bool,
    pub unavailable_reason: Option<BehaviorReadinessUnavailableReason>,
    pub startup_demoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveBehaviorReadinessAdmission {
    Ready,
    Unavailable(BehaviorReadinessUnavailableReason),
    Unassigned,
}

pub fn effective_behavior_readiness_admission(
    dispatcher_present: bool,
    unavailable_reason: Option<BehaviorReadinessUnavailableReason>,
    startup_demoted: bool,
) -> EffectiveBehaviorReadinessAdmission {
    if startup_demoted {
        EffectiveBehaviorReadinessAdmission::Unavailable(
            BehaviorReadinessUnavailableReason::ExecutorStartFailed,
        )
    } else if let Some(reason) = unavailable_reason {
        EffectiveBehaviorReadinessAdmission::Unavailable(reason)
    } else if dispatcher_present {
        EffectiveBehaviorReadinessAdmission::Ready
    } else {
        EffectiveBehaviorReadinessAdmission::Unassigned
    }
}

/// Pure source projector shared by the runtime publisher and Lean-generated
/// conformance harness. Unavailability and startup demotion veto a dispatcher.
pub fn project_behavior_readiness_source(
    process_state: BehaviorReadinessProcessState,
    active_generation: u64,
    router_generation: u64,
    default_behavior_id: impl Into<String>,
    sources: impl IntoIterator<Item = BehaviorReadinessSourceEntry>,
) -> Result<BehaviorReadinessSnapshot, String> {
    let default_behavior_id = default_behavior_id.into();
    if !is_canonical_id(&default_behavior_id) {
        return Err(format!(
            "default behavior {default_behavior_id:?} is not canonical"
        ));
    }

    let mut behaviors = BTreeMap::new();
    for source in sources {
        if !is_canonical_id(&source.behavior_id) {
            return Err(format!(
                "behavior identifier {:?} is not canonical",
                source.behavior_id
            ));
        }
        let entry = match effective_behavior_readiness_admission(
            source.dispatcher_present,
            source.unavailable_reason,
            source.startup_demoted,
        ) {
            EffectiveBehaviorReadinessAdmission::Ready => Some(BehaviorReadinessEntry {
                behavior_id: source.behavior_id.clone(),
                state: BehaviorReadinessState::Ready,
                reason: None,
            }),
            EffectiveBehaviorReadinessAdmission::Unavailable(reason) => {
                Some(BehaviorReadinessEntry {
                    behavior_id: source.behavior_id.clone(),
                    state: BehaviorReadinessState::Unavailable,
                    reason: Some(reason),
                })
            }
            EffectiveBehaviorReadinessAdmission::Unassigned => None,
        };
        if behaviors.insert(source.behavior_id, entry).is_some() {
            return Err("duplicate behavior readiness source".to_string());
        }
    }
    let behaviors = behaviors.into_values().flatten().collect::<Vec<_>>();
    if !behaviors
        .iter()
        .any(|entry| entry.behavior_id == default_behavior_id)
    {
        return Err(format!(
            "default behavior {default_behavior_id:?} is not assigned"
        ));
    }

    Ok(BehaviorReadinessSnapshot {
        format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
        process_state,
        active_generation,
        router_generation,
        default_behavior_id,
        behaviors,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBehaviorReadinessRow {
    pub agent_did: String,
    pub snapshot_json: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorReadinessUnknownReason {
    ReadinessMissing,
    ReadinessMalformed,
    ReadinessVersionUnsupported,
    ProcessNotReady,
    RouterGenerationStale,
    BehaviorNotAssigned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedBehaviorReadiness {
    Ready,
    Unavailable(BehaviorReadinessUnavailableReason),
    Unknown(BehaviorReadinessUnknownReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorReadinessProjection {
    pub active_generation: Option<u64>,
    pub router_generation: Option<u64>,
    pub default_behavior_id: Option<String>,
    pub updated_at: Option<String>,
    pub unknown_reason: Option<BehaviorReadinessUnknownReason>,
    pub behaviors: BTreeMap<String, ProjectedBehaviorReadiness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorReadinessSummary {
    pub snapshot: BehaviorReadinessSnapshot,
    pub ready_count: usize,
    pub unavailable_behaviors: BTreeMap<String, BehaviorReadinessUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedBehaviorReadinessSummary {
    Observed(BehaviorReadinessSummary),
    Unknown(BehaviorReadinessUnknownReason),
}

fn is_canonical_id(value: &str) -> bool {
    !value.is_empty() && value == value.trim()
}

fn unknown_projection(
    behavior_ids: BTreeSet<String>,
    reason: BehaviorReadinessUnknownReason,
) -> BehaviorReadinessProjection {
    BehaviorReadinessProjection {
        active_generation: None,
        router_generation: None,
        default_behavior_id: None,
        updated_at: None,
        unknown_reason: Some(reason),
        behaviors: behavior_ids
            .into_iter()
            .map(|behavior_id| (behavior_id, ProjectedBehaviorReadiness::Unknown(reason)))
            .collect(),
    }
}

fn decode_canonical_readiness_snapshot(
    row: &AgentBehaviorReadinessRow,
    expected_agent_did: &str,
) -> Result<BehaviorReadinessSnapshot, BehaviorReadinessUnknownReason> {
    if !is_canonical_id(expected_agent_did)
        || row.agent_did != expected_agent_did
        || !is_canonical_id(&row.agent_did)
    {
        return Err(BehaviorReadinessUnknownReason::ReadinessMalformed);
    }
    let snapshot = serde_json::from_str::<BehaviorReadinessSnapshot>(&row.snapshot_json)
        .map_err(|_| BehaviorReadinessUnknownReason::ReadinessMalformed)?;
    if snapshot.format_version != BEHAVIOR_READINESS_FORMAT_VERSION {
        return Err(BehaviorReadinessUnknownReason::ReadinessVersionUnsupported);
    }
    let entries_are_canonical = snapshot.behaviors.iter().all(|entry| {
        is_canonical_id(&entry.behavior_id)
            && match entry.state {
                BehaviorReadinessState::Ready => entry.reason.is_none(),
                BehaviorReadinessState::Unavailable => entry.reason.is_some(),
            }
    }) && snapshot
        .behaviors
        .windows(2)
        .all(|pair| pair[0].behavior_id < pair[1].behavior_id);
    if !entries_are_canonical || !is_canonical_id(&snapshot.default_behavior_id) {
        return Err(BehaviorReadinessUnknownReason::ReadinessMalformed);
    }
    if !snapshot
        .behaviors
        .iter()
        .any(|entry| entry.behavior_id == snapshot.default_behavior_id)
    {
        return Err(BehaviorReadinessUnknownReason::BehaviorNotAssigned);
    }
    Ok(snapshot)
}

/// Strict operational summary from the sole durable readiness authority.
/// Missing, malformed, non-ready, or generation-skewed observations fail
/// closed and never manufacture behavior counts from configuration rows.
pub fn project_behavior_readiness_summary(
    row: Option<&AgentBehaviorReadinessRow>,
    expected_agent_did: &str,
) -> ProjectedBehaviorReadinessSummary {
    let Some(row) = row else {
        return ProjectedBehaviorReadinessSummary::Unknown(
            BehaviorReadinessUnknownReason::ReadinessMissing,
        );
    };
    let snapshot = match decode_canonical_readiness_snapshot(row, expected_agent_did) {
        Ok(snapshot) => snapshot,
        Err(reason) => return ProjectedBehaviorReadinessSummary::Unknown(reason),
    };
    if !snapshot.process_state.accepts_work() {
        return ProjectedBehaviorReadinessSummary::Unknown(
            BehaviorReadinessUnknownReason::ProcessNotReady,
        );
    }
    if snapshot.active_generation == 0 || snapshot.router_generation != snapshot.active_generation {
        return ProjectedBehaviorReadinessSummary::Unknown(
            BehaviorReadinessUnknownReason::RouterGenerationStale,
        );
    }
    let mut ready_count = 0;
    let mut unavailable_behaviors = BTreeMap::new();
    for entry in &snapshot.behaviors {
        match entry.state {
            BehaviorReadinessState::Ready => ready_count += 1,
            BehaviorReadinessState::Unavailable => {
                unavailable_behaviors.insert(
                    entry.behavior_id.clone(),
                    entry
                        .reason
                        .expect("canonical unavailable entry has reason"),
                );
            }
        }
    }
    ProjectedBehaviorReadinessSummary::Observed(BehaviorReadinessSummary {
        snapshot,
        ready_count,
        unavailable_behaviors,
    })
}

/// Project the runtime-authored row into the only legal client readiness
/// states. Configured identifiers are validated exactly, never normalized.
pub fn project_behavior_readiness<'a>(
    row: Option<&AgentBehaviorReadinessRow>,
    expected_agent_did: &str,
    configured_behavior_ids: impl IntoIterator<Item = &'a str>,
    configured_default_behavior_id: Option<&str>,
) -> BehaviorReadinessProjection {
    let mut behavior_ids = BTreeSet::new();
    let mut configured_ids_malformed = false;
    for behavior_id in configured_behavior_ids {
        if !is_canonical_id(behavior_id) {
            configured_ids_malformed = true;
        } else {
            behavior_ids.insert(behavior_id.to_owned());
        }
    }
    if let Some(default_behavior_id) = configured_default_behavior_id {
        if !is_canonical_id(default_behavior_id) {
            configured_ids_malformed = true;
        } else {
            behavior_ids.insert(default_behavior_id.to_owned());
        }
    }
    if configured_ids_malformed {
        return unknown_projection(
            behavior_ids,
            BehaviorReadinessUnknownReason::ReadinessMalformed,
        );
    }

    let Some(row) = row else {
        return unknown_projection(
            behavior_ids,
            BehaviorReadinessUnknownReason::ReadinessMissing,
        );
    };
    let snapshot = match decode_canonical_readiness_snapshot(row, expected_agent_did) {
        Ok(snapshot) => snapshot,
        Err(reason) => return unknown_projection(behavior_ids, reason),
    };

    let entries = snapshot
        .behaviors
        .into_iter()
        .map(|entry| (entry.behavior_id.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    behavior_ids.extend(entries.keys().cloned());
    debug_assert!(entries.contains_key(&snapshot.default_behavior_id));
    let global_unknown = if !snapshot.process_state.accepts_work() {
        Some(BehaviorReadinessUnknownReason::ProcessNotReady)
    } else if snapshot.active_generation == 0
        || snapshot.router_generation != snapshot.active_generation
    {
        Some(BehaviorReadinessUnknownReason::RouterGenerationStale)
    } else {
        None
    };
    let behaviors = behavior_ids
        .into_iter()
        .map(|behavior_id| {
            let state = if let Some(reason) = global_unknown {
                ProjectedBehaviorReadiness::Unknown(reason)
            } else {
                match entries.get(&behavior_id) {
                    Some(BehaviorReadinessEntry {
                        state: BehaviorReadinessState::Ready,
                        reason: None,
                        ..
                    }) => ProjectedBehaviorReadiness::Ready,
                    Some(BehaviorReadinessEntry {
                        state: BehaviorReadinessState::Unavailable,
                        reason: Some(reason),
                        ..
                    }) => ProjectedBehaviorReadiness::Unavailable(*reason),
                    Some(_) => unreachable!("canonical readiness entry checked above"),
                    None => ProjectedBehaviorReadiness::Unknown(
                        BehaviorReadinessUnknownReason::BehaviorNotAssigned,
                    ),
                }
            };
            (behavior_id, state)
        })
        .collect();
    BehaviorReadinessProjection {
        active_generation: Some(snapshot.active_generation),
        router_generation: Some(snapshot.router_generation),
        default_behavior_id: Some(snapshot.default_behavior_id),
        updated_at: Some(row.updated_at.clone()),
        unknown_reason: global_unknown,
        behaviors,
    }
}

#[cfg(test)]
#[path = "behavior_readiness/tests.rs"]
mod tests;
