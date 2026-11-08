use super::*;
use gents::NodeIdentity;
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
use serde_json::json;

const NOW: &str = "2026-10-21T00:00:00Z";

/// Sets or clears the session's current Goal as the session's Agent writes
/// one: bound to the Agent-owned session document, related to it so the
/// session's readers (the enrolled User) observe it.
async fn set_goal(
    home: &crate::shared::test_support::ClientHome,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session: &str,
    goal: Option<&str>,
) -> Result<()> {
    let node_did = home.node_did().to_owned();
    let agent_did = binding.agent_did.clone();
    let agent = home.agent_access(binding).await;
    let Some(goal) = goal else {
        let current = agent
            .transact_readonly("test.manual_task.current_goal", |txn| {
                let node_did = &node_did;
                Box::pin(gents::goal::load_canonical_goal_in_txn(
                    txn, node_did, session,
                ))
            })
            .await?;
        if let Some(current) = current {
            anyhow::ensure!(gents::goal::delete_goal_from_access(&agent, &current).await?);
        }
        return Ok(());
    };
    agent
        .transact_write("test.manual_task.goal_state", |txn| {
            let (node_did, agent_did) = (&node_did, &agent_did);
            Box::pin(async move {
                let current =
                    gents::goal::load_canonical_goal_in_txn(txn, node_did, session).await?;
                if let Some(current) = current {
                    txn.execute_with_variables(
                        &format!(
                            "mutation($input: GoalMutationInputArg!) {{update_Goal(docID: \"{}\", input: $input) {{_docID}}}}",
                            escape_graphql_string(&current.doc_id)
                        ),
                        &json!({"input": {"goal_id": goal, "updated_at": NOW}}),
                    )
                    .await?;
                    return Ok(());
                }
                let session_doc =
                    gents::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session,
                    )
                    .await?
                    .context("Agent-owned Task session")?
                    .doc_id;
                let created = txn
                    .execute_with_variables(
                        "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                        &json!({"input": {
                            "goal_id": goal, "node_did": node_did, "session_id": session,
                            "session_doc_id": session_doc, "agent_did": agent_did,
                            "requester_did": agent_did, "creator_did": agent_did,
                            "parent_linked": true, "objective": "Task work", "status": "active",
                            "created_at": NOW, "updated_at": NOW,
                        }}),
                    )
                    .await?;
                let doc = gents::store::graphql::created_doc_id(&created, "Goal")?;
                txn.add_resource_relationship("Goal", &doc, "parent", "sessions", &session_doc)
                    .await?;
                txn.add_actor_relationship("Goal", &doc, "working_agent", &agent_did.parse()?)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
}

