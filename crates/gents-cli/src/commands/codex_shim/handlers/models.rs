use anyhow::{Context, Result};
use gents::{
    backend_registry::list_enabled_backends, list_agent_behaviors, load_agent_behavior,
    AgentBehaviorDocument, ConfigReferences, InferenceBackend,
};
use gents_codex_protocol as codex;
use gents_protocol::row::{
    project_behavior_readiness_summary, BehaviorReadinessUnavailableReason,
    ProjectedBehaviorReadinessSummary,
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
    // The model list is a *configuration* surface — pick a backend to bind a
    // behavior to — not an admission decision, so it starts from the
    // document's configured intent (`enabled` backends, as before #1332)
    // and must keep working before this runtime has published any
    // readiness at all (fresh stores, config-only sessions, a backend that
    // has no behavior bound to it yet). It only drops a backend on an
    // *explicit* readiness veto; `fleet_slots.rs`/`healthz` are the
    // admission-reporting surfaces and keep their fail-closed behavior.
    let mut backends = list_enabled_backends(state.node.as_ref()).await?;
    let behaviors = list_agent_behaviors(state.node.as_ref(), state.agent_did.as_ref())
        .await
        .context("listing agent behaviors for model selection")?;
    let readiness_row = crate::commands::status::load_behavior_readiness(
        &ConfigAccess::Local(state.node.clone()),
        state.agent_did.as_ref(),
    )
    .await
    .context("loading behavior readiness for model selection")?;
    let observed_at = chrono::Utc::now();
    backends.retain(|backend| {
        !backend_vetoed_by_readiness(
            &backend.backend_id,
            &behaviors,
            readiness_row.as_ref(),
            state.agent_did.as_ref(),
            observed_at,
        )
    });
    backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    Ok(backends)
}

