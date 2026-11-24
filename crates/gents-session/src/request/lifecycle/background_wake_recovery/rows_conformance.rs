use gents_lean_vocab::families::contracts_background_wake_rows as family;
use gents_protocol::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde_json::{json, Value};

use super::{load_candidates, redrive_one, RedriveOutcome};
use crate::test_support::AgentHome;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExtraRow {
    Nothing,
    NewerInteractive,
    PublishedSuccessor,
}

/// The model's numbers for the production ids and timestamps one case writes.
#[derive(Default)]
struct Names {
    ids: Vec<(String, u64)>,
    times: Vec<(chrono::DateTime<chrono::FixedOffset>, u64)>,
}

impl Names {
    fn id(&mut self, production: &str, model: u64) {
        self.ids.push((production.to_owned(), model));
    }

    fn time(&mut self, production: &str, model: u64) {
        self.times.push((parse_time(production), model));
    }

    fn of_id(&self, production: &str) -> u64 {
        self.ids
            .iter()
            .find(|(known, _)| known == production)
            .unwrap_or_else(|| panic!("unnamed production id {production}"))
            .1
    }

    fn of_time(&self, production: &str) -> u64 {
        let at = parse_time(production);
        self.times
            .iter()
            .find(|(known, _)| *known == at)
            .unwrap_or_else(|| panic!("unnamed production time {production}"))
            .1
    }
}

fn parse_time(value: &str) -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339(value).expect("RFC 3339 timestamp")
}

fn fixture_time(model: u64) -> String {
    format!("2026-01-01T00:00:{model:02}Z")
}

/// A well-formed document id no row holds.
fn absent_doc_id() -> String {
    format!("bae-{}", uuid::Uuid::new_v4())
}

async fn create_request(db: &AgentHome, session_doc: &str, input: Value) -> String {
    let literal = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let agent_did = db.agent_did();
    let node_did = db.node_did();
    db.agent_access
        .transact_write("test.wake_rows.request", |txn| {
            let literal = &literal;
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "mutation {{ create_AgentRequest(input: {literal}) {{ _docID }} }}"
                    ))
                    .await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                let (agent_doc, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture Agent unavailable"))?;
                crate::request::lifecycle::stage_request_relationships_in_txn(
                    txn,
                    &doc,
                    &agent_doc,
                    session_doc,
                    agent_did,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .expect("publish fixture request with its Agent relationships")
}

async fn seed_stale_observation(access: &ConfigAccess, session: &str, names: &mut Names) {
    let (doc, request) = (absent_doc_id(), format!("{session}-999"));
    names.id(&doc, 999);
    names.id(&request, 999);
    let observation = gents_protocol::session::SessionObservation {
        last_activity_at: fixture_time(2),
        preview: None,
        reply_preview: None,
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: doc,
            request_id: request,
            lifecycle_state: gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
        }),
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&json!({ "observation": observation }))
            .unwrap();
    access
        .write(
            "test.wake_rows.stale_observation",
            &format!(
                r#"mutation {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, input: {input}) {{ _docID }} }}"#,
                escape_graphql_string(session)
            ),
        )
        .await
        .expect("seed stale session observation");
}

async fn session_rows(access: &ConfigAccess, session: &str) -> Vec<Value> {
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
                _docID request_id purpose node_did session_id requester_did agent_did created_at
                lifecycle_state terminalized_at execution_origin retry_count request_hop deadline
                caused_by_parent_request_id execution_generation backend_id
            }} }}"#,
            escape_graphql_string(session)
        ))
        .await
        .expect("read session rows as their Agent");
    response["data"]["AgentRequest"]
        .as_array()
        .expect("session rows")
        .clone()
}

fn head(names: &Names, row: &Value) -> family::RequestHeadObservation {
    family::RequestHeadObservation {
        doc_id: names.of_id(row["_docID"].as_str().unwrap()),
        request_id: names.of_id(row["request_id"].as_str().unwrap()),
        state: row["lifecycle_state"].as_str().unwrap().to_owned(),
    }
}

