use std::time::{Duration, Instant};

use gents::request::lifecycle::{prepare_pending_user_edit, PendingMessageEdit, PendingQueueEdit};
use gents::runtime::agent::p2p_reconcile::session_commands::run_session_command_reconciler;
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents::NodeIdentity as _;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
use gents_protocol::session_input_edit::{
    SessionInputEdit, SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};
use rapidhash::RapidHashMap;
use tokio_util::sync::CancellationToken;

use crate::support::{self, TestDb};

struct Stop(CancellationToken);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn rows_as(
    access: &ConfigAccess,
    collection: &str,
    filter: &str,
    fields: &str,
) -> Vec<serde_json::Value> {
    access
        .execute(&format!(
            "{{ {collection}(filter: {filter}) {{ {fields} }} }}"
        ))
        .await
        .unwrap()["data"][collection]
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_row_as(
    access: &ConfigAccess,
    collection: &str,
    field: &str,
    value: &str,
    fields: &str,
    required: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = rows_as(
            access,
            collection,
            &format!(
                "{{ {field}: {{ _eq: \"{}\" }} }}",
                escape_graphql_string(value)
            ),
            fields,
        )
        .await;
        if let Some(row) = found
            .into_iter()
            .find(|row| required.is_empty() || !row[required].is_null())
        {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "{collection}.{field}={value} did not appear with {required}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn signed_edit_targets(
    db: &TestDb,
    signer: &dyn gents::store::identity::NodeIdentity,
    node_did: &str,
    session_id: &str,
    replacements: &[AgentRequestCreate],
) -> (String, String, String, Vec<serde_json::Value>) {
    let access =
        ConfigAccess::local_as(db.node.clone(), ::identity::Did::new(signer.did()).unwrap());
    let (session_doc_id, agent_did, agent_doc_id) = access
        .transact_readonly("test.input_edit.targets", |txn| {
            Box::pin(async move {
                let session =
                    gents::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                    .expect("session exists");
                let (agent_doc, _) = gents::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Agent,
                    node_did,
                    &session.session.agent_did,
                )
                .await?
                .expect("selected Agent exists");
                Ok::<_, anyhow::Error>((session.doc_id, session.session.agent_did, agent_doc))
            })
        })
        .await
        .unwrap();
    let mut evidence = Vec::with_capacity(replacements.len());
    for replacement in replacements {
        evidence.push(
            gents::store::config_client::prepare_request_evidence(
                signer,
                gents::store::config_client::client_request_input(replacement).unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    (session_doc_id, agent_did, agent_doc_id, evidence)
}

fn model_ids(values: &serde_json::Value) -> Vec<u64> {
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap())
        .collect()
}

fn lean_edit_case(name: &str) -> serde_json::Value {
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    contract["session_input_edit_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("Lean input-edit case {name}"))
        .clone()
}

/// One queued session seeded by the enrolled User through the signed client
/// endpoint, with the Lean model's request IDs bound to native identities.
struct SeededSession {
    session_id: String,
    request_ids: RapidHashMap<u64, String>,
    doc_ids: RapidHashMap<u64, String>,
}

/// The P2P edit route is gone: every edit is published by its author through
/// the signed client endpoint, and concurrency is two authors racing there.
struct EndpointEdits {
    db: TestDb,
    user: gents::KeyIdentity,
    client: ConfigAccess,
    agent_did: String,
    agent_doc: String,
}

impl EndpointEdits {
    async fn start(name: &str) -> Self {
        let db = support::test_db(name).await;
        let support::ManagedAgentFixture {
            user,
            access: client,
            agent,
            ..
        } = support::managed_agent_fixture(&db).await;
        let did = db.node_identity.did().to_owned();
        let agent_did = agent.did().to_owned();
        let agent_doc = client
            .transact_readonly("test.input_edit.agent", |txn| {
                let did = &did;
                let agent_did = &agent_did;
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            gents::Collection::Agent,
                            did,
                            agent_did,
                        )
                        .await?
                        .expect("fixture Agent")
                        .0,
                    )
                })
            })
            .await
            .unwrap();
        Self {
            db,
            user,
            client,
            agent_did,
            agent_doc,
        }
    }

    fn node_did(&self) -> &str {
        self.db.node_identity.did()
    }

    async fn reconciler(&self) -> (Stop, tokio::task::JoinHandle<()>) {
        let cancel = CancellationToken::new();
        let snapshot = support::fixtures::session_command_agent_snapshot(&self.db).await;
        let task = tokio::spawn(run_session_command_reconciler(
            self.db.node.clone(),
            self.db.node_identity.clone(),
            snapshot,
            cancel.clone(),
        ));
        let task = tokio::spawn(async move { task.await.unwrap().unwrap() });
        (Stop(cancel), task)
    }

    /// Seed the model's pending entries, in order, as the User's own signed
    /// requests. The model's active request is runtime state the endpoint does
    /// not author; the queue owner's unit conformance binds it.
    async fn seed(&self, session_id: &str, pending: &[serde_json::Value]) -> SeededSession {
        let mut seeded = SeededSession {
            session_id: session_id.into(),
            request_ids: RapidHashMap::default(),
            doc_ids: RapidHashMap::default(),
        };
        let mut session_doc = None;
        for (arrival, entry) in pending.iter().enumerate() {
            let id = entry["request_id"].as_u64().unwrap();
            assert_eq!(entry["requester_id"], 1, "requester 1 is the enrolled User");
            assert_eq!(entry["fresh"], true);
            let request_id = format!("{session_id}-{id}");
            let mut create = AgentRequestCreate::base(
                RequestPurpose::Normal,
                request_id.clone(),
                self.node_did(),
                self.user.did(),
                &self.agent_did,
                session_id,
                format!("message {id}"),
                entry["execution_origin"].as_str().unwrap(),
                (chrono::Utc::now() - chrono::Duration::minutes(1)
                    + chrono::Duration::seconds(arrival as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                AgentRequestAdmissionRecord::client(self.user.did()),
            );
            create.input.queue = Some(RequestQueue {
                source: serde_json::from_value(entry["source"].clone()).unwrap(),
                policy: serde_json::from_value(entry["policy"].clone()).unwrap(),
                delivery: serde_json::from_value(entry["delivery"].clone()).unwrap(),
                position: None,
                key: None,
                queued_after_request_id: entry["queued_after"]
                    .as_u64()
                    .map(|after| format!("{session_id}-{after}")),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
            if session_doc.is_none() {
                session_doc = Some(
                    gents::request::lifecycle::reserve_goal_session(&self.client, &create)
                        .await
                        .unwrap(),
                );
            }
            create.session_doc_id = session_doc.clone();
            create.agent_doc_id = Some(self.agent_doc.clone());
            gents::sign_agent_request_create(&self.user, &mut create)
                .await
                .unwrap();
            gents::request::lifecycle::submit_client_request(&self.client, &create)
                .await
                .unwrap();
            let row = wait_row_as(
                &self.client,
                "AgentRequest",
                "request_id",
                &request_id,
                "_docID",
                "",
            )
            .await;
            seeded.request_ids.insert(id, request_id);
            seeded
                .doc_ids
                .insert(id, row["_docID"].as_str().unwrap().to_owned());
        }
        seeded
    }

    /// Build the case's signed command for `seeded`, tampering only where the
    /// model's authority says the evidence is invalid.
    async fn command(
        &self,
        case: &serde_json::Value,
        seeded: &mut SeededSession,
        command_id: &str,
    ) -> SessionInputEdit {
        let model = &case["command"];
        let authority = &case["authority"];
        let pending = case["initial"]["queue"]["pending"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["request_id"].as_u64().unwrap())
            .collect::<Vec<_>>();
        let (offset, count) = (
            model["offset"].as_u64().unwrap() as usize,
            model["count"].as_u64().unwrap() as usize,
        );
        let selected = pending.iter().skip(offset).take(count).collect::<Vec<_>>();
        let replacements = model["replacements"].as_array().unwrap();
        let mut edit = PendingQueueEdit {
            expected_request_doc_ids: pending.iter().map(|id| seeded.doc_ids[id].clone()).collect(),
            selected_request_doc_ids: selected.iter().map(|id| seeded.doc_ids[id].clone()).collect(),
            messages: replacements
                .iter()
                .zip(&selected)
                .map(|(replacement, id)| PendingMessageEdit {
                    authored_input: None,
                    request_doc_id: seeded.doc_ids[id].clone(),
                    content: format!("message {}", replacement["request_id"]),
                })
                .collect(),
        };
        let mut native = prepare_pending_user_edit(
            &self.client,
            &self.user,
            AgentRequestAdmissionRecord::client(self.user.did()),
            self.node_did(),
            &seeded.session_id,
            self.user.did(),
            &edit,
        )
        .await
        .unwrap();
        edit.expected_request_doc_ids = model_ids(&model["expected"])
            .iter()
            .map(|id| seeded.doc_ids[id].clone())
            .collect();
        if authority["replacements_valid"] == false {
            native[0].admission.signature[0] ^= 1;
        }
        for (replacement, create) in replacements.iter().zip(&native) {
            seeded.request_ids.insert(
                replacement["request_id"].as_u64().unwrap(),
                create.request_id.clone(),
            );
        }
        let now = case["now"].as_u64().unwrap();
        let instant = |value: &serde_json::Value| {
            let value = value.as_u64().unwrap();
            let offset = chrono::Duration::seconds(value as i64);
            if value <= now {
                chrono::Utc::now() - chrono::Duration::hours(1) + offset
            } else {
                chrono::Utc::now() + chrono::Duration::hours(1) + offset
            }
        };
        let (session_doc_id, agent_did, agent_doc_id, replacement_evidence) = signed_edit_targets(
            &self.db,
            &self.user,
            self.node_did(),
            &seeded.session_id,
            &native,
        )
        .await;
        let mut command = SessionInputEdit {
            version: SESSION_INPUT_EDIT_VERSION,
            command_id: command_id.into(),
            requester_did: self.user.did().into(),
            requester_peer_id: None,
            node_did: self.node_did().into(),
            session_id: if authority["session_owned"] == true {
                seeded.session_id.clone()
            } else {
                format!("{}-foreign", seeded.session_id)
            },
            session_doc_id,
            agent_did,
            agent_doc_id,
            issued_at: instant(&model["issued_at"]).to_rfc3339(),
            expires_at: instant(&model["expires_at"]).to_rfc3339(),
            edit,
            replacements: native,
            replacement_evidence,
            signature: Vec::new(),
        };
        command.signature = self.user.sign(&command.signing_payload()).await.unwrap();
        if authority["signature_valid"] == false {
            command.signature[0] ^= 1;
        }
        command
    }

    async fn publish(&self, command: &SessionInputEdit) -> anyhow::Result<String> {
        gents::request::lifecycle::publish_edit_command(&self.client, command).await
    }

    async fn command_rows(&self, command_id: &str) -> Vec<serde_json::Value> {
        rows_as(
            &self.client,
            "AgentSessionInputEdit",
            &format!(
                "{{ command_id: {{ _eq: \"{}\" }} }}",
                escape_graphql_string(command_id)
            ),
            "_docID session_id intent_json receipt_json",
        )
        .await
    }

    async fn receipt(&self, command: &SessionInputEdit) -> (String, SessionInputEditReceipt) {
        let row = wait_row_as(
            &self.client,
            "AgentSessionInputEdit",
            "command_id",
            &command.command_id,
            "receipt_json",
            "receipt_json",
        )
        .await;
        let raw = row["receipt_json"].as_str().unwrap().to_owned();
        let receipt: SessionInputEditReceipt = serde_json::from_str(&raw).unwrap();
        receipt.validate_for(command).unwrap();
        assert!(self
            .db
            .node_identity
            .verify(
                self.node_did(),
                &receipt.signing_payload(),
                &receipt.signature
            )
            .await
            .unwrap());
        (raw, receipt)
    }

    async fn pending(&self, session_id: &str) -> Vec<String> {
        gents::request::lifecycle::pending_user_queue(
            &self.client,
            self.node_did(),
            session_id,
            self.user.did(),
        )
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.request_id)
        .collect()
    }

    async fn session_rows(&self, session_id: &str) -> Vec<serde_json::Value> {
        let mut rows = rows_as(
            &self.client,
            "AgentRequest",
            &format!(
                "{{ session_id: {{ _eq: \"{}\" }} }}",
                escape_graphql_string(session_id)
            ),
            "_docID request_id content lifecycle_state superseded_by_request_doc_id",
        )
        .await;
        rows.sort_by(|left, right| left["_docID"].as_str().cmp(&right["_docID"].as_str()));
        rows
    }

    /// Two barrier edits the restarted reconciler must decide prove it swept
    /// past every earlier command.
    async fn barrier(&self, suffix: &str) {
        let case = lean_edit_case("local_self_applied");
        for index in ["one", "two"] {
            let session = format!("barrier-{suffix}-{index}");
            let mut seeded = self
                .seed(&session, case["initial"]["queue"]["pending"].as_array().unwrap())
                .await;
            let command = self
                .command(&case, &mut seeded, &format!("barrier-{suffix}-{index}"))
                .await;
            self.publish(&command).await.unwrap();
            let (_, receipt) = self.receipt(&command).await;
            assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
        }
    }

    /// The model's pending queue and receipt for `case`, bound to native IDs.
    fn modeled_pending(&self, seeded: &SeededSession, state: &serde_json::Value) -> Vec<String> {
        state["queue"]["pending"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| seeded.request_ids[&entry["request_id"].as_u64().unwrap()].clone())
            .collect()
    }
}

/// Every endpoint-authored, fresh-identity case the model decides, published
/// concurrently by the User while the reconciler runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_client_edits_publish_concurrently_with_signed_effect_and_receipt() {
    let edits = EndpointEdits::start("input-edit-endpoint").await;
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["session_input_edit_cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| {
            case["authority"]["command_identity_unique"] == true
                && case["authority"]["editor"] == "requester"
                && case["history"].as_array().unwrap().is_empty()
                && !case["receipt"].is_null()
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        cases.iter().map(|case| case["name"].as_str().unwrap()).collect::<Vec<_>>(),
        [
            "local_self_applied",
            "expired_rejected",
            "signature_rejected",
            "replacement_signature_rejected",
            "foreign_session_rejected",
            "stale_queue_rejected",
        ],
        "endpoint-authored generated input-edit cases"
    );
    let mut prepared = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        assert_eq!(case["initial"]["queue"]["folding"], serde_json::json!([]));
        let mut seeded = edits
            .seed(
                &format!("client-{name}"),
                case["initial"]["queue"]["pending"].as_array().unwrap(),
            )
            .await;
        let command = edits.command(case, &mut seeded, &format!("edit-{name}")).await;
        assert_eq!(
            edits.pending(&seeded.session_id).await,
            edits.modeled_pending(&seeded, &case["initial"]),
            "{name}: seeded queue"
        );
        prepared.push((case, seeded, command));
    }
    let (_stop, worker) = edits.reconciler().await;
    let published = futures::future::join_all(
        prepared
            .iter()
            .map(|(_, _, command)| edits.publish(command)),
    )
    .await;
    for ((case, seeded, command), published) in prepared.iter().zip(published) {
        let name = case["name"].as_str().unwrap();
        if case["authority"]["session_owned"] == false {
            // The command's shape owner refuses a foreign session before
            // publication, so the runtime never observes it; the model's
            // rejection leaves the same queue.
            assert!(published.is_err(), "{name}: foreign session is refused");
            assert!(edits.command_rows(&command.command_id).await.is_empty());
            assert_eq!(case["before"]["queue"], case["after"]["queue"]);
            assert_eq!(
                edits.pending(&seeded.session_id).await,
                edits.modeled_pending(seeded, &case["after"]),
                "{name}: queue state"
            );
            continue;
        }
        published.unwrap_or_else(|error| panic!("{name}: endpoint publication: {error:#}"));
        let (_, receipt) = edits.receipt(command).await;
        let expected = &case["receipt"];
        assert_eq!(
            serde_json::to_value(receipt.outcome).unwrap(),
            expected["outcome"],
            "{name}: outcome"
        );
        assert_eq!(
            receipt.request_ids,
            model_ids(&expected["request_ids"])
                .iter()
                .map(|id| seeded.request_ids[id].clone())
                .collect::<Vec<_>>(),
            "{name}: receipt request IDs"
        );
        assert_eq!(
            edits.pending(&seeded.session_id).await,
            edits.modeled_pending(seeded, &case["after"]),
            "{name}: queue state"
        );
        for terminal in model_ids(&case["after"]["queue"]["terminal"]) {
            let Some(doc) = seeded.doc_ids.get(&terminal) else {
                continue;
            };
            let row = wait_row_as(&edits.client, "AgentRequest", "_docID", doc, "lifecycle_state", "")
                .await;
            assert_eq!(row["lifecycle_state"], "superseded", "{name}: replaced input");
        }
    }
    drop(_stop);
    worker.await.unwrap();
}

/// The same signed intent raced twice through the endpoint is one physical
/// command: the unique command identity admits exactly one create, the edit
/// applies once, and a restarted reconciler replays its receipt unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_signed_edit_published_concurrently_applies_and_replays_once() {
    let edits = EndpointEdits::start("same-edit-endpoint").await;
    let case = lean_edit_case("local_self_applied");
    let replay = lean_edit_case("exact_replay_after_expiry");
    assert_eq!(replay["before"], replay["after"], "exact replay changes nothing");
    assert_eq!(replay["receipt"], case["receipt"], "exact replay returns the receipt");
    let mut seeded = edits
        .seed("same-intent", case["initial"]["queue"]["pending"].as_array().unwrap())
        .await;
    let command = edits.command(&case, &mut seeded, "same-intent-command").await;
    let (left, right) = tokio::join!(edits.publish(&command), edits.publish(&command));
    assert_eq!(
        usize::from(left.is_ok()) + usize::from(right.is_ok()),
        1,
        "exactly one racing publication is admitted: {left:?} / {right:?}"
    );
    assert_eq!(edits.command_rows(&command.command_id).await.len(), 1);
    let (stop, worker) = edits.reconciler().await;
    let (raw, receipt) = edits.receipt(&command).await;
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    assert_eq!(
        edits.pending(&seeded.session_id).await,
        edits.modeled_pending(&seeded, &case["after"])
    );
    drop(stop);
    worker.await.unwrap();
    let before = edits.session_rows(&seeded.session_id).await;
    assert_eq!(
        before
            .iter()
            .filter(|row| row["request_id"] == command.replacements[0].request_id)
            .count(),
        1,
        "the replacement exists once"
    );

    let (stop, worker) = edits.reconciler().await;
    edits.barrier("same-intent").await;
    drop(stop);
    worker.await.unwrap();
    let rows = edits.command_rows(&command.command_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["receipt_json"], raw, "replayed receipt is final");
    assert_eq!(edits.session_rows(&seeded.session_id).await, before);
}

