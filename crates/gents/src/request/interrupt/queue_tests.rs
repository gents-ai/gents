use super::*;
use crate::lean_vocab_test::{
    LeanInterruptQueueCase, LeanInterruptQueueEntry, LeanInterruptQueueInput,
};
use crate::request::lifecycle::queue::{
    persist_background_completion_with_message_waking_with_access, QueuePolicy, QueueSource,
    RequestInput, RequestQueue,
};
use crate::request::lifecycle::ExecutionOrigin;
use crate::request::watcher::AgentRequest;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::identity::NodeIdentity;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct QueueFixture {
    db: crate::support::TestDb,
    user: crate::KeyIdentity,
    client: ConfigAccess,
    agent: Arc<dyn NodeIdentity>,
    agent_access: ConfigAccess,
    parent: AgentRequest,
    _lifecycle: crate::request::lifecycle::RequestLifecycle,
}

async fn publish_client_request(
    client: &ConfigAccess,
    user: &dyn NodeIdentity,
    identity: crate::request::lifecycle::RequestIdentity,
    input: RequestInput,
) -> String {
    let request_id = identity.request_id.clone();
    let mut spec = crate::request::lifecycle::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        identity,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
    );
    spec.input = input;
    let create = crate::request::lifecycle::build_signed_client_request(
        client,
        spec,
        crate::request::lifecycle::RequestSigner::Identity(user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(client, &create)
        .await
        .unwrap();
    let response = client.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
        escape_graphql_string(&request_id),
    )).await.unwrap();
    let [row] = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("signed fixture request must be unique");
    };
    row["_docID"].as_str().unwrap().to_owned()
}

async fn rows(access: &ConfigAccess, session: &str) -> Vec<Value> {
    let session = escape_graphql_string(session);
    let result = access.execute(
        &format!("{{AgentRequest(filter:{{session_id:{{_eq:\"{session}\"}}}}){{_docID request_id node_did requester_did lifecycle_state interrupt_requested_at input execution_origin}}}}"),
    ).await.unwrap();
    result["data"]["AgentRequest"].as_array().unwrap().clone()
}

async fn latch(access: &ConfigAccess, parent: &AgentRequest) -> Option<String> {
    let observed = observe_interrupt_requested_at_with_access(
        access,
        &parent.request_id,
        &parent.node_did,
        parent.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(
        observed.request_doc_id.as_deref(),
        Some(parent.doc_id.as_str())
    );
    observed.requested_at
}

async fn fixture(case: &LeanInterruptQueueCase) -> QueueFixture {
    use crate::request::lifecycle::{ClaimOutcome, RequestIdentity, RequestLifecycle};

    assert_eq!(case.node_id, 1, "fixture maps modeled node 1 to its DID");
    assert_eq!(
        case.requester_id,
        Some(case.node_id),
        "modeled requester scope"
    );
    let (db, user, client, binding) =
        crate::test_support::signed_test_node(&format!("interrupt-queue-{}", case.name)).await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let request_id = case.active_request_id.unwrap().to_string();
    let session = case.session_id.to_string();
    let doc = publish_client_request(
        &client,
        &user,
        RequestIdentity {
            request_id: request_id.clone(),
            node_did: db.node_identity.did().to_owned(),
            requester_did: Some(user.did().to_owned()),
            agent_did: binding.agent_did.clone(),
            session_id: session,
            content: "active".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: "2026-09-01T00:00:00Z".into(),
        },
        RequestInput::default(),
    )
    .await;
    let response = agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&doc),
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(
        admission.is_some(),
        "client request needs verified claim admission"
    );
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        db.node.clone(),
        agent.did(),
        db.node_identity.did(),
        request,
        60,
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        db.node.clone(),
        &lifecycle,
        Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let parent = lifecycle.request().clone();
    QueueFixture {
        db,
        user,
        client,
        agent,
        agent_access,
        parent,
        _lifecycle: lifecycle,
    }
}

