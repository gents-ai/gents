use anyhow::{Context, Result};
use gents::backend_registry::{
    list_enabled_backends_for_agent, lookup_backend_observation, lookup_backend_observation_in_txn,
};
use gents::config_client::{
    apply_desired_state_plan, load_inference_backend_in_txn, read_desired_state_record_in_txn,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{BackendAuth, BackendModelCatalog};
use gents::{
    list_agent_behaviors, list_inference_profile_records, load_inference_profile, Collection,
    InferenceBackend, InferenceProfile, ReasoningEffort,
};
use gents_codex_protocol as codex;
use gents_protocol::row::{
    project_behavior_readiness_summary, BehaviorReadinessUnavailableReason,
    ProjectedBehaviorReadinessSummary,
};
use serde_json::{json, Value};

use super::super::bound_behavior::model_selection_id;
use super::super::protocol::{
    absolute_path, backend_model_summary, send_error, send_typed_json_result,
};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};
use crate::config_writes::ConfigAccess;

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

/// The behavior the Codex shim binds to plus the canonical inference profile it
/// selects. `AgentBehavior` carries `inference_profile_id` only — backend,
/// model, and effort choices live on the profile, never as behavior copies.
pub(super) struct BoundBehavior {
    pub(super) behavior: gents::AgentBehaviorDocument,
    pub(super) inference_profile: InferenceProfile,
}

/// One backend enabled for this principal plus its discovered model catalog in
/// the exact credential scope this principal may use. `catalog` is absent until
/// a successful discovery has been observed; absence is never invented into
/// synthetic model entries.
pub(super) struct AvailableBackend {
    pub(super) backend: InferenceBackend,
    pub(super) catalog: Option<BackendModelCatalog>,
}

async fn load_bound_behavior_document(state: &ShimState) -> Result<gents::AgentBehaviorDocument> {
    let agent_did = state.agent_did.as_ref();
    let behavior_id = state.behavior_id.as_ref();
    let raw =
        ConfigAccess::transact_local(state.node.as_ref(), None, "codex.bound_behavior", |txn| {
            Box::pin(async move {
                read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentBehavior,
                    agent_did,
                    behavior_id,
                )
                .await?
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    anyhow::anyhow!("bound AgentBehavior {behavior_id:?} missing for {agent_did:?}")
                })
            })
        })
        .await?;
    // Full canonical decode: `deny_unknown_fields` makes malformed
    // configuration a hard error instead of a silently half-filled document.
    let behavior: gents::AgentBehaviorDocument = serde_json::from_value(raw)
        .with_context(|| format!("decoding bound AgentBehavior {behavior_id:?}"))?;
    Ok(behavior)
}

pub(super) async fn load_bound_behavior(state: &ShimState) -> Result<BoundBehavior> {
    let behavior = load_bound_behavior_document(state).await?;
    let agent_did = state.agent_did.as_ref();
    let inference_profile = load_inference_profile(
        state.node.as_ref(),
        agent_did,
        &behavior.inference_profile_id,
    )
    .await?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "bound inference profile {:?} missing for {agent_did:?}",
            behavior.inference_profile_id
        )
    })?;
    inference_profile
        .validate()
        .context("bound inference profile is invalid")?;
    Ok(BoundBehavior {
        behavior,
        inference_profile,
    })
}