/// Whether the readiness projection *explicitly* vetoes `backend_id` — never
/// a "we don't know" signal. True only when a readiness row exists for this
/// agent and every behavior currently bound to `backend_id` is reported
/// `Unavailable` with a backend-related reason (`BackendDisabled` or
/// `BackendTemporarilyUnavailable`). No readiness row, no behaviors bound
/// yet, or at least one bound behavior that's `Ready` or unavailable for an
/// unrelated reason — all read as "not vetoed."
fn backend_vetoed_by_readiness(
    backend_id: &str,
    behaviors: &[AgentBehaviorDocument],
    readiness_row: Option<&gents_protocol::row::AgentBehaviorReadinessRow>,
    agent_did: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(readiness_row) = readiness_row else {
        return false;
    };
    let summary =
        match project_behavior_readiness_summary(Some(readiness_row), agent_did, observed_at) {
            ProjectedBehaviorReadinessSummary::Observed(summary) => summary,
            ProjectedBehaviorReadinessSummary::Unknown(_) => return false,
        };

    let bound_behavior_ids = behaviors
        .iter()
        .filter(|behavior| {
            behavior
                .backend_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                == Some(backend_id)
        })
        .map(|behavior| behavior.behavior_id.as_str())
        .collect::<Vec<_>>();

    if bound_behavior_ids.is_empty() {
        return false;
    }

    bound_behavior_ids.into_iter().all(|behavior_id| {
        matches!(
            summary.unavailable_behaviors.get(behavior_id),
            Some(
                BehaviorReadinessUnavailableReason::BackendDisabled
                    | BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable
            )
        )
    })
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
    // Single owner (#1331): confirm the backend still exists and still
    // advertises this model through the same validator every other write
    // door calls, rather than trusting `resolve_model_selection`'s own
    // backend/model match alone. `resolve_model_selection` picks a
    // candidate from the readiness-filtered "available" set for product
    // reasons (don't offer a disabled/unready backend); this is the
    // unconditional correctness gate right before the write.
    let refs = ConfigReferences::load(&state.node, state.agent_did.as_ref())
        .await
        .context("loading config references to validate the selected model")?;
    behavior
        .validate_references(&refs)
        .context("selected backend/model failed reference validation")?;
    let access = ConfigAccess::Graphql(state.graphql.as_ref().to_string());
    write_agent_behavior_document(&access, &behavior)
        .await
        .context("writing AgentBehavior with selected backend model")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::codex_shim::CodexSidecar;
    use gents::config_client::{
        write_inference_backend_document, ConfigAccess as LocalConfigAccess,
        InferenceBackendUpsertDocument,
    };
    use gents::{defra_node::EmbeddedNode, upsert_agent_behavior, BackendProviderKind};
    use gents_protocol::row::{
        AgentBehaviorReadinessRow, BehaviorReadinessEntry, BehaviorReadinessProcessState,
        BehaviorReadinessSnapshot, BehaviorReadinessState, BehaviorReadinessUnavailableReason,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

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
    fn backend_vetoed_when_every_bound_behavior_is_explicitly_vetoed() {
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

        assert!(
            backend_vetoed_by_readiness(
                "backend-a",
                &behaviors,
                Some(&row),
                agent_did,
                observed_at
            ),
            "a backend the local prober vetoed via readiness must not be offered for \
             selection even though the InferenceBackend document itself would read healthy"
        );
    }

    #[test]
    fn backend_not_vetoed_when_bound_behavior_is_ready() {
        let agent_did = "did:test:codex-shim";
        let behaviors = vec![behavior("default", "backend-a")];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(!backend_vetoed_by_readiness(
            "backend-a",
            &behaviors,
            Some(&row),
            agent_did,
            observed_at
        ));
    }

    #[test]
    fn missing_readiness_row_leaves_configured_backend_offered() {
        // The model list is a configuration surface (choose a backend to
        // configure a behavior with) and must work before this runtime has
        // published any readiness at all — a fresh store, a config-only
        // session. Absence of a readiness row is not a veto.
        let agent_did = "did:test:codex-shim";
        let behaviors = vec![behavior("default", "backend-a")];
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(!backend_vetoed_by_readiness(
            "backend-a",
            &behaviors,
            None,
            agent_did,
            observed_at
        ));
    }

    #[test]
    fn backend_with_no_bound_behaviors_is_not_vetoed() {
        // A brand-new backend with no `AgentBehavior` bound to it yet (the
        // exact shape of a backend just created for configuration) has
        // nothing in the readiness projection to veto it with.
        let agent_did = "did:test:codex-shim";
        let behaviors = vec![behavior("default", "backend-a")];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(!backend_vetoed_by_readiness(
            "backend-b",
            &behaviors,
            Some(&row),
            agent_did,
            observed_at
        ));
    }

    async fn shim_state_for(
        node: &Arc<EmbeddedNode>,
        agent_did: &str,
        behavior_id: &str,
    ) -> ShimState {
        ShimState {
            codex_home: std::env::temp_dir(),
            trace_path: std::env::temp_dir().join("codex-shim-model-test-trace.jsonl"),
            cwd: std::env::temp_dir(),
            fs_root: None,
            node: node.clone(),
            background_execution_registry: gents::BackgroundExecutionRegistry::default(),
            // Never dialed: `apply_model_to_bound_behavior` validates
            // references before it ever reaches the write, so a rejected
            // selection never touches this endpoint.
            graphql: Arc::from("http://127.0.0.1:0/graphql"),
            agent_did: Arc::from(agent_did),
            behavior_id: Arc::from(behavior_id),
            id_counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
            sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
            auth_token: None,
        }
    }

    /// Single owner (#1331): `apply_model_to_bound_behavior` must reject a
    /// backend/model pair the backend doesn't advertise through
    /// `AgentBehavior::validate_references`, the same validator every other
    /// config write door calls — not just trust `resolve_model_selection`'s
    /// own match.
    #[tokio::test]
    async fn apply_model_to_bound_behavior_rejects_an_unadvertised_model() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();

        let local_access = LocalConfigAccess::Local(node.clone());
        write_inference_backend_document(
            &local_access,
            &InferenceBackendUpsertDocument {
                backend_id: "backend-a".to_string(),
                name: "Backend A".to_string(),
                provider_kind: BackendProviderKind::OpenAiCompatible,
                openai_wire_api: None,
                endpoint: "http://127.0.0.1:11434/v1".to_string(),
                api_key: None,
                api_key_env_var: None,
                max_concurrent: 4,
                max_queue_depth: 100,
                enabled: true,
                models_on_add: vec!["model-x".to_string()],
                models_on_update: None,
                probe_status: "healthy".to_string(),
            },
        )
        .await
        .expect("seed backend");

        let agent_did = "did:test:codex-shim";
        let mut default_behavior = behavior("default", "backend-a");
        default_behavior.model_name = Some("model-x".to_string());
        upsert_agent_behavior(&node, &default_behavior)
            .await
            .expect("seed behavior");

        let state = shim_state_for(&node, agent_did, "default").await;
        let selection = ModelSelection {
            backend_id: "backend-a".to_string(),
            model_name: "not-advertised".to_string(),
        };

        let error = apply_model_to_bound_behavior(&state, &selection)
            .await
            .expect_err("an unadvertised model must be rejected");
        // `{:#}` (anyhow's alternate Display) walks the full context chain;
        // `{}` would only show this call's own wrapping context.
        assert!(
            format!("{error:#}").contains("does not advertise"),
            "{error:#}"
        );
    }

    /// The mirror-image happy path: a genuinely-advertised model commits.
    #[tokio::test]
    async fn apply_model_to_bound_behavior_accepts_an_advertised_model() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();

        let local_access = LocalConfigAccess::Local(node.clone());
        write_inference_backend_document(
            &local_access,
            &InferenceBackendUpsertDocument {
                backend_id: "backend-a".to_string(),
                name: "Backend A".to_string(),
                provider_kind: BackendProviderKind::OpenAiCompatible,
                openai_wire_api: None,
                endpoint: "http://127.0.0.1:11434/v1".to_string(),
                api_key: None,
                api_key_env_var: None,
                max_concurrent: 4,
                max_queue_depth: 100,
                enabled: true,
                models_on_add: vec!["model-x".to_string(), "model-y".to_string()],
                models_on_update: None,
                probe_status: "healthy".to_string(),
            },
        )
        .await
        .expect("seed backend");

        let agent_did = "did:test:codex-shim";
        let mut default_behavior = behavior("default", "backend-a");
        default_behavior.model_name = Some("model-x".to_string());
        upsert_agent_behavior(&node, &default_behavior)
            .await
            .expect("seed behavior");

        let state = shim_state_for(&node, agent_did, "default").await;
        let selection = ModelSelection {
            backend_id: "backend-a".to_string(),
            model_name: "model-y".to_string(),
        };

        // The write itself dials the dummy `graphql` endpoint and will fail
        // to connect — that failure, distinct from a validation rejection,
        // is exactly the signal that validation passed.
        let error = apply_model_to_bound_behavior(&state, &selection)
            .await
            .expect_err("the dummy graphql endpoint is unreachable");
        assert!(
            !format!("{error:#}").contains("does not advertise"),
            "an advertised model must clear validation: {error:#}"
        );
    }
}
