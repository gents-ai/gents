use super::command::{agents::agent_params, contracts::help_patch_contracts};
use super::*;
use gents_protocol::tool_names::{CONFIG_TOOL_NAME, SELF_CONFIG_TOOL_NAMES};
use gents_documents::patch::{
    apply_patch, ensure_admissible, SelfConfigPatch, SelfConfigTarget, ALL_SELF_CONFIG_TARGETS,
};
use gents_lean_vocab::{
    lean_agent_decision_cases, lean_agent_materialization_cases, lean_self_config_cases,
    lean_self_config_field_tables, lean_self_config_selection_cases, lean_sibling_tools_cases,
    LeanAgentCandidate, LeanAgentDecisionInputs, LeanSelfConfigCase, LeanSelfConfigPatchEntry,
};
use gents_protocol::operations::inference::InferenceActor;
use crate::builtin::self_config::agent::{admit_sibling_tools_target, SiblingToolsTarget};
use crate::builtin::self_config::agent::{
    decide_agent_operation, materialize_agent, AgentActor, AgentCandidate, AgentCatalogView,
    AgentCreateRequest, AgentOperation,
};
use crate::builtin::self_config::{
    apply_tool_grant_selection, validate_tool_network_selection,
};
use crate::builtin::self_config::{
    guard_agent_keeps_reach, guard_tools_keep_control, guard_tools_keep_grants,
    reselection_keeps_grants, OperatorGrants,
};
use crate::native::CommandNetworkMode;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

