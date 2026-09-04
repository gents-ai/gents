use std::collections::BTreeMap;

use anyhow::{Context, Result};
use gents::{
    backend_registry::list_enabled_backends, list_agent_behaviors, load_agent_behavior,
    AgentBehaviorDocument, InferenceBackend,
};
use gents_codex_protocol as codex;
use gents_protocol::row::{
    project_behavior_readiness_summary, BehaviorReadinessState, ProjectedBehaviorReadinessSummary,
};
use serde_json::{json, Value};

use super::super::bound_behavior::{model_selection_id, parse_model_selection_id};
use super::super::protocol::{
    absolute_path, backend_model_summary, send_error, send_typed_json_result,
};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};

pub(super) async fn apply_config_writes(
    outbound: &Outbound,
    state: &ShimState,
    request_id: codex::RequestId,
    writes: Vec<(String, Value)>,
) -> Result<()> {
    for (key_path, value) in writes {
        if key_path != "model" {
            continue;
        }
        let new_model_id = match value.as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    "ConfigValueWrite for `model` requires a non-empty string".to_string(),
                )
                .await;
            }
        };
        let selection = match resolve_model_selection(state, &new_model_id).await {
            Ok(selection) => selection,
            Err(err) => {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    err.to_string(),
                )
                .await;
            }
        };
        apply_model_to_bound_behavior(state, &selection).await?;
    }
    send_typed_json_result::<codex::ConfigWriteResponse>(
        outbound,
        request_id,
        json!({
            "status": "ok",
            "version": "gents-shim",
            "filePath": absolute_path(&state.codex_home.join("config.toml")),
            "overriddenMetadata": null
        }),
    )
    .await
}

struct ModelSelection {
    backend_id: String,
    model_name: String,
}

pub(super) async fn load_bound_behavior(state: &ShimState) -> Result<AgentBehaviorDocument> {
    let behavior_id = state.behavior_id.as_ref();
    load_agent_behavior(state.node.as_ref(), behavior_id)
        .await
        .context("loading bound AgentBehavior")?
        .ok_or_else(|| anyhow::anyhow!("bound AgentBehavior {behavior_id:?} disappeared"))
}

pub(super) async fn available_model_backends(state: &ShimState) -> Result<Vec<InferenceBackend>> {
    let mut backends = list_enabled_backends(state.node.as_ref()).await?;
    let accepting = backend_admission_from_local_readiness(state).await?;
    backends.retain(|backend| accepting.get(&backend.backend_id).copied().unwrap_or(false));
    backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    Ok(backends)
}

/// Per-backend "accepting admission" from this agent's own behavior
/// readiness projection — never from `InferenceBackend`'s
/// `enabled`/`probe_status` (measured health stays unpersisted; #640, see
/// `backend_health.rs`). Mirrors `fleet_slots.rs::backend_admission_from_readiness`:
/// a backend accepts once any locally bound behavior is reported `Ready`;
/// with no such signal it fails closed to not-accepting.
async fn backend_admission_from_local_readiness(
    state: &ShimState,
) -> Result<BTreeMap<String, bool>> {
    let behaviors = list_agent_behaviors(state.node.as_ref(), state.agent_did.as_ref())
        .await
        .context("listing agent behaviors for model selection")?;
    let readiness_row = crate::commands::status::load_behavior_readiness(
        &ConfigAccess::Local(state.node.clone()),
        state.agent_did.as_ref(),
    )
    .await
    .context("loading behavior readiness for model selection")?;
    Ok(backend_admission_from_readiness_row(
        &behaviors,
        readiness_row.as_ref(),
        state.agent_did.as_ref(),
        chrono::Utc::now(),
    ))
}

/// Pure core of `backend_admission_from_local_readiness`, split out so it is
/// testable without a live `EmbeddedNode`/`ShimState`.
fn backend_admission_from_readiness_row(
    behaviors: &[AgentBehaviorDocument],
    readiness_row: Option<&gents_protocol::row::AgentBehaviorReadinessRow>,
    agent_did: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> BTreeMap<String, bool> {
    let projected = project_behavior_readiness_summary(readiness_row, agent_did, observed_at);
    let mut accepting = BTreeMap::<String, bool>::new();
    for behavior in behaviors {
        let Some(backend_id) = behavior
            .backend_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let ready = matches!(
            &projected,
            ProjectedBehaviorReadinessSummary::Observed(summary)
                if summary.snapshot.behaviors.iter().any(|entry| {
                    entry.behavior_id == behavior.behavior_id
                        && entry.state == BehaviorReadinessState::Ready
                })
        );
        let entry = accepting.entry(backend_id.to_string()).or_insert(false);
        *entry = *entry || ready;
    }
    accepting
}

pub(super) fn model_list_entries(
    backends: &[InferenceBackend],
    behavior: &AgentBehaviorDocument,
) -> Vec<Value> {
    let current_backend_id = behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let current_model_name = behavior
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut entries = backends
        .iter()
        .flat_map(|backend| {
            backend
                .models
                .iter()
                .map(move |model_name| (backend, model_name.trim()))
        })
        .filter(|(_, model_name)| !model_name.is_empty())
        .map(|(backend, model_name)| {
            let selection_id = model_selection_id(&backend.backend_id, model_name);
            let is_default = current_backend_id == Some(backend.backend_id.as_str())
                && current_model_name == Some(model_name);
            backend_model_summary(backend, model_name, &selection_id, is_default)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.get("displayName")
            .and_then(Value::as_str)
            .cmp(&right.get("displayName").and_then(Value::as_str))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("id").and_then(Value::as_str))
            })
    });
    entries
}

