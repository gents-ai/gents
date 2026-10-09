//! Generated field tables and patch results checked against the existing patch owner.
//! Every Tools row replays the Lean always-on operator-grant verdict through
//! `guard_tools_keep_grants`, and every Context and Agent row replays it
//! through `reselection_keeps_grants` on the Tools documents Lean resolves;
//! guarded rows also replay the Lean guard verdict through the production guard
//! of their target (Tools and Agent no-lockout, Backend auth, and the Profile
//! account choice). Reference validation and
//! transactional rejection need an end-to-end ConfigApplyTxn consumer; this
//! test does not simulate them.
//! Agent management and sibling selection replay their Lean owners.
use crate::lean_vocab_test::{
    lean_agent_decision_cases, lean_agent_materialization_cases, lean_self_config_cases,
    lean_self_config_field_tables, lean_self_config_selection_cases, lean_sibling_tools_cases,
    LeanAgentCandidate, LeanAgentDecisionInputs, LeanSelfConfigPatchEntry, LeanSelfConfigCase,
};
use gents::config_client::patch::{
    apply_patch, ensure_admissible, SelfConfigPatch, SelfConfigTarget, ALL_SELF_CONFIG_TARGETS,
};
use gents::self_config::{
    guard_backend_auth, guard_backend_choice, guard_agent_keeps_reach, guard_tools_keep_control,
    guard_tools_keep_grants, reselection_keeps_grants, OperatorGrants,
};
use gents::self_config::agent::{
    admit_sibling_tools_target, decide_agent_operation, materialize_agent, AgentCandidate,
    AgentCatalogView, AgentCreateRequest, AgentOperation, SiblingToolsTarget,
};
use gents::self_config::{apply_tool_grant_selection, validate_tool_network_selection};
use gents::toolset::CommandNetworkMode;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

fn doc_map(entries: &[crate::lean_vocab_test::LeanSelfConfigFieldValue]) -> Map<String, Value> {
    entries
        .iter()
        .map(|entry| (entry.field.clone(), Value::String(entry.value.clone())))
        .collect()
}

fn entries_patch(entries: &[LeanSelfConfigPatchEntry]) -> SelfConfigPatch {
    entries
        .iter()
        .map(|entry| {
            let value = match entry.action.as_str() {
                "set" => Some(Value::String(
                    entry
                        .value
                        .clone()
                        .expect("set patch entry must carry a value"),
                )),
                "clear" => None,
                other => panic!("unknown patch action {other}"),
            };
            (entry.field.clone(), value)
        })
        .collect()
}

pub(super) fn self_config_field_tables_match_lean_contract() {
    let tables = lean_self_config_field_tables();
    let actual: BTreeSet<_> = ALL_SELF_CONFIG_TARGETS
        .iter()
        .map(|t| t.collection_name())
        .collect();
    let expected: BTreeSet<_> = tables.iter().map(|t| t.collection.as_str()).collect();
    assert_eq!(actual, expected, "runtime self-config target inventory");
    for table in tables {
        let target =
            SelfConfigTarget::from_collection_name(&table.collection).expect("runtime target");
        assert_eq!(
            table.unique_field,
            target.unique_field(),
            "{}",
            table.collection
        );
        assert_eq!(table.category, target.category(), "{}", table.collection);
        assert_eq!(
            table.all_fields,
            target.all_fields(),
            "{}",
            table.collection
        );
        assert_eq!(
            table.writable_fields,
            target.writable_fields(),
            "{}",
            table.collection
        );
        assert_eq!(
            table.protected_fields,
            target.protected_fields(),
            "{}",
            table.collection
        );
    }
}