/// Publishes, as the User, the Agent's Tools with goal creation enabled: the
/// tool source that lets a creator Agent retire the Goals it owns
/// (`GrantSets.GrantSet.permissions`, `Configuration.goal_creator_deletes_its_goal`).
async fn enable_goal_creation(
    home: &crate::shared::test_support::ClientHome,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> Result<()> {
    use gents::documents::{apply_desired_state_plan, AgentContext, BuiltInTools, Tools};
    use gents::store::config_client::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;
    let node_did = home.node_did().to_owned();
    let agent_did = binding.agent_did.clone();
    let tools_id = format!("{agent_did}:tools");
    let context_id = format!("{agent_did}:context");
    let tools = Tools {
        tools_id: tools_id.clone(),
        node_did: node_did.clone(),
        built_ins: Some(BuiltInTools {
            enable_goal_tools: Some(true),
            enable_goal_creation: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let context = AgentContext {
        context_id: context_id.clone(),
        node_did: node_did.clone(),
        display_name: None,
        description: None,
        system_prompt: None,
        tools_id: Some(tools_id),
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    home.access
        .transact_write("test.manual_task.goal_tools", |txn| {
            let (node_did, agent_did, context_id) = (&node_did, &agent_did, &context_id);
            let (tools, context) = (&tools, &context);
            Box::pin(async move {
                let (_, mut agent) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_did)
                        .await?
                        .context("published Agent")?;
                agent["context_id"] = context_id.as_str().into();
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::Tools, serde_json::to_value(tools)?),
                        (Collection::AgentContext, serde_json::to_value(context)?),
                        (Collection::Agent, agent),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
}

/// The Task invocation's request, signed by the enrolled User as
/// `enqueue_task_run` signs it.
async fn signed_request(
    home: &crate::shared::test_support::ClientHome,
    agent_did: &str,
    fire: &TriggerFire,
    input: RequestInput,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let user = &home.user;
    let requester = gents::session::client_requester_scope(
        &home.access,
        home.node_did(),
        &fire.session_id,
        user.did(),
    )
    .await?;
    gents::request::lifecycle::build_signed_client_request(
        &home.access,
        gents::RequestSpec {
            input,
            retry_key: Some(fire.fire_key.clone()),
            trigger_lineage: gents::request::lifecycle::TriggerLineage {
                trigger_kind: Some("manual".into()),
                ..Default::default()
            },
            ..gents::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents::RequestIdentity {
                    request_id: fire.request_id.clone(),
                    node_did: home.node_did().into(),
                    requester_did: Some(requester),
                    agent_did: agent_did.into(),
                    session_id: fire.session_id.clone(),
                    content: "Task work".into(),
                    execution_origin: gents::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: NOW.into(),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
            )
        },
        gents::RequestSigner::Identity(user),
    )
    .await
}

#[tokio::test]
async fn generated_manual_task_identity_is_frozen_before_signed_admission() -> Result<()> {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot()?;
    let cases = snapshot["goal_first_request_binding_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 14);
    let home = crate::shared::test_support::ClientHome::new().await;
    let binding = home.agent("Engineer").await;
    enable_goal_creation(&home, &binding).await?;
    let access = &home.access;
    let owner = home.node_did().to_owned();
    for case in cases {
        let session = format!("manual-task:{}", case["name"].as_str().unwrap());
        home.agent_session(&binding, &session).await;
        let initial = gents_protocol::goal::deterministic_goal_id(&owner, &session);
        let goal_id = |value: &str| {
            if value == "initial-goal" {
                initial.clone()
            } else {
                value.to_owned()
            }
        };
        set_goal(
            &home,
            &binding,
            &session,
            case["current_task_goal"].as_str().map(goal_id).as_deref(),
        )
        .await?;
        let selected = prepare_task_run_goal_id(access, &owner, &session, true, None).await?;
        assert_eq!(
            selected,
            Some(goal_id(case["expected_task_goal"].as_str().unwrap())),
            "{case}"
        );
        set_goal(
            &home,
            &binding,
            &session,
            case["observed_task_goal"].as_str().map(goal_id).as_deref(),
        )
        .await?;
        let identity = FireIdentity {
            owner_did: owner.clone(),
            trigger_id: format!("manual:task:{}", case["name"].as_str().unwrap()),
            source_collection: "Task".into(),
            source_doc_id: "task-document".into(),
        };
        let mut fire = TriggerFire {
            fire_key: identity.fire_key(),
            request_id: identity.request_id(),
            identity,
            task_id: "task".into(),
            session_id: session.clone(),
            goal_id: selected.clone(),
            goal_objective: Some("Task work".into()),
            goal_token_budget: None,
            goal_assignment_applied: false,
            emit_outcome: false,
            queued_serial: false,
            source_handoff_id: None,
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: NOW.into(),
        };
        let input = crate::request_helpers::goal_input_tests::authored_input();
        let request = signed_request(&home, &binding.agent_did, &fire, input.clone()).await?;
        assert_eq!(request.input, input);
        let admitted =
            gents::request::lifecycle::write_task_delivery(access, &fire, true, &request).await;
        assert_eq!(
            admitted.is_ok(),
            case["task_binding_valid"] == true,
            "{case}: {:?}",
            admitted.as_ref().err()
        );
        let row = lookup_request_by_retry_key(access, &owner, &fire.fire_key).await?;
        if let Err(error) = admitted {
            assert!(
                format!("{error:#}").contains("Task Goal assignment changed"),
                "{error:#}"
            );
            assert!(row.is_none(), "{case}");
            let persisted = access
                .execute(&format!(
                    "{{TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}) {{goal_id}}}}",
                    escape_graphql_string(&fire.fire_key),
                ))
                .await?;
            assert!(
                persisted["data"]["TriggerFire"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{case}"
            );
        } else {
            let row = row.context("admitted signed request is missing")?;
            assert_eq!(row.input, Some(input.clone()), "{case}");
            if case["name"] == "task-after-client-replacement" {
                set_goal(&home, &binding, &session, None).await?;
                let retained = prepare_task_run_input(
                    access,
                    &owner,
                    &session,
                    true,
                    true,
                    Some(&row),
                    RequestInput::default(),
                )
                .await?;
                assert_eq!(retained, input);
                fire.goal_id =
                    prepare_task_run_goal_id(access, &owner, &session, true, Some(&row)).await?;
                assert_eq!(fire.goal_id, None);
                let retry = signed_request(&home, &binding.agent_did, &fire, retained).await?;
                assert_eq!(retry.admission.signature, request.admission.signature);
                let replay =
                    gents::request::lifecycle::write_task_delivery(access, &fire, true, &retry)
                        .await?;
                assert!(replay.duplicate);
                assert_eq!(Some(replay.request.doc_id), row.doc_id);
                let persisted = access
                    .execute(&format!(
                        "{{TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}) {{goal_id}}}}",
                        escape_graphql_string(&fire.fire_key),
                    ))
                    .await?;
                assert_eq!(
                    persisted["data"]["TriggerFire"][0]["goal_id"],
                    json!(selected)
                );
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn manual_task_replay_keeps_signed_goal_identity_without_observing_current_goal() -> Result<()>
{
    let source = crate::request_helpers::goal_input_tests::fixture(true, false, None, None).await?;
    let access = ConfigAccess::Graphql(source.graphql.clone());
    for reference in [
        None,
        Some(GoalAssignmentInput {
            goal_id: "retained-assignment".into(),
            root_request_doc_id: None,
        }),
    ] {
        let mut input = crate::request_helpers::goal_input_tests::authored_input();
        input.goal_assignment = reference.clone();
        let row = super::tests::signed_task_receipt(&source, input).await?;
        let receipt: AgentRequestRow = serde_json::from_value(row)?;
        let selected =
            prepare_task_run_goal_id(&access, &source.node_did, "chat", true, Some(&receipt))
                .await?;
        assert_eq!(selected, reference.map(|reference| reference.goal_id));
    }
    assert_eq!(source.exchanges(), 0);
    Ok(())
}
