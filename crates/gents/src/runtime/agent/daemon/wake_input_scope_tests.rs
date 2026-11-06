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
use std::{collections::BTreeMap, sync::Arc};

fn key(id: u64) -> String {
    format!("background-completion-notification:scope-{id}:tool")
}
fn goal(id: u64) -> String {
    format!("wake-scope-goal-{id}")
}

struct ScopeFixture {
    db: crate::support::TestDb,
    user: KeyIdentity,
    client: ConfigAccess,
    agent: Arc<dyn NodeIdentity>,
    access: ConfigAccess,
}

impl ScopeFixture {
    async fn new(name: &str) -> Self {
        use crate::store::config_client::NodePermission;
        let (db, user, client, binding) = crate::test_support::signed_test_node(name).await;
        let agent =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let admin = ConfigAccess::local_as(
            db.node.clone(),
            identity::Did::new(db.node_identity.did()).unwrap(),
        );
        let agent_did = identity::Did::new(agent.did()).unwrap();
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DocumentDelete,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&agent_did, permission)
                .await
                .unwrap();
        }
        let access =
            ConfigAccess::local_managed_agent(db.node.clone(), agent_did, agent.as_ref()).unwrap();
        Self {
            db,
            user,
            client,
            agent,
            access,
        }
    }
}