/// Two different intents racing for one command ID: the model fails closed on
/// two physical documents, and the endpoint's unique identity ensures only one
/// ever exists, so that one is decided and the loser's session is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_edit_intents_racing_for_one_command_id_admit_one() {
    let edits = EndpointEdits::start("racing-edit-endpoint").await;
    let collision = lean_edit_case("simultaneous_command_identity_collision");
    assert_eq!(collision["before"], collision["after"]);
    assert!(collision["receipt"].is_null(), "two documents never apply");
    let case = lean_edit_case("local_self_applied");
    let pending = case["initial"]["queue"]["pending"].as_array().unwrap();
    let mut left_session = edits.seed("racing-left", pending).await;
    let mut right_session = edits.seed("racing-right", pending).await;
    let left = edits.command(&case, &mut left_session, "racing-command").await;
    let right = edits.command(&case, &mut right_session, "racing-command").await;
    assert_ne!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(&right).unwrap()
    );
    let right_before = edits.session_rows(&right_session.session_id).await;
    let left_before = edits.session_rows(&left_session.session_id).await;
    let (left_published, right_published) = tokio::join!(edits.publish(&left), edits.publish(&right));
    assert_eq!(
        usize::from(left_published.is_ok()) + usize::from(right_published.is_ok()),
        1,
        "exactly one racing intent is admitted: {left_published:?} / {right_published:?}"
    );
    let rows = edits.command_rows("racing-command").await;
    assert_eq!(rows.len(), 1, "one physical command document");
    let (winner, winner_session, loser_session, loser_before) = if left_published.is_ok() {
        (&left, &left_session, &right_session, right_before)
    } else {
        (&right, &right_session, &left_session, left_before)
    };
    let (stop, worker) = edits.reconciler().await;
    let (_, receipt) = edits.receipt(winner).await;
    assert_eq!(
        serde_json::to_value(receipt.outcome).unwrap(),
        case["receipt"]["outcome"]
    );
    assert_eq!(
        edits.pending(&winner_session.session_id).await,
        edits.modeled_pending(winner_session, &case["after"])
    );
    edits.barrier("racing").await;
    drop(stop);
    worker.await.unwrap();
    assert_eq!(
        edits.session_rows(&loser_session.session_id).await,
        loser_before,
        "the refused intent has no effect"
    );
}

