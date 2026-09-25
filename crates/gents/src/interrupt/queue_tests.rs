use super::*;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::lean_vocab_test::{
    LeanInterruptQueueCase, LeanInterruptQueueEntry, LeanInterruptQueueInput,
};
use crate::lifecycle::queue::{
    persist_background_completion_with_message, QueuePolicy, QueueSource, RequestInput,
    RequestQueue,
};
use crate::lifecycle::ExecutionOrigin;
use crate::watcher::AgentRequest;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

const BEHAVIOR: &str = "general";

fn free_http_address() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

async fn wait_http_ready(access: &ConfigAccess) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if access
                .execute("query { AgentRequest { _docID } }")
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("real embedded HTTP GraphQL endpoint must become ready");
}

async fn insert_request(node: &EmbeddedNode, input: Value) -> String {
    ConfigAccess::transact_local(node, None, "test.interrupt_queue.insert", |txn| {
        let input = input.clone();
        Box::pin(async move {
            let result = txn
                .execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": input}),
                )
                .await?;
            gents_protocol::graphql::extract_mutation_doc_id(&result, "AgentRequest")
        })
    })
    .await
    .unwrap()
}

async fn rows(node: &EmbeddedNode, session: &str) -> Vec<Value> {
    let session = escape_graphql_string(session);
    let result = graphql_with_transaction_retry(
        node,
        &format!("{{AgentRequest(filter:{{session_id:{{_eq:\"{session}\"}}}}){{_docID request_id agent_did requester_did lifecycle_state interrupt_requested_at input execution_origin}}}}"),
        "interrupt queue test rows",
    )
    .await
    .unwrap();
    result.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone()
}

async fn fixture(
    case: &LeanInterruptQueueCase,
) -> (Arc<EmbeddedNode>, tempfile::TempDir, AgentRequest) {
    fixture_with_http(case, None).await
}

async fn fixture_with_http(
    case: &LeanInterruptQueueCase,
    http_address: Option<std::net::SocketAddr>,
) -> (Arc<EmbeddedNode>, tempfile::TempDir, AgentRequest) {
    assert_eq!(case.agent_id, 1, "fixture maps modeled agent 1 to its DID");
    assert_eq!(
        case.requester_id,
        Some(case.agent_id),
        "runtime wake requester must share the modeled scope"
    );
    let dir = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
    let did = identity.did().to_owned();
    let builder = EmbeddedNode::builder()
        .data_path(dir.path().join("db"))
        .with_node_identity_did(&did);
    let builder = match http_address {
        Some(address) => builder.with_http(defra_node::HttpConfig::with_addr(address)),
        None => builder,
    };
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, &did, BEHAVIOR).await;
    let session = case.session_id.to_string();
    // Runtime-signed background wakes use the signing DID as requester. The
    // generated agent/requester identity pair maps to that same native DID.
    crate::session::ensure_session_with_behavior_id_and_requester_did(
        &node,
        &session,
        BEHAVIOR,
        &did,
        BEHAVIOR,
        Some(&did),
    )
    .await
    .unwrap();
    let active = case.active_request_id.unwrap().to_string();
    let doc = insert_request(
        &node,
        json!({"request_id":active,"agent_did":did,"requester_did":did,
            "session_id":session,"behavior_id":BEHAVIOR,"content":"active",
            "lifecycle_state":"processing","execution_origin":"interactive",
            "created_at":"2026-09-01T00:00:00Z"}),
    )
    .await;
    let parent = AgentRequest {
        doc_id: doc,
        request_id: active,
        agent_did: did.clone(),
        requester_did: Some(did),
        behavior_id: BEHAVIOR.into(),
        session_id: session,
        content: "active".into(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: Some("interactive".into()),
        created_at: "2026-09-01T00:00:00Z".into(),
        deadline: None,
        execution_generation: None,
        execution_lease_secs: None,
        execution_lease_expires_at: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_agent_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    };
    (node, dir, parent)
}