pub(super) fn generated_self_config_cases_fence_patch_merge() {
    let cases = lean_self_config_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let target =
            SelfConfigTarget::from_collection_name(&case.collection).expect("runtime target");
        let stored = doc_map(&case.doc);
        let patch = entries_patch(&case.patch);
        assert_eq!(
            ensure_admissible(target, &patch).is_ok(),
            case.admissible,
            "{}: runtime patch admissibility",
            case.name
        );
        // Accepted rows expose the merge result. Rejected rows expose stored
        // state, so comparing a locally selected fallback would prove nothing
        // about the real transaction or reference/no-lockout validator.
        if case.accepted {
            assert_eq!(
                apply_patch(target, &stored, &patch),
                doc_map(&case.result),
                "{}: runtime patch merge",
                case.name
            );
        }
        let grant_guarded = matches!(
            target,
            SelfConfigTarget::Tools
                | SelfConfigTarget::AgentContext
                | SelfConfigTarget::Agent
        );
        if (case.guarded || grant_guarded) && case.admissible && case.validates {
            let held = OperatorGrants {
                pack_install: case.held_grants.pack_install,
            };
            let typed = || typed_merge(target, case, &patch);
            let grants = match target {
                SelfConfigTarget::Tools => {
                    let (stored, candidate) = typed();
                    guard_tools_keep_grants(&held, Some(&stored), &candidate)
                }
                SelfConfigTarget::AgentContext | SelfConfigTarget::Agent => {
                    let selected =
                        |doc: &Option<Vec<crate::lean_vocab_test::LeanSelfConfigFieldValue>>| {
                            doc.as_deref()
                                .map(|doc| typed_doc(SelfConfigTarget::Tools, doc))
                        };
                    reselection_keeps_grants(
                        &held,
                        selected(&case.selected_before).as_ref(),
                        selected(&case.selected_after).as_ref(),
                    )
                }
                _ => Ok(()),
            };
            let guard = if case.guarded {
                let (stored, candidate) = typed();
                match target {
                    SelfConfigTarget::Tools => guard_tools_keep_control(&stored, &candidate),
                    SelfConfigTarget::Agent => {
                        guard_agent_keeps_reach(&stored, &candidate)
                    }
                    SelfConfigTarget::InferenceBackend => guard_backend_auth(&stored, &candidate),
                    SelfConfigTarget::InferenceProfile => {
                        let backend = |doc: &Map<String, Value>| {
                            let id = doc.get("backend_id")?;
                            case.backends
                                .iter()
                                .find(|backend| {
                                    &parse_nested(backend.backend_id.clone().into()) == id
                                })
                                .map(typed_backend)
                        };
                        match backend(&candidate) {
                            // The Lean rows fix the default account to the original one (`fun _ => none`).
                            Some(next) => {
                                guard_backend_choice(backend(&stored).as_ref(), &next, None)
                            }
                            None => Err(anyhow::anyhow!("{}: unknown next backend", case.name)),
                        }
                    }
                    other => panic!("{}: no runtime guard for {other:?}", case.name),
                }
            } else {
                Ok(())
            };
            assert_eq!(
                grants.and(guard).is_ok(),
                case.accepted,
                "{}: runtime guard",
                case.name
            );
        }
    }
}

fn typed_backend(
    backend: &crate::lean_vocab_test::LeanSelfConfigBackend,
) -> gents::InferenceBackend {
    let id = parse_nested(backend.backend_id.clone().into());
    serde_json::from_value(serde_json::json!({
        "node_did": "did:key:node-a",
        "backend_id": id,
        "name": id,
        "provider_kind": backend.provider_kind,
        "endpoint": "http://127.0.0.1:1/v1",
        "auth": parse_nested(Value::String(backend.auth.clone())),
    }))
    .unwrap_or_else(|error| panic!("{}: {error}", backend.backend_id))
}