/// A later intent reusing an applied command ID in another session: the model
/// preserves the prior receipt and both queues, and the endpoint refuses the
/// second physical document outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_command_id_twin_in_another_session_is_refused() {
    let edits = EndpointEdits::start("late-twin-endpoint").await;
    let twin = lean_edit_case("cross_session_collision_preserves_prior_receipt");
    assert_eq!(twin["before"], twin["after"]);
    assert!(twin["receipt"].is_null());
    let case = lean_edit_case("local_self_applied");
    let pending = case["initial"]["queue"]["pending"].as_array().unwrap();
    let mut prior_session = edits.seed("late-prior", pending).await;
    let mut twin_session = edits.seed("late-twin", pending).await;
    let prior = edits.command(&case, &mut prior_session, "late-command").await;
    edits.publish(&prior).await.unwrap();
    let (stop, worker) = edits.reconciler().await;
    let (raw, receipt) = edits.receipt(&prior).await;
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    let prior_rows = edits.session_rows(&prior_session.session_id).await;
    let twin_rows = edits.session_rows(&twin_session.session_id).await;
    let late = edits.command(&case, &mut twin_session, "late-command").await;
    assert!(
        edits.publish(&late).await.is_err(),
        "a second physical document under an applied command ID is refused"
    );
    edits.barrier("late-twin").await;
    drop(stop);
    worker.await.unwrap();
    let rows = edits.command_rows("late-command").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["receipt_json"], raw, "prior receipt is final");
    assert_eq!(edits.session_rows(&prior_session.session_id).await, prior_rows);
    assert_eq!(edits.session_rows(&twin_session.session_id).await, twin_rows);
}