async fn write_request(fixture: &ScopeFixture, mut create: AgentRequestCreate) -> AgentRequestRow {
    create.session_doc_id = Some(
        crate::request::lifecycle::reserve_goal_session(&fixture.client, &create)
            .await
            .unwrap(),
    );
    let agent_did = escape_graphql_string(fixture.agent.did());
    let agent = fixture
        .client
        .execute(&format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{agent_did}"}}}}, limit: 2) {{_docID}} }}"#
        ))
        .await
        .unwrap();
    let [agent] = agent["data"]["Agent"].as_array().unwrap().as_slice() else {
        panic!("one selected agent")
    };
    create.agent_doc_id = Some(agent["_docID"].as_str().unwrap().to_owned());
    crate::sign_agent_request_create(&fixture.user, &mut create)
        .await
        .unwrap();
    crate::request::lifecycle::submit_client_request(&fixture.client, &create)
        .await
        .unwrap();
    let request_id = escape_graphql_string(&create.request_id);
    let response = fixture
        .access
        .execute(&format!(
            "{{AgentRequest(filter: {{request_id: {{_eq: \"{request_id}\"}}}}) {{{}}}}}",
            crate::request::admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    crate::verify_request_receipt_signature(&row).unwrap();
    row
}

async fn terminal_source(fixture: &ScopeFixture, row: &AgentRequestRow) {
    let request: crate::AgentRequest = row.clone().try_into().unwrap();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        fixture.db.node.clone(),
        fixture.db.node_identity.clone(),
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, fixture.agent.as_ref())
        .await
        .unwrap();
    assert!(admission.is_some());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        fixture.db.node.clone(),
        fixture.agent.did(),
        fixture.db.node_identity.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        "scope-test-backend",
    );
    lifecycle
        .bind_managed_agent(fixture.agent.as_ref())
        .unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        fixture.db.node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
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
    fixture: &ScopeFixture,
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
        fixture.db.node_identity.did(),
        fixture.user.did(),
        fixture.agent.did(),
        session,
        "Review background completion results",
        "scheduled",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::runtime_local_control(fixture.agent.did(), &parent.request_id),
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
    create.session_doc_id = parent.session_doc_id.clone();
    create.agent_doc_id = parent.agent_doc_id.clone();
    crate::sign_agent_request_create(fixture.agent.as_ref(), &mut create)
        .await
        .unwrap();
    let mutation = create.graphql_mutation().unwrap();
    let session_doc = create.session_doc_id.as_deref().unwrap();
    let agent_doc = create.agent_doc_id.as_deref().unwrap();
    let agent_did = identity::Did::new(fixture.agent.did()).unwrap();
    let doc = fixture
        .access
        .transact_write("test.wake_scope_internal_request", |txn| {
            let mutation = &mutation;
            let agent_did = &agent_did;
            Box::pin(async move {
                let result = txn.execute(mutation).await?;
                let doc = crate::store::graphql::created_doc_id(&result, "AgentRequest")?;
                txn.add_resource_relationship("AgentRequest", &doc, "parent", "agents", agent_doc)
                    .await?;
                txn.add_resource_relationship(
                    "AgentRequest",
                    &doc,
                    "session",
                    "sessions",
                    session_doc,
                )
                .await?;
                txn.add_actor_relationship("AgentRequest", &doc, "working_agent", agent_did)
                    .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .unwrap();
    let response = fixture
        .access
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
    row.try_into().unwrap()
}

async fn append_through(
    fixture: &ScopeFixture,
    session: &str,
    notifications: &[Value],
    sources: &BTreeMap<u64, AgentRequestRow>,
    appended: &mut u64,
    cutoff: u64,
) {
    use crate::session::canonical_rows::{
        output_segment_bound_variables, transcript_message_bound_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::*;
    for row in notifications {
        let sequence = row["sequence"].as_u64().unwrap();
        if sequence <= *appended || sequence > cutoff {
            continue;
        }
        let id = row["message"].as_u64().unwrap();
        let text = format!("Notification {id}");
        let message_key = key(id);
        let request_doc = sources[&id].doc_id.as_ref().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let segment = OutputSegment {
            node_did: fixture.db.node_identity.did().to_owned(),
            requester_did: Some(fixture.user.did().to_owned()),
            session_id: session.to_owned(),
            request_doc_id: request_doc.clone(),
            source: OutputSource::Authored {
                key: message_key.clone(),
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: text.len().try_into().unwrap(),
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: text.clone(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![text.len() as u64],
            }),
            created_at: now.clone(),
        };
        fixture
            .access
            .transact_write("test.wake_scope_history", |txn| {
                let segment = &segment;
                let message_key = &message_key;
                let now = &now;
                Box::pin(async move {
                    let vars = output_segment_bound_variables(txn, segment).await?;
                    let response = txn
                        .execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &vars)
                        .await?;
                    let close_doc_id =
                        crate::store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                    let header = TranscriptMessage {
                        native_request_id: None,
                        message_key: message_key.clone(),
                        session_id: segment.session_id.clone(),
                        node_did: segment.node_did.clone(),
                        requester_did: segment.requester_did.clone(),
                        request_doc_id: Some(segment.request_doc_id.clone()),
                        publication: MessagePublication::RequestExecution {
                            execution_generation: "observed-generation".into(),
                        },
                        outcome: OutputOutcome::Complete,
                        sequence: sequence.try_into()?,
                        role: MessageRole::User,
                        native_id: None,
                        blocks: vec![MessageBlock::Text {
                            text: PresentedPayload {
                                output: PayloadRef {
                                    close_doc_id,
                                    stream: 0,
                                },
                                presentation: PayloadPresentation::Full,
                            },
                        }],
                        created_at: now.clone(),
                    };
                    let vars = transcript_message_bound_variables(txn, &header).await?;
                    txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &vars)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        crate::session::child_ownership::publish_agent_session_child_relationships_with_access(
            &fixture.access,
            fixture.db.node_identity.did(),
            &identity::Did::new(fixture.agent.did()).unwrap(),
            session,
        )
        .await
        .unwrap();
        *appended = sequence;
    }
}

async fn claim(fixture: &ScopeFixture, request: crate::AgentRequest) -> RequestLifecycle {
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        fixture.db.node.clone(),
        fixture.db.node_identity.clone(),
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, fixture.agent.as_ref())
        .await
        .unwrap();
    assert!(admission.is_none());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        fixture.db.node.clone(),
        fixture.agent.did(),
        fixture.db.node_identity.did(),
        request,
        60,
        ExecutionOrigin::Scheduled,
        "scope-test-backend",
    );
    lifecycle
        .bind_managed_agent(fixture.agent.as_ref())
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        fixture.db.node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
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
        let fixture = ScopeFixture::new(&format!("goal-wake-scope-{name}")).await;
        let node = fixture.db.node.clone();
        let access = &fixture.access;
        let session = format!("wake-scope-{name}");
        let make = |id: String| {
            AgentRequestCreate::base(
                RequestPurpose::Normal,
                id,
                fixture.db.node_identity.did(),
                fixture.user.did(),
                fixture.agent.did(),
                &session,
                "Source work",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::client(fixture.user.did()),
            )
        };
        let parent = write_request(&fixture, make("unassigned-parent".into())).await;
        terminal_source(&fixture, &parent).await;
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
            let row = write_request(&fixture, create).await;
            terminal_source(&fixture, &row).await;
            roots.insert(pair, row);
        }
        let mut sources = BTreeMap::new();
        for notification in notifications {
            let id = notification["message"].as_u64().unwrap();
            let mut create = make(format!("source-{id}"));
            create.input.goal_assignment = reference(&notification["scope"], &roots);
            let row = write_request(&fixture, create.clone()).await;
            if case["superseded_origins"] == true {
                create.request_id = format!("source-{id}-replacement");
                let replacement = write_request(&fixture, create).await;
                let mutation = crate::request::lifecycle::queue::supersede_pending_mutation(
                    row.doc_id.as_deref().unwrap(),
                    fixture.db.node_identity.did(),
                    &replacement.request_id,
                    replacement.doc_id.as_deref().unwrap(),
                    "modeled superseded origin",
                );
                let result = access
                    .write("test.wake_scope_superseded_origin", &mutation)
                    .await
                    .unwrap();
                assert_eq!(
                    crate::store::graphql::mutation_documents(&result, "update_AgentRequest")
                        .unwrap()
                        .unwrap()
                        .len(),
                    1
                );
                terminal_source(&fixture, &replacement).await;
            } else {
                terminal_source(&fixture, &row).await;
            }
            sources.insert(id, row);
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
                    "goal_id": "unrelated-goal", "node_did": fixture.db.node_identity.did(), "session_id": session,
                    "session_doc_id": parent.session_doc_id.as_deref(), "requester_did": fixture.user.did(),
                    "agent_did": fixture.agent.did(), "creator_did": fixture.agent.did(), "parent_linked": false,
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
            crate::session::child_ownership::publish_agent_session_child_relationships_with_access(
                access,
                fixture.db.node_identity.did(),
                &identity::Did::new(fixture.agent.did()).unwrap(),
                &session,
            )
            .await
            .unwrap();
            conflicting_history_cases += 1;
        }
        if case["prior"].as_array().unwrap().is_empty()
            && case["retry_parent"].is_null()
            && case["claim"]["scope"]["kind"] == "referenced"
            && notifications
                .iter()
                .all(|notice| notice["scope"]["kind"] == "referenced")
        {
            assert!(
                crate::goal::load_request_assignment(access, sources.values().next().unwrap())
                    .await
                    .is_err()
            );
        }
        let mut appended = 0;
        let mut retry_parent = None;
        for prior in case["prior"].as_array().unwrap() {
            let prior_claim = &prior["claim"];
            append_through(
                &fixture,
                &session,
                notifications,
                &sources,
                &mut appended,
                prior_claim["cutoff"].as_u64().unwrap(),
            )
            .await;
            let request = wake(&fixture, &session, prior_claim, &roots, &parent, None).await;
            let doc = request.doc_id.clone();
            let mut lifecycle = claim(&fixture, request).await;
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
                let created = crate::goal::create_goal_for_request_from_access(
                    &node,
                    Some(access),
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
            &fixture,
            &session,
            notifications,
            &sources,
            &mut appended,
            current_claim["cutoff"].as_u64().unwrap(),
        )
        .await;
        let request = wake(
            &fixture,
            &session,
            current_claim,
            &roots,
            &parent,
            retry_parent.as_ref(),
        )
        .await;
        let lifecycle = claim(&fixture, request.clone()).await;
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
            &fixture,
            &session,
            notifications,
            &sources,
            &mut appended,
            u64::MAX,
        )
        .await;
        let rows = load(
            access,
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
        node.shutdown().await.expect("node shutdown");
    }
    assert!(conflicting_history_cases > 0);
}

#[tokio::test]
async fn claimed_background_wake_reads_user_session_as_selected_agent() {
    use crate::request::lifecycle::queue::persist_background_completion_with_message_waking_with_access;
    use crate::request::lifecycle::{RequestHopCause, RequestTerminalOutcome};
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use crate::tools::call::lifecycle::AwaitMode;

    let (fixture, mut parent) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "wake-agent-observation".into(),
        await_mode: AwaitMode::Background,
        start_running: false,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = fixture.node.clone();
    let selected = parent.request().agent_did.clone();
    let agent_row = fixture
        .client
        .execute(&format!(
            r#"{{Agent(filter: {{agent_did: {{_eq: "{}"}}}}, limit: 2) {{agent_did creation_nonce}}}}"#,
            escape_graphql_string(&selected),
        ))
        .await
        .unwrap();
    let [agent_row] = agent_row["data"]["Agent"].as_array().unwrap().as_slice() else {
        panic!("fixture Agent must be unique");
    };
    let binding: gents_protocol::agent_identity::AgentIdentityBinding =
        serde_json::from_value(agent_row.clone()).unwrap();
    let agent =
        crate::store::identity::resolve_agent_identity(fixture.runtime.as_ref(), &binding).unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let original = parent.request().clone();
    let wake = persist_background_completion_with_message_waking_with_access(
        &agent_access,
        &node,
        &original,
        "background result",
        "background-completion-notification:agent-observation:tool",
        "Review background completion results",
        RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some("background_completion:agent-observation".into()),
            queued_after_request_id: Some(original.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        None,
        RequestHopCause::Continuation,
    )
    .await
    .unwrap()
    .request
    .expect("background wake request");
    parent
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    let query = format!(
        r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{{}}}}}"#,
        escape_graphql_string(&wake.doc_id),
        crate::request::watcher::AGENT_REQUEST_FIELDS,
    );
    let row = agent_access.execute(&query).await.unwrap();
    let [row] = row["data"]["AgentRequest"].as_array().unwrap().as_slice() else {
        panic!("selected Agent must see one wake request");
    };
    let row: AgentRequestRow = serde_json::from_value(row.clone()).unwrap();
    let request: crate::AgentRequest = row.try_into().unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        fixture.runtime.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(admission.is_none(), "runtime wake is not a client claim");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent.did(),
        &fixture.node_did,
        request,
        60,
        ExecutionOrigin::Scheduled,
        "wake-test-backend",
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let cutoff = lifecycle.background_completion_input_through_sequence();
    assert!(
        cutoff.is_some(),
        "claim must capture the wake notification cutoff"
    );
    let request = lifecycle.request();
    let runtime_access = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(fixture.runtime.did()).unwrap(),
    );
    let visible = runtime_access.execute(&query).await.unwrap();
    assert!(
        visible["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the runtime must not inherit the User's session read authority"
    );
    assert!(load(&runtime_access, request, cutoff).await.is_err());
    let actual = load(&agent_access, request, cutoff).await.unwrap();
    assert_eq!(actual.len(), 1);
    assert_eq!(
        actual[0].message.rag_text().as_deref(),
        Some("background result")
    );
    assert!(actual[0].physical_header.is_some());

    let other_binding = crate::store::identity::provision_agent_identity_authorized(
        &node,
        fixture.runtime.as_ref(),
        &identity::Did::new(fixture.requester_identity.did()).unwrap(),
    )
    .await
    .unwrap();
    crate::test_support::install_test_agent_binding_with_access(
        fixture.runtime.did(),
        "Other Agent",
        &other_binding,
        &fixture.client,
    )
    .await;
    let other =
        crate::store::identity::resolve_agent_identity(fixture.runtime.as_ref(), &other_binding)
            .unwrap();
    let other_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(other.did()).unwrap(),
        other.as_ref(),
    )
    .unwrap();
    assert!(
        load(&other_access, request, cutoff).await.is_err(),
        "a different Agent must not observe the selected Agent's wake"
    );
    drop(lifecycle);
    drop(parent);
    node.shutdown().await.expect("node shutdown");
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
