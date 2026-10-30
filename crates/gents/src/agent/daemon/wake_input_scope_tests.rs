use super::*;
use crate::request::lifecycle::{
    ClaimOutcome, ExecutionOrigin, RequestLifecycle, RequestTerminalOutcome,
};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{GoalAssignmentInput, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::row::AgentRequestRow;
use std::collections::BTreeMap;

fn key(id: u64) -> String {
    format!("background-completion-notification:scope-{id}:tool")
}
fn goal(id: u64) -> String {
    format!("wake-scope-goal-{id}")
}

async fn write_request(
    access: &ConfigAccess,
    identity: &KeyIdentity,
    mut create: AgentRequestCreate,
) -> AgentRequestRow {
    crate::sign_agent_request_create(identity, &mut create)
        .await
        .unwrap();
    let response = access
        .write(
            "test.wake_scope_signed_request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    let response = access
        .execute(&format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
            escape_graphql_string(&doc),
            crate::request::admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    crate::verify_request_receipt_signature(&row).unwrap();
    row
}

async fn terminal_source(access: &ConfigAccess, row: &AgentRequestRow, superseded: bool) {
    access.write("test.wake_scope_source_terminal", &format!("mutation {{update_AgentRequest(docID: \"{}\", input: {{lifecycle_state: \"{}\"}}) {{_docID}}}}", escape_graphql_string(row.doc_id.as_deref().unwrap()), if superseded { "superseded" } else { "completed" })).await.unwrap();
}

fn reference(
    scope: &Value,
    roots: &BTreeMap<(u64, u64), AgentRequestRow>,
) -> Option<GoalAssignmentInput> {
    (scope["kind"] == "referenced").then(|| GoalAssignmentInput {
        goal_id: goal(scope["goal"].as_u64().unwrap()),
        root_request_doc_id: Some(
            roots[&(
                scope["goal"].as_u64().unwrap(),
                scope["root"].as_u64().unwrap(),
            )]
                .doc_id
                .clone()
                .unwrap(),
        ),
    })
}

async fn wake(
    access: &ConfigAccess,
    identity: &KeyIdentity,
    session: &str,
    claim: &Value,
    roots: &BTreeMap<(u64, u64), AgentRequestRow>,
    unassigned_parent: &AgentRequestRow,
    retry_parent: Option<&AgentRequestRow>,
) -> crate::AgentRequest {
    let scope = &claim["scope"];
    let parent = if let Some(parent) = retry_parent {
        parent
    } else if scope["kind"] == "referenced" {
        &roots[&(
            scope["goal"].as_u64().unwrap(),
            scope["root"].as_u64().unwrap(),
        )]
    } else {
        unassigned_parent
    };
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("wake-{}", claim["wake"]),
        identity.did(),
        identity.did(),
        "general",
        session,
        "Review background completion results",
        "scheduled",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::runtime_local_control(identity.did(), &parent.request_id),
    );
    create.input.goal_assignment = if retry_parent.is_some() {
        None
    } else {
        reference(scope, roots)
    };
    create.input.queue = Some(RequestQueue {
        delivery: Default::default(),
        position: None,
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: Some(1),
    });
    create.caused_by_parent_request_id = Some(parent.request_id.clone());
    create.caused_by_parent_request_doc_id = parent.doc_id.clone();
    if retry_parent.is_some() {
        create.input = parent.input.clone().unwrap();
        create.retry_parent_request = Some(parent.request_id.clone());
        create.retry_parent_request_doc_id = parent.doc_id.clone();
        create.retry_root_request = Some(
            parent
                .retry_root_request
                .clone()
                .unwrap_or_else(|| parent.request_id.clone()),
        );
        create.retry_count = parent.retry_count.unwrap_or_default() + 1;
        create.max_retries = parent.max_retries.unwrap_or(3);
    }
    write_request(access, identity, create)
        .await
        .try_into()
        .unwrap()
}

async fn append_through(
    node: &Arc<EmbeddedNode>,
    identity: &KeyIdentity,
    session: &str,
    notifications: &[Value],
    sources: &BTreeMap<u64, AgentRequestRow>,
    appended: &mut u64,
    cutoff: u64,
) {
    for row in notifications {
        let sequence = row["sequence"].as_u64().unwrap();
        if sequence <= *appended || sequence > cutoff {
            continue;
        }
        let id = row["message"].as_u64().unwrap();
        crate::session::import_history_observation(
            node,
            sources[&id].doc_id.as_deref().unwrap(),
            session,
            identity.did(),
            Some(identity.did()),
            &format!("Notification {id}"),
            &key(id),
            sequence.try_into().unwrap(),
            None,
        )
        .await;
        *appended = sequence;
    }
}

async fn claim(
    node: &Arc<EmbeddedNode>,
    identity: &KeyIdentity,
    request: crate::AgentRequest,
) -> RequestLifecycle {
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        identity.did(),
        request,
        60,
        ExecutionOrigin::Scheduled,
        "scope-test-backend",
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new(
        node.clone(),
        identity.did(),
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    lifecycle
}

#[tokio::test]
async fn generated_goal_wake_receipts_scope_claims_and_exact_acknowledgement() {
    let snapshot = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    let cases = snapshot["goal_wake_receipt_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 11);
    let mut conflicting_history_cases = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(dir.path().join("owner.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let session = format!("wake-scope-{name}");
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &node,
            &session,
            identity.did(),
            "general",
            Some(identity.did()),
        )
        .await
        .unwrap();
        let make = |id: String| {
            AgentRequestCreate::base(
                RequestPurpose::Normal,
                id,
                identity.did(),
                identity.did(),
                "general",
                &session,
                "Source work",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(identity.did()),
            )
        };
        let parent = write_request(&access, &identity, make("unassigned-parent".into())).await;
        terminal_source(&access, &parent, false).await;
        let notifications = case["notifications"].as_array().unwrap();
        let mut roots = BTreeMap::new();
        for scope in notifications.iter().map(|row| &row["scope"]).chain(
            case["retry_parent"]
                .is_null()
                .then_some(&case["claim"]["scope"])
                .into_iter(),
        ) {
            if scope["kind"] != "referenced" {
                continue;
            }
            let pair = (
                scope["goal"].as_u64().unwrap(),
                scope["root"].as_u64().unwrap(),
            );
            if roots.contains_key(&pair) {
                continue;
            }
            let mut create = make(format!("opening-{}-{}", pair.0, pair.1));
            create.input.goal_assignment = Some(GoalAssignmentInput {
                goal_id: goal(pair.0),
                root_request_doc_id: None,
            });
            let row = write_request(&access, &identity, create).await;
            terminal_source(&access, &row, false).await;
            roots.insert(pair, row);
        }
        if case["prior"].as_array().unwrap().is_empty()
            && case["retry_parent"].is_null()
            && case["claim"]["scope"]["kind"] == "referenced"
            && notifications
                .iter()
                .all(|notice| notice["scope"]["kind"] == "referenced")
        {
            let opening = roots.values().next().unwrap().doc_id.as_deref().unwrap();
            access.transact_write("test.unrelated_goal_opening", |txn| {
                let input = serde_json::json!({
                    "goal_id": "unrelated-goal", "node_did": identity.did(), "session_id": session,
                    "objective": "Unreadable retained history", "status": "invalid-retained-status",
                    "assignment_root_request_doc_id": opening,
                    "created_at": "2026-10-21T00:00:00Z", "updated_at": "2026-10-21T00:00:00Z"
                });
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                        &serde_json::json!({"input": input}),
                    ).await
                })
            }).await.unwrap();
            conflicting_history_cases += 1;
        }
        let mut sources = BTreeMap::new();
        for notification in notifications {
            let id = notification["message"].as_u64().unwrap();
            let mut create = make(format!("source-{id}"));
            create.input.goal_assignment = reference(&notification["scope"], &roots);
            let row = write_request(&access, &identity, create).await;
            terminal_source(&access, &row, case["superseded_origins"] == true).await;
            sources.insert(id, row);
        }
        if case["prior"].as_array().unwrap().is_empty()
            && case["retry_parent"].is_null()
            && case["claim"]["scope"]["kind"] == "referenced"
            && notifications
                .iter()
                .all(|notice| notice["scope"]["kind"] == "referenced")
        {
            assert!(
                crate::goal::load_request_assignment(&node, sources.values().next().unwrap())
                    .await
                    .is_err()
            );
        }
        let mut appended = 0;
        let mut retry_parent = None;
        for prior in case["prior"].as_array().unwrap() {
            let prior_claim = &prior["claim"];
            append_through(
                &node,
                &identity,
                &session,
                notifications,
                &sources,
                &mut appended,
                prior_claim["cutoff"].as_u64().unwrap(),
            )
            .await;
            let request = wake(
                &access,
                &identity,
                &session,
                prior_claim,
                &roots,
                &parent,
                None,
            )
            .await;
            let doc = request.doc_id.clone();
            let mut lifecycle = claim(&node, &identity, request).await;
            if prior["create_goal"] == true {
                let response = access
                    .execute(&format!(
                        "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
                        escape_graphql_string(&doc),
                        crate::request::admission::SIGNED_REQUEST_FIELDS
                    ))
                    .await
                    .unwrap();
                let caller: AgentRequestRow =
                    serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
                let created = crate::goal::create_goal_for_request(
                    &node,
                    &caller,
                    "Goal opened during this wake",
                    None,
                )
                .await
                .unwrap();
                assert_eq!(
                    created.goal().assignment_root_request_doc_id.as_deref(),
                    Some(doc.as_str())
                );
            }
            let outcome = match prior["state"].as_str().unwrap() {
                "completed" => RequestTerminalOutcome::Completed,
                "failed" => RequestTerminalOutcome::Failed,
                "interrupted" => RequestTerminalOutcome::Interrupted,
                state => panic!("{state}"),
            };
            lifecycle
                .terminalize_owned(
                    outcome,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("Fixture attempt outcome"),
                )
                .await
                .unwrap();
            drop(lifecycle);
            if case["retry_parent"] == prior_claim["wake"] {
                let response = access
                    .execute(&format!(
                        "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
                        escape_graphql_string(&doc),
                        crate::request::admission::SIGNED_REQUEST_FIELDS
                    ))
                    .await
                    .unwrap();
                retry_parent = Some(
                    serde_json::from_value::<AgentRequestRow>(
                        response["data"]["AgentRequest"][0].clone(),
                    )
                    .unwrap(),
                );
            }
        }
        let current_claim = &case["claim"];
        append_through(
            &node,
            &identity,
            &session,
            notifications,
            &sources,
            &mut appended,
            current_claim["cutoff"].as_u64().unwrap(),
        )
        .await;
        let request = wake(
            &access,
            &identity,
            &session,
            current_claim,
            &roots,
            &parent,
            retry_parent.as_ref(),
        )
        .await;
        let lifecycle = claim(&node, &identity, request.clone()).await;
        let response=access.execute(&format!("{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{background_completion_notification_keys_json}}}}",escape_graphql_string(&request.doc_id))).await.unwrap();
        let captured: Vec<String> = serde_json::from_str(
            response["data"]["AgentRequest"][0]["background_completion_notification_keys_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let expected = |field: &str| {
            case[field]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| key(id.as_u64().unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            captured,
            expected("captured"),
            "{name}: claim admitted another assignment's notification"
        );
        append_through(
            &node,
            &identity,
            &session,
            notifications,
            &sources,
            &mut appended,
            u64::MAX,
        )
        .await;
        let rows = load(
            &node,
            &request,
            lifecycle.background_completion_input_through_sequence(),
        )
        .await
        .unwrap();
        let actual = rows
            .iter()
            .map(|row| row.message.rag_text().expect("notification user text"))
            .collect::<Vec<_>>();
        let expected_content = case["current"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| format!("Notification {}", id.as_u64().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected_content,
            "{name}: provider input or exact acknowledgement changed"
        );
        drop(lifecycle);
        node.shutdown().await;
    }
    assert!(conflicting_history_cases > 0);
}
