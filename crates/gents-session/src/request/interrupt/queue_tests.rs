use super::*;
use crate::request::lifecycle::queue::{
    persist_background_completion_with_message_waking_with_access, QueuePolicy, QueueSource,
    RequestInput, RequestQueue,
};
use crate::request::lifecycle::ExecutionOrigin;
use gents_lean_vocab::families::interrupt_queue_contracts_sequential::{
    Entry as ModeledEntry, Input as ModeledCase, Observation as ModeledObservation,
};
use gents_protocol::row::AgentRequest;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::identity::NodeIdentity;
use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct QueueFixture {
    db: gents_inference::test_support::TestDb,
    user: gents_store::identity::KeyIdentity,
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
        &format!("{{AgentRequest(filter:{{session_id:{{_eq:\"{session}\"}}}}){{_docID request_id node_did requester_did lifecycle_state interrupt_generation input execution_origin}}}}"),
    ).await.unwrap();
    result["data"]["AgentRequest"].as_array().unwrap().clone()
}

async fn latch(access: &ConfigAccess, parent: &AgentRequest) -> Option<String> {
    let observed = observe_request_interrupted_at_with_access(
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
    observed.interrupted_at
}

/// Runs the production claim of every pending request of the session as its
/// working Agent. A claim drains a wake an interrupt covered
/// (`InterruptGeneration.wakeAtClaim`); with the parent still running, every
/// other pending request stays queued.
async fn claim_pending(fixture: &QueueFixture) {
    use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
    let parent = &fixture.parent;
    for row in rows(&fixture.agent_access, &parent.session_id).await {
        if row["lifecycle_state"] != "pending" {
            continue;
        }
        let request = crate::request::binding::load_agent_request_by_doc_id_with_access(
            &fixture.agent_access,
            row["_docID"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let mut lifecycle =
            RequestLifecycle::new_with_node_did(fixture.db.node.clone(), request, 60);
        lifecycle
            .bind_managed_agent(fixture.agent.as_ref())
            .unwrap();
        let outcome = lifecycle.claim_with_identity().await.unwrap();
        assert_ne!(
            outcome,
            ClaimOutcome::Claimed,
            "the parent is still running"
        );
    }
}

struct UnclaimedFixture {
    db: gents_inference::test_support::TestDb,
    user: gents_store::identity::KeyIdentity,
    client: ConfigAccess,
    agent: Arc<dyn NodeIdentity>,
    agent_access: ConfigAccess,
    lifecycle: crate::request::lifecycle::RequestLifecycle,
}

/// A verified client request, pending and unclaimed, in a fresh session.
async fn unclaimed_fixture(name: &str, request_id: String, session: String) -> UnclaimedFixture {
    use crate::request::lifecycle::{RequestIdentity, RequestLifecycle};

    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        &format!("interrupt-queue-{name}"),
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let doc = publish_client_request(
        &client,
        &user,
        RequestIdentity {
            request_id,
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
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
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
    let mut lifecycle = RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    UnclaimedFixture {
        db,
        user,
        client,
        agent,
        agent_access,
        lifecycle,
    }
}

async fn fixture(case: &ModeledCase) -> QueueFixture {
    use crate::request::lifecycle::ClaimOutcome;

    assert_eq!(case.node_id, 1, "fixture maps modeled node 1 to its DID");
    assert_eq!(
        case.requester_id,
        Some(case.node_id),
        "modeled requester scope"
    );
    let UnclaimedFixture {
        db,
        user,
        client,
        agent,
        agent_access,
        mut lifecycle,
    } = unclaimed_fixture(
        &case.name,
        case.active_request_id.unwrap().to_string(),
        case.session_id.to_string(),
    )
    .await;
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
    case: &ModeledCase,
    entry: &ModeledEntry,
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
        crate::request::lifecycle::queue::BackgroundCompletionNotification {
            content: &format!("notification {}", entry.request_id),
            message_key: &format!(
                "background-completion-notification:{}:{}",
                case.name, entry.request_id
            ),
            wake_content: "review notifications",
            queue: RequestQueue {
                delivery: Default::default(),
                position: None,
                source,
                policy,
                key: Some(format!("background_completion:{key}")),
                queued_after_request_id: entry.queued_after.map(|id| id.to_string()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            },
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        },
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
    entry: &ModeledEntry,
    source: QueueSource,
    policy: QueuePolicy,
    origin: ExecutionOrigin,
) -> String {
    let created_at = chrono::DateTime::from_timestamp(entry.created_at as i64, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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
    case: &ModeledCase,
    by_doc: &RapidHashMap<String, &Value>,
    bound: &RapidHashMap<u64, String>,
) {
    for entry in case.inputs.iter().filter_map(|event| event.entry.as_ref()) {
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
    let cases = gents_lean_vocab::families::interrupt_queue_contracts_sequential::cases();
    for modeled in &cases {
        let case = &modeled.input;
        let mut observations = Vec::new();
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
                match (event.kind.as_str(), &event.entry) {
                    ("interrupt", None) => {
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
                    ("enqueue", Some(entry)) => {
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
                    (kind, _) => panic!("{}: no sequential runner for {kind}", case.name),
                }
            }
            claim_pending(&fixture).await;
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
            let mut request_of: RapidHashMap<&String, u64> = RapidHashMap::new();
            for entry in case.inputs.iter().filter_map(|event| event.entry.as_ref()) {
                if let Some(doc) = bound.get(&entry.request_id) {
                    request_of.entry(doc).or_insert(entry.request_id);
                }
            }
            let modeled_ids = |docs: &RapidHashSet<String>| {
                let mut ids = docs
                    .iter()
                    .map(|doc| {
                        *request_of
                            .get(doc)
                            .unwrap_or_else(|| panic!("{}: unbound row {doc}", case.name))
                    })
                    .collect::<Vec<_>>();
                ids.sort();
                ids
            };
            observations.push(ModeledObservation {
                pending: modeled_ids(&physical_pending),
                terminal: modeled_ids(&physical_terminal),
                latched: first_latch.is_some(),
            });
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
        let http = observations.pop().unwrap();
        let local = observations.pop().unwrap();
        assert_eq!(
            serde_json::to_value(&local).unwrap(),
            serde_json::to_value(&http).unwrap(),
            "{}: local and HTTP observations",
            case.name
        );
        modeled.check(Some(local));
    }
    signed_interrupt_rebuilds_after_an_execution_renewal_conflict(&cases[0].input).await;
}

#[tokio::test]
async fn generated_http_overlap_cases_preserve_cutoff() {
    for modeled in gents_lean_vocab::families::interrupt_queue_contracts_overlap::cases() {
        let case: ModeledCase = gents_lean_vocab::family::recode(&modeled.input);
        let case = &case;
        let kinds = case
            .inputs
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        let entries = case
            .inputs
            .iter()
            .filter_map(|event| event.entry.as_ref())
            .collect::<Vec<_>>();
        let (before_capture, during_pause, after_commit) =
            match (kinds.as_slice(), entries.as_slice()) {
                (["capture_interrupt", "enqueue", "commit_interrupt", "interrupt"], [entry]) => {
                    (None, Some(*entry), None)
                }
                (
                    ["enqueue", "capture_interrupt", "commit_interrupt", "enqueue", "interrupt"],
                    [old, later],
                ) => (Some(*old), None, Some(*later)),
                _ => panic!(
                    "{}: generated overlap script changed without a native adapter",
                    case.name
                ),
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
        // A write committed inside the pause can abort the interrupt; like
        // the production owner, it then rebuilds from fresh reads
        // (InterruptLatchContention.tla).
        let interrupt = tokio::spawn(async move {
            access
                .transact_signed_conflict_rebuild("test.http_interrupt_overlap", |txn| {
                    let reached = Arc::clone(&reached_in);
                    let release = Arc::clone(&release_in);
                    let paused = Arc::clone(&paused_in);
                    let filter = &filter;
                    Box::pin(async move {
                        let interrupted = interrupt_request_matching_in_txn(txn, filter).await??;
                        if !paused.swap(true, Ordering::SeqCst) {
                            reached.notify_one();
                            release.notified().await;
                        }
                        Ok::<_, anyhow::Error>(interrupted)
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
        let interrupted = tokio::time::timeout(Duration::from_secs(10), interrupt)
            .await
            .expect("HTTP interrupt must commit after release")
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                interrupted,
                RequestInterrupt::Interrupted { already: false, .. }
            ),
            "{interrupted:?}"
        );
        if let Some(entry) = after_commit {
            bound.insert(
                entry.request_id,
                enqueue_generated_wake(&fixture.agent_access, &node, parent, case, entry).await,
            );
        }
        let latched = latch(&fixture.agent_access, parent).await;
        claim_pending(&fixture).await;
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
        let mut request_of: RapidHashMap<&String, u64> = RapidHashMap::new();
        for entry in case.inputs.iter().filter_map(|event| event.entry.as_ref()) {
            if let Some(doc) = bound.get(&entry.request_id) {
                request_of.entry(doc).or_insert(entry.request_id);
            }
        }
        let modeled_ids = |docs: &RapidHashSet<String>| {
            let mut ids = docs
                .iter()
                .map(|doc| {
                    *request_of
                        .get(doc)
                        .unwrap_or_else(|| panic!("{}: unbound row {doc}", case.name))
                })
                .collect::<Vec<_>>();
            ids.sort();
            ids
        };
        let observation =
            gents_lean_vocab::families::interrupt_queue_contracts_overlap::Observation {
                pending: modeled_ids(&actual_pending),
                terminal: modeled_ids(&actual_terminal),
                latched: latched.is_some(),
            };
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
        modeled.check(Some(observation));
        node.shutdown().await.expect("shut down test node");
    }
}

async fn a_failed_interrupt_covers_nothing(case: &ModeledCase) {
    let fixture = fixture(case).await;
    let node = fixture.db.node.clone();
    let parent = &fixture.parent;
    let wake = persist_background_completion_with_message_waking_with_access(
        &fixture.agent_access,
        &node,
        parent,
        crate::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "notification",
            message_key: "background-completion-notification:rollback:tool",
            wake_content: "review notifications",
            queue: RequestQueue {
                delivery: Default::default(),
                position: None,
                source: QueueSource::BackgroundCompletion,
                policy: QueuePolicy::Coalesce,
                key: Some("background_completion:900".into()),
                queued_after_request_id: Some(parent.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            },
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        },
    )
    .await
    .unwrap()
    .request
    .unwrap();
    let (result, count) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        interrupt_request_by_doc_id_with_access(
            &fixture.agent_access,
            &parent.doc_id,
            &parent.node_did,
            parent.requester_did.as_deref(),
        ),
    )
    .await;
    assert!(result.is_err(), "the marker write must fail");
    assert!(count >= 1, "the interrupt writes the marker");
    assert_eq!(latch(&fixture.agent_access, parent).await, None);
    claim_pending(&fixture).await;
    let actual = rows(&fixture.agent_access, &parent.session_id).await;
    let wake_row = actual
        .iter()
        .find(|row| row["_docID"] == wake.doc_id)
        .unwrap();
    assert_eq!(wake_row["lifecycle_state"], "pending");
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn desktop_user_signed_interrupt_latches_and_drains_the_modeled_wake() {
    for modeled in
        gents_lean_vocab::families::interrupt_queue_contracts_completion_before_interrupt::cases()
    {
        let case: ModeledCase = gents_lean_vocab::family::recode(&modeled.input);
        a_failed_interrupt_covers_nothing(&case).await;
        desktop_interrupt_observation(&case, &modeled).await;
    }
}

async fn desktop_interrupt_observation(
    case: &ModeledCase,
    modeled: &gents_lean_vocab::family::Case<
        gents_lean_vocab::families::interrupt_queue_contracts_completion_before_interrupt::Input,
        Option<gents_lean_vocab::families::interrupt_queue_contracts_completion_before_interrupt::Observation>,
    >,
) {
    let entry = case
        .inputs
        .iter()
        .find_map(|event| event.entry.as_ref())
        .expect("modeled pending wake");
    let fixture = fixture(case).await;
    let parent = &fixture.parent;
    let wake =
        enqueue_generated_wake(&fixture.agent_access, &fixture.db.node, parent, case, entry).await;
    let (interrupt_tx, mut interrupt_rx) = tokio::sync::watch::channel(None);
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let observer = spawn_request_interrupt_observer(
        fixture.db.node.clone(),
        fixture.agent_access.clone(),
        parent,
        interrupt_tx,
        shutdown_rx,
    );

    interrupt_request_by_doc_id_with_access(
        &fixture.client,
        &parent.doc_id,
        &parent.node_did,
        parent.requester_did.as_deref(),
    )
    .await
    .unwrap();
    let first = latch(&fixture.agent_access, parent).await;
    assert_eq!(
        request_interrupted_at_by_doc_id_with_access(
            &gents_store::test_support::unrelated_document_reader(&fixture.db.node).await,
            &parent.doc_id
        )
        .await
        .unwrap(),
        None,
        "an unrelated reader cannot observe the User-owned request"
    );
    assert_eq!(
        request_interrupted_at_by_doc_id_with_access(&fixture.agent_access, &parent.doc_id)
            .await
            .unwrap(),
        first,
        "the final latch check must use the selected Agent"
    );
    let intent = tokio::time::timeout(
        Duration::from_secs(5),
        interrupt_rx.wait_for(|intent| intent.is_some()),
    )
    .await
    .expect("the selected Agent must observe the interrupt")
    .unwrap()
    .clone()
    .unwrap();
    assert_eq!(
        intent.at,
        chrono::DateTime::parse_from_rfc3339(first.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc)
    );
    observer.await.unwrap();
    claim_pending(&fixture).await;
    let after = rows(&fixture.agent_access, &parent.session_id).await;
    let state = &after.iter().find(|row| row["_docID"] == wake).unwrap()["lifecycle_state"];
    let observation =
        gents_lean_vocab::families::interrupt_queue_contracts_completion_before_interrupt::Observation {
            pending: if state == "pending" { vec![entry.request_id] } else { Vec::new() },
            terminal: if state == "interrupted" { vec![entry.request_id] } else { Vec::new() },
            latched: first.is_some(),
        };

    interrupt_request_by_doc_id_with_access(
        &fixture.client,
        &parent.doc_id,
        &parent.node_did,
        parent.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(latch(&fixture.agent_access, parent).await, first);
    modeled.check(Some(observation));
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

async fn signed_interrupt_rebuilds_after_an_execution_renewal_conflict(case: &ModeledCase) {
    let fixture = fixture(case).await;
    let access = fixture.db.client_access(fixture.agent.as_ref());
    let parent = &fixture.parent;
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let interrupt = ConfigApplyTxn::with_successful_mutation_pause_and_count(
        1,
        Arc::clone(&reached),
        Arc::clone(&release),
        interrupt_request_by_doc_id_with_access(
            &access,
            &parent.doc_id,
            &parent.node_did,
            parent.requester_did.as_deref(),
        ),
    );
    let stream = async {
        tokio::time::timeout(Duration::from_secs(10), reached.notified())
            .await
            .unwrap();
        let physical = escape_graphql_string(&parent.doc_id);
        fixture.agent_access.transact_write("test.interrupt.execution_renewal_overlap", |txn| {
            let physical = &physical;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: AgentRequestMutationInputArg!) {{ update_AgentRequest(filter: {{_docID: {{_eq: "{physical}"}}}}, input: $input) {{_docID}} }}"#),
                    &json!({"input": {"execution_lease_expires_at": "2026-12-01T00:00:00Z"}}),
                    gents_protocol::schemas::AGENT_REQUEST,
                ).await
            })
        }).await.unwrap();
        release.notify_one();
    };
    let ((result, paused, mutations), ()) = tokio::join!(interrupt, stream);
    result.unwrap();
    assert!(paused);
    assert!(
        mutations >= 2,
        "confirmed conflict must rebuild the signed latch"
    );
    assert!(latch(&fixture.agent_access, parent).await.is_some());
    let observed = fixture
        .agent_access
        .execute(&format!(
            r#"{{AgentRequest(filter:{{_docID:{{_eq:"{}"}}}}){{execution_lease_expires_at}}}}"#,
            escape_graphql_string(&parent.doc_id),
        ))
        .await
        .unwrap();
    assert_eq!(
        observed["data"]["AgentRequest"][0]["execution_lease_expires_at"],
        "2026-12-01T00:00:00Z"
    );
}

/// Runs `write` once `future`'s transaction has read everything it reads and
/// is about to commit, so `write` commits first. Returns whether that point
/// was reached.
async fn with_write_before_commit<F, W>(future: F, write: W) -> (F::Output, bool)
where
    F: std::future::Future,
    W: std::future::Future<Output = ()>,
{
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let paused = Arc::new(AtomicBool::new(false));
    let owner = std::pin::pin!(ConfigApplyTxn::with_embedded_before_commit_pause(
        Arc::clone(&reached),
        Arc::clone(&release),
        future,
    ));
    let driver = std::pin::pin!({
        let paused = Arc::clone(&paused);
        async move {
            reached.notified().await;
            paused.store(true, Ordering::SeqCst);
            write.await;
            release.notify_one();
        }
    });
    let (mut owner, mut driver) = (owner, driver);
    let mut driven = false;
    loop {
        tokio::select! {
            output = &mut owner => return (output, paused.load(Ordering::SeqCst)),
            () = &mut driver, if !driven => driven = true,
        }
    }
}

/// The production interrupt owner and a background completion of the same
/// session race: the wake commits after the interrupt's pending scan and
/// before its commit. The interrupt covers every wake committed before it
/// commits (`InterruptCases`, `empty_scan_completion_before_latch_commit`),
/// so the wake is drained, not left to run after the interrupt.
#[tokio::test]
async fn an_interrupt_drains_a_wake_committed_before_it_commits() {
    let case: ModeledCase = serde_json::from_value(json!({
        "name": "wake-race", "node_id": 1, "requester_id": 1, "session_id": 900,
        "active_request_id": 100,
        "inputs": [{"kind": "enqueue", "entry": {
            "request_id": 301, "created_at": 10, "execution_origin": "scheduled",
            "source": "background_completion", "policy": "coalesce",
            "queue_key": 900, "queued_after": null}}]
    }))
    .unwrap();
    let entry = case
        .inputs
        .iter()
        .find_map(|event| event.entry.as_ref())
        .expect("modeled wake");
    let fixture = fixture(&case).await;
    let node = fixture.db.node.clone();
    let parent = &fixture.parent;
    let wake = std::sync::Mutex::new(None);
    let (interrupted, paused) = with_write_before_commit(
        interrupt_request_by_doc_id_with_access(
            &fixture.agent_access,
            &parent.doc_id,
            &parent.node_did,
            parent.requester_did.as_deref(),
        ),
        async {
            let doc =
                enqueue_generated_wake(&fixture.agent_access, &node, parent, &case, entry).await;
            *wake.lock().unwrap() = Some(doc);
        },
    )
    .await;
    assert!(paused, "the wake must commit inside the interrupt");
    interrupted.unwrap();
    let wake = wake.into_inner().unwrap().expect("wake published");
    claim_pending(&fixture).await;
    let after = rows(&fixture.agent_access, &parent.session_id).await;
    let state = |doc: &str| {
        after
            .iter()
            .find(|row| row["_docID"] == doc)
            .map(|row| row["lifecycle_state"].clone())
    };
    assert!(latch(&fixture.agent_access, parent).await.is_some());
    assert_eq!(state(&wake), Some(json!("interrupted")), "{after:?}");
    node.shutdown().await.expect("shut down test node");
}

/// The production session interrupt and a claim of the session's pending
/// request race: the claim commits after the interrupt's active-request scan
/// and before its commit. The interrupt covers the request claimed before it
/// commits, so the claimed request carries the latch.
#[tokio::test]
async fn a_session_interrupt_latches_a_request_claimed_before_it_commits() {
    use crate::request::lifecycle::ClaimOutcome;

    let UnclaimedFixture {
        db,
        agent_access,
        mut lifecycle,
        ..
    } = unclaimed_fixture("claim-race", "500".into(), "901".into()).await;
    let request = lifecycle.request().clone();
    let scope = gents_protocol::session::SessionScope {
        node_did: request.node_did.clone(),
        session_id: request.session_id.clone(),
        requester_did: request.requester_did.clone(),
    };
    let claimed = std::sync::Mutex::new(None);
    let (interrupted, paused) = with_write_before_commit(
        crate::session::message::interrupt_session_with_access(&db.node, &agent_access, &scope),
        async {
            let outcome = lifecycle.claim_with_identity().await;
            *claimed.lock().unwrap() = Some(outcome);
        },
    )
    .await;
    assert!(paused, "the claim must commit inside the interrupt");
    assert_eq!(
        claimed.into_inner().unwrap().expect("claim ran").unwrap(),
        ClaimOutcome::Claimed
    );
    interrupted.unwrap();
    assert!(
        latch(&agent_access, &request).await.is_some(),
        "the interrupt reaches the request claimed before it committed"
    );
    db.node.shutdown().await.expect("shut down test node");
}

/// The session interrupt writes only its marker (`InterruptGeneration`,
/// `InterruptCoverage.tla` `InterruptNotPrevented`): with the interrupt read
/// and about to commit, the session's running turn reaches its terminal,
/// wakes are published into the session and claims of its pending requests
/// run, and the interrupt still commits. The wakes committed before it are
/// drained when claimed.
#[tokio::test]
async fn an_interrupt_commits_over_concurrent_claims_wakes_and_terminals() {
    use crate::request::lifecycle::RequestTerminalOutcome;
    let case: ModeledCase = serde_json::from_value(json!({
        "name": "concurrent", "node_id": 1, "requester_id": 1, "session_id": 902,
        "active_request_id": 102, "inputs": []
    }))
    .unwrap();
    let mut fixture = fixture(&case).await;
    let node = fixture.db.node.clone();
    let parent = fixture.parent.clone();
    let scope = gents_protocol::session::SessionScope {
        node_did: parent.node_did.clone(),
        session_id: parent.session_id.clone(),
        requester_did: parent.requester_did.clone(),
    };
    let wakes = (0..2_u64)
        .map(|key| {
            serde_json::from_value::<ModeledEntry>(json!({
                "request_id": 310 + key, "created_at": 10, "execution_origin": "scheduled",
                "source": "background_completion", "policy": "coalesce",
                "queue_key": 910 + key, "queued_after": null
            }))
            .unwrap()
        })
        .collect::<Vec<_>>();
    let first =
        enqueue_generated_wake(&fixture.agent_access, &node, &parent, &case, &wakes[0]).await;
    let published = std::sync::Mutex::new(Vec::new());
    let agent_access = fixture.agent_access.clone();
    let lifecycle = &mut fixture._lifecycle;
    let (interrupted, paused) = with_write_before_commit(
        crate::session::message::interrupt_session_with_access(&node, &agent_access, &scope),
        async {
            let second =
                enqueue_generated_wake(&agent_access, &node, &parent, &case, &wakes[1]).await;
            published.lock().unwrap().push(second);
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Completed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        },
    )
    .await;
    assert!(paused, "the other writes must commit inside the interrupt");
    interrupted.expect("nothing but another interrupt aborts the interrupt");
    claim_pending(&fixture).await;
    let after = rows(&fixture.agent_access, &parent.session_id).await;
    let state = |doc: &str| {
        after
            .iter()
            .find(|row| row["_docID"] == doc)
            .map(|row| row["lifecycle_state"].clone())
    };
    assert_eq!(state(&parent.doc_id), Some(json!("completed")), "{after:?}");
    for wake in std::iter::once(first).chain(published.into_inner().unwrap()) {
        assert_eq!(state(&wake), Some(json!("interrupted")), "{after:?}");
    }
    node.shutdown().await.expect("shut down test node");
}

/// A session's marker is read in the session's requester scope, so a request
/// whose requester is not its session's would record no generation and a
/// session interrupt could not cover it. Client submission refuses such a
/// request: it is never created.
#[tokio::test]
async fn a_request_whose_requester_is_not_its_sessions_is_refused() {
    use crate::request::lifecycle::RequestIdentity;
    let UnclaimedFixture {
        db,
        user,
        client,
        agent,
        ..
    } = unclaimed_fixture(
        "requester-mismatch",
        "1".into(),
        "requester-mismatch".into(),
    )
    .await;
    let mut spec = crate::request::lifecycle::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        RequestIdentity {
            request_id: "2".into(),
            node_did: db.node_identity.did().to_owned(),
            requester_did: Some(agent.did().to_owned()),
            agent_did: agent.did().to_owned(),
            session_id: "requester-mismatch".into(),
            content: "another requester".into(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: "2026-09-01T00:00:01Z".into(),
        },
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
    );
    spec.input = RequestInput::default();
    let submitted = match crate::request::lifecycle::build_signed_client_request(
        &client,
        spec,
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    {
        Ok(create) => crate::request::lifecycle::submit_client_request(&client, &create)
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from),
        Err(error) => Err(error),
    };
    assert!(submitted.is_err(), "{submitted:?}");
    let rows = db
        .client_access(agent.as_ref())
        .execute(r#"{ AgentRequest(filter: { request_id: { _eq: "2" } }) { _docID } }"#)
        .await
        .unwrap();
    assert_eq!(rows["data"]["AgentRequest"], json!([]));
}