/// Republishing the identical signed intent after its receipt: the model
/// leaves an observed duplicate without effect and the endpoint refuses the
/// second document, so the receipt and queue are unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_edit_republished_after_receipt_is_refused() {
    let edits = EndpointEdits::start("identical-edit-endpoint").await;
    let observed = lean_edit_case("observed_collision_preserves_prior_receipt");
    assert_eq!(observed["before"], observed["after"]);
    assert!(observed["receipt"].is_null());
    let case = lean_edit_case("local_self_applied");
    let mut seeded = edits
        .seed("identical", case["initial"]["queue"]["pending"].as_array().unwrap())
        .await;
    let command = edits.command(&case, &mut seeded, "identical-command").await;
    edits.publish(&command).await.unwrap();
    let (stop, worker) = edits.reconciler().await;
    let (raw, receipt) = edits.receipt(&command).await;
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    let before = edits.session_rows(&seeded.session_id).await;
    assert!(
        edits.publish(&command).await.is_err(),
        "the identical intent is one command identity"
    );
    edits.barrier("identical").await;
    drop(stop);
    worker.await.unwrap();
    let rows = edits.command_rows(&command.command_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["receipt_json"], raw);
    assert_eq!(edits.session_rows(&seeded.session_id).await, before);
}

#[tokio::test]
async fn persisted_edit_survives_database_reopen_and_receipt_replay_is_idempotent() {
    use gents::NodeIdentity as _;
    let mut db = support::test_db("input-edit-reopen").await;
    let did = db.node_identity.did().to_owned();
    // The enrolled User created this Agent and edits its own queued input.
    let support::ManagedAgentFixture {
        user,
        access: mut client,
        agent,
        ..
    } = support::managed_agent_fixture(&db).await;
    let agent_doc = client
        .transact_readonly("test.input_edit.agent", |txn| {
            let did = &did;
            let agent_did = agent.did();
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::Agent,
                        did,
                        agent_did,
                    )
                    .await?
                    .expect("fixture Agent")
                    .0,
                )
            })
        })
        .await
        .unwrap();
    let session = "edit-reopen-session";
    let mut original = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "reopen-original",
        &did,
        user.did(),
        agent.did(),
        session,
        "original durable message",
        "interactive",
        "2026-10-10T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    original.input.queue = Some(RequestQueue {
        delivery: QueueDelivery::Steer,
        position: None,
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    original.session_doc_id = Some(
        gents::request::lifecycle::reserve_goal_session(&client, &original)
            .await
            .unwrap(),
    );
    original.agent_doc_id = Some(agent_doc);
    gents::sign_agent_request_create(&user, &mut original)
        .await
        .unwrap();
    gents::request::lifecycle::submit_client_request(&client, &original)
        .await
        .unwrap();
    let created = client
        .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "reopen-original"}}) {_docID} }"#)
        .await
        .unwrap();
    let original_doc = created["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let edit = PendingQueueEdit {
        expected_request_doc_ids: vec![original_doc.clone()],
        selected_request_doc_ids: vec![original_doc.clone()],
        messages: vec![PendingMessageEdit {
            authored_input: None,
            request_doc_id: original_doc.clone(),
            content: "replacement durable message".into(),
        }],
    };
    let replacements = prepare_pending_user_edit(
        &client,
        &user,
        AgentRequestAdmissionRecord::client(user.did()),
        &did,
        session,
        user.did(),
        &edit,
    )
    .await
    .unwrap();
    let (session_doc_id, agent_did, agent_doc_id, replacement_evidence) =
        signed_edit_targets(&db, &user, &did, session, &replacements).await;
    let now = chrono::Utc::now();
    let mut command = SessionInputEdit {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: "edit-before-reopen".into(),
        requester_did: user.did().into(),
        requester_peer_id: None,
        node_did: did.clone(),
        session_id: session.into(),
        session_doc_id,
        agent_did,
        agent_doc_id,
        issued_at: (now - chrono::Duration::seconds(1)).to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
        edit,
        replacements,
        replacement_evidence,
        signature: vec![],
    };
    command.signature = user.sign(&command.signing_payload()).await.unwrap();
    gents::request::lifecycle::publish_edit_command(&client, &command)
        .await
        .unwrap();
    db.simulate_process_crash().await.unwrap();
    assert_eq!(db.node_identity.did(), did);
    client = db.client_access(&user);
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let task = tokio::spawn(run_session_command_reconciler(
        db.node.clone(),
        db.node_identity.clone(),
        support::fixtures::session_command_agent_snapshot(&db).await,
        cancel.clone(),
    ));
    let row = wait_row_as(
        &client,
        "AgentSessionInputEdit",
        "command_id",
        &command.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    let receipt_json = row["receipt_json"].as_str().unwrap().to_owned();
    let receipt: SessionInputEditReceipt = serde_json::from_str(&receipt_json).unwrap();
    receipt.validate_for(&command).unwrap();
    assert!(db
        .node_identity
        .verify(&did, &receipt.signing_payload(), &receipt.signature)
        .await
        .unwrap());
    assert_eq!(receipt.outcome, SessionInputEditOutcome::Applied);
    assert_eq!(
        receipt.request_ids,
        vec![command.replacements[0].request_id.clone()]
    );
    cancel.cancel();
    task.await.unwrap().unwrap();
    let fields = "_docID request_id content lifecycle_state superseded_by_request_doc_id";
    let before = rows_as(&client, "AgentRequest", "{}", fields).await;
    assert_eq!(before.len(), 2);
    let old = before
        .iter()
        .find(|row| row["_docID"] == original_doc)
        .unwrap();
    assert_eq!(old["content"], "original durable message");
    assert_eq!(old["lifecycle_state"], "superseded");
    assert_eq!(
        old["superseded_by_request_doc_id"],
        receipt.request_doc_ids[0]
    );
    let replacement = before
        .iter()
        .find(|row| row["_docID"] == receipt.request_doc_ids[0])
        .unwrap();
    assert_eq!(replacement["content"], "replacement durable message");
    db.simulate_process_crash().await.unwrap();
    client = db.client_access(&user);
    let mut stale = command.clone();
    stale.command_id = "edit-after-reopen-stale".into();
    stale.replacements = prepare_pending_user_edit(
        &client,
        &user,
        AgentRequestAdmissionRecord::client(user.did()),
        &did,
        session,
        user.did(),
        &PendingQueueEdit {
            expected_request_doc_ids: receipt.request_doc_ids.clone(),
            selected_request_doc_ids: receipt.request_doc_ids.clone(),
            messages: vec![PendingMessageEdit {
                authored_input: None,
                request_doc_id: receipt.request_doc_ids[0].clone(),
                content: "stale replacement".into(),
            }],
        },
    )
    .await
    .unwrap();
    stale.replacement_evidence =
        signed_edit_targets(&db, &user, &did, session, &stale.replacements)
            .await
            .3;
    stale.signature = user.sign(&stale.signing_payload()).await.unwrap();
    gents::request::lifecycle::publish_edit_command(&client, &stale)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let _stop = Stop(cancel.clone());
    let task = tokio::spawn(run_session_command_reconciler(
        db.node.clone(),
        db.node_identity.clone(),
        support::fixtures::session_command_agent_snapshot(&db).await,
        cancel.clone(),
    ));
    let rejected = wait_row_as(
        &client,
        "AgentSessionInputEdit",
        "command_id",
        &stale.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    let rejected: SessionInputEditReceipt =
        serde_json::from_str(rejected["receipt_json"].as_str().unwrap()).unwrap();
    rejected.validate_for(&stale).unwrap();
    assert!(db
        .node_identity
        .verify(&did, &rejected.signing_payload(), &rejected.signature)
        .await
        .unwrap());
    assert_eq!(rejected.outcome, SessionInputEditOutcome::Rejected);
    let after = rows_as(&client, "AgentRequest", "{}", fields).await;
    assert_eq!(after, before);
    let replay = wait_row_as(
        &client,
        "AgentSessionInputEdit",
        "command_id",
        &command.command_id,
        "receipt_json",
        "receipt_json",
    )
    .await;
    assert_eq!(replay["receipt_json"], receipt_json);
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(rows_as(&client, "AgentRequest", "{}", fields).await, before);
}
