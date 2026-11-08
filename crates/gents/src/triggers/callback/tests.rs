use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use crate::ensure_runtime_schemas;
use crate::tools::workspace::{
    action_plan_canonical_json, emit_create_workspace_plan, execute_create_workspace_plan,
    parse_action_plan_json, ActionJournalEntry, ActionJournalState, CreateWorkspaceAction,
    CreationPolicy, HostAction, HostExecutorContext, MemoryWorkspaceDocuments,
    RepositoryPlacementRef, WorkspaceAdapterKind, WorkspaceDocuments, CAP_CLONE_ARTIFACTS,
    CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE,
};

use super::claim::invocation_is_claimable;
use super::documents::{
    create_callback_result_mutation, strip_secret_fields, succeeded_missing_result,
    succeeded_repair_cutoff, update_invocation_mutation, validate_callback_binding,
    CallbackBindingDoc, CallbackInvocationDoc, CallbackModuleDoc, CallbackResultDoc,
    CallbackResultInvocationRow,
};
use super::planner::{
    compute_module_id, fixture_create_workspace_artifact, invoke_plugin_planner, plan_from_module,
    validate_callback_module, CallbackModuleLimits, MAX_ARTIFACT_BYTES,
};
use super::run::{
    apply_planner_deny, can_emit_callback_result, can_start_executing, decode_journal,
    emit_plan_from_source, plan_from_callback, resolve_action_plan,
    resolve_action_plan_with_module,
};
use super::{
    LIFECYCLE_DENIED, LIFECYCLE_FAILED, LIFECYCLE_PENDING, LIFECYCLE_RUNNING, LIFECYCLE_SUCCEEDED,
};

fn binding() -> CallbackBindingDoc {
    serde_json::from_value(json!({"binding_id":"bind-1","node_did":"did:key:zWriter","agent_did":crate::triggers::callback::test_agent_did(),"event_source_id":"events","callback_id":"cb-1","input_fields":["work_unit_id","repository_id","base_sha","branch","owned_files"]})).unwrap()
}
fn callback() -> crate::documents::Callback {
    serde_json::from_value(json!({"callback_id":"cb-1","node_did":"did:key:zWriter","handler":{"kind":"built_in","emitter":"create_workspace", "workspace_agents": {"writer_agent_did":crate::triggers::callback::test_agent_did(), "integrator_agent_did":crate::triggers::callback::test_agent_did()}},"capabilities":["create_workspace","observe_dirty_base","clone_artifacts"]})).unwrap()
}

fn test_workspace_agents() -> crate::documents::WorkspaceAgentSelection {
    crate::documents::WorkspaceAgentSelection {
        writer_agent_did: crate::triggers::callback::test_agent_did(),
        integrator_agent_did: crate::triggers::callback::test_agent_did(),
        readers: Vec::new(),
    }
}

#[test]
fn workspace_agent_selection_is_configured_and_frozen() {
    use crate::store::identity::NodeIdentity;
    let directory = tempfile::tempdir().unwrap();
    let node = crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    let writer =
        crate::KeyIdentity::load_or_create(directory.path().join("writer.key"), None).unwrap();
    let integrator =
        crate::KeyIdentity::load_or_create(directory.path().join("integrator.key"), None).unwrap();
    let source = json!({"work_unit_id":"unit", "repository_id":"repo", "base_sha":"base", "branch":"topic",
        "owned_files":["src/a.rs"], "writer_agent_did":node.did(), "integrator_agent_did":node.did()});
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().workspace_agent_selection_cases;
    for case in cases {
        let selection = match case["name"].as_str().unwrap() {
            "missing" => None,
            "separate" => Some((writer.did(), integrator.did())),
            "same" => Some((writer.did(), writer.did())),
            "node" => Some((node.did(), integrator.did())),
            "blank" => Some(("", integrator.did())),
            name => panic!("unknown modeled selection {name}"),
        };
        let mut configured = callback();
        configured.node_did = node.did().into();
        configured.handler = crate::documents::CallbackHandler::BuiltIn {
            emitter: crate::documents::BuiltInCallback::CreateWorkspace,
            workspace_agents: selection.map(|(writer, integrator)| {
                crate::documents::WorkspaceAgentSelection {
                    writer_agent_did: writer.into(),
                    integrator_agent_did: integrator.into(),
                    readers: vec![crate::documents::WorkspaceReaderSelection {
                        agent_did: integrator.into(),
                    }],
                }
            }),
        };
        let plan = plan_from_callback(&configured, &source, None);
        assert_eq!(plan.is_ok(), case["accepted"].as_bool().unwrap(), "{case}");
        if let Ok(plan) = plan {
            let HostAction::CreateWorkspace(action) = &plan.actions[0] else {
                panic!("workspace plan")
            };
            let (expected_writer, expected_integrator) = selection.unwrap();
            assert_eq!(action.writer_agent_did.as_deref(), Some(expected_writer));
            assert_eq!(
                action.integrator_agent_did.as_deref(),
                Some(expected_integrator)
            );
            assert!(action.writer_agent_doc_id.is_none());
            assert_eq!(action.readers.len(), 1);
            assert_eq!(action.readers[0].agent_did, expected_integrator);
            assert!(action.readers[0].agent_doc_id.is_none());
            let claimed_handler = configured.handler.clone();
            let mut invocation = CallbackInvocationDoc {
                invocation_id: "invocation".into(),
                agent_did: writer.did().into(),
                owner_node_did: node.did().into(),
                callback_id: configured.callback_id.clone(),
                configuration_generation: None,
                claimed_callback: Some(serde_json::to_string(&configured).unwrap()),
                input: source.clone(),
                origin: crate::documents::CallbackInvocationOrigin::Event {
                    binding_id: "binding".into(),
                    source_collection: "Work".into(),
                    source_doc_id: "source".into(),
                    source_version: None,
                },
                caused_by_correlation: None,
                idempotency_key: "id".into(),
                lifecycle_state: LIFECYCLE_RUNNING.into(),
                attempts: Some(1),
                action_plan: Some(action_plan_canonical_json(&plan).unwrap()),
                action_journal: None,
                error: None,
                claimed_at: None,
                created_at: None,
            };
            configured.handler = crate::documents::CallbackHandler::BuiltIn {
                emitter: crate::documents::BuiltInCallback::CreateWorkspace,
                workspace_agents: None,
            };
            let claimed: crate::documents::Callback =
                serde_json::from_str(invocation.claimed_callback.as_deref().unwrap()).unwrap();
            assert_eq!(claimed.handler, claimed_handler);
            assert_eq!(
                resolve_action_plan(&invocation, &configured, &source).unwrap(),
                plan
            );
            invocation.action_plan = None;
            assert!(resolve_action_plan(&invocation, &configured, &source).is_err());
        }
    }
}

#[tokio::test]
async fn workspace_agent_selection_binds_visible_physical_targets() -> anyhow::Result<()> {
    use crate::store::identity::NodeIdentity;
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let creator_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let writer_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let integrator_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let creator = crate::store::identity::resolve_agent_identity(&runtime, &creator_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(runtime.did().to_owned())
            .build()
            .await?,
    );
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection_as(&context, gents_protocol::schemas::AGENT)
        .await?;
    let admin = crate::ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for permission in [
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
        crate::store::config_client::NodePermission::CollectionGet,
    ] {
        admin
            .grant_node_permission(&creator.did().parse()?, permission)
            .await?;
    }
    let mut docs = Vec::new();
    for binding in [&writer_binding, &integrator_binding] {
        let response = admin.write("test.workspace.target", &format!(r#"mutation {{ create_Agent(input: {{node_did: "{}", agent_did: "{}", creation_nonce: "{}", enabled: true}}) {{_docID}} }}"#,
            runtime.did(), binding.agent_did, binding.creation_nonce)).await?;
        docs.push(crate::store::graphql::created_doc_id(&response, "Agent")?);
    }
    admin
        .add_actor_relationship("Agent", &docs[0], "reader", &creator.did().parse()?)
        .await?;
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        creator.did().parse()?,
        creator.as_ref(),
    )?;
    let mut configured = callback();
    configured.node_did = runtime.did().into();
    configured.handler = crate::documents::CallbackHandler::BuiltIn {
        emitter: crate::documents::BuiltInCallback::CreateWorkspace,
        workspace_agents: Some(crate::documents::WorkspaceAgentSelection {
            writer_agent_did: writer_binding.agent_did.clone(),
            integrator_agent_did: integrator_binding.agent_did.clone(),
            readers: vec![crate::documents::WorkspaceReaderSelection {
                agent_did: integrator_binding.agent_did.clone(),
            }],
        }),
    };
    let source = json!({"work_unit_id":"unit", "repository_id":"repo", "base_sha":"base", "branch":"topic", "owned_files":["src/a.rs"]});
    let mut plan = plan_from_callback(&configured, &source, None).unwrap();
    assert!(
        super::run::bind_workspace_agent_documents(&access, runtime.did(), &mut plan)
            .await
            .is_err()
    );
    admin
        .add_actor_relationship("Agent", &docs[1], "reader", &creator.did().parse()?)
        .await?;
    super::run::bind_workspace_agent_documents(&access, runtime.did(), &mut plan).await?;
    let HostAction::CreateWorkspace(action) = &plan.actions[0] else {
        panic!("workspace plan")
    };
    assert_eq!(
        action.writer_agent_doc_id.as_deref(),
        Some(docs[0].as_str())
    );
    assert_eq!(
        action.integrator_agent_doc_id.as_deref(),
        Some(docs[1].as_str())
    );
    assert_eq!(
        action.readers[0].agent_doc_id.as_deref(),
        Some(docs[1].as_str())
    );
    node.shutdown().await?;
    Ok(())
}

fn lean_journal(states: &[String]) -> Vec<ActionJournalEntry> {
    serde_json::from_value(json!(states
        .iter()
        .enumerate()
        .map(|(index, state)| json!({ "index": index, "state": state }))
        .collect::<Vec<_>>()))
    .unwrap()
}

#[test]
fn generated_recovery_and_denial_match_runtime_owners() {
    let cases = crate::lean_vocab_test::lean_callback_recovery_cases();
    assert_eq!(cases.len(), 28, "Lean must emit the whole recovery matrix");
    for case in cases {
        let journal = lean_journal(&case.journal);
        let (state, recovered) = match crate::tools::workspace::recover_running(&journal) {
            Some(failed) => (LIFECYCLE_FAILED, failed),
            None => (LIFECYCLE_RUNNING, journal.clone()),
        };
        assert_eq!(state, case.post_state, "{}: recovered state", case.name);
        assert_eq!(recovered, lean_journal(&case.post_journal), "{}", case.name);
        assert_eq!(
            crate::tools::workspace::retry_allowed(
                state,
                &recovered,
                case.attempts,
                case.max_attempts
            ),
            case.retry_allowed_after,
            "{}: retry after recovery",
            case.name,
        );

        let mut denied = CallbackInvocationDoc {
            invocation_id: "inv-1".into(),
            agent_did: crate::triggers::callback::test_agent_did(),
            owner_node_did: "did:key:zWriter".into(),
            callback_id: "cb-1".into(),
            configuration_generation: None,
            claimed_callback: None,
            input: json!({}),
            origin: crate::documents::CallbackInvocationOrigin::Event {
                binding_id: "bind-1".into(),
                source_collection: "WorkUnit".into(),
                source_doc_id: "doc-1".into(),
                source_version: None,
            },
            caused_by_correlation: None,
            idempotency_key: "bind-1:doc-1".into(),
            lifecycle_state: LIFECYCLE_RUNNING.into(),
            attempts: Some(i64::from(case.attempts)),
            action_plan: None,
            action_journal: Some(serde_json::to_string(&journal).unwrap()),
            error: None,
            claimed_at: None,
            created_at: None,
        };
        apply_planner_deny(&mut denied, "callback is disabled");
        let denied_journal = decode_journal(denied.action_journal.as_deref()).unwrap();
        assert_eq!(
            denied.lifecycle_state, case.deny_post_state,
            "{}: denied state",
            case.name
        );
        assert_eq!(
            denied_journal,
            lean_journal(&case.deny_post_journal),
            "{}",
            case.name
        );
        assert_eq!(
            crate::tools::workspace::retry_allowed(
                &denied.lifecycle_state,
                &denied_journal,
                case.attempts,
                case.max_attempts
            ),
            case.retry_allowed_after_deny,
            "{}: retry after denial",
            case.name,
        );
    }
}