fn row_observation(names: &Names, row: &Value) -> family::RowObservation {
    family::RowObservation {
        purpose: row["purpose"].as_str().unwrap().to_owned(),
        node: names.of_id(row["node_did"].as_str().unwrap()),
        session: names.of_id(row["session_id"].as_str().unwrap()),
        requester: row["requester_did"].as_str().map(|did| names.of_id(did)),
        agent: names.of_id(row["agent_did"].as_str().unwrap()),
        created_at: names.of_time(row["created_at"].as_str().unwrap()),
        head: head(names, row),
        terminalized_at: row["terminalized_at"].as_str().map(|at| names.of_time(at)),
    }
}

async fn publication(
    db: &AgentHome,
    names: &Names,
    session: &str,
    wake_doc: &str,
    successor: &Value,
    rows: &[Value],
) -> family::WakePublication {
    let pin = crate::request::retry_budget::read_pin(&db.agent_access, db.node_did(), wake_doc)
        .await
        .unwrap();
    let (node_did, requester) = (db.node_did(), db.user.did());
    let owner = db
        .agent_access
        .transact_readonly("test.wake_rows.session", |txn| {
            Box::pin(async move {
                gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    node_did,
                    session,
                    Some(requester),
                )
                .await
            })
        })
        .await
        .unwrap()
        .expect("wake session");
    let held = |field: &str| !successor[field].is_null();
    let mut observed = rows
        .iter()
        .map(|row| row_observation(names, row))
        .collect::<Vec<_>>();
    observed.sort_by_key(|row| (row.created_at, row.head.doc_id));
    family::WakePublication {
        successor_context: family::WakeContextObservation {
            state: successor["lifecycle_state"].as_str().unwrap().to_owned(),
            origin: successor["execution_origin"].as_str().unwrap().to_owned(),
            admission: if held("execution_generation") || held("backend_id") {
                "acquired"
            } else {
                "released"
            }
            .to_owned(),
            retry_count: successor["retry_count"].as_u64().unwrap(),
            max_retries: crate::request::retry_budget::claim_pin(true, 0, true, pin).unwrap(),
            depth: successor["request_hop"].as_u64().unwrap(),
            parent_request_id: successor["caused_by_parent_request_id"]
                .as_str()
                .map(|id| names.of_id(id)),
            deadline: successor["deadline"].as_str().map(|at| names.of_time(at)),
        },
        session_observation: owner.session.observation.map(|observation| {
            family::SessionObservation {
                activity: names.of_time(&observation.last_activity_at),
                preview: observation.preview.map(|preview| {
                    assert_eq!(
                        preview, BACKGROUND_COMPLETION_WAKE_PROMPT,
                        "publication previews the wake"
                    );
                    "wake".to_owned()
                }),
                latest: observation
                    .latest_request
                    .map(|latest| family::RequestHeadObservation {
                        doc_id: names.of_id(&latest.request_doc_id),
                        request_id: names.of_id(&latest.request_id),
                        state: latest.lifecycle_state.as_str().to_owned(),
                    }),
                reply_preview: observation.reply_preview,
            }
        }),
        rows: observed,
    }
}

