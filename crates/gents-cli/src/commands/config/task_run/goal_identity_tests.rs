use super::*;
use gents::NodeIdentity;
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
use serde_json::json;
use std::sync::Arc;

const NOW: &str = "2026-10-21T00:00:00Z";

async fn set_goal(
    access: &ConfigAccess,
    owner: &str,
    session: &str,
    goal: Option<&str>,
) -> Result<()> {
    access
        .transact_write("test.manual_task.goal_state", |txn| {
            Box::pin(async move {
                if let Some(goal) = goal {
                    let current = gents::goal::load_canonical_goal_in_txn(txn, owner, session).await?;
                    let mutation = match current {
                        Some(current) => format!(
                            "mutation($input: GoalMutationInputArg!) {{update_Goal(docID: \"{}\", input: $input) {{_docID}}}}",
                            escape_graphql_string(&current.doc_id)
                        ),
                        None => "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}".to_owned(),
                    };
                    txn.execute_with_variables(
                        &mutation,
                        &json!({"input": {
                            "goal_id":goal, "node_did":owner, "session_id":session,
                            "objective":"Task work", "status":"active", "created_at":NOW,
                            "updated_at":NOW
                        }}),
                    ).await?;
                } else {
                    txn.execute(&format!(
                        "mutation {{delete_Goal(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{_docID}}}}",
                        escape_graphql_string(owner), escape_graphql_string(session)
                    )).await?;
                }
                Ok(())
            })
        })
        .await
}

async fn signed_request(
    signer: &gents::KeyIdentity,
    fire: &TriggerFire,
    input: RequestInput,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    gents::build_signed_request(
        gents::RequestSpec {
            input,
            retry_key: Some(fire.fire_key.clone()),
            trigger_lineage: gents::lifecycle::TriggerLineage {
                trigger_kind: Some("manual".into()),
                ..Default::default()
            },
            ..gents::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents::RequestIdentity {
                    request_id: fire.request_id.clone(),
                    node_did: signer.did().into(),
                    requester_did: None,
                    agent_id: "engineer".into(),
                    session_id: fire.session_id.clone(),
                    content: "Task work".into(),
                    execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                    created_at: NOW.into(),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                    signer.did(),
                ),
            )
        },
        gents::RequestSigner::Identity(signer),
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
    let temp = tempfile::tempdir()?;
    let signer = gents::KeyIdentity::load_or_create(temp.path().join("node.key"), None)?;
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::local(node.clone());
    for case in cases {
        let session = format!("manual-task:{}", case["name"].as_str().unwrap());
        let initial = gents::goal::deterministic_goal_id(signer.did(), &session);
        let goal_id = |value: &str| {
            if value == "initial-goal" {
                initial.clone()
            } else {
                value.to_owned()
            }
        };
        set_goal(
            &access,
            signer.did(),
            &session,
            case["current_task_goal"].as_str().map(&goal_id).as_deref(),
        )
        .await?;
        access.transact_write("test.manual_task.session", |txn| {
            let session = &session;
            let signer = &signer;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                    &json!({"input": {"session_id":session, "node_did":signer.did(),
                        "requester_did":signer.did(), "agent_id":"engineer", "created_at":NOW}}),
                ).await?;
                Ok(())
            })
        }).await?;
        let selected =
            prepare_task_run_goal_id(&access, signer.did(), &session, true, None).await?;
        assert_eq!(
            selected,
            Some(goal_id(case["expected_task_goal"].as_str().unwrap())),
            "{case}"
        );
        set_goal(
            &access,
            signer.did(),
            &session,
            case["observed_task_goal"].as_str().map(&goal_id).as_deref(),
        )
        .await?;
        let identity = FireIdentity {
            owner_did: signer.did().into(),
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
        let request = signed_request(&signer, &fire, input.clone()).await?;
        assert_eq!(request.input, input);
        let admitted = gents::lifecycle::write_task_delivery(&access, &fire, true, &request).await;
        assert_eq!(
            admitted.is_ok(),
            case["task_binding_valid"] == true,
            "{case}: {:?}",
            admitted.as_ref().err()
        );
        let row = lookup_request_by_retry_key(&access, signer.did(), &fire.fire_key).await?;
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
                set_goal(&access, signer.did(), &session, None).await?;
                let retained = prepare_task_run_input(
                    &access,
                    signer.did(),
                    &session,
                    true,
                    true,
                    Some(&row),
                    RequestInput::default(),
                )
                .await?;
                assert_eq!(retained, input);
                fire.goal_id =
                    prepare_task_run_goal_id(&access, signer.did(), &session, true, Some(&row))
                        .await?;
                assert_eq!(fire.goal_id, None);
                let retry = signed_request(&signer, &fire, retained).await?;
                assert_eq!(retry.admission.signature, request.admission.signature);
                let replay =
                    gents::lifecycle::write_task_delivery(&access, &fire, true, &retry).await?;
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
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn manual_task_replay_keeps_signed_goal_identity_without_observing_current_goal() -> Result<()>
{
    let (identity, endpoint, source, server) =
        crate::request_helpers::goal_input_tests::fixture(true, false, None, None).await?;
    let access = ConfigAccess::Graphql(endpoint);
    for reference in [
        None,
        Some(GoalAssignmentInput {
            goal_id: "retained-assignment".into(),
            root_request_doc_id: None,
        }),
    ] {
        let mut input = crate::request_helpers::goal_input_tests::authored_input();
        input.goal_assignment = reference.clone();
        let row = super::tests::signed_task_receipt(&identity, input).await?;
        let receipt: AgentRequestRow = serde_json::from_value(row)?;
        let selected =
            prepare_task_run_goal_id(&access, identity.did(), "chat", true, Some(&receipt)).await?;
        assert_eq!(selected, reference.map(|reference| reference.goal_id));
    }
    server.abort();
    assert!(source.queries.lock().unwrap().is_empty());
    Ok(())
}