/// Lean rows abstract nested Tools groups as their canonical JSON text.
fn parse_nested(value: Value) -> Value {
    let text = value.as_str().expect("Lean group value is text");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

/// The stored document and the merged candidate in their typed shapes; Lean
/// rows carry nested groups as canonical JSON text.
fn typed_merge(
    target: SelfConfigTarget,
    case: &LeanSelfConfigCase,
    patch: &SelfConfigPatch,
) -> (Map<String, Value>, Map<String, Value>) {
    let stored = typed_doc(target, &case.doc);
    let patch: SelfConfigPatch = patch
        .iter()
        .map(|(field, value)| (field.clone(), value.clone().map(parse_nested)))
        .collect();
    let candidate = apply_patch(target, &stored, &patch);
    (stored, candidate)
}

fn typed_doc(
    target: SelfConfigTarget,
    entries: &[crate::lean_vocab_test::LeanSelfConfigFieldValue],
) -> Map<String, Value> {
    let mut doc: Map<String, Value> = entries
        .iter()
        .map(|entry| {
            let value = Value::String(entry.value.clone());
            let value = if entry.field == target.unique_field() || entry.field == "agent_did" {
                value
            } else {
                parse_nested(value)
            };
            (entry.field.clone(), value)
        })
        .collect();
    doc.entry(target.unique_field())
        .or_insert_with(|| Value::String("doc-1".into()));
    doc.insert("node_did".into(), Value::String("did:key:node-a".into()));
    doc
}

fn agent_catalog(inputs: &LeanAgentDecisionInputs) -> AgentCatalogView {
    AgentCatalogView {
        agents: inputs
            .agents
            .iter()
            .map(|agent| (agent.agent_id.clone(), agent.enabled))
            .collect(),
        protected_ids: inputs.protected_ids.iter().cloned().collect(),
        default_id: inputs.default_id.clone(),
        published_profiles: inputs.published_profiles.iter().cloned().collect(),
    }
}

fn agent_operation(inputs: &LeanAgentDecisionInputs) -> AgentOperation {
    match inputs.operation.as_str() {
        "create" => {
            let input = inputs
                .create_input
                .as_ref()
                .unwrap_or_else(|| panic!("{}: create without create_input", inputs.name));
            AgentOperation::Create(AgentCreateRequest {
                display_name: input.display_name.clone(),
                system_prompt: input.system_prompt.clone(),
                inference_profile_id: input.inference_profile_id.clone(),
                clone_from: Some(input.clone_from.clone()).filter(|source| !source.is_empty()),
            })
        }
        "edit" => AgentOperation::Edit(entries_patch(
            inputs
                .edit_patch
                .as_ref()
                .unwrap_or_else(|| panic!("{}: edit without edit_patch", inputs.name)),
        )),
        "disable" => AgentOperation::Disable,
        other => panic!("{}: unknown agent operation {other}", inputs.name),
    }
}

/// Lean `agentOperationAdmitted` through the production agent admission owner.
#[test]
fn generated_agent_decision_cases_drive_production_agent_admission() {
    let cases = lean_agent_decision_cases();
    assert!(!cases.is_empty());
    let mut names = BTreeSet::new();
    for case in cases {
        let inputs = &case.inputs;
        assert!(
            names.insert(inputs.name.as_str()),
            "duplicate {}",
            inputs.name
        );
        assert_eq!(
            decide_agent_operation(
                &agent_catalog(inputs),
                &agent_operation(inputs),
                &inputs.target,
                inputs.make_default,
            )
            .is_ok(),
            case.accepted,
            "{}: production agent admission",
            inputs.name
        );
    }
}

/// Native registry documents decoded from the serialized Lean `CandidateFixture`.
/// Fields the model does not carry (backend endpoint/auth, provider kind) take
/// the canonical unauthenticated local-backend values, which no Lean owner reads.
fn decode_candidate(candidate: &LeanAgentCandidate) -> AgentCandidate {
    let node_did = &candidate.node_did;
    AgentCandidate {
        agents: candidate
            .agents
            .iter()
            .map(|agent| {
                serde_json::from_value(serde_json::json!({
                    "agent_id": agent.agent_id,
                    "node_did": node_did,
                    "context_id": agent.context_id,
                    "inference_profile_id": agent.inference_profile_id,
                    "enabled": agent.enabled,
                }))
                .unwrap_or_else(|error| panic!("{}: Agent: {error}", agent.agent_id))
            })
            .collect(),
        contexts: candidate
            .contexts
            .iter()
            .map(|context| {
                assert!(
                    context.tool_names.is_empty(),
                    "{}: AgentContext has no tool-name field; select Tools by tools_id",
                    context.context_id
                );
                serde_json::from_value(serde_json::json!({
                    "context_id": context.context_id,
                    "node_did": node_did,
                    "system_prompt": context.system_prompt,
                    "skill_ids": context.skill_ids,
                }))
                .unwrap_or_else(|error| panic!("{}: AgentContext: {error}", context.context_id))
            })
            .collect(),
        inference_profiles: candidate
            .inference_profiles
            .iter()
            .map(|profile| {
                serde_json::from_value(serde_json::json!({
                    "profile_id": profile.profile_id,
                    "node_did": node_did,
                    "backend_id": profile.backend_id,
                    "model_name": profile.model_name,
                    "reasoning_effort": profile.reasoning_effort,
                }))
                .unwrap_or_else(|error| panic!("{}: InferenceProfile: {error}", profile.profile_id))
            })
            .collect(),
        inference_backends: candidate
            .inference_backends
            .iter()
            .map(|backend| {
                serde_json::from_value(serde_json::json!({
                    "backend_id": backend.backend_id,
                    "node_did": node_did,
                    "name": backend.backend_id,
                    "provider_kind": "OpenAiCompatible",
                    "endpoint": "http://127.0.0.1:1/v1",
                    "auth": {"kind": "unauthenticated"},
                    "enabled": backend.enabled,
                }))
                .unwrap_or_else(|error| panic!("{}: InferenceBackend: {error}", backend.backend_id))
            })
            .collect(),
    }
}

/// Lean `materializedAgent`: admitted create/edit rows resolve one context and
/// inference session; rejected and disable rows materialize nothing.
#[test]
fn generated_agent_materialization_cases_drive_production_agent_materialization() {
    let cases = lean_agent_materialization_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let inputs = &case.inputs;
        let node_did = case.candidate.node_did.as_str();
        let candidate = decode_candidate(&case.candidate);
        let actual = materialize_agent(
            &agent_catalog(inputs),
            &agent_operation(inputs),
            &inputs.target,
            inputs.make_default,
            &candidate,
            node_did,
        );
        match (&case.session, actual) {
            (None, None) => {}
            (Some(expected), Some(actual)) => {
                assert_eq!(
                    actual.instructions, expected.instructions,
                    "{}",
                    inputs.name
                );
                assert_eq!(actual.skill_ids, expected.skill_ids, "{}", inputs.name);
                assert_eq!(actual.tool_names, expected.tool_names, "{}", inputs.name);
                assert_eq!(actual.backend_id, expected.backend_id, "{}", inputs.name);
                assert_eq!(actual.model, expected.model, "{}", inputs.name);
                assert_eq!(
                    actual
                        .effort
                        .map(|effort| serde_json::to_value(effort).expect("effort serializes")),
                    expected.effort.clone().map(Value::String),
                    "{}",
                    inputs.name
                );
            }
            (expected, actual) => panic!(
                "{}: production materialization disagrees with Lean (expected session {}, got {})",
                inputs.name,
                expected.is_some(),
                actual.is_some()
            ),
        }
    }
}