async fn replay(
    db: &AgentHome,
    other: &str,
    index: usize,
    input: &family::WakeRowsInput<ExtraRow>,
) -> Option<family::WakePublication> {
    let mut names = Names::default();
    let session = format!("wake-rows-{index}");
    let (node_did, agent_did, requester) = (db.node_did(), db.agent_did(), db.user.did());
    names.id(node_did, 1);
    names.id(&session, 900);
    names.id(requester, 2);
    names.id(other, 99);
    names.id(agent_did, 3);
    for model in 0..=2 {
        names.time(&fixture_time(model), model);
    }
    let session_doc = crate::test_support::create_agent_owned_session_at(
        &db.agent_access,
        node_did,
        &session,
        Some(requester),
        &fixture_time(0),
    )
    .await;
    if input.stale_session_observation {
        seed_stale_observation(&db.agent_access, &session, &mut names).await;
    }
    let queue_session = match input.queue_key {
        Some(900) => session.clone(),
        Some(key) => format!("{session}-{key}"),
        None => panic!("every modeled wake names its queue"),
    };
    let wake_request = format!("{session}-901");
    let wake_input = RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(QueueSource::BackgroundCompletion.queue_key(&queue_session)),
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    };
    let wake_doc = create_request(
        db,
        &session_doc,
        json!({
            "request_id": wake_request, "purpose": "normal", "node_did": node_did,
            "requester_did": requester, "agent_did": agent_did, "session_id": session,
            "content": "background input", "input": wake_input,
            "execution_origin": "scheduled", "lifecycle_state": "failed",
            "failure_reason": "backend admission failed", "created_at": fixture_time(1),
            "deadline": fixture_time(1), "retry_count": 0, "retry_root_request": wake_request,
            "terminal_redrive_attempts": 0, "request_hop": 2,
        }),
    )
    .await;
    names.id(&wake_doc, 101);
    names.id(&wake_request, 901);
    crate::request::retry_budget::write_pin(
        db.node.clone(),
        db.node_identity.clone(),
        node_did,
        &wake_doc,
        3,
    )
    .await
    .unwrap();

    let accesses =
        std::collections::BTreeMap::from([(agent_did.to_owned(), db.agent_access.clone())]);
    let (candidates, _, _) = load_candidates(node_did, &accesses).await.unwrap();
    let mut candidate: AgentRequestRow = candidates
        .into_iter()
        .find(|row| row.doc_id.as_deref() == Some(wake_doc.as_str()))
        .expect("the failed wake is a recovery candidate");
    let pin = super::candidate_pin(&accesses, &candidate).await.unwrap();
    let signer = db.agent_identity.as_ref();

    match input.extra_row {
        ExtraRow::Nothing => {}
        ExtraRow::NewerInteractive => {
            let request = format!("{session}-903");
            let doc = create_request(
                db,
                &session_doc,
                json!({
                    "request_id": request, "purpose": "normal", "node_did": node_did,
                    "requester_did": other, "agent_did": agent_did, "session_id": session,
                    "content": "newer input", "execution_origin": "interactive",
                    "lifecycle_state": "processing", "created_at": fixture_time(2),
                    "retry_count": 0, "retry_root_request": request,
                    "terminal_redrive_attempts": 0, "request_hop": 0,
                }),
            )
            .await;
            names.id(&doc, 103);
            names.id(&request, 903);
        }
        ExtraRow::PublishedSuccessor => {
            let outcome = redrive_one(&db.agent_access, signer, &candidate, pin)
                .await
                .unwrap();
            assert!(
                matches!(outcome, RedriveOutcome::Created { .. }),
                "{}: {outcome:?}",
                input.name
            );
        }
    }
    if input.parent_doc != 101 {
        let doc = absent_doc_id();
        names.id(&doc, input.parent_doc);
        candidate.doc_id = Some(doc);
    }

    let outcome = redrive_one(&db.agent_access, signer, &candidate, pin)
        .await
        .unwrap();
    let RedriveOutcome::Created { request_id } = outcome else {
        return None;
    };
    let rows = session_rows(&db.agent_access, &session).await;
    let successor = rows
        .iter()
        .find(|row| row["request_id"] == request_id.as_str())
        .expect("published successor")
        .clone();
    names.id(successor["_docID"].as_str().unwrap(), input.successor_doc);
    names.id(&request_id, input.successor_request);
    names.time(
        successor["created_at"].as_str().unwrap(),
        input.successor_created_at,
    );
    Some(publication(db, &names, &session, &wake_doc, &successor, &rows).await)
}

#[tokio::test]
async fn generated_background_wake_rows_redrive_through_the_recovery_owner() {
    let db = AgentHome::start("background-wake-rows").await;
    let other = gents_store::identity::KeyIdentity::load_or_create(
        db.path().join("other-requester.key"),
        None,
    )
    .unwrap();
    for (index, case) in family::cases::<ExtraRow>().into_iter().enumerate() {
        let observed = replay(&db, other.did(), index, &case.input).await;
        case.check(observed);
    }
    db.node.shutdown().await.expect("shut down test node");
}