fn doc_map(entries: &[gents_lean_vocab::LeanSelfConfigFieldValue]) -> Map<String, Value> {
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

#[test]
fn self_config_field_tables_match_lean_contract() {
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

#[test]
fn generated_self_config_cases_fence_patch_merge() {
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
            SelfConfigTarget::Tools | SelfConfigTarget::AgentContext | SelfConfigTarget::Agent
        );
        if (case.guarded || grant_guarded) && case.admissible && case.validates {
            let held = OperatorGrants {
                pack_install: case.held_grants.pack_install,
                policy: case.held_grants.policy,
                pack_authority: case.held_grants.pack_authority,
            };
            let typed = || typed_merge(target, case, &patch);
            let grants = match target {
                SelfConfigTarget::Tools => {
                    let (stored, candidate) = typed();
                    guard_tools_keep_grants(&held, Some(&stored), &candidate)
                }
                SelfConfigTarget::AgentContext | SelfConfigTarget::Agent => {
                    let selected =
                        |doc: &Option<Vec<gents_lean_vocab::LeanSelfConfigFieldValue>>| {
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
                    SelfConfigTarget::Agent => guard_agent_keeps_reach(&stored, &candidate),
                    SelfConfigTarget::InferenceBackend => {
                        let auth = |doc: &Map<String, Value>| {
                            serde_json::from_value::<gents_documents::BackendAuth>(
                                doc.get("auth").cloned().unwrap_or(Value::Null),
                            )
                        };
                        match (auth(&stored), auth(&candidate)) {
                            (Ok(stored), Ok(candidate)) => {
                                gents_inference::operations::backend_update_decision(
                                    InferenceActor::Agent,
                                    &stored,
                                    &candidate,
                                )
                                .admit()
                                .map_err(anyhow::Error::from)
                            }
                            _ => Err(anyhow::anyhow!("{}: backend auth is not valid", case.name)),
                        }
                    }
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
                            Some(next) => gents_inference::operations::profile_choice_decision(
                                InferenceActor::Agent,
                                backend(&stored).as_ref(),
                                &next,
                                None,
                            )
                            .admit(&next.backend_id, &next.provider_kind.to_string())
                            .map_err(anyhow::Error::from),
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
    backend: &gents_lean_vocab::LeanSelfConfigBackend,
) -> gents_documents::InferenceBackend {
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

fn parse_nested(value: Value) -> Value {
    let text = value.as_str().expect("Lean group value is text");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

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
    entries: &[gents_lean_vocab::LeanSelfConfigFieldValue],
) -> Map<String, Value> {
    let mut doc: Map<String, Value> = entries
        .iter()
        .map(|entry| {
            let value = Value::String(entry.value.clone());
            let value = if entry.field == target.unique_field() || entry.field == "node_did" {
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
            .map(|agent| (agent.agent_did.clone(), agent.enabled))
            .collect(),
        default_id: inputs.default_id.clone(),
        published_profiles: inputs.published_profiles.iter().cloned().collect(),
    }
}

fn agent_actor(inputs: &LeanAgentDecisionInputs) -> AgentActor {
    AgentActor {
        did: inputs.actor.did.clone(),
        catalog: inputs.actor.catalog,
        configures: inputs.actor.configures.iter().cloned().collect(),
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
        "edit" => AgentOperation::Edit(
            entries_patch(
                inputs
                    .edit_patch
                    .as_ref()
                    .unwrap_or_else(|| panic!("{}: edit without edit_patch", inputs.name)),
            )
            .into_iter()
            .map(|(field, value)| match field.as_str() {
                "enabled" | "tags" => {
                    let value = value.map(parse_nested);
                    (field, value)
                }
                _ => (field, value),
            })
            .collect(),
        ),
        "disable" => AgentOperation::Disable,
        other => panic!("{}: unknown agent operation {other}", inputs.name),
    }
}

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
        let decision = decide_agent_operation(
            &agent_catalog(inputs),
            &agent_actor(inputs),
            &agent_operation(inputs),
            &inputs.target,
            inputs.make_default,
        );
        assert_eq!(
            decision
                .as_ref()
                .err()
                .map(|error| error.refusal().map(|refusal| refusal.as_str())),
            case.refusal.as_deref().map(Some),
            "{}: production agent admission (actor {:?}, front ends {:?}, accepted {}): {decision:?}",
            inputs.name,
            inputs.actor,
            inputs.front_ends,
            case.accepted
        );
        assert_eq!(decision.is_ok(), case.accepted, "{}", inputs.name);
    }
}

fn decode_candidate(candidate: &LeanAgentCandidate) -> AgentCandidate {
    let node_did = &candidate.node_did;
    AgentCandidate {
        agents: candidate
            .agents
            .iter()
            .map(|agent| {
                serde_json::from_value(serde_json::json!({
                    "agent_did": agent.agent_did,
                    "creation_nonce": agent.creation_nonce,
                    "node_did": node_did,
                    "context_id": agent.context_id,
                    "inference_profile_id": agent.inference_profile_id,
                    "enabled": agent.enabled,
                }))
                .unwrap_or_else(|error| panic!("{}: Agent: {error}", agent.agent_did))
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
            &agent_actor(inputs),
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

#[test]
fn generated_sibling_tools_cases_drive_production_tools_selection() {
    let cases = lean_sibling_tools_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let requested = case.requested_network.as_deref().map(network_mode);
        let existing = network_mode(&case.existing_network);
        let admitted = admit_sibling_tools_target(&SiblingToolsTarget {
            owner_matches: case.owner_matches,
            shared_context: case.shared_context,
            shared_tools: case.shared_tools,
        })
        .is_ok()
            && validate_tool_network_selection(requested).is_ok();
        assert_eq!(admitted, case.admitted, "{case:?}: production admission");
        if !admitted {
            continue;
        }

        let mut tools = gents_documents::Tools::default();
        apply_tool_grant_selection(&mut tools, None, Some(existing));
        apply_tool_grant_selection(&mut tools, None, requested);
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

#[test]
fn generated_self_config_selection_cases_drive_production_tools_selection() {
    let cases = lean_self_config_selection_cases();
    assert!(!cases.tool_flag_cases.is_empty());
    for case in &cases.tool_flag_cases {
        let mut tools = gents_documents::Tools::default();
        apply_tool_grant_selection(&mut tools, Some(case.existing), None);
        apply_tool_grant_selection(&mut tools, case.requested, None);
        assert_eq!(
            tools
                .integrations
                .as_ref()
                .and_then(|v| v.lsp.as_ref())
                .is_some(),
            case.selected,
            "{case:?}: lsp"
        );
        assert!(tools.self_config.is_none());
    }
}

/// Check the self-config grant slice for native-authorized model rows. The
/// native authorization outcome is exercised by the signed database test.
#[test]
fn generated_self_config_job_guard_cases_drive_native_guard() {
    let cases = lean_self_config_selection_cases();
    assert!(!cases.job_guard_cases.is_empty());
    let tools = |integrations: Option<&Value>| -> Map<String, Value> {
        let mut tools = Map::new();
        tools.insert(
            "self_config".into(),
            serde_json::json!({"enable_self_config": true}),
        );
        if let Some(integrations) = integrations {
            tools.insert("integrations".into(), integrations.clone());
        }
        tools
    };
    for case in &cases.job_guard_cases {
        if !case.native_authorized || !case.native_source_authorized {
            continue;
        }
        let held = OperatorGrants::default();
        let candidate = tools(Some(&case.candidate_integrations));
        let accepted = match case.writer.as_str() {
            "model" if case.selected_before => {
                let stored = tools(case.stored_integrations.as_ref());
                guard_tools_keep_grants(&held, Some(&stored), &candidate).is_ok()
            }
            "model" => reselection_keeps_grants(&held, None, Some(&candidate)).is_ok(),
            "operator" => {
                assert!(
                    case.accepted,
                    "{}: operator writes bypass the guard",
                    case.name
                );
                continue;
            }
            other => panic!("{}: unknown writer {other:?}", case.name),
        };
        assert_eq!(accepted, case.accepted, "{}: {case:?}", case.name);
    }
}

#[test]
fn every_tool_name_is_reserved_builtin() {
    for name in SELF_CONFIG_TOOL_NAMES {
        assert!(
            gents_documents::is_reserved_builtin_tool_name(name),
            "{name} must be reserved so write_tools declarations cannot shadow it"
        );
    }
}

#[test]
fn help_contracts_conform_to_canonical_types_and_enum_vocabulary() {
    let all_contracts = [
        "agent",
        "tools",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "datastore",
        "skill",
        "page",
        "plugin-source",
        "agent-target",
        "execution",
    ]
    .into_iter()
    .flat_map(|resource| {
        help_patch_contracts(Some(resource))
            .as_array()
            .unwrap()
            .clone()
    })
    .collect::<Vec<_>>();
    for target in gents_documents::patch::ALL_SELF_CONFIG_TARGETS {
        let contract = all_contracts
            .iter()
            .find(|contract| contract["collection"] == target.collection_name())
            .unwrap_or_else(|| panic!("missing help contract for {}", target.collection_name()));
        let described = contract["field_shapes"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let writable = target
            .writable_fields()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            described,
            writable,
            "{} help drift",
            target.collection_name()
        );
    }

    let tools = all_contracts
        .iter()
        .find(|contract| contract["collection"] == "Tools")
        .unwrap();
    let shapes = &tools["field_shapes"];
    let assert_fields = |shape: &Value, canonical: &[&str], name: &str| {
        let described = shape
            .as_object()
            .unwrap_or_else(|| panic!("{name} help shape must be an object"))
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let canonical = canonical.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(described, canonical, "{name} nested help drift");
    };
    use gents_documents::canonical_struct_fields;
    use gents_documents::*;
    assert_fields(
        &shapes["host"],
        canonical_struct_fields::<HostTools>().unwrap(),
        "host",
    );
    assert_fields(
        &shapes["host"]["files"],
        canonical_struct_fields::<FileTools>().unwrap(),
        "files",
    );
    assert_fields(
        &shapes["host"]["bash"],
        canonical_struct_fields::<BashTools>().unwrap(),
        "bash",
    );
    assert_fields(
        &shapes["host"]["cli"][0],
        canonical_struct_fields::<CliTool>().unwrap(),
        "cli[]",
    );
    assert_fields(
        &shapes["remote"],
        canonical_struct_fields::<RemoteTools>().unwrap(),
        "remote",
    );
    assert_fields(
        &shapes["agents"],
        canonical_struct_fields::<AgentTools>().unwrap(),
        "agents",
    );
    let built_in_fields = canonical_struct_fields::<BuiltInTools>()
        .unwrap()
        .iter()
        .copied()
        .filter(|field| !field.ends_with("_graph_tools"))
        .collect::<Vec<_>>();
    assert_fields(&shapes["built_ins"], &built_in_fields, "built_ins");
    assert_fields(
        &shapes["datastore"],
        canonical_struct_fields::<DatastoreTools>().unwrap(),
        "datastore",
    );
    // `eth_tool_ids` is accepted on read from v1.0.0-rc0 documents and
    // dropped; help must not offer it, so it is not part of the canonical set.
    let integration_fields = canonical_struct_fields::<IntegrationTools>()
        .unwrap()
        .iter()
        .copied()
        .filter(|field| *field != "eth_tool_ids")
        .collect::<Vec<_>>();
    assert_fields(&shapes["integrations"], &integration_fields, "integrations");
    assert_fields(
        &shapes["integrations"]["lsp"],
        canonical_struct_fields::<LspTools>().unwrap(),
        "lsp",
    );
    assert_fields(
        &shapes["self_config"],
        canonical_struct_fields::<SelfConfigTools>().unwrap(),
        "self_config",
    );

    let rendered = serde_json::to_string(&all_contracts).unwrap();
    let serialized_values = gents_inference::backends::provider::BackendProviderKind::ALL
        .into_iter()
        .map(|value| value.as_str())
        .chain(
            gents_documents::inference_defaults::ReasoningEffort::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            gents_loop::openai_wire::OpenAiWireApi::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            crate::native::CommandNetworkMode::ALL
                .into_iter()
                .map(|value| value.as_str()),
        );
    for value in serialized_values {
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
    for value in gents_documents::FileToolMode::ALL
        .into_iter()
        .map(|value| serde_json::to_value(value).unwrap())
        .chain(
            gents_documents::BashMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::native::CommandExecutionMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            gents_documents::RemoteToolStyle::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            gents_documents::ConcurrencyMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            gents_inference::compaction::CompactionStrategy::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
    {
        let value = value.as_str().unwrap();
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
}

fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        agent_did: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        preview: false,
        enable_pack_install: false,
        enable_pack_authority: false,
        process_ceiling: Default::default(),
        selected_plugins: Vec::new(),
        ceiling_plugins: None,
    }
}

#[tokio::test]
async fn build_fails_closed_without_node_did() {
    let db = gents_inference::test_support::test_db(
        "self-config-empty-node-did",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let tools = build_self_config_tools(
        std::sync::Arc::default(),
        db.node.clone(),
        String::new(),
        None,
        &config(&["agent"]),
        Arc::new(crate::plugin_host::NoPluginHost),
        Arc::new(crate::task_run::NoTaskRunService),
    );
    assert!(
        tools.is_empty(),
        "an empty node DID must register no self-config tools"
    );
}

#[tokio::test]
async fn build_registers_gated_family() {
    let db = gents_inference::test_support::test_db(
        "self-config-gated-family",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let tools = build_self_config_tools(
        std::sync::Arc::default(),
        db.node.clone(),
        "did:key:zSelfConfigTest".to_string(),
        None,
        &config(&["agent", "backend"]),
        Arc::new(crate::plugin_host::NoPluginHost),
        Arc::new(crate::task_run::NoTaskRunService),
    );
    assert_eq!(
        tools.len(),
        1,
        "self-configuration has one model-facing tool"
    );
    assert_eq!(tools[0].name(), CONFIG_TOOL_NAME);
    let definition = tools[0].definition(String::new()).await;
    assert!(
        definition.description.len() <= 700,
        "config description grew to {} chars",
        definition.description.len()
    );
    for core in [
        "native API",
        "copy the exact agent_did",
        "Preview writes nothing.",
        "[\"validate\"] audits saved configuration",
        "[\"help\"] lists granted resources",
    ] {
        assert!(definition.description.contains(core), "missing {core}");
    }
    assert_eq!(definition.parameters["required"], json!(["argv"]));
}