#[test]
fn executing_action_waits_for_prior_result_docs() {
    let illegal = vec![
        ActionJournalEntry::new(0, ActionJournalState::Validated),
        ActionJournalEntry::new(1, ActionJournalState::Executing),
    ];
    assert!(!can_start_executing(&illegal, 1));

    let legal = vec![
        ActionJournalEntry::new(0, ActionJournalState::ResultDocsWritten),
        ActionJournalEntry::new(1, ActionJournalState::Executing),
    ];
    assert!(can_start_executing(&legal, 1));
    assert!(can_start_executing(&[], 0));
}

#[test]
fn callback_result_requires_succeeded_complete_journal_and_docs() {
    let journal = vec![ActionJournalEntry::new(
        0,
        ActionJournalState::ResultDocsWritten,
    )];
    let workspace = crate::tools::workspace::IsolatedWorkspaceDoc {
        path_capability: crate::tools::workspace::WorkspacePathCapability::exact_paths(Vec::new())
            .unwrap(),
        workspace_id: "ws-1".into(),
        work_unit_id: "unit-1".into(),
        repository_id: "repo-1".into(),
        base_sha: "abc".into(),
        branch: "topic".into(),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_node_did: "deploy-1".into(),
        writer_principal: "did:key:zW".into(),
        integrator_principal: "did:key:zI".into(),
        instruction_manifest: "{}".into(),
        seal_hash: None,
        lifecycle_state: "ready".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
    };
    let placement = crate::tools::workspace::WorkspacePlacementDoc {
        workspace_id: "ws-1".into(),
        owner_node_did: "deploy-1".into(),
        host_path: "/tmp/ws".into(),
        repository_placement_id: "repo-1".into(),
        adapter: "git_worktree".into(),
        adapter_version: "gents-workspace-adapter/1".into(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "{}".into(),
        observed_tree_hash: "tree".into(),
    };
    assert!(!can_emit_callback_result(
        LIFECYCLE_RUNNING,
        &journal,
        Some(&workspace),
        Some(&placement)
    ));
    assert!(!can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &[],
        Some(&workspace),
        Some(&placement)
    ));
    assert!(!can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        None,
        Some(&placement)
    ));
    assert!(!can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        Some(&workspace),
        None
    ));
    assert!(can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        Some(&workspace),
        Some(&placement)
    ));
}

#[test]
fn non_owner_does_not_claim_invocation_or_workspace_request() {
    let invocation = CallbackInvocationDoc {
        invocation_id: "inv-1".into(),
        agent_did: crate::triggers::callback::test_agent_did(),
        owner_node_did: "deploy-owner".into(),
        callback_id: "cb-1".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: "doc-1".into(),
            source_version: Some("created".into()),
        },
        caused_by_correlation: None,
        idempotency_key: "bind-1:doc-1:created".into(),
        lifecycle_state: LIFECYCLE_PENDING.into(),
        attempts: Some(0),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    assert!(!invocation_is_claimable("deploy-replica", &invocation));
    assert!(invocation_is_claimable("deploy-owner", &invocation));
}

#[test]
fn apply_rejects_secret_bearing_source_fields() {
    let mut secret = binding();
    secret.input_fields = vec!["work_unit_id".into(), "api_token".into()];
    assert!(validate_callback_binding(&secret).is_err());
    assert!(validate_callback_binding(&binding()).is_ok());
    let stripped = strip_secret_fields(json!({"branch": "topic", "api_token": "secret"}));
    assert_eq!(stripped["branch"], "topic");
    assert!(stripped.get("api_token").is_none());
}

#[test]
fn apply_rejects_secret_bearing_filter_fields() {
    let error = super::documents::reject_secret_bearing_callback_fields(
        "bind-1",
        Some(r#"{ api_token: { _eq: "x" } }"#),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("secret-bearing"), "{error}");
    assert!(super::documents::reject_secret_bearing_callback_fields(
        "bind-1",
        Some(r#"{ work_unit_id: { _eq: "TOKEN" } }"#),
        None
    )
    .is_ok());
}

#[test]
fn succeeded_without_result_repair_is_windowed_and_batched() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(succeeded_repair_cutoff(now), "2026-08-20T12:00:00Z");

    let row = |id: &str| CallbackInvocationDoc {
        invocation_id: id.into(),
        agent_did: crate::triggers::callback::test_agent_did(),
        owner_node_did: "deploy-1".into(),
        callback_id: "cb-1".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: id.into(),
            source_version: Some("created".into()),
        },
        caused_by_correlation: None,
        idempotency_key: format!("bind-1:{id}:created"),
        lifecycle_state: LIFECYCLE_SUCCEEDED.into(),
        attempts: Some(1),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let missing = succeeded_missing_result(
        vec![row("inv-ok"), row("inv-gap")],
        &["inv-ok".to_string()].into_iter().collect(),
    );
    assert_eq!(
        missing
            .iter()
            .map(|row| row.invocation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["inv-gap"]
    );

    let projected: Vec<CallbackResultInvocationRow> = serde_json::from_value(json!([
        { "invocation_id": "inv-ok" },
        { "invocation_id": "inv-gap" }
    ]))
    .expect("narrow CallbackResult projection should decode without provenance fields");
    assert_eq!(
        projected
            .iter()
            .map(|row| row.invocation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["inv-ok", "inv-gap"]
    );
}

#[test]
fn callback_handler_accepts_module_and_rejects_mixed_handlers() {
    let mut value = serde_json::to_value(callback()).unwrap();
    value["handler"] = json!({"kind":"module","module_id":"module-1"});
    assert!(serde_json::from_value::<crate::documents::Callback>(value.clone()).is_ok());
    value["handler"]["emitter"] = json!("create_workspace");
    assert!(serde_json::from_value::<crate::documents::Callback>(value).is_err());
}

#[test]
fn recovery_reuses_stored_action_plan() {
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-stored"
    });
    let plan = emit_plan_from_source(&callback(), &source).unwrap();
    let mut invocation = CallbackInvocationDoc {
        invocation_id: "inv-1".into(),
        agent_did: crate::triggers::callback::test_agent_did(),
        owner_node_did: "deploy-1".into(),
        callback_id: "cb-1".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: "doc-1".into(),
            source_version: Some("created".into()),
        },
        caused_by_correlation: None,
        idempotency_key: "bind-1:doc-1:created".into(),
        lifecycle_state: LIFECYCLE_RUNNING.into(),
        attempts: Some(1),
        action_plan: Some(serde_json::to_string(&plan).unwrap()),
        action_journal: Some(
            serde_json::to_string(&[ActionJournalEntry::new(0, ActionJournalState::Executing)])
                .unwrap(),
        ),
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let mutated = json!({
        "owned_files": ["other.md"],
        "work_unit_id": "unit-OTHER",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic-OTHER",
        "workspace_id": "ws-mutated"
    });
    let resolved = resolve_action_plan(&invocation, &callback(), &mutated).unwrap();
    match &resolved.actions[0] {
        crate::tools::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-stored");
            assert_eq!(
                action.path_capability,
                crate::tools::workspace::WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
                "recovery cannot expand the stored manifest from changed source content"
            );
            assert_eq!(action.branch, "topic");
        }
        crate::tools::workspace::HostAction::SealWorkspace(_)
        | crate::tools::workspace::HostAction::FreezeWorkspaceBase(_)
        | crate::tools::workspace::HostAction::IntegrateWorkspace(_)
        | crate::tools::workspace::HostAction::CleanupWorkspace(_) => {
            panic!("expected create_workspace")
        }
    }
    invocation.action_plan = None;
    let missing = resolve_action_plan(&invocation, &callback(), &mutated).unwrap_err();
    assert!(missing.contains("missing stored ActionPlan"), "{missing}");
}

#[test]
fn wasm_recovery_reuses_stored_plan_without_reloading_module() {
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-stored"
    });
    let plan = emit_plan_from_source(&callback(), &source).unwrap();
    let journal = vec![ActionJournalEntry::new(0, ActionJournalState::Executing)];
    let invocation = CallbackInvocationDoc {
        invocation_id: "inv-wasm-recover".into(),
        agent_did: crate::triggers::callback::test_agent_did(),
        owner_node_did: "deploy-1".into(),
        callback_id: "cb-1".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: "bind-1".into(),
            source_collection: "WorkUnit".into(),
            source_doc_id: "doc-1".into(),
            source_version: Some("created".into()),
        },
        caused_by_correlation: None,
        idempotency_key: "bind-1:doc-1:created".into(),
        lifecycle_state: LIFECYCLE_RUNNING.into(),
        attempts: Some(1),
        action_plan: Some(action_plan_canonical_json(&plan).unwrap()),
        action_journal: Some(serde_json::to_string(&journal).unwrap()),
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let mut module_binding = callback();
    module_binding.handler = crate::documents::CallbackHandler::Module {
        module_id: "mod-gone".into(),
        workspace_agents: Some(test_workspace_agents()),
    };
    let mutated = json!({
        "owned_files": ["other.md"],
        "work_unit_id": "unit-OTHER",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic-OTHER",
        "workspace_id": "ws-mutated"
    });
    let resolved =
        resolve_action_plan_with_module(&invocation, &module_binding, &mutated, None).unwrap();
    match &resolved.actions[0] {
        crate::tools::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-stored");
            assert_eq!(
                action.path_capability,
                crate::tools::workspace::WorkspacePathCapability::exact_paths(Vec::new()).unwrap(),
                "recovery cannot expand the stored manifest from changed source content"
            );
        }
        crate::tools::workspace::HostAction::SealWorkspace(_)
        | crate::tools::workspace::HostAction::FreezeWorkspaceBase(_)
        | crate::tools::workspace::HostAction::IntegrateWorkspace(_)
        | crate::tools::workspace::HostAction::CleanupWorkspace(_) => {
            panic!("expected create_workspace")
        }
    }
    let mut denied = invocation.clone();
    apply_planner_deny(&mut denied, "CallbackModule mod-gone not found");
    assert_eq!(denied.lifecycle_state, LIFECYCLE_FAILED);
    assert_eq!(
        denied.action_journal,
        Some(
            serde_json::to_string(&[ActionJournalEntry::new(0, ActionJournalState::Interrupted)])
                .unwrap()
        )
    );
    assert_eq!(denied.action_plan, invocation.action_plan);
    assert!(denied.error.as_deref().unwrap().contains("mod-gone"));

    let mut pre_exec = invocation.clone();
    pre_exec.action_journal = Some("[]".into());
    pre_exec.action_plan = None;
    apply_planner_deny(&mut pre_exec, "CallbackModule missing");
    assert_eq!(pre_exec.lifecycle_state, LIFECYCLE_DENIED);
    assert_eq!(pre_exec.action_journal.as_deref(), Some("[]"));
}

#[test]
fn builtin_emitter_accepts_assignment_and_base_revision_aliases() {
    let source = json!({
        "owned_files": [],
        "assignment_id": "cluster:patch",
        "repository_id": "defending_code",
        "base_revision": "abc123"
    });
    let plan = emit_plan_from_source(&callback(), &source).expect("plan");
    match &plan.actions[0] {
        crate::tools::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.work_unit_id, "cluster:patch");
            assert_eq!(action.base_sha, "abc123");
            assert_eq!(action.branch, "gents/cluster-patch");
            assert_eq!(action.workspace_id, "cluster:patch");
        }
        _ => panic!("expected create_workspace"),
    }
}

#[test]
fn builtin_emitter_builds_create_workspace_plan() {
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-1"
    });
    let plan = emit_plan_from_source(&callback(), &source).expect("plan");
    let encoded = serde_json::to_string(&plan).unwrap();
    assert!(!encoded.contains("host_path"));
    assert_eq!(plan.abi, 1);
    match &plan.actions[0] {
        crate::tools::workspace::HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-1");
            assert_eq!(action.work_unit_id, "unit-1");
        }
        crate::tools::workspace::HostAction::SealWorkspace(_)
        | crate::tools::workspace::HostAction::FreezeWorkspaceBase(_)
        | crate::tools::workspace::HostAction::IntegrateWorkspace(_)
        | crate::tools::workspace::HostAction::CleanupWorkspace(_) => {
            panic!("expected create_workspace")
        }
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct GitFixture {
    _root: TempDir,
    repo: PathBuf,
    base_sha: String,
}

impl GitFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "cb@example.com"]);
        git(&repo, &["config", "user.name", "Callback Test"]);
        fs::write(repo.join("README.md"), "hello\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "init"]);
        let base_sha = git(&repo, &["rev-parse", "HEAD"]);
        Self {
            _root: root,
            repo,
            base_sha,
        }
    }
}