async fn enqueue_generated_wake(
    access: &ConfigAccess,
    node: &Arc<EmbeddedNode>,
    parent: &AgentRequest,
    case: &LeanInterruptQueueCase,
    entry: &LeanInterruptQueueEntry,
) -> String {
    let source: QueueSource = serde_json::from_value(json!(entry.source)).unwrap();
    let policy: QueuePolicy = serde_json::from_value(json!(entry.policy)).unwrap();
    assert_eq!(
        ExecutionOrigin::from_persisted(Some(entry.execution_origin.as_str())).unwrap(),
        ExecutionOrigin::Scheduled
    );
    assert_eq!(source, QueueSource::BackgroundCompletion);
    assert_eq!(policy, QueuePolicy::Coalesce);
    let key = entry.queue_key.expect("generated wake key");
    persist_background_completion_with_message_waking_with_access(
        access,
        node,
        parent,
        &format!("notification {}", entry.request_id),
        &format!(
            "background-completion-notification:{}:{}",
            case.name, entry.request_id
        ),
        "review notifications",
        RequestQueue {
            delivery: Default::default(),
            position: None,
            source,
            policy,
            key: Some(format!("background_completion:{key}")),
            queued_after_request_id: entry.queued_after.map(|id| id.to_string()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        None,
        crate::request::lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap()
    .request
    .expect("background wake request")
    .doc_id
}

async fn insert_generated_pending(
    fixture: &QueueFixture,
    parent: &AgentRequest,
    entry: &LeanInterruptQueueEntry,
    source: QueueSource,
    policy: QueuePolicy,
    origin: ExecutionOrigin,
) -> String {
    let created_at = chrono::DateTime::from_timestamp(entry.created_at as i64, 0)
        .unwrap()
        .to_rfc3339();
    let input = RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source,
            policy,
            key: entry
                .queue_key
                .map(|key| format!("background_completion:{key}")),
            queued_after_request_id: entry.queued_after.map(|id| id.to_string()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        }),
        ..Default::default()
    };
    publish_client_request(
        &fixture.client,
        &fixture.user,
        crate::request::lifecycle::RequestIdentity {
            request_id: entry.request_id.to_string(),
            node_did: parent.node_did.clone(),
            requester_did: parent.requester_did.clone(),
            agent_did: parent.agent_did.clone(),
            session_id: parent.session_id.clone(),
            content: "generated pending fixture".into(),
            execution_origin: origin,
            created_at,
        },
        input,
    )
    .await
}

fn assert_generated_queue_row_metadata(
    case: &LeanInterruptQueueCase,
    by_doc: &RapidHashMap<String, &Value>,
    bound: &RapidHashMap<u64, String>,
) {
    for event in &case.inputs {
        let LeanInterruptQueueInput::Enqueue { entry } = event else {
            continue;
        };
        let row = by_doc.get(&bound[&entry.request_id]).unwrap();
        assert_eq!(
            row["execution_origin"], entry.execution_origin,
            "{}: {} origin",
            case.name, entry.request_id
        );
        let input: RequestInput = serde_json::from_value(row["input"].clone()).unwrap();
        let queue = input.queue.expect("generated queue row input");
        let source: QueueSource = serde_json::from_value(json!(entry.source)).unwrap();
        let policy: QueuePolicy = serde_json::from_value(json!(entry.policy)).unwrap();
        assert_eq!(
            queue.source, source,
            "{}: {} source",
            case.name, entry.request_id
        );
        assert_eq!(
            queue.policy, policy,
            "{}: {} policy",
            case.name, entry.request_id
        );
        assert_eq!(
            queue.key,
            entry
                .queue_key
                .map(|key| format!("background_completion:{key}")),
            "{}: {} key",
            case.name,
            entry.request_id
        );
        assert_eq!(
            queue.queued_after_request_id,
            entry.queued_after.map(|id| id.to_string()),
            "{}: {} queued_after",
            case.name,
            entry.request_id
        );
    }
}

#[tokio::test]
async fn generated_interrupt_queue_events_bind_to_native_rows() {
    let mut overlap_cases = RapidHashSet::new();
    for case in crate::lean_vocab_test::lean_interrupt_queue_cases() {
        let has_overlap_event = case.inputs.iter().any(|event| {
            matches!(
                event,
                LeanInterruptQueueInput::CaptureInterrupt
                    | LeanInterruptQueueInput::CommitInterrupt
            )
        });
        if has_overlap_event {
            assert!(matches!(
                case.name.as_str(),
                "empty_scan_completion_before_latch_commit"
                    | "observed_old_then_later_same_key_completion"
            ));
            assert!(
                overlap_cases.insert(case.name.as_str()),
                "duplicate generated overlap case"
            );
            continue;
        }
        for use_http in [false, true] {
            let fixture = fixture(case).await;
            let node = fixture.db.node.clone();
            let parent = &fixture.parent;
            let access = if use_http {
                fixture.db.client_access(fixture.agent.as_ref())
            } else {
                fixture.agent_access.clone()
            };
            let mut bound: RapidHashMap<u64, String> = RapidHashMap::new();
            let mut notification_bindings = Vec::new();
            let mut first_latch = None;
            for event in &case.inputs {
                match event {
                    LeanInterruptQueueInput::Interrupt => {
                        interrupt_request_by_doc_id_with_access(
                            &access,
                            &parent.doc_id,
                            &parent.node_did,
                            parent.requester_did.as_deref(),
                        )
                        .await
                        .unwrap();
                        let latch = latch(&fixture.agent_access, parent).await;
                        assert!(latch.is_some(), "{}: interrupt must latch", case.name);
                        if let Some(first) = &first_latch {
                            assert_eq!(&latch, first, "{}: replay changed latch", case.name);
                        } else {
                            first_latch = Some(latch);
                        }
                    }
                    LeanInterruptQueueInput::Enqueue { entry } => {
                        let source: QueueSource =
                            serde_json::from_value(json!(entry.source)).unwrap();
                        let policy: QueuePolicy =
                            serde_json::from_value(json!(entry.policy)).unwrap();
                        let origin =
                            ExecutionOrigin::from_persisted(Some(entry.execution_origin.as_str()))
                                .unwrap();
                        let doc = match (origin, source) {
                            (ExecutionOrigin::Scheduled, QueueSource::BackgroundCompletion) => {
                                let doc = enqueue_generated_wake(
                                    &fixture.agent_access,
                                    &node,
                                    parent,
                                    case,
                                    entry,
                                )
                                .await;
                                notification_bindings.push(doc.clone());
                                doc
                            }
                            (
                                ExecutionOrigin::Interactive,
                                QueueSource::User | QueueSource::BackgroundCompletion,
                            ) => {
                                insert_generated_pending(
                                    &fixture, parent, entry, source, policy, origin,
                                )
                                .await
                            }
                            _ => panic!(
                                "{}: no native adapter for generated origin/source",
                                case.name
                            ),
                        };
                        bound.insert(entry.request_id, doc);
                    }
                    LeanInterruptQueueInput::CaptureInterrupt
                    | LeanInterruptQueueInput::CommitInterrupt => {
                        unreachable!("overlap runner owns this case")
                    }
                }
            }
            let actual = rows(&fixture.agent_access, &parent.session_id).await;
            let by_doc: RapidHashMap<_, _> = actual
                .iter()
                .map(|row| (row["_docID"].as_str().unwrap().to_owned(), row))
                .collect();
            let expected_docs: RapidHashSet<_> = std::iter::once(parent.doc_id.clone())
                .chain(bound.values().cloned())
                .collect();
            assert_eq!(
                by_doc.keys().cloned().collect::<RapidHashSet<_>>(),
                expected_docs,
                "{}: exact physical row inventory",
                case.name
            );
            let active = by_doc.get(&parent.doc_id).unwrap();
            assert_eq!(
                active["lifecycle_state"], "processing",
                "{}: active lifecycle",
                case.name
            );
            assert_generated_queue_row_metadata(case, &by_doc, &bound);
            let physical_pending: RapidHashSet<_> = actual
                .iter()
                .filter(|row| row["_docID"] != parent.doc_id && row["lifecycle_state"] == "pending")
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect();
            let physical_terminal: RapidHashSet<_> = actual
                .iter()
                .filter(|row| {
                    row["_docID"] != parent.doc_id && row["lifecycle_state"] == "interrupted"
                })
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                physical_pending,
                case.expected
                    .pending
                    .iter()
                    .map(|id| bound[id].clone())
                    .collect(),
                "{}: pending physical rows",
                case.name
            );
            assert_eq!(
                physical_terminal,
                case.expected
                    .terminal
                    .iter()
                    .map(|id| bound[id].clone())
                    .collect(),
                "{}: terminal physical rows",
                case.name
            );
            assert_eq!(
                first_latch.is_some(),
                case.expected.latched,
                "{}: latch",
                case.name
            );
            let messages = fixture
                .agent_access
                .execute(&format!(
                "{{AgentMessage(filter:{{session_id:{{_eq:\"{}\"}}}}){{_docID request_doc_id}}}}",
                escape_graphql_string(&parent.session_id)
            ))
                .await
                .unwrap();
            let data = &messages["data"];
            let mut actual_bindings = data["AgentMessage"]
                .as_array()
                .unwrap()
                .iter()
                .map(|message| message["request_doc_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            actual_bindings.sort();
            notification_bindings.sort();
            assert_eq!(
                actual_bindings, notification_bindings,
                "{}: notification facts",
                case.name
            );
            node.shutdown().await.expect("shut down test node");
        }
    }
    assert_eq!(
        overlap_cases,
        RapidHashSet::from_iter([
            "empty_scan_completion_before_latch_commit",
            "observed_old_then_later_same_key_completion",
        ]),
        "both generated overlap cases must have native runners"
    );
}

#[tokio::test]
async fn generated_http_overlap_cases_preserve_cutoff() {
    let cases = crate::lean_vocab_test::lean_interrupt_queue_cases()
        .iter()
        .filter(|case| {
            matches!(
                case.name.as_str(),
                "empty_scan_completion_before_latch_commit"
                    | "observed_old_then_later_same_key_completion"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cases.len(),
        2,
        "exactly two generated overlap cases are required"
    );
    assert_eq!(
        cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<RapidHashSet<_>>(),
        RapidHashSet::from_iter([
            "empty_scan_completion_before_latch_commit",
            "observed_old_then_later_same_key_completion",
        ])
    );
    for case in cases {
        let (before_capture, during_pause, after_commit) = match (
            case.name.as_str(),
            case.inputs.as_slice(),
        ) {
            (
                "empty_scan_completion_before_latch_commit",
                [LeanInterruptQueueInput::CaptureInterrupt, LeanInterruptQueueInput::Enqueue { entry }, LeanInterruptQueueInput::CommitInterrupt, LeanInterruptQueueInput::Interrupt],
            ) => (None, Some(entry), None),
            (
                "observed_old_then_later_same_key_completion",
                [LeanInterruptQueueInput::Enqueue { entry: old }, LeanInterruptQueueInput::CaptureInterrupt, LeanInterruptQueueInput::CommitInterrupt, LeanInterruptQueueInput::Enqueue { entry: later }, LeanInterruptQueueInput::Interrupt],
            ) => (Some(old), None, Some(later)),
            _ => panic!("generated overlap script changed without a native adapter"),
        };
        let fixture = fixture(case).await;
        let node = fixture.db.node.clone();
        let parent = &fixture.parent;
        let access = fixture.db.client_access(fixture.agent.as_ref());
        let mut bound = RapidHashMap::new();
        if let Some(entry) = before_capture {
            bound.insert(
                entry.request_id,
                enqueue_generated_wake(&fixture.agent_access, &node, parent, case, entry).await,
            );
        }

        let reached = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let paused = Arc::new(AtomicBool::new(false));
        let filter = exact_request_filter(
            &parent.doc_id,
            &parent.node_did,
            parent.requester_did.as_deref(),
        )
        .unwrap();
        let reached_in = Arc::clone(&reached);
        let release_in = Arc::clone(&release);
        let paused_in = Arc::clone(&paused);
        let expected_drained = case.expected.terminal.len();
        let interrupt = tokio::spawn(async move {
            access
                .transact_write("test.http_interrupt_overlap", |txn| {
                    let reached = Arc::clone(&reached_in);
                    let release = Arc::clone(&release_in);
                    let paused = Arc::clone(&paused_in);
                    let filter = &filter;
                    Box::pin(async move {
                        let drained = interrupt_request_matching_in_txn(txn, filter).await?;
                        anyhow::ensure!(
                            drained.len() == expected_drained,
                            "captured modeled pending wake count"
                        );
                        if !paused.swap(true, Ordering::SeqCst) {
                            reached.notify_one();
                            release.notified().await;
                        }
                        Ok(drained)
                    })
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), reached.notified())
            .await
            .expect("HTTP transaction must pause after modeled queue scan");
        assert_eq!(
            latch(&fixture.agent_access, parent).await,
            None,
            "staged HTTP latch must remain invisible before commit"
        );
        if let Some(entry) = during_pause {
            let doc = tokio::time::timeout(
                Duration::from_secs(10),
                enqueue_generated_wake(&fixture.agent_access, &node, parent, case, entry),
            )
            .await
            .expect("embedded wake publication must commit while HTTP interrupt is open");
            bound.insert(entry.request_id, doc);
        }
        let before_commit = rows(&fixture.agent_access, &parent.session_id).await;
        assert_eq!(
            before_commit
                .iter()
                .find(|row| row["_docID"] == parent.doc_id)
                .unwrap()["interrupt_requested_at"],
            Value::Null
        );
        for doc in bound.values() {
            assert_eq!(
                before_commit
                    .iter()
                    .find(|row| row["_docID"] == *doc)
                    .unwrap()["lifecycle_state"],
                "pending"
            );
        }
        release.notify_one();
        let drained = tokio::time::timeout(Duration::from_secs(10), interrupt)
            .await
            .expect("HTTP interrupt must commit after release")
            .unwrap()
            .unwrap();
        let mut expected_receipt = case
            .expected
            .terminal
            .iter()
            .map(|id| {
                before_commit
                    .iter()
                    .find(|row| row["_docID"] == bound[id])
                    .unwrap()["request_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let mut drained = drained;
        expected_receipt.sort();
        drained.sort();
        assert_eq!(drained, expected_receipt, "exact committed drain receipt");
        if let Some(entry) = after_commit {
            bound.insert(
                entry.request_id,
                enqueue_generated_wake(&fixture.agent_access, &node, parent, case, entry).await,
            );
        }
        let latched = latch(&fixture.agent_access, parent).await;
        assert_eq!(latched.is_some(), case.expected.latched);
        let after_commit = rows(&fixture.agent_access, &parent.session_id).await;
        let by_doc: RapidHashMap<_, _> = after_commit
            .iter()
            .map(|row| (row["_docID"].as_str().unwrap().to_owned(), row))
            .collect();
        assert_generated_queue_row_metadata(case, &by_doc, &bound);
        let expected_docs: RapidHashSet<_> = std::iter::once(parent.doc_id.clone())
            .chain(bound.values().cloned())
            .collect();
        assert_eq!(
            after_commit
                .iter()
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect::<RapidHashSet<_>>(),
            expected_docs
        );
        assert_eq!(
            after_commit
                .iter()
                .find(|row| row["_docID"] == parent.doc_id)
                .unwrap()["lifecycle_state"],
            "processing"
        );
        let actual_pending: RapidHashSet<_> = after_commit
            .iter()
            .filter(|row| row["lifecycle_state"] == "pending")
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect();
        let actual_terminal: RapidHashSet<_> = after_commit
            .iter()
            .filter(|row| row["lifecycle_state"] == "interrupted")
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            actual_pending,
            case.expected
                .pending
                .iter()
                .map(|id| bound[id].clone())
                .collect()
        );
        assert_eq!(
            actual_terminal,
            case.expected
                .terminal
                .iter()
                .map(|id| bound[id].clone())
                .collect()
        );
        interrupt_request_by_doc_id_with_access(
            &fixture.agent_access,
            &parent.doc_id,
            &parent.node_did,
            parent.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            latch(&fixture.agent_access, parent).await,
            latched,
            "generated replay preserves first latch"
        );
        let after_replay = rows(&fixture.agent_access, &parent.session_id).await;
        let by_doc = |records: Vec<Value>| {
            records
                .into_iter()
                .map(|row| (row["_docID"].as_str().unwrap().to_owned(), row))
                .collect::<RapidHashMap<_, _>>()
        };
        assert_eq!(
            by_doc(after_replay),
            by_doc(after_commit),
            "replay must leave exact physical rows unchanged"
        );
        let messages = fixture
            .agent_access
            .execute(&format!(
                "{{AgentMessage(filter:{{session_id:{{_eq:\"{}\"}}}}){{request_doc_id}}}}",
                escape_graphql_string(&parent.session_id)
            ))
            .await
            .unwrap();
        let mut actual_bindings = messages["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["request_doc_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let mut expected_bindings = bound.values().cloned().collect::<Vec<_>>();
        actual_bindings.sort();
        expected_bindings.sort();
        assert_eq!(
            actual_bindings, expected_bindings,
            "{}: notification facts",
            case.name
        );
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn interrupted_queue_drain_rolls_back_with_latch() {
    let case = crate::lean_vocab_test::lean_interrupt_queue_cases()
        .iter()
        .find(|case| case.name == "completion_before_interrupt")
        .expect("generated pre-interrupt completion case");
    let fixture = fixture(case).await;
    let node = fixture.db.node.clone();
    let parent = &fixture.parent;
    let wake = persist_background_completion_with_message_waking_with_access(
        &fixture.agent_access,
        &node,
        parent,
        "notification",
        "background-completion-notification:rollback:tool",
        "review notifications",
        RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some("background_completion:900".into()),
            queued_after_request_id: Some(parent.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        None,
        crate::request::lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap()
    .request
    .unwrap();
    let (result, count) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(2),
        interrupt_request_by_doc_id_with_access(
            &fixture.agent_access,
            &parent.doc_id,
            &parent.node_did,
            parent.requester_did.as_deref(),
        ),
    )
    .await;
    assert!(result.is_err(), "second transaction mutation must fail");
    assert!(count >= 2, "latch and drain must share one transaction");
    let actual = rows(&fixture.agent_access, &parent.session_id).await;
    let parent_row = actual
        .iter()
        .find(|row| row["_docID"] == parent.doc_id)
        .unwrap();
    let wake_row = actual
        .iter()
        .find(|row| row["_docID"] == wake.doc_id)
        .unwrap();
    assert!(parent_row["interrupt_requested_at"].is_null());
    assert_eq!(wake_row["lifecycle_state"], "pending");
    node.shutdown().await.expect("shut down test node");
}