async fn resolve_model_selection(
    state: &ShimState,
    requested_model: &str,
) -> Result<ModelSelection> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        anyhow::bail!("ConfigValueWrite for `model` requires a non-empty string");
    }

    let behavior = load_bound_behavior(state).await?;
    let current_backend_id = behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let backends = available_model_backends(state).await?;
    let target = if let Some((backend_id, model_name)) = parse_model_selection_id(requested_model) {
        backends
            .iter()
            .find(|backend| {
                backend.backend_id == backend_id && backend_has_model(backend, model_name)
            })
            .map(|backend| (backend, model_name))
    } else {
        backends
            .iter()
            .find(|backend| {
                current_backend_id == Some(backend.backend_id.as_str())
                    && backend_has_model(backend, requested_model)
            })
            .or_else(|| {
                backends
                    .iter()
                    .find(|backend| backend_has_model(backend, requested_model))
            })
            .map(|backend| (backend, requested_model))
    };
    let Some((backend, model_name)) = target else {
        let available = backends
            .iter()
            .flat_map(|backend| backend.models.iter())
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "model {requested_model:?} not found in any available InferenceBackend; available models: [{available}]"
        );
    };

    Ok(ModelSelection {
        backend_id: backend.backend_id.clone(),
        model_name: model_name.to_string(),
    })
}

fn backend_has_model(backend: &InferenceBackend, model_name: &str) -> bool {
    backend
        .models
        .iter()
        .any(|model| model.trim() == model_name)
}

async fn apply_model_to_bound_behavior(
    state: &ShimState,
    selection: &ModelSelection,
) -> Result<()> {
    let mut behavior = load_bound_behavior(state).await?;
    behavior.backend_id = Some(selection.backend_id.clone());
    behavior.model_name = Some(selection.model_name.clone());
    let access = ConfigAccess::Graphql(state.graphql.as_ref().to_string());
    write_agent_behavior_document(&access, &behavior)
        .await
        .context("writing AgentBehavior with selected backend model")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::row::{
        AgentBehaviorReadinessRow, BehaviorReadinessEntry, BehaviorReadinessProcessState,
        BehaviorReadinessSnapshot, BehaviorReadinessUnavailableReason,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };

    fn behavior(behavior_id: &str, backend_id: &str) -> AgentBehaviorDocument {
        AgentBehaviorDocument {
            behavior_id: behavior_id.to_string(),
            agent_did: "did:test:codex-shim".to_string(),
            display_name: None,
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: Some(backend_id.to_string()),
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            created_at: None,
        }
    }

    fn readiness_row(
        agent_did: &str,
        default_behavior_id: &str,
        entries: Vec<(&str, bool)>,
        updated_at: &str,
    ) -> AgentBehaviorReadinessRow {
        AgentBehaviorReadinessRow {
            agent_did: agent_did.to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state: BehaviorReadinessProcessState::Ready,
                active_generation: 1,
                router_generation: 1,
                default_behavior_id: default_behavior_id.to_string(),
                behaviors: entries
                    .into_iter()
                    .map(|(behavior_id, ready)| BehaviorReadinessEntry {
                        behavior_id: behavior_id.to_string(),
                        state: if ready {
                            BehaviorReadinessState::Ready
                        } else {
                            BehaviorReadinessState::Unavailable
                        },
                        reason: if ready {
                            None
                        } else {
                            Some(BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable)
                        },
                    })
                    .collect(),
            })
            .unwrap(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn backend_vetoed_by_readiness_is_not_offered_even_though_document_is_healthy() {
        // `available_model_backends` starts from `list_enabled_backends`, so
        // this exercises the case that matters: the `InferenceBackend`
        // document itself would read enabled+healthy, but this runtime's
        // local prober vetoed it — that veto only ever reaches the readiness
        // projection (#640; measured health is never persisted to the
        // document).
        let agent_did = "did:test:codex-shim";
        let behaviors = vec![behavior("default", "backend-a")];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", false)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        let accepting =
            backend_admission_from_readiness_row(&behaviors, Some(&row), agent_did, observed_at);

        assert_eq!(
            accepting.get("backend-a").copied(),
            Some(false),
            "a backend the local prober vetoed via readiness must not be offered for \
             selection even though the InferenceBackend document itself would read healthy"
        );
    }

    #[test]
    fn ready_behavior_offers_its_backend() {
        let agent_did = "did:test:codex-shim";
        let behaviors = vec![behavior("default", "backend-a")];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        let accepting =
            backend_admission_from_readiness_row(&behaviors, Some(&row), agent_did, observed_at);

        assert_eq!(accepting.get("backend-a").copied(), Some(true));
    }

    #[test]
    fn missing_readiness_row_fails_closed() {
        let agent_did = "did:test:codex-shim";
        let behaviors = vec![behavior("default", "backend-a")];
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        let accepting =
            backend_admission_from_readiness_row(&behaviors, None, agent_did, observed_at);

        assert_eq!(accepting.get("backend-a").copied(), Some(false));
    }
}