pub(super) async fn available_model_backends(state: &ShimState) -> Result<Vec<AvailableBackend>> {
    // The model list is a *configuration* surface — pick a backend/profile to
    // bind a behavior to — not an admission decision, so it starts from the
    // document's configured intent (`enabled` backends, as before #1332)
    // and must keep working before this runtime has published any
    // readiness at all (fresh stores, config-only sessions, a backend that
    // has no behavior bound to it yet). It only drops a backend on an
    // *explicit* readiness veto; `fleet_slots.rs`/`healthz` are the
    // admission-reporting surfaces and keep their fail-closed behavior.
    // Backends are scoped to the exact bound principal; another principal's
    // backend is never selectable here.
    let agent_did = state.agent_did.as_ref();
    let mut backends = list_enabled_backends_for_agent(state.node.as_ref(), agent_did)
        .await
        .context("listing enabled inference backends")?;

    let behaviors = list_agent_behaviors(state.node.as_ref(), agent_did)
        .await
        .context("listing agent behaviors for model selection")?;
    let profiles = list_inference_profile_records(state.node.as_ref(), agent_did)
        .await
        .context("listing inference profiles for model selection")?;
    let profile_backend: std::collections::BTreeMap<&str, &str> = profiles
        .iter()
        .map(|(_, profile)| (profile.profile_id.as_str(), profile.backend_id.as_str()))
        .collect();
    // A behavior binds to a backend only through its selected inference
    // profile; behaviors whose profile is unresolvable attribute to no backend.
    let bindings = behaviors
        .iter()
        .filter_map(|behavior| {
            profile_backend
                .get(behavior.inference_profile_id.as_str())
                .map(|backend_id| (behavior.behavior_id.clone(), (*backend_id).to_string()))
        })
        .collect::<Vec<_>>();

    let readiness_row = crate::commands::status::load_behavior_readiness(
        &ConfigAccess::Local(state.node.clone()),
        agent_did,
    )
    .await
    .context("loading behavior readiness for model selection")?;
    let observed_at = chrono::Utc::now();
    backends.retain(|backend| {
        !backend_vetoed_by_readiness(
            &backend.backend_id,
            &bindings,
            readiness_row.as_ref(),
            agent_did,
            observed_at,
        )
    });
    backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));

    let mut available = Vec::with_capacity(backends.len());
    for backend in backends {
        // Exact authentication scope: shared credentials observe the backend's
        // shared (anonymous) catalog; principal OAuth observes only this
        // principal's catalog. One principal never inherits another's
        // advertised list.
        let credential_scope =
            matches!(backend.auth, BackendAuth::PrincipalOAuth).then_some(agent_did);
        let catalog =
            lookup_backend_observation(state.node.as_ref(), agent_did, &backend.backend_id)
                .await
                .with_context(|| {
                    format!(
                        "loading catalog observation for backend {:?}",
                        backend.backend_id
                    )
                })?
                .map(|observation| {
                    observation
                        .catalog_for(credential_scope)
                        .map(|catalog| catalog.cloned())
                })
                .transpose()?
                .flatten();
        available.push(AvailableBackend { backend, catalog });
    }
    Ok(available)
}