#[test]
fn callback_result_only_after_workspace_docs_are_durable() {
    let fx = GitFixture::new();
    let mut docs = MemoryWorkspaceDocuments::default();
    let action = CreateWorkspaceAction {
        writer_agent_did: None,
        writer_agent_doc_id: None,
        integrator_agent_did: None,
        integrator_agent_doc_id: None,
        readers: Vec::new(),
        path_capability: crate::tools::workspace::WorkspacePathCapability::exact_paths(Vec::new())
            .unwrap(),
        workspace_id: "ws-result".into(),
        work_unit_id: "unit-1".into(),
        repository_id: "repo-1".into(),
        base_sha: fx.base_sha.clone(),
        branch: "topic-result".into(),
        creation_policy: CreationPolicy::GitWorktreeDiff,
        adapter: WorkspaceAdapterKind::GitWorktree,
        clone_artifacts: None,
    };
    let plan = emit_create_workspace_plan(action);
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut journal = Vec::new();
    assert!(!can_emit_callback_result(
        LIFECYCLE_RUNNING,
        &journal,
        docs.load_isolated_workspace("ws-result").unwrap().as_ref(),
        docs.load_placement("ws-result").unwrap().as_ref(),
    ));

    let mut ctx = HostExecutorContext {
        owner_node_did: "deploy-1".into(),
        repository: RepositoryPlacementRef {
            repository_id: "repo-1".into(),
            owner_node_did: "deploy-1".into(),
            host_path: fx.repo.clone(),
            enabled: true,
        },
        ceiling: fx.repo.parent(),
        capabilities: caps,
        writer_principal: "did:key:zW".into(),
        integrator_principal: "did:key:zI".into(),
        caused_by_invocation_id: "inv-1".into(),
        caused_by_correlation: "corr-1".into(),
        documents: &mut docs,
    };
    let outcome = execute_create_workspace_plan(&plan, &mut journal, &mut ctx).expect("provision");
    assert_eq!(
        journal.last().map(|entry| entry.state),
        Some(ActionJournalState::ResultDocsWritten)
    );
    assert!(!can_emit_callback_result(
        LIFECYCLE_RUNNING,
        &journal,
        Some(&outcome.workspace),
        Some(&outcome.placement)
    ));
    assert!(can_emit_callback_result(
        LIFECYCLE_SUCCEEDED,
        &journal,
        Some(&outcome.workspace),
        Some(&outcome.placement)
    ));
}

async fn test_node() -> Arc<defra_node::Node> {
    let node = Arc::new(super::test_node_builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    node
}

#[tokio::test]
async fn workspace_reads_keep_the_held_agent_actor() {
    let node = test_node().await;
    let node_did = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    super::seed_test_agent(&node, node_did).await;
    let agent_did = super::test_agent_did();
    let agent_access = super::test_agent_access(&node);
    let node_access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(node_did).unwrap());
    let empty_access = crate::ConfigAccess::graphql_as("http://127.0.0.1:1/api/v0/graphql", "");
    for case in &crate::lean_vocab_test::lean_contract_snapshot().callback_workspace_actor_cases {
        let access = match case["actor"].as_str().unwrap() {
            "agent" => &agent_access,
            "node" => &node_access,
            "empty" => &empty_access,
            actor => panic!("unknown modeled actor {actor}"),
        };
        let expected = case["expected"].as_bool().unwrap();
        let selected = if case["actor"] == "empty" {
            ""
        } else {
            &agent_did
        };
        let documents = super::documents::load_memory_workspace_docs(
            access,
            "missing-workspace",
            node_did,
            selected,
        )
        .await;
        assert_eq!(documents.is_ok(), expected, "workspace actor: {case}");
        let repository = super::documents::load_repository_placement(
            access,
            "missing-repository",
            node_did,
            selected,
        )
        .await;
        assert_eq!(repository.is_ok(), expected, "repository actor: {case}");
    }
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn first_seen_source_create_materializes_owner_invocation() {
    let node = test_node().await;
    let owner_node_did = crate::request::lifecycle::test_support::PIN_FIXED_DID.to_owned();
    super::seed_test_agent(&node, &owner_node_did).await;
    node.add_collection(
        r#"
        type WorkUnit {
            work_unit_id: String
            owned_files: String
            repository_id: String
            base_sha: String
            branch: String
        }
        "#,
    )
    .await
    .expect("WorkUnit schema");

    super::seed_test_configuration(&node, &owner_node_did, vec![
        (crate::Collection::Callback, json!({"callback_id":"cb-scan","handler":{"kind":"built_in","emitter":"create_workspace"},
            "capabilities":["create_workspace","observe_dirty_base"],"enabled":true})),
        (crate::Collection::EventSource, json!({"event_source_id":"events-scan","source_collection":"WorkUnit","event_kind":"created"})),
        (crate::Collection::CallbackBinding, json!({"binding_id":"bind-scan","agent_did":super::test_agent_did(),
            "event_source_id":"events-scan","callback_id":"cb-scan",
            "input_fields":["work_unit_id","repository_id","base_sha","branch","owned_files"],"enabled":true})),
    ]).await;

    let held = crate::store::config_client::ConfigApplyTxn::begin_local(&node, None)
        .await
        .unwrap();
    let source = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        super::documents::load_event_source(&node, "events-scan", &owner_node_did),
    )
    .await
    .expect("callback configuration lookup must not wait for the mutation gate")
    .unwrap()
    .expect("configured event source");
    assert_eq!(source.event_source_id, "events-scan");
    held.discard().await.unwrap();

    let cancel = tokio_util::sync::CancellationToken::new();
    let mut engine = super::CallbackEngine::new(node.clone(), owner_node_did.clone(), None, cancel);
    engine.snapshots = Some(super::test_snapshot());
    engine.reconcile_bindings().await;

    let create = r#"mutation {
        create_WorkUnit(input: {
            work_unit_id: "unit-scan",
            owned_files: "[]",
            repository_id: "repo-1",
            base_sha: "abc",
            branch: "topic"
        }) { _docID }
    }"#;
    let created = node.execute(create).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let doc_id = crate::store::graphql::single_mutation_document(&created, "create_WorkUnit")
        .unwrap()
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("WorkUnit doc id")
        .to_string();

    let mut projection = binding();
    projection.input_fields.clear();
    let mut cache = super::scan::SourceSchemaCache::default();
    let empty =
        super::scan::fetch_source_doc(&node, &mut cache, "WorkUnit", &doc_id, None, &projection)
            .await
            .unwrap();
    assert_eq!(empty, json!({}), "empty input_fields grants no source data");
    projection.input_fields = vec![
        "work_unit_id".into(),
        "repository_id".into(),
        "base_sha".into(),
        "branch".into(),
    ];
    let narrow =
        super::scan::fetch_source_doc(&node, &mut cache, "WorkUnit", &doc_id, None, &projection)
            .await
            .unwrap();
    assert!(narrow.get("owned_files").is_none());
    assert!(
        emit_plan_from_source(&callback(), &narrow).is_err(),
        "omitted manifest cannot authorize workspace paths"
    );
    projection.input_fields.push("owned_files".into());
    let admitted =
        super::scan::fetch_source_doc(&node, &mut cache, "WorkUnit", &doc_id, None, &projection)
            .await
            .unwrap();
    assert!(emit_plan_from_source(&callback(), &admitted).is_ok());

    engine.deliver_arrivals(Some("WorkUnit")).await;

    let query = format!(
        r#"{{
            CallbackInvocation(
                filter: {{ owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 10
            ) {{
                invocation_id
                owner_node_did
                agent_did
                lifecycle_state
                idempotency_key
                input
                origin
            }}
        }}"#,
        owner = crate::store::graphql::escape_graphql_string(&owner_node_did),
    );
    let access = super::test_agent_access(&node);
    let response = access.execute(&query).await.unwrap();
    let rows = response
        .get("data")
        .and_then(|data| data.get("CallbackInvocation"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["owner_node_did"], owner_node_did);
    assert_eq!(
        rows[0]["agent_did"],
        crate::triggers::callback::test_agent_did()
    );
    assert_eq!(rows[0]["origin"]["source_doc_id"], doc_id);
    assert_eq!(rows[0]["input"]["work_unit_id"], "unit-scan");
    assert!(rows[0]["input"].get("_docID").is_none());
    engine.deliver_arrivals(Some("WorkUnit")).await;
    let repeated = access.execute(&query).await.unwrap();
    assert_eq!(
        repeated["data"]["CallbackInvocation"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn artifact_bytes_for_id() -> Vec<u8> {
    crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(b"{}"))
}

fn module_doc(wasm: &[u8], args: &serde_json::Value, signer: &str) -> CallbackModuleDoc {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let module_id = compute_module_id(wasm, args, 1).unwrap();
    CallbackModuleDoc {
        module_id,
        node_did: "did:key:zWriter".into(),
        tags: vec![],
        abi_version: Some(1),
        artifact_bytes: Some(STANDARD.encode(wasm)),
        canonical_args: Some(serde_json::to_string(args).unwrap()),
        signer_did: Some(signer.into()),
        provenance: Some("fixture_create_workspace".into()),
        enabled: true,
        fuel_limit: Some(50_000_000),
        memory_pages: Some(256),
        max_input_bytes: Some(1_000_000),
        max_output_bytes: Some(1_000_000),
    }
}

fn trusted(signer: &str) -> BTreeSet<String> {
    [signer.to_string()].into_iter().collect()
}

fn wat_returns_json(json: &str) -> Vec<u8> {
    crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
        json.as_bytes(),
    ))
}

fn tight_limits(fuel: u64, pages: u32, max_in: usize, max_out: usize) -> CallbackModuleLimits {
    CallbackModuleLimits {
        fuel_limit: fuel,
        memory_pages: pages,
        max_input_bytes: max_in,
        max_output_bytes: max_out,
    }
}

#[test]
fn module_id_is_stable_across_host_paths_and_arg_key_order() {
    let wasm = artifact_bytes_for_id();
    let root = TempDir::new().unwrap();
    let path_a = root.path().join("one").join("module.wasm");
    let path_b = root.path().join("other-host").join("copy.wasm");
    fs::create_dir_all(path_a.parent().unwrap()).unwrap();
    fs::create_dir_all(path_b.parent().unwrap()).unwrap();
    fs::write(&path_a, &wasm).unwrap();
    fs::write(&path_b, &wasm).unwrap();

    let id_a = compute_module_id(&fs::read(&path_a).unwrap(), &json!({"b": 1, "a": 2}), 1).unwrap();
    let id_b = compute_module_id(&fs::read(&path_b).unwrap(), &json!({"a": 2, "b": 1}), 1).unwrap();
    assert_eq!(id_a, id_b);
    assert!(id_a.starts_with("sha256:"));
    assert_ne!(
        compute_module_id(path_a.to_string_lossy().as_bytes(), &json!({}), 1).unwrap(),
        id_a,
        "module_id must hash decoded bytes, not the host path"
    );
}

#[test]
fn signer_policy_fail_closes_when_missing_or_untrusted() {
    let wasm = artifact_bytes_for_id();
    let mut module = module_doc(&wasm, &json!({}), "did:key:zTrusted");
    let empty = BTreeSet::new();
    let error = validate_callback_module(&module, &empty).unwrap_err();
    assert!(error.contains("no trusted principals"), "{error}");

    module.signer_did = None;
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("signer_did is missing"), "{error}");

    module.signer_did = Some("did:key:zTrusted".into());
    module.provenance = None;
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("provenance is missing"), "{error}");

    module.provenance = Some("fixture".into());
    module.signer_did = Some("did:key:zStranger".into());
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("not a trusted installer"), "{error}");

    module.signer_did = Some("did:key:zTrusted".into());
    validate_callback_module(&module, &trusted("did:key:zTrusted")).expect("trusted signer");
}

#[test]
fn planner_uses_shared_plugin_fuel_memory_and_output_limits() {
    let infinite = crate::plugin::tests::build_plugin_afb(
        r#"(module (memory (export "memory") 1)
          (func (export "_start") (loop $spin (br $spin))))"#,
    );
    let error = invoke_plugin_planner(&infinite, &tight_limits(100, 1, 1024, 1024), &json!({}))
        .unwrap_err();
    assert!(error.contains("OutOfFuel"), "{error}");
    let memory = crate::plugin::tests::build_plugin_afb(
        r#"(module (memory (export "memory") 8) (func (export "_start")))"#,
    );
    assert!(
        invoke_plugin_planner(&memory, &tight_limits(10_000, 1, 1024, 1024), &json!({})).is_err()
    );
    let output = wat_returns_json(r#"{"value":"long output"}"#);
    let error = invoke_plugin_planner(
        &output,
        &tight_limits(100_000, 1, 4, 1024),
        &json!({"input":"too long"}),
    )
    .unwrap_err();
    assert!(error.contains("max_input_bytes"), "{error}");
    let error =
        invoke_plugin_planner(&output, &tight_limits(100_000, 1, 1024, 8), &json!({})).unwrap_err();
    assert!(error.contains("BadOutput"), "{error}");
}