fn network_mode(name: &str) -> CommandNetworkMode {
    serde_json::from_value(Value::String(name.to_string()))
        .unwrap_or_else(|error| panic!("network mode {name}: {error}"))
}

/// Lean `SiblingToolsOperation.admitted` and `resultNetwork` through the
/// production target fence, network admission fence and canonical Tools writer.
#[test]
fn generated_sibling_tools_cases_drive_production_tools_selection() {
    let cases = lean_sibling_tools_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let requested = case.requested_network.as_deref().map(network_mode);
        let existing = network_mode(&case.existing_network);
        let admitted = admit_sibling_tools_target(&SiblingToolsTarget {
            owner_matches: case.owner_matches,
            protected: case.protected,
            shared_context: case.shared_context,
            shared_tools: case.shared_tools,
        })
        .is_ok()
            && validate_tool_network_selection(requested).is_ok();
        assert_eq!(admitted, case.admitted, "{case:?}: production admission");
        if !admitted {
            continue;
        }

        let mut tools = gents::document_config::Tools::default();
        apply_tool_grant_selection(&mut tools, None, None, Some(existing));
        apply_tool_grant_selection(&mut tools, None, None, requested);
        let selected = tools
            .host
            .as_ref()
            .and_then(|host| host.bash.as_ref())
            .and_then(|bash| bash.network_mode)
            .expect("existing network mode is materialized");
        assert_eq!(selected, network_mode(&case.result_network), "{case:?}");
        assert!(
            selected.meet(existing) == selected,
            "{case:?}: selection widened"
        );
    }
}

/// Lean `selectedToolFlag`: omission preserves the stored selection and an
/// explicit value overrides it, through the canonical Tools adapter.
#[test]
fn generated_self_config_selection_cases_drive_production_tools_selection() {
    let cases = lean_self_config_selection_cases();
    assert!(!cases.tool_flag_cases.is_empty());
    for case in &cases.tool_flag_cases {
        let mut tools = gents::document_config::Tools::default();
        apply_tool_grant_selection(&mut tools, Some(case.existing), Some(case.existing), None);
        apply_tool_grant_selection(&mut tools, case.requested, case.requested, None);
        assert_eq!(
            tools
                .integrations
                .as_ref()
                .and_then(|v| v.lsp.as_ref())
                .is_some(),
            case.selected,
            "{case:?}: lsp"
        );
        assert_eq!(
            tools
                .built_ins
                .as_ref()
                .and_then(|v| v.enable_graph_tools)
                .unwrap_or(false),
            case.selected,
            "{case:?}: graph tools"
        );
        assert!(tools.self_config.is_none());
    }
}

/// Lean `graphToolPresented` through the production self-config tool surface.
#[test]
fn generated_graph_presentation_cases_drive_production_tool_surface() {
    let cases = lean_self_config_selection_cases();
    assert!(!cases.graph_presentation_cases.is_empty());
    for case in &cases.graph_presentation_cases {
        let names = gents::self_config::self_config_tool_names(
            &gents::tool_surface::SelfConfigToolConfig {
                enabled: case.self_config,
                enable_pack_install: case.pack_install,
                enable_graph_tools: case.requested,
                ..Default::default()
            },
        );
        assert_eq!(
            names.iter().any(|v| v == "run_graph"),
            case.presented,
            "{case:?}"
        );
    }
}