/// Whether the readiness projection *explicitly* vetoes `backend_id` — never
/// a "we don't know" signal. True only when a readiness row exists for this
/// agent and every behavior currently bound to `backend_id` through its
/// inference profile is reported `Unavailable` with a backend-related reason
/// (`BackendDisabled` or `BackendTemporarilyUnavailable`). No readiness row,
/// no behaviors bound yet, or at least one bound behavior that's `Ready` or
/// unavailable for an unrelated reason — all read as "not vetoed."
fn backend_vetoed_by_readiness(
    backend_id: &str,
    bindings: &[(String, String)],
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

    let bound_behavior_ids = bindings
        .iter()
        .filter(|(_, binding_backend_id)| binding_backend_id == backend_id)
        .map(|(behavior_id, _)| behavior_id.as_str())
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
    backends: &[AvailableBackend],
    bound: &BoundBehavior,
) -> Vec<Value> {
    let mut entries = backends
        .iter()
        .filter_map(|available| {
            available
                .catalog
                .as_ref()
                .map(|catalog| (available, catalog))
        })
        .flat_map(|(available, catalog)| {
            catalog
                .models
                .iter()
                .filter(|model| !model.model_name.trim().is_empty())
                .map(move |model| (available, model))
        })
        .map(|(available, model)| {
            let backend = &available.backend;
            let model_name = model.model_name.as_str();
            let selection_id = model_selection_id(&backend.backend_id, model_name);
            let is_default = bound.inference_profile.backend_id == backend.backend_id
                && bound.inference_profile.model_name == model_name;
            let mut entry = backend_model_summary(backend, model_name, &selection_id, is_default);
            entry["supportedReasoningEfforts"] =
                advertised_effort_options(model.reasoning_efforts.as_deref());
            entry
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

/// Advertise exactly the efforts discovery observed for one model — never
/// synthetic effort-model variants. `None` means the efforts are unknown and
/// `Some(empty)` means the model has no configurable reasoning effort; both
/// render as no selectable effort options. Efforts outside the Codex protocol
/// vocabulary (`max`, `ultra`) cannot be expressed in a `ModelListResponse`
/// and are omitted rather than invented.
fn advertised_effort_options(efforts: Option<&[ReasoningEffort]>) -> Value {
    let Some(efforts) = efforts else {
        return json!([]);
    };
    let options = efforts
        .iter()
        .filter_map(|effort| {
            let name = effort.as_str();
            matches!(
                name,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
            .then(|| {
                json!({
                    "reasoningEffort": name,
                    "description": format!("{name} reasoning effort"),
                })
            })
        })
        .collect::<Vec<_>>();
    json!(options)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelSelection {
    backend_id: String,
    model_name: String,
}

async fn resolve_model_selection(
    state: &ShimState,
    requested_model: &str,
) -> Result<ModelSelection> {
    let bound = load_bound_behavior(state).await?;
    select_advertised_model(
        &available_model_backends(state).await?,
        &bound.inference_profile.backend_id,
        requested_model,
    )
}

fn select_advertised_model(
    backends: &[AvailableBackend],
    current_backend: &str,
    requested_model: &str,
) -> Result<ModelSelection> {
    anyhow::ensure!(
        !requested_model.trim().is_empty(),
        "model selection must be nonempty"
    );
    let explicit =
        requested_model.contains(super::super::bound_behavior::MODEL_SELECTION_SEPARATOR);
    let mut matches = backends
        .iter()
        .flat_map(|available| {
            available
                .catalog
                .iter()
                .flat_map(move |catalog| catalog.models.iter().map(move |model| (available, model)))
        })
        .filter(|(available, model)| {
            if explicit {
                model_selection_id(&available.backend.backend_id, &model.model_name)
                    == requested_model
            } else {
                model.model_name == requested_model
            }
        })
        .collect::<Vec<_>>();
    if !explicit
        && matches
            .iter()
            .any(|(backend, _)| backend.backend.backend_id == current_backend)
    {
        matches.retain(|(backend, _)| backend.backend.backend_id == current_backend);
    }
    anyhow::ensure!(
        !matches.is_empty(),
        "no backend advertises model {requested_model:?} in this principal's credential scope"
    );
    anyhow::ensure!(
        matches.len() == 1,
        "ambiguous advertised model {requested_model:?}; specify backend::model"
    );
    let (backend, model) = matches[0];
    Ok(ModelSelection {
        backend_id: backend.backend.backend_id.clone(),
        model_name: model.model_name.clone(),
    })
}

async fn apply_model_to_bound_behavior(
    state: &ShimState,
    selection: &ModelSelection,
) -> Result<()> {
    let access = ConfigAccess::Graphql(state.graphql.as_ref().to_string());
    apply_model_to_bound_behavior_with_access(state, selection, &access).await
}

async fn apply_model_to_bound_behavior_with_access(
    state: &ShimState,
    selection: &ModelSelection,
    access: &ConfigAccess,
) -> Result<()> {
    apply_model_selection(
        access,
        state.agent_did.as_ref(),
        state.behavior_id.as_ref(),
        selection,
    )
    .await
}

async fn apply_model_selection(
    access: &ConfigAccess,
    owner: &str,
    behavior_id: &str,
    selection: &ModelSelection,
) -> Result<()> {
    let new_profile_id = uuid::Uuid::new_v4().to_string();
    access
        .transact("codex.model_selection", |txn| {
            let new_profile_id = &new_profile_id;
            Box::pin(async move {
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentBehavior,
                    owner,
                    behavior_id,
                )
                .await?
                .context("bound behavior does not exist for this principal")?;
                let mut behavior: gents::AgentBehaviorDocument = serde_json::from_value(value)?;
                anyhow::ensure!(behavior.enabled, "bound behavior is disabled");
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    owner,
                    &behavior.inference_profile_id,
                )
                .await?
                .context("bound inference profile does not exist for this principal")?;
                let mut profile: InferenceProfile = serde_json::from_value(value)?;
                let backend = load_inference_backend_in_txn(txn, owner, &selection.backend_id)
                    .await?
                    .context("selected backend does not exist for this principal")?;
                backend.validate()?;
                anyhow::ensure!(backend.enabled, "selected backend is disabled");
                let observation =
                    lookup_backend_observation_in_txn(txn, owner, &selection.backend_id)
                        .await?
                        .context("selected backend has no discovery observation")?;
                let scope = matches!(backend.auth, BackendAuth::PrincipalOAuth).then_some(owner);
                let catalog = observation
                    .catalog_for(scope)?
                    .context("selected backend has no catalog in this credential scope")?;
                let mut models = catalog
                    .models
                    .iter()
                    .filter(|model| model.model_name == selection.model_name);
                let model = models
                    .next()
                    .context("selected backend does not advertise requested model")?;
                anyhow::ensure!(models.next().is_none(), "ambiguous advertised model");
                if let (Some(effort), Some(supported)) =
                    (profile.reasoning_effort, model.reasoning_efforts.as_ref())
                {
                    anyhow::ensure!(
                        supported.contains(&effort),
                        "selected model does not advertise configured reasoning effort {effort:?}"
                    );
                }
                if profile.backend_id == selection.backend_id
                    && profile.model_name == selection.model_name
                {
                    return Ok(());
                }
                // Copy the current controls; changing this behavior must not mutate a shared profile.
                profile.profile_id = new_profile_id.clone();
                profile.backend_id = selection.backend_id.clone();
                profile.model_name = selection.model_name.clone();
                behavior.inference_profile_id = profile.profile_id.clone();
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::InferenceProfile, serde_json::to_value(profile)?),
                        (Collection::AgentBehavior, serde_json::to_value(behavior)?),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await?;
                Ok(())
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_writes::{
        write_agent_behavior_document, write_inference_backend_document,
        write_inference_profile_document,
    };
    use gents::defra_node::EmbeddedNode;
    use gents::document_config::AdvertisedModel;
    use gents::{record_model_catalog_in_txn, BackendProviderKind};
    use gents_protocol::row::{
        AgentBehaviorReadinessRow, BehaviorReadinessEntry, BehaviorReadinessProcessState,
        BehaviorReadinessSnapshot, BehaviorReadinessState, BehaviorReadinessUnavailableReason,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };
    use std::sync::Arc;

    fn behavior(behavior_id: &str, profile_id: &str) -> gents::AgentBehaviorDocument {
        gents::AgentBehaviorDocument {
            behavior_id: behavior_id.to_string(),
            agent_did: "did:test:codex-shim".to_string(),
            display_name: None,
            description: None,
            context_id: None,
            inference_profile_id: profile_id.to_string(),
            enabled: true,
            tags: Vec::new(),
            created_at: None,
        }
    }

    fn backend(backend_id: &str) -> InferenceBackend {
        InferenceBackend {
            agent_did: "did:test:codex-shim".to_string(),
            backend_id: backend_id.to_string(),
            name: "Backend A".to_string(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: None,
            endpoint: "http://127.0.0.1:11434/v1".to_string(),
            auth: BackendAuth::Unauthenticated,
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: true,
            tags: Vec::new(),
        }
    }

    fn profile(profile_id: &str, model_name: &str) -> InferenceProfile {
        InferenceProfile {
            agent_did: "did:test:codex-shim".to_string(),
            profile_id: profile_id.to_string(),
            display_name: None,
            description: None,
            backend_id: "backend-a".to_string(),
            model_name: model_name.to_string(),
            reasoning_effort: None,
            context_window: None,
            max_output_tokens: None,
            sampling_id: None,
            execution_id: None,
            tags: Vec::new(),
        }
    }

    fn advertised(model_name: &str) -> AdvertisedModel {
        AdvertisedModel {
            model_name: model_name.to_string(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            reasoning_efforts: None,
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
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", false)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(
            backend_vetoed_by_readiness("backend-a", &bindings, Some(&row), agent_did, observed_at),
            "a backend the local prober vetoed via readiness must not be offered for \
             selection even though the InferenceBackend document itself would read healthy"
        );
    }

    #[test]
    fn backend_not_vetoed_when_bound_behavior_is_ready() {
        let agent_did = "did:test:codex-shim";
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(!backend_vetoed_by_readiness(
            "backend-a",
            &bindings,
            Some(&row),
            agent_did,
            observed_at
        ));
    }

    #[test]
    fn missing_readiness_row_leaves_configured_backend_offered() {
        // The model list is a configuration surface (choose a backend/profile
        // to configure a behavior with) and must work before this runtime has
        // published any readiness at all — a fresh store, a config-only
        // session. Absence of a readiness row is not a veto.
        let agent_did = "did:test:codex-shim";
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(!backend_vetoed_by_readiness(
            "backend-a",
            &bindings,
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
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let row = readiness_row(
            agent_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );
        let observed_at = "2026-09-03T12:00:00Z".parse().unwrap();

        assert!(!backend_vetoed_by_readiness(
            "backend-b",
            &bindings,
            Some(&row),
            agent_did,
            observed_at
        ));
    }

    /// Discovery-backed list: exactly the advertised models, with the efforts
    /// the server advertises — no synthetic effort-model variants, no invented
    /// defaults for unknown effort sets, and efforts outside the Codex
    /// protocol vocabulary are omitted rather than invented.
    #[test]
    fn model_list_entries_advertise_discovered_models_and_efforts() {
        let agent_did = "did:test:codex-shim";
        let catalog = BackendModelCatalog {
            agent_did: None,
            observed_at: "2026-09-03T12:00:00Z".to_string(),
            models: vec![
                AdvertisedModel {
                    model_name: "model-y".to_string(),
                    display_name: None,
                    context_window: None,
                    max_output_tokens: None,
                    reasoning_efforts: None,
                },
                AdvertisedModel {
                    model_name: "model-x".to_string(),
                    display_name: None,
                    context_window: Some(32_000),
                    max_output_tokens: None,
                    reasoning_efforts: Some(vec![ReasoningEffort::High, ReasoningEffort::Max]),
                },
            ],
        };
        let available = AvailableBackend {
            backend: backend("backend-a"),
            catalog: Some(catalog),
        };
        let bound = BoundBehavior {
            behavior: behavior("default", "profile-x"),
            inference_profile: profile("profile-x", "model-x"),
        };

        let entries = model_list_entries(&[available], &bound);
        assert_eq!(
            entries.len(),
            2,
            "each advertised model is listed once; efforts never become synthetic model variants"
        );
        let x = entries
            .iter()
            .find(|entry| entry["id"] == json!("backend-a::model-x"))
            .expect("advertised model-x listed");
        assert_eq!(
            x["isDefault"],
            json!(true),
            "the bound profile's model is default"
        );
        let efforts = x["supportedReasoningEfforts"].as_array().unwrap();
        assert_eq!(
            efforts.len(),
            1,
            "efforts outside the Codex protocol vocabulary are omitted, not invented: {efforts:?}"
        );
        assert_eq!(efforts[0]["reasoningEffort"], json!("high"));
        let y = entries
            .iter()
            .find(|entry| entry["id"] == json!("backend-a::model-y"))
            .expect("advertised model-y listed");
        assert_eq!(y["isDefault"], json!(false));
        assert_eq!(
            y["supportedReasoningEfforts"],
            json!([]),
            "unknown effort sets advertise no selectable efforts"
        );
    }

    async fn seed_backend_with_catalog(
        access: &ConfigAccess,
        node: &EmbeddedNode,
        models: Vec<AdvertisedModel>,
    ) -> InferenceBackend {
        let backend = backend("backend-a");
        write_inference_backend_document(access, &backend)
            .await
            .expect("seed backend");
        // Shared (unauthenticated) credential scope: the catalog is anonymous.
        ConfigAccess::transact_local(node, None, "codex.models.test.catalog", |txn| {
            let backend = &backend;
            let models = models.clone();
            Box::pin(async move {
                record_model_catalog_in_txn(
                    txn,
                    backend,
                    BackendModelCatalog {
                        agent_did: None,
                        observed_at: chrono::Utc::now().to_rfc3339(),
                        models,
                    },
                )
                .await
            })
        })
        .await
        .expect("seed catalog");
        backend
    }

    /// Single owner (#1331 successor): `apply_model_to_bound_behavior` must
    /// reject a profile whose backend does not advertise the profile's model
    /// through the scoped catalog — not just trust
    /// `resolve_model_selection`'s own match.
    #[tokio::test]
    async fn apply_model_to_bound_behavior_rejects_an_unadvertised_model() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let agent_did = "did:test:codex-shim";
        let access = ConfigAccess::Local(node.clone());
        seed_backend_with_catalog(&access, node.as_ref(), vec![advertised("model-x")]).await;

        let mut unadvertised = profile("profile-unadvertised", "not-advertised");
        unadvertised.backend_id = "backend-a".to_string();
        write_inference_profile_document(&access, &unadvertised)
            .await
            .expect("seed unadvertised profile");
        write_inference_profile_document(&access, &profile("profile-x", "model-x"))
            .await
            .expect("seed advertised profile");
        write_agent_behavior_document(&access, &behavior("default", "profile-x"))
            .await
            .expect("seed behavior");

        let selection = ModelSelection {
            backend_id: "backend-a".into(),
            model_name: "not-advertised".into(),
        };

        let error = apply_model_selection(&access, agent_did, "default", &selection)
            .await
            .expect_err("an unadvertised model must be rejected");
        // `{:#}` (anyhow's alternate Display) walks the full context chain;
        // `{}` would only show this call's own wrapping context.
        assert!(
            format!("{error:#}").contains("does not advertise"),
            "{error:#}"
        );
    }

    /// The mirror-image happy path: a genuinely-advertised profile commits,
    /// selecting `inference_profile_id` only — no backend/model copies on the
    /// behavior.
    #[tokio::test]
    async fn apply_model_to_bound_behavior_accepts_an_advertised_model() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let agent_did = "did:test:codex-shim";
        let access = ConfigAccess::Local(node.clone());
        seed_backend_with_catalog(
            &access,
            node.as_ref(),
            vec![advertised("model-x"), advertised("model-y")],
        )
        .await;

        write_inference_profile_document(&access, &profile("profile-x", "model-x"))
            .await
            .expect("seed advertised profile-x");
        write_inference_profile_document(&access, &profile("profile-y", "model-y"))
            .await
            .expect("seed advertised profile-y");
        write_agent_behavior_document(&access, &behavior("default", "profile-x"))
            .await
            .expect("seed behavior");

        let selection = ModelSelection {
            backend_id: "backend-a".into(),
            model_name: "model-y".into(),
        };

        apply_model_selection(&access, agent_did, "default", &selection)
            .await
            .expect("an advertised profile must pass validation and commit");
        let updated = gents::load_agent_behavior(node.as_ref(), "default")
            .await
            .expect("reload behavior")
            .expect("behavior remains present");
        assert_ne!(updated.inference_profile_id, "profile-y");
        assert_ne!(updated.inference_profile_id, "profile-x");
        let selected =
            gents::load_inference_profile(node.as_ref(), agent_did, &updated.inference_profile_id)
                .await
                .expect("reload profile")
                .expect("selected profile remains present");
        assert_eq!(selected.backend_id, "backend-a");
        assert_eq!(selected.model_name, "model-y");
        // The selection id the model list and ConfigRead agree on still
        // resolves from the profile's canonical fields.
        assert_eq!(
            model_selection_id(&selected.backend_id, &selected.model_name),
            "backend-a::model-y"
        );
    }
    #[test]
    fn selection_requires_exact_unambiguous_advertisement() {
        let make = |id: &str, names: &[&str]| AvailableBackend {
            backend: backend(id),
            catalog: Some(BackendModelCatalog {
                agent_did: None,
                observed_at: "2026-09-03T12:00:00Z".into(),
                models: names.iter().map(|name| advertised(name)).collect(),
            }),
        };
        let backends = vec![make("a", &["model"]), make("b", &["model"])];
        assert!(select_advertised_model(&backends, "other", "model").is_err());
        assert_eq!(
            select_advertised_model(&backends, "a", "model")
                .unwrap()
                .backend_id,
            "a"
        );
        assert_eq!(
            select_advertised_model(&backends, "a", "b::model")
                .unwrap()
                .backend_id,
            "b"
        );
        assert!(select_advertised_model(&backends, "a", " b::model").is_err());
        assert!(select_advertised_model(&backends, "a", "missing").is_err());
        assert!(select_advertised_model(
            &[AvailableBackend {
                backend: backend("a"),
                catalog: None
            }],
            "a",
            "a::model"
        )
        .is_err());
        let collision = vec![make("a", &["b::c"]), make("a::b", &["c"])];
        assert!(select_advertised_model(&collision, "a", "a::b::c").is_err());
    }

    #[tokio::test]
    async fn model_change_copies_controls_and_preserves_shared_profiles_and_noop() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        let owner = "did:test:codex-shim";
        seed_backend_with_catalog(&access, &node, vec![advertised("x"), advertised("y")]).await;
        let plan = DesiredStateApplyPlan::new(
            [
                (
                    Collection::InferenceSampling,
                    json!({"agent_did":owner,"sampling_id":"sample","temperature":0.4}),
                ),
                (
                    Collection::InferenceExecution,
                    json!({"agent_did":owner,"execution_id":"exec","max_turns":1000}),
                ),
            ]
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
        )
        .unwrap();
        access
            .transact("test.controls", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
        let mut original = profile("shared", "x");
        original.sampling_id = Some("sample".into());
        original.execution_id = Some("exec".into());
        original.context_window = Some(32000);
        original.max_output_tokens = Some(8192);
        original.reasoning_effort = Some(ReasoningEffort::High);
        original.tags = vec!["keep".into()];
        write_inference_profile_document(&access, &original)
            .await
            .unwrap();
        // A matching profile deliberately has different controls; /model must not select it.
        write_inference_profile_document(&access, &profile("other-policy", "y"))
            .await
            .unwrap();
        for name in ["selected", "neighbor"] {
            write_agent_behavior_document(&access, &behavior(name, "shared"))
                .await
                .unwrap();
        }
        let selection = ModelSelection {
            backend_id: "backend-a".into(),
            model_name: "y".into(),
        };
        apply_model_selection(&access, owner, "selected", &selection)
            .await
            .unwrap();
        let selected = gents::load_agent_behavior(&node, "selected")
            .await
            .unwrap()
            .unwrap();
        assert_ne!(selected.inference_profile_id, "shared");
        assert_ne!(selected.inference_profile_id, "other-policy");
        let new_profile =
            gents::load_inference_profile(&node, owner, &selected.inference_profile_id)
                .await
                .unwrap()
                .unwrap();
        let mut expected = original.clone();
        expected.profile_id = selected.inference_profile_id.clone();
        expected.model_name = "y".into();
        assert_eq!(new_profile, expected);
        assert_eq!(
            gents::load_inference_profile(&node, owner, "shared")
                .await
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(
            gents::load_agent_behavior(&node, "neighbor")
                .await
                .unwrap()
                .unwrap()
                .inference_profile_id,
            "shared"
        );
        let before = gents::list_inference_profile_records(&node, owner)
            .await
            .unwrap()
            .len();
        apply_model_selection(&access, owner, "selected", &selection)
            .await
            .unwrap();
        assert_eq!(
            gents::list_inference_profile_records(&node, owner)
                .await
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            gents::load_agent_behavior(&node, "selected")
                .await
                .unwrap()
                .unwrap(),
            selected
        );
        // A retained dangling reference must fail the shared publication transaction
        // without leaving an orphan copied profile or changing the behavior binding.
        let escaped = gents::graphql::escape_graphql_string(&selected.inference_profile_id);
        let response = node.execute(&format!(r#"mutation {{ update_InferenceProfile(filter: {{agent_did: {{_eq: "did:test:codex-shim"}}, profile_id: {{_eq: "{escaped}"}}}}, input: {{sampling_id:"missing"}}) {{_docID}} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert!(apply_model_selection(
            &access,
            owner,
            "selected",
            &ModelSelection {
                backend_id: "backend-a".into(),
                model_name: "x".into()
            }
        )
        .await
        .is_err());
        assert_eq!(
            gents::list_inference_profile_records(&node, owner)
                .await
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            gents::load_agent_behavior(&node, "selected")
                .await
                .unwrap()
                .unwrap(),
            selected
        );
        // Restore the actual retained profile through the common owner for the next case.
        write_inference_profile_document(&access, &new_profile)
            .await
            .unwrap();
        let mut unsupported = advertised("y");
        unsupported.reasoning_efforts = Some(vec![ReasoningEffort::Low]);
        seed_backend_with_catalog(&access, &node, vec![unsupported]).await;
        assert!(
            apply_model_selection(&access, owner, "selected", &selection)
                .await
                .is_err()
        );
        assert_eq!(
            gents::list_inference_profile_records(&node, owner)
                .await
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            gents::load_agent_behavior(&node, "selected")
                .await
                .unwrap()
                .unwrap(),
            selected
        );
    }
    #[tokio::test]
    async fn model_list_scope_excludes_foreign_malformed_backend_before_decode() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        write_inference_backend_document(&access, &backend("same"))
            .await
            .unwrap();
        let response = node.execute(r#"mutation { create_InferenceBackend(input: {agent_did:"did:test:foreign",backend_id:"same",name:"Foreign",provider_kind:"OpenAiCompatible",endpoint:"http://127.0.0.1:1",enabled:true,auth:{kind:"invalid"}}) {_docID} }"#).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let own = list_enabled_backends_for_agent(&node, "did:test:codex-shim")
            .await
            .unwrap();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].agent_did, "did:test:codex-shim");
        assert!(list_enabled_backends_for_agent(&node, "did:test:foreign")
            .await
            .is_err());
    }
}