#[test]
fn unknown_action_type_denies_the_entire_plan() {
    let wasm = wat_returns_json(r#"{"abi":1,"actions":[{"type":"not_a_real_action"}]}"#);
    let module = module_doc(&wasm, &json!({}), "did:key:zTrusted");
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let error = plan_from_module(&module, &json!({}), &caps).unwrap_err();
    assert!(error.contains("unknown ActionPlan action type"), "{error}");
}

#[test]
fn host_path_in_plan_is_denied() {
    let error = parse_action_plan_json(
        r#"{"abi":1,"actions":[{"type":"create_workspace","path_capability":{"mode":"exactPaths","paths":[]},"workspace_id":"ws","work_unit_id":"u","repository_id":"r","base_sha":"s","branch":"/tmp/evil"}]}"#,
    )
    .unwrap_err();
    assert!(error.contains("host path"), "{error}");
    for branch in [
        r"C:\Users\evil",
        r"\\?\C:\foo",
        "file:///tmp/foo",
        "~/secret",
        "topic/../escape",
        r"topic\..\escape",
        "C:foo",
    ] {
        let raw = serde_json::to_string(&json!({
            "abi": 1,
            "actions": [{
                "type": "create_workspace",
                "path_capability": {"mode": "exactPaths", "paths": []},
                "workspace_id": "ws",
                "work_unit_id": "u",
                "repository_id": "r",
                "base_sha": "s",
                "branch": branch,
            }]
        }))
        .unwrap();
        let error = parse_action_plan_json(&raw).unwrap_err();
        assert!(error.contains("host path"), "{branch}: {error}");
    }
}

#[test]
fn extra_action_fields_deny_the_plan() {
    let error = parse_action_plan_json(
        r#"{"abi":1,"actions":[{"type":"create_workspace","path_capability":{"mode":"exactPaths","paths":[]},"workspace_id":"ws","work_unit_id":"u","repository_id":"r","base_sha":"s","branch":"topic","path":"relative"}]}"#,
    )
    .unwrap_err();
    assert!(
        error.contains("schema rejected")
            || error.contains("unknown field")
            || error.contains("host_path"),
        "{error}"
    );
    let error = parse_action_plan_json(
        r#"{"abi":1,"actions":[{"type":"create_workspace","path_capability":{"mode":"exactPaths","paths":[]},"workspace_id":"ws","work_unit_id":"u","repository_id":"r","base_sha":"s","branch":"topic","host_path":"/tmp/ws"}]}"#,
    )
    .unwrap_err();
    assert!(error.contains("host_path"), "{error}");
}

#[test]
fn callback_requires_bounded_afterburner_artifact() {
    let artifact = artifact_bytes_for_id();
    let mut module = module_doc(&artifact, &json!({}), "did:key:zTrusted");
    module.fuel_limit = Some(i64::MAX);
    let error = validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("host maximum"), "{error}");
    let malformed = module_doc(b"not an artifact", &json!({}), "did:key:zTrusted");
    let error = validate_callback_module(&malformed, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("Afterburner"), "{error}");
    let oversized = module_doc(
        &vec![0; MAX_ARTIFACT_BYTES + 1],
        &json!({}),
        "did:key:zTrusted",
    );
    let error = validate_callback_module(&oversized, &trusted("did:key:zTrusted")).unwrap_err();
    assert!(error.contains("max_artifact_bytes"), "{error}");
}

#[test]
fn capability_miss_denies_create_workspace_plan() {
    let Some(wasm) = fixture_create_workspace_artifact() else {
        return;
    };
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-1"
    });
    let module = module_doc(wasm, &json!({}), "did:key:zTrusted");
    validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap();
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let plan = plan_from_module(&module, &source, &caps).expect("plan");
    let missing = BTreeSet::from([CAP_OBSERVE_DIRTY_BASE.to_string()]);
    let error = plan.validate_against(&missing).unwrap_err().to_string();
    assert!(
        error.contains("missing capability create_workspace"),
        "{error}"
    );
}