async fn enqueue_generated_wake(
    node: &EmbeddedNode,
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
    persist_background_completion_with_message(
        node,
        parent,
        &format!("notification {}", entry.request_id),
        &format!(
            "background-completion-notification:{}:{}",
            case.name, entry.request_id
        ),
        "review notifications",
        RequestQueue {
            source,
            policy,
            key: Some(format!("background_completion:{key}")),
            queued_after_request_id: entry.queued_after.map(|id| id.to_string()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        None,
    )
    .await
    .unwrap()
    .request
    .expect("background wake request")
    .doc_id
}

async fn insert_generated_pending(
    node: &EmbeddedNode,
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
    insert_request(
        node,
        json!({"request_id":entry.request_id.to_string(),"agent_did":parent.agent_did,
            "requester_did":parent.requester_did,"session_id":parent.session_id,
            "behavior_id":BEHAVIOR,"content":"generated pending fixture",
            "lifecycle_state":"pending","execution_origin":origin.as_str(),
            "input":input,"created_at":created_at}),
    )
    .await
}

fn assert_generated_queue_row_metadata(
    case: &LeanInterruptQueueCase,
    by_doc: &HashMap<String, &Value>,
    bound: &HashMap<u64, String>,
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
    let mut overlap_cases = HashSet::new();
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
            let address = use_http.then(free_http_address);
            let (node, _dir, parent) = fixture_with_http(case, address).await;
            let access = match address {
                Some(address) => {
                    let access = ConfigAccess::Graphql(format!("http://{address}/api/v0/graphql"));
                    wait_http_ready(&access).await;
                    access
                }
                None => ConfigAccess::Local(node.clone()),
            };
            let mut bound: HashMap<u64, String> = HashMap::new();
            let mut notification_bindings = Vec::new();
            let mut first_latch = None;
            for event in &case.inputs {
                match event {
                    LeanInterruptQueueInput::Interrupt => {
                        interrupt_request_by_doc_id_with_access(
                            &access,
                            &parent.doc_id,
                            &parent.agent_did,
                            parent.requester_did.as_deref(),
                        )
                        .await
                        .unwrap();
                        let latch = fetch_interrupt_requested_at_by_doc_id(&node, &parent.doc_id)
                            .await
                            .unwrap();
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
                                let doc = enqueue_generated_wake(&node, &parent, case, entry).await;
                                notification_bindings.push(doc.clone());
                                doc
                            }
                            (
                                ExecutionOrigin::Interactive,
                                QueueSource::User | QueueSource::BackgroundCompletion,
                            ) => {
                                insert_generated_pending(
                                    &node, &parent, entry, source, policy, origin,
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
            let actual = rows(&node, &parent.session_id).await;
            let by_doc: HashMap<_, _> = actual
                .iter()
                .map(|row| (row["_docID"].as_str().unwrap().to_owned(), row))
                .collect();
            let expected_docs: HashSet<_> = std::iter::once(parent.doc_id.clone())
                .chain(bound.values().cloned())
                .collect();
            assert_eq!(
                by_doc.keys().cloned().collect::<HashSet<_>>(),
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
            let physical_pending: HashSet<_> = actual
                .iter()
                .filter(|row| row["_docID"] != parent.doc_id && row["lifecycle_state"] == "pending")
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect();
            let physical_terminal: HashSet<_> = actual
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
            let messages = graphql_with_transaction_retry(
                &node,
                &format!(
                "{{AgentMessage(filter:{{session_id:{{_eq:\"{}\"}}}}){{_docID request_doc_id}}}}",
                escape_graphql_string(&parent.session_id)
            ),
                "interrupt queue notification facts",
            )
            .await
            .unwrap();
            let data = messages.data.unwrap();
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
            node.shutdown().await;
        }
    }
    assert_eq!(
        overlap_cases,
        HashSet::from([
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
            .collect::<HashSet<_>>(),
        HashSet::from([
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
        let address = free_http_address();
        let (node, _dir, parent) = fixture_with_http(case, Some(address)).await;
        let endpoint = format!("http://{address}/api/v0/graphql");
        let access = ConfigAccess::Graphql(endpoint);
        wait_http_ready(&access).await;
        let mut bound = HashMap::new();
        if let Some(entry) = before_capture {
            bound.insert(
                entry.request_id,
                enqueue_generated_wake(&node, &parent, case, entry).await,
            );
        }

        let reached = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let paused = Arc::new(AtomicBool::new(false));
        let filter = exact_request_filter(
            &parent.doc_id,
            &parent.agent_did,
            parent.requester_did.as_deref(),
        )
        .unwrap();
        let reached_in = Arc::clone(&reached);
        let release_in = Arc::clone(&release);
        let paused_in = Arc::clone(&paused);
        let expected_drained = case.expected.terminal.len();
        let interrupt = tokio::spawn(async move {
            access
                .transact("test.http_interrupt_overlap", |txn| {
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
            fetch_interrupt_requested_at_by_doc_id(&node, &parent.doc_id)
                .await
                .unwrap(),
            None,
            "staged HTTP latch must remain invisible before commit"
        );
        if let Some(entry) = during_pause {
            let doc = tokio::time::timeout(
                Duration::from_secs(10),
                enqueue_generated_wake(&node, &parent, case, entry),
            )
            .await
            .expect("embedded wake publication must commit while HTTP interrupt is open");
            bound.insert(entry.request_id, doc);
        }
        let before_commit = rows(&node, &parent.session_id).await;
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
                enqueue_generated_wake(&node, &parent, case, entry).await,
            );
        }
        let latched = fetch_interrupt_requested_at_by_doc_id(&node, &parent.doc_id)
            .await
            .unwrap();
        assert_eq!(latched.is_some(), case.expected.latched);
        let after_commit = rows(&node, &parent.session_id).await;
        let by_doc: HashMap<_, _> = after_commit
            .iter()
            .map(|row| (row["_docID"].as_str().unwrap().to_owned(), row))
            .collect();
        assert_generated_queue_row_metadata(case, &by_doc, &bound);
        let expected_docs: HashSet<_> = std::iter::once(parent.doc_id.clone())
            .chain(bound.values().cloned())
            .collect();
        assert_eq!(
            after_commit
                .iter()
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect::<HashSet<_>>(),
            expected_docs
        );
        assert_eq!(
            after_commit
                .iter()
                .find(|row| row["_docID"] == parent.doc_id)
                .unwrap()["lifecycle_state"],
            "processing"
        );
        let actual_pending: HashSet<_> = after_commit
            .iter()
            .filter(|row| row["lifecycle_state"] == "pending")
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect();
        let actual_terminal: HashSet<_> = after_commit
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
        interrupt_request_by_doc_id(
            &node,
            &parent.doc_id,
            &parent.agent_did,
            parent.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            fetch_interrupt_requested_at_by_doc_id(&node, &parent.doc_id)
                .await
                .unwrap(),
            latched,
            "generated replay preserves first latch"
        );
        let after_replay = rows(&node, &parent.session_id).await;
        let by_doc = |records: Vec<Value>| {
            records
                .into_iter()
                .map(|row| (row["_docID"].as_str().unwrap().to_owned(), row))
                .collect::<HashMap<_, _>>()
        };
        assert_eq!(
            by_doc(after_replay),
            by_doc(after_commit),
            "replay must leave exact physical rows unchanged"
        );
        let messages = graphql_with_transaction_retry(
            &node,
            &format!(
                "{{AgentMessage(filter:{{session_id:{{_eq:\"{}\"}}}}){{request_doc_id}}}}",
                escape_graphql_string(&parent.session_id)
            ),
            "HTTP overlap notification bindings",
        )
        .await
        .unwrap();
        let mut actual_bindings = messages.data.unwrap()["AgentMessage"]
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
        node.shutdown().await;
    }
}

#[tokio::test]
async fn interrupted_queue_drain_rolls_back_with_latch() {
    let case = crate::lean_vocab_test::lean_interrupt_queue_cases()
        .iter()
        .find(|case| case.name == "completion_before_interrupt")
        .expect("generated pre-interrupt completion case");
    let (node, _dir, parent) = fixture(case).await;
    let wake = persist_background_completion_with_message(
        &node,
        &parent,
        "notification",
        "background-completion-notification:rollback:tool",
        "review notifications",
        RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some("background_completion:900".into()),
            queued_after_request_id: Some(parent.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        None,
    )
    .await
    .unwrap()
    .request
    .unwrap();
    let (result, count) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(2),
        interrupt_request_by_doc_id(
            &node,
            &parent.doc_id,
            &parent.agent_did,
            parent.requester_did.as_deref(),
        ),
    )
    .await;
    assert!(result.is_err(), "second transaction mutation must fail");
    assert!(count >= 2, "latch and drain must share one transaction");
    let actual = rows(&node, &parent.session_id).await;
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
    node.shutdown().await;
}