#[test]
fn fixture_artifact_emits_valid_create_workspace_plan() {
    let Some(wasm) = fixture_create_workspace_artifact() else {
        return;
    };
    let source = json!({
        "owned_files": [],
        "work_unit_id": "unit-1",
        "repository_id": "repo-1",
        "base_sha": "abc",
        "branch": "topic",
        "workspace_id": "ws-1",
        "api_token": "should-be-stripped"
    });
    let module = module_doc(
        wasm,
        &json!({"adapter": "git_worktree"}),
        "did:key:zTrusted",
    );
    validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap();
    let caps: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let stripped = strip_secret_fields(source);
    let plan = plan_from_module(&module, &stripped, &caps).expect("fixture plan");
    let canonical = action_plan_canonical_json(&plan).expect("canonical");
    assert!(!canonical.contains("host_path"), "{canonical}");
    assert!(!canonical.contains("/tmp"), "{canonical}");
    assert!(!canonical.contains("api_token"), "{canonical}");
    assert_eq!(canonical, action_plan_canonical_json(&plan).unwrap());
    assert!(canonical.contains("\"abi\":1"));
    assert!(canonical.contains("\"adapter\":\"git_worktree\""));
    match &plan.actions[0] {
        HostAction::CreateWorkspace(action) => {
            assert_eq!(action.workspace_id, "ws-1");
            assert_eq!(action.work_unit_id, "unit-1");
            assert_eq!(action.adapter.as_str(), "git_worktree");
        }
        HostAction::FreezeWorkspaceBase(_)
        | HostAction::SealWorkspace(_)
        | HostAction::IntegrateWorkspace(_)
        | HostAction::CleanupWorkspace(_) => panic!("expected create_workspace"),
    }
    plan.validate_against(&caps).expect("capabilities");

    let mut module_binding = callback();
    module_binding.handler = crate::documents::CallbackHandler::Module {
        module_id: module.module_id.clone(),
        workspace_agents: Some(test_workspace_agents()),
    };
    module_binding.capabilities = vec![CAP_CREATE_WORKSPACE.into(), CAP_OBSERVE_DIRTY_BASE.into()];
    let via_binding = plan_from_callback(&module_binding, &stripped, Some(&module)).expect("wired");
    let mut expected = plan.clone();
    if let HostAction::CreateWorkspace(action) = &mut expected.actions[0] {
        let agents = test_workspace_agents();
        action.writer_agent_did = Some(agents.writer_agent_did);
        action.integrator_agent_did = Some(agents.integrator_agent_did);
    }
    assert_eq!(via_binding, expected);

    let clone_caps: BTreeSet<String> = [
        CAP_CREATE_WORKSPACE,
        CAP_OBSERVE_DIRTY_BASE,
        CAP_CLONE_ARTIFACTS,
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let builtin_binding = callback();
    let builtin = emit_plan_from_source(&builtin_binding, &stripped).expect("builtin");
    builtin.validate_against(&clone_caps).expect("builtin caps");
}

#[test]
fn canonical_action_plan_sorts_object_keys() {
    let plan = emit_plan_from_source(
        &callback(),
        &json!({
            "owned_files": [],
            "work_unit_id": "unit-1",
            "repository_id": "repo-1",
            "base_sha": "abc",
            "branch": "topic",
            "workspace_id": "ws-1"
        }),
    )
    .unwrap();
    let canonical = action_plan_canonical_json(&plan).unwrap();
    let abi = canonical.find("\"abi\"").unwrap();
    let actions = canonical.find("\"actions\"").unwrap();
    assert!(abi < actions, "{canonical}");
    let parsed = parse_action_plan_json(&canonical).unwrap();
    assert_eq!(parsed, plan);
}

/// Absent optional fields render as GraphQL `null`, never `""`: the empty
/// string is a real value, so a later query must be able to tell the two apart.
#[test]
fn create_callback_result_mutation_renders_absent_fields_as_null_not_empty_string() {
    let result = CallbackResultDoc {
        result_id: "res-1".to_owned(),
        invocation_id: "inv-1".to_owned(),
        binding_id: None,
        agent_did: crate::triggers::callback::test_agent_did(),
        owner_node_did: "did:key:zWriter".to_owned(),
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: None,
        created_at: Some("2024-01-01T00:00:00Z".to_owned()),
    };
    let mutation = create_callback_result_mutation(&result, "agent-doc", false);
    assert!(mutation.contains("binding_id: null,"), "{mutation}");
    assert!(mutation.contains("workspace_id: null,"), "{mutation}");
    assert!(mutation.contains("work_unit_id: null,"), "{mutation}");
    assert!(
        mutation.contains("caused_by_correlation: null,"),
        "{mutation}"
    );
    assert!(!mutation.contains(r#""""#), "{mutation}");

    let present = CallbackResultDoc {
        binding_id: Some("bind-1".to_owned()),
        workspace_id: Some("ws-1".to_owned()),
        work_unit_id: Some("unit-1".to_owned()),
        caused_by_correlation: Some("corr-1".to_owned()),
        ..result
    };
    let mutation = create_callback_result_mutation(&present, "agent-doc", false);
    assert!(mutation.contains(r#"binding_id: "bind-1","#), "{mutation}");
    assert!(mutation.contains(r#"workspace_id: "ws-1","#), "{mutation}");
    assert!(
        mutation.contains(r#"work_unit_id: "unit-1","#),
        "{mutation}"
    );
    assert!(
        mutation.contains(r#"caused_by_correlation: "corr-1","#),
        "{mutation}"
    );
}

#[test]
fn update_invocation_mutation_renders_absent_fields_as_null_not_empty_string() {
    let origin = crate::documents::CallbackInvocationOrigin::Event {
        binding_id: "bind-1".into(),
        source_collection: "WorkUnit".into(),
        source_doc_id: "doc-1".into(),
        source_version: Some("created".into()),
    };
    let invocation = CallbackInvocationDoc {
        invocation_id: "inv-1".into(),
        agent_did: crate::triggers::callback::test_agent_did(),
        owner_node_did: "did:key:zWriter".into(),
        callback_id: "cb-1".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: origin.clone(),
        caused_by_correlation: None,
        idempotency_key: "key-1".into(),
        lifecycle_state: LIFECYCLE_PENDING.into(),
        attempts: Some(1),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let mutation = update_invocation_mutation(&invocation, None);
    assert!(mutation.contains("action_plan: null,"), "{mutation}");
    assert!(mutation.contains("error: null,"), "{mutation}");
    assert!(mutation.contains("claimed_at: null,"), "{mutation}");
    assert!(!mutation.contains(r#""""#), "{mutation}");

    let failed = CallbackInvocationDoc {
        origin,
        lifecycle_state: LIFECYCLE_FAILED.into(),
        attempts: Some(2),
        action_plan: Some("[]".into()),
        action_journal: Some("[]".into()),
        error: Some("boom".into()),
        claimed_at: Some("2024-01-01T00:00:00Z".into()),
        ..invocation
    };
    let mutation = update_invocation_mutation(&failed, Some("running"));
    assert!(mutation.contains(r#"action_plan: "[]","#), "{mutation}");
    assert!(mutation.contains(r#"error: "boom","#), "{mutation}");
    assert!(
        mutation.contains(r#"claimed_at: "2024-01-01T00:00:00Z","#),
        "{mutation}"
    );
}

/// Workspace packs select one binding's results with
/// `{ binding_id: { _eq: ... } }`, so the stored result must carry it.
#[tokio::test]
async fn callback_results_are_selectable_by_their_binding() {
    let node = test_node().await;
    super::seed_test_agent(
        &node,
        crate::request::lifecycle::test_support::PIN_FIXED_DID,
    )
    .await;
    for (invocation_id, binding_id) in [
        ("inv-a", "maintenance-execute-workspace"),
        ("inv-b", "other"),
    ] {
        let stored = super::documents::create_callback_result(
            &node,
            &super::test_agent_access(&node),
            &super::documents::CallbackResultDoc {
                result_id: format!("res-{invocation_id}"),
                invocation_id: invocation_id.to_owned(),
                binding_id: Some(binding_id.to_owned()),
                agent_did: crate::triggers::callback::test_agent_did(),
                owner_node_did: crate::request::lifecycle::test_support::PIN_FIXED_DID.to_owned(),
                workspace_id: None,
                work_unit_id: None,
                caused_by_correlation: None,
                created_at: None,
            },
        )
        .await
        .expect("result stored");
        assert_eq!(stored.binding_id.as_deref(), Some(binding_id));
    }

    let response = super::test_agent_access(&node)
        .execute(
            r#"{ CallbackResult(filter: { binding_id: { _eq: "maintenance-execute-workspace" } }) { invocation_id } }"#,
        )
        .await.unwrap();
    let rows: Vec<serde_json::Value> =
        serde_json::from_value(response["data"]["CallbackResult"].clone()).expect("rows");
    assert_eq!(rows, vec![json!({"invocation_id": "inv-a"})]);
}

#[tokio::test]
async fn callback_result_is_agent_owned_and_parent_link_grants_user_read() {
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let outsider = KeyIdentity::load_or_create(home.path().join("outsider.key"), None).unwrap();
    let node = test_node().await;
    super::seed_test_agent(
        &node,
        crate::request::lifecycle::test_support::PIN_FIXED_DID,
    )
    .await;
    super::grant_test_node_administrator(&node, user.did()).await;
    super::grant_test_node_read(&node, outsider.did()).await;
    let agent = super::test_agent_did();
    let user_access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(user.did()).unwrap(),
    );
    let outsider_access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(outsider.did()).unwrap(),
    );
    let result = CallbackResultDoc {
        result_id: "owned-result".into(),
        invocation_id: "owned-invocation".into(),
        binding_id: None,
        owner_node_did: crate::request::lifecycle::test_support::PIN_FIXED_DID.into(),
        agent_did: agent,
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: None,
        created_at: None,
    };
    super::documents::create_callback_result(&node, &super::test_agent_access(&node), &result)
        .await
        .unwrap();
    let query = "{ CallbackResult { _docID result_id } }";
    for access in [&user_access, &super::test_agent_access(&node)] {
        assert_eq!(
            access.execute(query).await.unwrap()["data"]["CallbackResult"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    assert!(
        outsider_access.execute(query).await.unwrap()["data"]["CallbackResult"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let agents = user_access.execute("{ Agent { _docID } }").await.unwrap();
    let agent_doc = agents["data"]["Agent"][0]["_docID"].as_str().unwrap();
    assert!(!agent_doc.is_empty());
    assert_eq!(
        user_access.execute(query).await.unwrap()["data"]["CallbackResult"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn callback_result_visibility_and_repair_match_the_executable_cursor_contract() {
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    let directory = tempfile::tempdir().unwrap();
    let outsider =
        KeyIdentity::load_or_create(directory.path().join("outsider.key"), None).unwrap();
    let node = test_node().await;
    let owner = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    super::seed_test_agent(&node, owner).await;
    super::grant_test_node_read(&node, outsider.did()).await;
    let agent = super::test_agent_did();
    let access = super::test_agent_access(&node);
    let agents = access.execute("{ Agent { _docID } }").await.unwrap();
    let parent = agents["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let result = CallbackResultDoc {
        result_id: "repair-result".into(),
        invocation_id: "repair-invocation".into(),
        binding_id: None,
        owner_node_did: owner.into(),
        agent_did: agent.clone(),
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: None,
        created_at: None,
    };
    let written = access
        .write(
            "test.callback.repair_seed",
            &create_callback_result_mutation(&result, parent, false),
        )
        .await
        .unwrap();
    let doc_id = crate::store::graphql::created_doc_id(&written, "CallbackResult").unwrap();
    let mut engine = super::CallbackEngine::new(
        node.clone(),
        owner.into(),
        None,
        tokio_util::sync::CancellationToken::new(),
    );
    engine.snapshots = Some(super::test_snapshot());
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().trigger_delivery["callback_visibility"];
    let mut interrupted_after_grant = false;
    for case in cases.as_array().expect("Lean callback visibility cases") {
        let visible = case["visible"].as_bool().unwrap();
        let linked = case["linked"].as_bool().unwrap();
        if linked {
            if !interrupted_after_grant {
                access
                    .add_resource_relationship(
                        "CallbackResult",
                        &doc_id,
                        "parent",
                        "agents",
                        parent,
                    )
                    .await
                    .unwrap();
                assert!(
                    !crate::triggers::engine::event_delivery::callback_result_ready(
                        &node,
                        "CallbackResult",
                        &doc_id,
                        &agent,
                    )
                    .await
                    .unwrap()
                );
                interrupted_after_grant = true;
            }
            // Recovery repairs a crash between result creation, native grant,
            // and publishing the marked revision.
            super::documents::list_recoverable_invocations(&node, owner, &engine)
                .await
                .unwrap();
        } else {
            access.write("test.callback.repair_reset", &format!(
                "mutation {{ update_CallbackResult(docID: \"{}\", input: {{parent_linked: false}}) {{_docID}} }}",
                crate::store::graphql::escape_graphql_string(&doc_id),
            )).await.unwrap();
        }
        let actor = if visible {
            agent.as_str()
        } else {
            outsider.did()
        };
        let ready = crate::triggers::engine::event_delivery::callback_result_ready(
            &node,
            "CallbackResult",
            &doc_id,
            actor,
        )
        .await
        .unwrap();
        assert_eq!(ready, case["ready"].as_bool().unwrap(), "{case}");
        assert_eq!(
            if ready { 5 } else { 4 },
            case["after"].as_u64().unwrap(),
            "{case}"
        );
    }
    let mut before_grant = result.clone();
    before_grant.result_id = "repair-before-grant-result".into();
    before_grant.invocation_id = "repair-before-grant-invocation".into();
    let written = access
        .write(
            "test.callback.repair_before_grant",
            &create_callback_result_mutation(&before_grant, parent, false),
        )
        .await
        .unwrap();
    let unlinked_doc = crate::store::graphql::created_doc_id(&written, "CallbackResult").unwrap();
    assert!(
        !crate::triggers::engine::event_delivery::callback_result_ready(
            &node,
            "CallbackResult",
            &unlinked_doc,
            &agent
        )
        .await
        .unwrap()
    );
    super::documents::list_recoverable_invocations(&node, owner, &engine)
        .await
        .unwrap();
    assert!(
        crate::triggers::engine::event_delivery::callback_result_ready(
            &node,
            "CallbackResult",
            &unlinked_doc,
            &agent
        )
        .await
        .unwrap()
    );
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn every_invocation_origin_names_its_binding() {
    use crate::documents::CallbackInvocationOrigin;
    let event = CallbackInvocationOrigin::Event {
        binding_id: "one".to_owned(),
        source_collection: "WorkUnit".to_owned(),
        source_doc_id: "doc".to_owned(),
        source_version: None,
    };
    let group = CallbackInvocationOrigin::EventGroup {
        binding_id: "two".to_owned(),
        group_key: "key".to_owned(),
    };
    assert_eq!(event.binding_id(), "one");
    assert_eq!(group.binding_id(), "two");
}

struct Backlog {
    _identity: crate::store::identity::KeyIdentity,
    _home: TempDir,
    node: Arc<defra_node::Node>,
    access: crate::store::config_client::ConfigAccess,
    history: String,
}

const BACKLOG_OWNER: &str = crate::request::lifecycle::test_support::PIN_FIXED_DID;

impl Backlog {
    async fn config_ids(&self) -> super::documents::CallbackConfigDocIds {
        let rows = self
            .access
            .execute("{CallbackBinding {_docID} EventSource {_docID} Callback {_docID}}")
            .await
            .unwrap();
        super::documents::CallbackConfigDocIds {
            binding: rows["data"]["CallbackBinding"][0]["_docID"]
                .as_str()
                .unwrap()
                .into(),
            source: rows["data"]["EventSource"][0]["_docID"]
                .as_str()
                .unwrap()
                .into(),
            callback: rows["data"]["Callback"][0]["_docID"]
                .as_str()
                .unwrap()
                .into(),
        }
    }
    async fn registered() -> Self {
        use crate::documents::apply_desired_state_plan;
        use crate::store::config_client::{
            ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use crate::Collection;
        let home = tempfile::tempdir().unwrap();
        let identity =
            crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path());
        let node = test_node().await;
        super::seed_test_agent(&node, BACKLOG_OWNER).await;
        let access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(BACKLOG_OWNER).unwrap());
        access
            .add_collection("type BacklogWork { label: String }")
            .await
            .unwrap();
        let mut backlog = Self {
            _identity: identity,
            _home: home,
            node,
            access,
            history: String::new(),
        };
        backlog.history = backlog.create("history").await;
        let documents = [
            (
                Collection::Callback,
                json!({"callback_id":"cb-backlog","handler":{"kind":"built_in","emitter":"create_workspace"},
                    "capabilities":["create_workspace"],"enabled":true}),
            ),
            (
                Collection::EventSource,
                json!({"event_source_id":"backlog","source_collection":"BacklogWork","event_kind":"created"}),
            ),
            (
                Collection::CallbackBinding,
                json!({"binding_id":"bind-backlog","agent_did":crate::triggers::callback::test_agent_did(),"event_source_id":"backlog","callback_id":"cb-backlog",
                    "input_fields":["label"],"enabled":true}),
            ),
        ]
        .into_iter()
        .map(|(collection, mut value)| {
            value["node_did"] = json!(BACKLOG_OWNER);
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect();
        let plan = DesiredStateApplyPlan::new(documents).unwrap();
        backlog
            .access
            .transact_write("test.backlog_config", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
        backlog
    }

    async fn add_binding(&self, binding_id: &str) {
        use crate::documents::apply_desired_state_plan;
        use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let value = json!({"binding_id":binding_id,"agent_did":crate::triggers::callback::test_agent_did(),"event_source_id":"backlog","callback_id":"cb-backlog",
            "input_fields":["label"],"enabled":true,"node_did":BACKLOG_OWNER});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: crate::Collection::CallbackBinding,
            add: value.clone(),
            update: value,
        }])
        .unwrap();
        self.access
            .transact_write("test.backlog_binding", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    async fn create(&self, label: &str) -> String {
        let response = self
            .access
            .write(
                "test.backlog_work",
                &format!(
                    "mutation {{ create_BacklogWork(input: {{label: \"{label}\"}}) {{ _docID }} }}"
                ),
            )
            .await
            .unwrap();
        crate::store::graphql::created_doc_id(&response, "BacklogWork").unwrap()
    }

    fn engine(&self) -> super::CallbackEngine {
        let mut engine = super::CallbackEngine::new(
            self.node.clone(),
            BACKLOG_OWNER.into(),
            None,
            tokio_util::sync::CancellationToken::new(),
        );
        engine.snapshots = Some(super::test_snapshot());
        engine.plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(None));
        engine
    }

    async fn start_engine(&self) {
        let mut engine = self.engine();
        engine.reconcile_bindings().await;
        engine.deliver_arrivals(None).await;
    }

    async fn invoked(&self) -> Vec<String> {
        let response = super::test_agent_access(&self.node)
            .execute("{ CallbackInvocation { origin } }")
            .await
            .unwrap();
        let mut docs = response["data"]["CallbackInvocation"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["origin"]["source_doc_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        docs.sort();
        docs
    }

    async fn cursor(&self) -> String {
        self.access
            .transact_write("test.backlog_cursor", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        crate::documents::event_consumer::load_or_seed(
                            txn,
                            BACKLOG_OWNER,
                            &gents_protocol::event_delivery::EventConsumer::CallbackBinding {
                                binding_id: "bind-backlog".into(),
                            },
                        )
                        .await?
                        .cursor
                        .after,
                    )
                })
            })
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn callback_claim_uses_modeled_selected_agent_grants() {
    let backlog = Backlog::registered().await;
    let case = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let modeled_agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let access = super::test_agent_access(&backlog.node);
    let callback = super::documents::load_callback(&backlog.node, "cb-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let binding = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let source =
        super::documents::load_event_source(&backlog.node, &binding.event_source_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let ids = backlog.config_ids().await;
    let generation =
        super::documents::configuration_generation(&binding, &source, &callback, &ids).unwrap();
    let binding_row = backlog
        .access
        .execute("{ CallbackBinding { _docID } }")
        .await
        .unwrap();
    let binding_doc = binding_row["data"]["CallbackBinding"][0]["_docID"]
        .as_str()
        .unwrap();
    for (index, revoked) in [false, true].into_iter().enumerate() {
        if revoked {
            backlog
                .access
                .delete_actor_relationship(
                    "CallbackBinding",
                    binding_doc,
                    "reader",
                    &identity::Did::new(super::test_agent_did()).unwrap(),
                )
                .await
                .unwrap();
        }
        let invocation = CallbackInvocationDoc {
            invocation_id: format!("modeled-{index}"),
            owner_node_did: BACKLOG_OWNER.into(),
            agent_did: super::test_agent_did(),
            callback_id: "cb-backlog".into(),
            configuration_generation: Some(generation.clone()),
            claimed_callback: None,
            input: json!({}),
            origin: crate::documents::CallbackInvocationOrigin::Event {
                binding_id: "bind-backlog".into(),
                source_collection: "BacklogWork".into(),
                source_doc_id: format!("source-{index}"),
                source_version: None,
            },
            idempotency_key: format!("modeled-{index}"),
            caused_by_correlation: None,
            lifecycle_state: LIFECYCLE_PENDING.into(),
            attempts: Some(0),
            action_plan: None,
            action_journal: Some("[]".into()),
            error: None,
            claimed_at: None,
            created_at: None,
        };
        let invocation =
            super::documents::create_pending_invocation(&backlog.node, &access, &invocation)
                .await
                .unwrap();
        let claim = super::claim::claim_invocation(
            &backlog.node,
            &access,
            BACKLOG_OWNER,
            &invocation,
            &callback,
            None,
            None,
        )
        .await;
        let expected = case
            .callback_configuration_cases
            .iter()
            .find(|observation| {
                observation["actor"] == *modeled_agent
                    && observation["id"] == "binding"
                    && observation["revoked"] == revoked
            })
            .unwrap()["claim_allowed"]
            .as_bool()
            .unwrap();
        assert_eq!(claim.is_ok_and(|value| value.is_some()), expected);
    }
    backlog.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn callback_claim_checks_enabled_and_invoke_only_before_admission() {
    let backlog = Backlog::registered().await;
    let access = super::test_agent_access(&backlog.node);
    let binding = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let source =
        super::documents::load_event_source(&backlog.node, &binding.event_source_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let callback =
        super::documents::load_callback(&backlog.node, &binding.callback_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let ids = backlog.config_ids().await;
    let pending = CallbackInvocationDoc {
        invocation_id: "invoke-check".into(),
        owner_node_did: BACKLOG_OWNER.into(),
        agent_did: super::test_agent_did(),
        callback_id: binding.callback_id.clone(),
        configuration_generation: Some(
            super::documents::configuration_generation(&binding, &source, &callback, &ids).unwrap(),
        ),
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: binding.binding_id.clone(),
            source_collection: source.source_collection.clone(),
            source_doc_id: "source".into(),
            source_version: None,
        },
        idempotency_key: "invoke-check".into(),
        caused_by_correlation: None,
        lifecycle_state: LIFECYCLE_PENDING.into(),
        attempts: Some(0),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let pending = super::documents::create_pending_invocation(&backlog.node, &access, &pending)
        .await
        .unwrap();
    let agent_rows = backlog.access.execute("{Agent {_docID}}").await.unwrap();
    let agent_doc = agent_rows["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let expected = |name: &str| {
        contract
            .unattended_agent_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["allowed"]
            .as_bool()
            .unwrap()
    };
    backlog
        .access
        .write(
            "test.callback.disable_agent",
            &format!(
                "mutation {{update_Agent(docID:\"{}\",input:{{enabled:false}}){{_docID}}}}",
                crate::store::graphql::escape_graphql_string(agent_doc)
            ),
        )
        .await
        .unwrap();
    let disabled = super::claim::claim_invocation(
        &backlog.node,
        &access,
        BACKLOG_OWNER,
        &pending,
        &callback,
        None,
        None,
    )
    .await;
    assert_eq!(
        disabled.is_ok_and(|row| row.is_some()),
        expected("disabled")
    );
    backlog
        .access
        .write(
            "test.callback.enable_agent",
            &format!(
                "mutation {{update_Agent(docID:\"{}\",input:{{enabled:true}}){{_docID}}}}",
                crate::store::graphql::escape_graphql_string(agent_doc)
            ),
        )
        .await
        .unwrap();
    backlog
        .access
        .add_actor_relationship(
            "Agent",
            agent_doc,
            "reader",
            &identity::Did::new(super::test_agent_did()).unwrap(),
        )
        .await
        .unwrap();
    backlog
        .access
        .delete_actor_relationship(
            "Agent",
            agent_doc,
            "executor",
            &identity::Did::new(super::test_agent_did()).unwrap(),
        )
        .await
        .unwrap();
    let denied = super::claim::claim_invocation(
        &backlog.node,
        &access,
        BACKLOG_OWNER,
        &pending,
        &callback,
        None,
        None,
    )
    .await;
    assert_eq!(
        denied.is_ok_and(|row| row.is_some()),
        expected("reader_only")
    );
    backlog
        .access
        .add_actor_relationship(
            "Agent",
            agent_doc,
            "executor",
            &identity::Did::new(super::test_agent_did()).unwrap(),
        )
        .await
        .unwrap();
    let claimed = super::claim::claim_invocation(
        &backlog.node,
        &access,
        BACKLOG_OWNER,
        &pending,
        &callback,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(expected("allowed"));
    backlog
        .access
        .delete_actor_relationship(
            "Agent",
            agent_doc,
            "executor",
            &identity::Did::new(super::test_agent_did()).unwrap(),
        )
        .await
        .unwrap();
    backlog
        .access
        .write(
            "test.callback.disable_after_claim",
            &format!(
                "mutation {{update_Agent(docID:\"{}\",input:{{enabled:false}}){{_docID}}}}",
                crate::store::graphql::escape_graphql_string(agent_doc)
            ),
        )
        .await
        .unwrap();
    let recovered = super::claim::claim_invocation(
        &backlog.node,
        &access,
        BACKLOG_OWNER,
        &claimed,
        &callback,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(recovered.is_some());
    backlog.node.shutdown().await.unwrap();
}

#[test]
fn callback_generation_covers_modeled_input_configuration() {
    let case = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let binding: crate::documents::CallbackBinding = serde_json::from_value(json!({
        "node_did": "node", "binding_id": "binding", "agent_did": "agent",
        "event_source_id": "source", "callback_id": "callback", "input_fields": ["name"]
    }))
    .unwrap();
    let source: crate::documents::EventSource = serde_json::from_value(json!({
        "node_did": "node", "event_source_id": "source", "source_collection": "WorkUnit"
    }))
    .unwrap();
    let callback: crate::documents::Callback = serde_json::from_value(json!({
        "node_did": "node", "callback_id": "callback",
        "handler": {"kind": "built_in", "emitter": "create_workspace"}
    }))
    .unwrap();
    let ids = super::documents::CallbackConfigDocIds {
        binding: "binding-doc".into(),
        source: "source-doc".into(),
        callback: "callback-doc".into(),
    };
    let observed =
        super::documents::configuration_generation(&binding, &source, &callback, &ids).unwrap();
    for observation in &case.callback_generation_cases {
        let mut changed_binding = binding.clone();
        let mut changed_source = source.clone();
        let mut changed_callback = callback.clone();
        let mut changed_ids = ids.clone();
        match observation["name"].as_str().unwrap() {
            "source_filter" => {
                changed_source.filter = Some("{ name: { _eq: \"changed\" } }".into())
            }
            "binding_input" => changed_binding.input_fields.push("status".into()),
            "callback_handler" => {
                changed_callback.handler = crate::documents::CallbackHandler::Module {
                    module_id: "changed".into(),
                    workspace_agents: None,
                }
            }
            "binding_recreated" => changed_ids.binding = "new-binding".into(),
            "source_recreated" => changed_ids.source = "new-source".into(),
            "callback_recreated" => changed_ids.callback = "new-callback".into(),
            _ => {}
        }
        let current = super::documents::configuration_generation(
            &changed_binding,
            &changed_source,
            &changed_callback,
            &changed_ids,
        )
        .unwrap();
        let actual = observation["source_visible"] == true
            && observation["observed"].is_string()
            && observed == current;
        assert_eq!(
            actual,
            observation["allowed"].as_bool().unwrap(),
            "{}",
            observation["name"]
        );
    }
}

#[tokio::test]
async fn callback_repository_placement_reads_with_selected_agent() -> anyhow::Result<()> {
    use crate::store::identity::NodeIdentity;
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let modeled_user = &contract
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let modeled_agent = &contract
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let modeled_sibling = contract
        .callback_placement_cases
        .iter()
        .find(|case| case["revoked"] == false && case["expected"] == false)
        .unwrap()["actor"]
        .as_str()
        .unwrap();
    let directory = tempfile::tempdir()?;
    let root = crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let binding = crate::store::identity::provision_agent_identity(&root)?;
    let other_binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding)?;
    let other = crate::store::identity::resolve_agent_identity(&root, &other_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    let owner = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    for executor in [agent.did(), other.did()] {
        crate::store::identity::GrantSet::AgentBaseline
            .grant(&owner, &identity::Did::new(executor)?)
            .await?;
    }
    let selected = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let sibling = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(other.did())?,
        other.as_ref(),
    )?;
    let context = defra_core::RequestContext::default().with_identity(Some(root.did().to_owned()));
    node.add_dac_policy_with_counter_as(&context, &contract.policy_yaml, 0)
        .await?;
    let directive = format!(
        "type RepositoryPlacement @policy(id: \"{}\", resource: \"configuration\")",
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
    );
    let schema = gents_protocol::schemas::REPOSITORY_PLACEMENT.replacen(
        "type RepositoryPlacement",
        &directive,
        1,
    );
    owner.add_collection(&schema).await?;
    let response = owner.write("test.callback.placement", &format!(
        "mutation {{create_RepositoryPlacement(input:{{node_did:\"{}\",repository_id:\"repo\",host_path:\"/tmp/repo\",enabled:true}}){{_docID}}}}",
        crate::store::graphql::escape_graphql_string(root.did()))).await?;
    let doc = crate::store::graphql::created_doc_id(&response, "RepositoryPlacement")?;
    owner
        .add_actor_relationship(
            "RepositoryPlacement",
            &doc,
            "reader",
            &identity::Did::new(agent.did())?,
        )
        .await?;
    for case in &contract.callback_placement_cases {
        if case["revoked"] == true {
            owner
                .delete_actor_relationship(
                    "RepositoryPlacement",
                    &doc,
                    "reader",
                    &identity::Did::new(agent.did())?,
                )
                .await?;
        }
        let modeled_actor = case["actor"].as_str().unwrap();
        let visible = if modeled_actor == modeled_user {
            assert!(super::documents::load_repository_placement(
                &owner,
                "repo",
                root.did(),
                agent.did(),
            )
            .await
            .is_err());
            let owner_did = root.did();
            owner
                .transact_readonly("test.callback.owner_placement", |txn| {
                    Box::pin(async move {
                        crate::store::config_client::read_desired_state_document_in_txn(
                            txn,
                            crate::Collection::RepositoryPlacement,
                            owner_did,
                            "repo",
                        )
                        .await
                    })
                })
                .await?
                .is_some()
        } else if modeled_actor == modeled_agent {
            super::documents::load_repository_placement(&selected, "repo", root.did(), agent.did())
                .await?
                .is_some()
        } else {
            assert_eq!(modeled_actor, modeled_sibling);
            super::documents::load_repository_placement(&sibling, "repo", root.did(), other.did())
                .await?
                .is_some()
        };
        assert_eq!(visible, case["expected"].as_bool().unwrap());
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn callback_children_publish_atomically_and_duplicate_does_not_regrant() -> anyhow::Result<()>
{
    use crate::store::identity::NodeIdentity;
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let expected = |name: &str, field: &str| {
        contract
            .callback_child_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()[field]
            .as_bool()
            .unwrap()
    };
    let directory = tempfile::tempdir()?;
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let node = test_node().await;
    let owner = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    super::seed_test_agent(&node, owner).await;
    super::grant_test_node_administrator(&node, user.did()).await;
    let agent = super::test_agent_access(&node);
    let reader = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(user.did())?);
    let agent_did = super::test_agent_did();
    let parents = agent.execute("{Agent {_docID}}").await?;
    let parent = parents["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let invocation = CallbackInvocationDoc {
        invocation_id: "atomic-pending".into(),
        owner_node_did: owner.into(),
        agent_did: agent_did.clone(),
        callback_id: "callback".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: "binding".into(),
            source_collection: "Work".into(),
            source_doc_id: "doc".into(),
            source_version: None,
        },
        idempotency_key: "atomic-pending".into(),
        caused_by_correlation: None,
        lifecycle_state: LIFECYCLE_PENDING.into(),
        attempts: Some(0),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    super::documents::create_pending_invocation(&node, &agent, &invocation).await?;
    let pending_query = "{CallbackInvocation(filter:{invocation_id:{_eq:\"atomic-pending\"}}){_docID parent_linked}}";
    let pending = agent.execute(pending_query).await?;
    let pending_doc = pending["data"]["CallbackInvocation"][0]["_docID"]
        .as_str()
        .unwrap();
    assert_eq!(
        pending["data"]["CallbackInvocation"][0]["parent_linked"],
        true
    );
    assert_eq!(
        !reader.execute(pending_query).await?["data"]["CallbackInvocation"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected("commit", "user_read")
    );
    agent
        .delete_resource_relationship(
            "CallbackInvocation",
            pending_doc,
            "parent",
            "agents",
            parent,
        )
        .await?;
    super::documents::create_pending_invocation(&node, &agent, &invocation).await?;
    assert_eq!(
        !reader.execute(pending_query).await?["data"]["CallbackInvocation"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected("duplicate", "user_read")
    );
    let result = CallbackResultDoc {
        result_id: "atomic-result".into(),
        invocation_id: "atomic-invocation".into(),
        binding_id: None,
        owner_node_did: owner.into(),
        agent_did: agent_did.clone(),
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: None,
        created_at: None,
    };
    super::documents::create_callback_result(&node, &agent, &result).await?;
    let query = "{CallbackResult(filter:{invocation_id:{_eq:\"atomic-invocation\"}}){_docID parent_linked}}";
    let rows = agent.execute(query).await?;
    let doc = rows["data"]["CallbackResult"][0]["_docID"]
        .as_str()
        .unwrap();
    assert_eq!(rows["data"]["CallbackResult"][0]["parent_linked"], true);
    assert_eq!(
        !reader.execute(query).await?["data"]["CallbackResult"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected("commit", "user_read")
    );
    agent
        .delete_resource_relationship("CallbackResult", doc, "parent", "agents", parent)
        .await?;
    super::documents::create_callback_result(&node, &agent, &result).await?;
    assert_eq!(
        !reader.execute(query).await?["data"]["CallbackResult"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected("duplicate", "user_read")
    );
    assert_eq!(
        !agent.execute(query).await?["data"]["CallbackResult"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected("duplicate", "agent_read")
    );

    let mut aborted = result.clone();
    aborted.result_id = "aborted-result".into();
    aborted.invocation_id = "aborted-invocation".into();
    let rolled_back: anyhow::Result<()> = agent
        .transact_write("test.callback.atomic_abort", |txn| {
            let aborted = &aborted;
            Box::pin(async move {
                let parent =
                    super::documents::callback_agent_parent_in_txn(txn, owner, &aborted.agent_did)
                        .await?;
                let response = txn
                    .execute(&create_callback_result_mutation(aborted, &parent, true))
                    .await?;
                super::documents::stage_callback_child_parent(
                    txn,
                    "CallbackResult",
                    &response,
                    &parent,
                )
                .await?;
                anyhow::bail!("abort after child and parent are staged")
            })
        })
        .await;
    assert!(rolled_back.is_err());
    let query = "{CallbackResult(filter:{invocation_id:{_eq:\"aborted-invocation\"}}){_docID}}";
    assert_eq!(
        !agent.execute(query).await?["data"]["CallbackResult"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected("abort", "agent_read")
    );
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn callback_claim_rechecks_modeled_source_visibility_and_generation() {
    let backlog = Backlog::registered().await;
    let access = super::test_agent_access(&backlog.node);
    let binding = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let source =
        super::documents::load_event_source(&backlog.node, &binding.event_source_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let callback =
        super::documents::load_callback(&backlog.node, &binding.callback_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let ids = backlog.config_ids().await;
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let source_rows = backlog
        .access
        .execute("{ EventSource { _docID } }")
        .await
        .unwrap();
    let source_doc = source_rows["data"]["EventSource"][0]["_docID"]
        .as_str()
        .unwrap();
    for name in [
        "same",
        "source_filter",
        "binding_recreated",
        "source_recreated",
        "callback_recreated",
        "missing",
        "source_revoked",
    ] {
        if name == "source_revoked" {
            backlog
                .access
                .delete_actor_relationship(
                    "EventSource",
                    source_doc,
                    "reader",
                    &identity::Did::new(super::test_agent_did()).unwrap(),
                )
                .await
                .unwrap();
        }
        let mut observed_source = source.clone();
        if name == "source_filter" {
            observed_source.filter = Some("{ label: { _eq: \"old\" } }".into());
        }
        let mut observed_ids = ids.clone();
        match name {
            "binding_recreated" => observed_ids.binding = "former-binding".into(),
            "source_recreated" => observed_ids.source = "former-source".into(),
            "callback_recreated" => observed_ids.callback = "former-callback".into(),
            _ => {}
        }
        let generation = (name != "missing").then(|| {
            super::documents::configuration_generation(
                &binding,
                &observed_source,
                &callback,
                &observed_ids,
            )
            .unwrap()
        });
        let invocation = CallbackInvocationDoc {
            invocation_id: format!("generation-{name}"),
            owner_node_did: BACKLOG_OWNER.into(),
            agent_did: super::test_agent_did(),
            callback_id: binding.callback_id.clone(),
            configuration_generation: generation,
            claimed_callback: None,
            input: json!({}),
            origin: crate::documents::CallbackInvocationOrigin::Event {
                binding_id: binding.binding_id.clone(),
                source_collection: source.source_collection.clone(),
                source_doc_id: format!("generation-{name}"),
                source_version: None,
            },
            idempotency_key: format!("generation-{name}"),
            caused_by_correlation: None,
            lifecycle_state: LIFECYCLE_PENDING.into(),
            attempts: Some(0),
            action_plan: None,
            action_journal: Some("[]".into()),
            error: None,
            claimed_at: None,
            created_at: None,
        };
        let pending =
            super::documents::create_pending_invocation(&backlog.node, &access, &invocation)
                .await
                .unwrap();
        let actual = super::claim::claim_invocation(
            &backlog.node,
            &access,
            BACKLOG_OWNER,
            &pending,
            &callback,
            None,
            None,
        )
        .await
        .is_ok_and(|claim| claim.is_some());
        let expected = contract
            .callback_generation_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["allowed"]
            .as_bool()
            .unwrap();
        assert_eq!(actual, expected, "{name}");
    }
    backlog.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn callback_module_plan_commits_only_with_modeled_native_authority() {
    use crate::documents::apply_desired_state_plan;
    use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let backlog = Backlog::registered().await;
    let access = super::test_agent_access(&backlog.node);
    let make = |collection, value: serde_json::Value| DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    };
    let plan = DesiredStateApplyPlan::new(vec![
        make(
            crate::Collection::CallbackModule,
            json!({"node_did":BACKLOG_OWNER,"module_id":"module"}),
        ),
        make(
            crate::Collection::Callback,
            json!({"node_did":BACKLOG_OWNER,"callback_id":"cb-backlog", "display_name":"admitted",
            "handler":{"kind":"module","module_id":"module"}}),
        ),
    ])
    .unwrap();
    backlog
        .access
        .transact_write("test.callback.module_config", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
    let binding = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let source =
        super::documents::load_event_source(&backlog.node, &binding.event_source_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let callback =
        super::documents::load_callback(&backlog.node, &binding.callback_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let module = super::documents::load_callback_module(&backlog.node, "module", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let ids = backlog.config_ids().await;
    let module_row = backlog
        .access
        .execute("{CallbackModule {_docID}}")
        .await
        .unwrap();
    let module_doc = module_row["data"]["CallbackModule"][0]["_docID"]
        .as_str()
        .unwrap();
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let mut admitted = None;
    for name in ["allowed", "changed", "recreated", "unprepared", "revoked"] {
        if name == "revoked" {
            backlog
                .access
                .delete_actor_relationship(
                    "CallbackModule",
                    module_doc,
                    "reader",
                    &identity::Did::new(super::test_agent_did()).unwrap(),
                )
                .await
                .unwrap();
        }
        let invocation = CallbackInvocationDoc {
            invocation_id: format!("module-{name}"),
            owner_node_did: BACKLOG_OWNER.into(),
            agent_did: super::test_agent_did(),
            callback_id: binding.callback_id.clone(),
            configuration_generation: Some(
                super::documents::configuration_generation(&binding, &source, &callback, &ids)
                    .unwrap(),
            ),
            claimed_callback: None,
            input: json!({}),
            origin: crate::documents::CallbackInvocationOrigin::Event {
                binding_id: binding.binding_id.clone(),
                source_collection: source.source_collection.clone(),
                source_doc_id: format!("module-{name}"),
                source_version: None,
            },
            idempotency_key: format!("module-{name}"),
            caused_by_correlation: None,
            lifecycle_state: LIFECYCLE_PENDING.into(),
            attempts: Some(0),
            action_plan: None,
            action_journal: Some("[]".into()),
            error: None,
            claimed_at: None,
            created_at: None,
        };
        let pending =
            super::documents::create_pending_invocation(&backlog.node, &access, &invocation)
                .await
                .unwrap();
        let mut prepared = module.clone();
        if name == "changed" {
            prepared.provenance = Some("changed".into());
        }
        let candidate_plan = (name != "unprepared").then_some("[]");
        let claim = super::claim::claim_invocation(
            &backlog.node,
            &access,
            BACKLOG_OWNER,
            &pending,
            &callback,
            Some((
                if name == "recreated" {
                    "former-module"
                } else {
                    module_doc
                },
                &prepared,
            )),
            candidate_plan,
        )
        .await;
        let expected = contract
            .callback_module_claim_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["allowed"]
            .as_bool()
            .unwrap();
        assert_eq!(
            claim.as_ref().is_ok_and(|value| value.is_some()),
            expected,
            "{name}"
        );
        if name == "allowed" {
            admitted = claim.unwrap();
        }
    }
    let admitted = admitted.unwrap();
    assert_eq!(admitted.action_plan.as_deref(), Some("[]"));
    let recovered = super::claim::claim_invocation(
        &backlog.node,
        &access,
        BACKLOG_OWNER,
        &admitted,
        &callback,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.action_plan, admitted.action_plan);
    let mut changed = callback.clone();
    changed.display_name = Some("changed".into());
    let plan = DesiredStateApplyPlan::new(vec![make(
        crate::Collection::Callback,
        serde_json::to_value(&changed).unwrap(),
    )])
    .unwrap();
    backlog
        .access
        .transact_write("test.callback.changed_config", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
    for case in &contract.callback_recovery_configuration_cases {
        let mut row = admitted.clone();
        if case["claimed"] == false {
            row.lifecycle_state = LIFECYCLE_PENDING.into();
        }
        let selected = super::run::callback_for_recovery(&backlog.node, &row)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.display_name.as_deref(), case["expected"].as_str());
    }
    backlog.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn callback_cursor_waits_for_a_successful_claim() {
    async fn checkpoint(access: &crate::ConfigAccess, through: &str) -> bool {
        access
            .transact_write("test.callback_claim_checkpoint", |txn| {
                Box::pin(async move {
                    crate::documents::event_consumer::checkpoint_prefix(
                        txn,
                        BACKLOG_OWNER,
                        &gents_protocol::event_delivery::EventConsumer::CallbackBinding {
                            binding_id: "bind-backlog".into(),
                        },
                        "BacklogWork",
                        through,
                        false,
                    )
                    .await
                })
            })
            .await
            .unwrap()
    }

    let cases = &crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .callback_checkpoint_claim_cases;
    let expected = |claimed: bool| {
        cases
            .iter()
            .find(|case| case["claim_time"] == claimed && case["retained_configuration"] == claimed)
            .unwrap()["expected"]
            .as_bool()
            .unwrap()
    };
    let backlog = Backlog::registered().await;
    let agent = super::test_agent_access(&backlog.node);
    let seeded = backlog.cursor().await;
    let doc_id = backlog.create("pending-claim").await;
    let arrival = agent
        .execute(&format!(
            "{{ _documentArrivals(collection: \"BacklogWork\", after: \"{}\", limit: 128) {{ head }} }}",
            crate::store::graphql::escape_graphql_string(&seeded)
        ))
        .await
        .unwrap();
    let through = arrival["data"]["_documentArrivals"]["head"]
        .as_str()
        .unwrap();
    let binding = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    let source =
        super::documents::load_event_source(&backlog.node, &binding.event_source_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let callback =
        super::documents::load_callback(&backlog.node, &binding.callback_id, BACKLOG_OWNER)
            .await
            .unwrap()
            .unwrap();
    let generation = super::documents::configuration_generation(
        &binding,
        &source,
        &callback,
        &backlog.config_ids().await,
    )
    .unwrap();
    let invocation = CallbackInvocationDoc {
        invocation_id: uuid::Uuid::new_v4().to_string(),
        owner_node_did: BACKLOG_OWNER.into(),
        agent_did: super::test_agent_did(),
        callback_id: binding.callback_id.clone(),
        configuration_generation: Some(generation),
        claimed_callback: None,
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: binding.binding_id.clone(),
            source_collection: "BacklogWork".into(),
            source_doc_id: doc_id.clone(),
            source_version: None,
        },
        input: json!({}),
        idempotency_key: gents_protocol::trigger_delivery::idempotency_key(
            &binding.binding_id,
            "BacklogWork",
            &doc_id,
        ),
        caused_by_correlation: None,
        lifecycle_state: LIFECYCLE_PENDING.into(),
        attempts: Some(0),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    let invocation =
        super::documents::create_pending_invocation(&backlog.node, &agent, &invocation)
            .await
            .unwrap();
    assert_eq!(checkpoint(&agent, through).await, expected(false));
    assert_eq!(backlog.cursor().await, seeded);
    assert!(super::claim::claim_invocation(
        &backlog.node,
        &agent,
        BACKLOG_OWNER,
        &invocation,
        &callback,
        None,
        None
    )
    .await
    .unwrap()
    .is_some());
    assert_eq!(checkpoint(&agent, through).await, expected(true));
    assert_eq!(backlog.cursor().await, through);
    backlog.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn document_written_before_engine_start_fires_once_across_restarts() {
    let backlog = Backlog::registered().await;
    let pending = backlog.create("pending").await;
    backlog.start_engine().await;
    assert_eq!(backlog.invoked().await, vec![pending.clone()]);
    backlog.start_engine().await;
    assert_eq!(backlog.invoked().await, vec![pending]);
    assert!(!backlog.invoked().await.contains(&backlog.history));
    backlog.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn one_delivery_pass_drains_a_backlog_beyond_one_journal_page() {
    let backlog = Backlog::registered().await;
    let mut written = Vec::new();
    for index in 0..130 {
        written.push(backlog.create(&format!("work-{index}")).await);
    }
    written.sort();
    backlog.start_engine().await;
    assert_eq!(backlog.invoked().await, written);
    backlog.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn edited_document_admitted_before_a_crash_is_not_readmitted() {
    let backlog = Backlog::registered().await;
    let pending = backlog.create("pending").await;
    let seeded = backlog.cursor().await;
    let mut crashed = backlog.engine();
    crashed.reconcile_bindings().await;
    let binding = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    assert!(crashed
        .materialize_for_binding(&binding, "BacklogWork", &pending, false)
        .await
        .unwrap());
    drop(crashed);
    assert_eq!(
        backlog.cursor().await,
        seeded,
        "the crash precedes the checkpoint"
    );
    backlog
        .access
        .write(
            "test.backlog_edit",
            &format!(
                "mutation {{ update_BacklogWork(docID: \"{}\", input: {{label: \"edited\"}}) {{ _docID }} }}",
                crate::store::graphql::escape_graphql_string(&pending)
            ),
        )
        .await
        .unwrap();
    backlog.start_engine().await;
    assert_eq!(backlog.invoked().await, vec![pending]);
    assert_ne!(
        backlog.cursor().await,
        seeded,
        "the recovered arrival is checkpointed"
    );
    backlog.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn admission_refuses_a_binding_disabled_after_its_snapshot() {
    let backlog = Backlog::registered().await;
    let pending = backlog.create("pending").await;
    let snapshot = super::documents::load_binding(&backlog.node, "bind-backlog", BACKLOG_OWNER)
        .await
        .unwrap()
        .unwrap();
    backlog
        .access
        .write(
            "test.backlog_disable",
            r#"mutation { update_CallbackBinding(filter: {binding_id: {_eq: "bind-backlog"}}, input: {enabled: false}) { _docID } }"#,
        )
        .await
        .unwrap();
    let mut engine = backlog.engine();
    assert!(!engine
        .materialize_for_binding(&snapshot, "BacklogWork", &pending, false)
        .await
        .unwrap());
    assert!(backlog.invoked().await.is_empty());
    backlog.node.shutdown().await.expect("node shutdown");
}

#[test]
fn callback_artifact_identity_and_output_are_host_validated() {
    let artifact = wat_returns_json(r#"{"abi":1,"actions":[]}"#);
    let mut module = module_doc(&artifact, &json!({}), "did:key:zTrusted");
    validate_callback_module(&module, &trusted("did:key:zTrusted")).unwrap();
    module.canonical_args = Some(r#"{"forged":true}"#.into());
    assert!(validate_callback_module(&module, &trusted("did:key:zTrusted")).is_err());
    module = module_doc(&artifact, &json!({}), "did:key:zTrusted");
    module.abi_version = Some(99);
    assert!(validate_callback_module(&module, &trusted("did:key:zTrusted")).is_err());

    let trap = crate::plugin::tests::build_plugin_afb(
        r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#,
    );
    let module = module_doc(&trap, &json!({}), "did:key:zTrusted");
    assert!(plan_from_module(&module, &json!({}), &BTreeSet::new()).is_err());
    let malformed = wat_returns_json("not JSON");
    let module = module_doc(&malformed, &json!({}), "did:key:zTrusted");
    assert!(plan_from_module(&module, &json!({}), &BTreeSet::new()).is_err());
}

#[tokio::test]
async fn retired_reserved_bindings_do_not_scan() {
    let backlog = Backlog::registered().await;
    let reserved = format!("graph-trigger-{}-{}", "a".repeat(64), "b".repeat(16));
    backlog.add_binding(&reserved).await;
    backlog.add_binding("ordinary").await;
    let ids: Vec<String> = super::documents::list_enabled_bindings(&backlog.node, BACKLOG_OWNER)
        .await
        .expect("bindings")
        .into_iter()
        .map(|binding| binding.binding_id)
        .collect();
    assert_eq!(ids, vec!["bind-backlog".to_owned(), "ordinary".to_owned()]);
}

#[tokio::test]
async fn callback_result_never_falls_back_when_agent_key_is_unavailable() {
    use identity::Identity as _;
    let node = test_node().await;
    let keypair = crypto::ed25519_key_from_seed(&[93_u8; 32][..]).unwrap();
    let unavailable =
        identity::RawIdentity::from_bytes(crypto::KeyType::Ed25519, keypair.as_slice()).unwrap();
    let result = CallbackResultDoc {
        result_id: "unavailable-agent-result".into(),
        invocation_id: "unavailable-agent-invocation".into(),
        binding_id: None,
        owner_node_did: "did:key:runtime".into(),
        agent_did: unavailable.did().unwrap().to_string(),
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: None,
        created_at: None,
    };
    let access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(&result.agent_did).unwrap());
    assert!(
        super::documents::create_callback_result(&node, &access, &result)
            .await
            .is_err()
    );
    let response = node.execute("{ CallbackResult { result_id } }").await;
    assert!(
        crate::store::graphql::rows::<serde_json::Value>(&response, "CallbackResult")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn callback_parent_requires_the_selected_authenticated_agent() {
    use crate::store::identity::{KeyIdentity, NodeIdentity};

    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let node = test_node().await;
    let owner = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    super::seed_test_agent(&node, owner).await;
    super::grant_test_node_administrator(&node, user.did()).await;
    let agent = super::test_agent_did();
    let foreign = identity::Did::new(user.did()).unwrap();
    let foreign_access =
        crate::store::config_client::ConfigAccess::local_as(node.clone(), foreign.clone());
    let visible = foreign_access
        .execute("{ Agent { _docID } }")
        .await
        .unwrap();
    assert_eq!(visible["data"]["Agent"].as_array().unwrap().len(), 1);

    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_callback_parent_actor_cases;
    assert_eq!(cases.len(), 3);
    for (index, case) in cases.iter().enumerate() {
        let actor = match case["actor"].as_str() {
            Some("selected") => Some(identity::Did::new(&agent).unwrap()),
            Some("foreign") => Some(foreign.clone()),
            None => None,
            Some(other) => panic!("unknown modeled actor {other}"),
        };
        assert_eq!(case["selected"], "selected");
        let result = CallbackResultDoc {
            result_id: format!("actor-case-{index}"),
            invocation_id: format!("actor-invocation-{index}"),
            binding_id: None,
            owner_node_did: owner.into(),
            agent_did: agent.clone(),
            workspace_id: None,
            work_unit_id: None,
            caused_by_correlation: None,
            created_at: None,
        };
        let selected_access = match actor {
            Some(actor) if actor.as_str() == agent => super::test_agent_access(&node),
            Some(actor) => crate::ConfigAccess::local_as(node.clone(), actor),
            None => crate::ConfigAccess::local(node.clone()),
        };
        let written = selected_access
            .transact_write("test.callback_parent_actor", |txn| {
                let result = &result;
                Box::pin(async move {
                    let parent = super::documents::callback_agent_parent_in_txn(
                        txn,
                        &result.owner_node_did,
                        &result.agent_did,
                    )
                    .await?;
                    txn.execute(&create_callback_result_mutation(result, &parent, false))
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        assert_eq!(
            written.is_ok(),
            case["allowed"].as_bool().unwrap(),
            "{case}"
        );
    }
    let results = super::test_agent_access(&node)
        .execute("{ CallbackResult { result_id } }")
        .await
        .unwrap();
    assert_eq!(
        results["data"]["CallbackResult"].as_array().unwrap().len(),
        1
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn callback_parent_recovery_matches_modeled_interrupted_grant_and_revocation() {
    use crate::store::identity::{KeyIdentity, NodeIdentity};
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let node = test_node().await;
    let owner = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    super::seed_test_agent(&node, owner).await;
    super::grant_test_node_administrator(&node, user.did()).await;
    let agent = super::test_agent_did();
    let access = super::test_agent_access(&node);
    let user_access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(user.did()).unwrap(),
    );
    let parents = access.execute("{ Agent { _docID } }").await.unwrap();
    let parent = parents["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_parent_publication_cases;
    let mut consumed = 0;
    for case in cases.iter().filter(|case| {
        matches!(
            case.name.as_str(),
            "marker_failure_then_recovery" | "revoked_after_completion"
        )
    }) {
        consumed += 1;
        let invocation = format!("modeled-parent-{}", case.name);
        let result = CallbackResultDoc {
            result_id: format!("result-{invocation}"),
            invocation_id: invocation.clone(),
            binding_id: None,
            owner_node_did: owner.into(),
            agent_did: agent.clone(),
            workspace_id: None,
            work_unit_id: None,
            caused_by_correlation: None,
            created_at: None,
        };
        let written = access
            .write(
                "test.modeled_callback_parent",
                &create_callback_result_mutation(&result, parent, false),
            )
            .await
            .unwrap();
        let doc = crate::store::graphql::created_doc_id(&written, "CallbackResult").unwrap();
        let query = format!(
            r#"{{ CallbackResult(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID parent_linked}} }}"#,
            crate::store::graphql::escape_graphql_string(&doc)
        );
        assert!(case.initial.committed);
        assert!(!case.initial.parent_granted);
        assert_eq!(
            access.execute(&query).await.unwrap()["data"]["CallbackResult"][0]["parent_linked"],
            case.initial.parent_linked
        );
        for step in &case.steps {
            assert_eq!(step.native_owner.as_deref(), Some("agent"));
            assert_eq!(step.claimed_creator.as_deref(), Some("agent"));
            assert_eq!(step.authenticated_creator.as_deref(), Some("agent"));
            if step.grant_ok && !step.mark_ok {
                // The grant has committed when the process stops before its marker write.
                access
                    .add_resource_relationship("CallbackResult", &doc, "parent", "agents", parent)
                    .await
                    .unwrap();
            } else if step.mark_ok {
                super::documents::ensure_callback_parent(
                    &node,
                    &access,
                    "CallbackResult",
                    &invocation,
                    owner,
                    &agent,
                )
                .await
                .unwrap();
            }
            if step.revoke_parent {
                access
                    .delete_resource_relationship(
                        "CallbackResult",
                        &doc,
                        "parent",
                        "agents",
                        parent,
                    )
                    .await
                    .unwrap();
                super::documents::ensure_callback_parent(
                    &node,
                    &access,
                    "CallbackResult",
                    &invocation,
                    owner,
                    &agent,
                )
                .await
                .unwrap();
            }
            let observed = access.execute(&query).await.unwrap();
            assert_eq!(
                observed["data"]["CallbackResult"][0]["parent_linked"], step.after.parent_linked,
                "{}",
                case.name
            );
            let visible = !user_access.execute(&query).await.unwrap()["data"]["CallbackResult"]
                .as_array()
                .unwrap()
                .is_empty();
            assert_eq!(visible, step.after.parent_granted, "{}", case.name);
            assert!(step.after.committed);
        }
    }
    assert_eq!(consumed, 2);
    node.shutdown().await.expect("node shutdown");
}
