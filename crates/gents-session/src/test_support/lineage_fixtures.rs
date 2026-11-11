use std::collections::BTreeMap;

use gents_protocol::request_admission::RequestPurpose;
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

/// The DID of [`fixture_signer`]; sessions it starts carry receipts it signed.
pub const FIXTURE_DID: &str = "did:key:z6MkvDqGT54cXesYGvABpF1UapVNwjCqRcafi4Px6Thv5T3Z";

/// A node builder with its own node identity, for fixtures that write rows as that node.
pub fn node_builder() -> defra_node::NodeBuilder {
    use gents_store::identity::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap();
    defra_node::Node::builder().with_node_identity_did(identity.did())
}

/// A fixed test key, so fixture rows can name its DID as a constant.
pub fn fixture_signer() -> gents_store::identity::KeyIdentity {
    use gents_store::identity::NodeIdentity as _;
    use std::io::Write as _;
    // A named temporary file is created private to its owner, as a key must be.
    let mut key = tempfile::NamedTempFile::new().unwrap();
    key.write_all(&crypto::ed25519_key_from_seed(&[7; 32]).unwrap())
        .unwrap();
    let signer = gents_store::identity::KeyIdentity::load_existing(key.path(), None).unwrap();
    assert_eq!(signer.did(), FIXTURE_DID);
    signer
}

/// Write `session` as creation writes a started session: the immutable
/// starter index at create, then the receipt [`fixture_signer`] signs for
/// the created physical document, binding `source`, `parent` and `opening`.
pub async fn create_started_session(
    node: &defra_node::Node,
    mut session: gents_protocol::session::AgentSession,
    source: gents_protocol::session::SessionReference,
    parent: &str,
    opening: &str,
) -> String {
    use gents_protocol::session::{
        SessionCreationCause, SessionCreationReceipt, SessionCreationStarter, SessionProvenance,
    };
    use gents_store::identity::NodeIdentity as _;
    assert_eq!(
        session.node_did, FIXTURE_DID,
        "the fixture key signs receipts"
    );
    let mut provenance = session.provenance.take().unwrap_or_default();
    provenance.parent_request_doc_id = Some(parent.to_owned());
    provenance.opening_request_doc_id = Some(opening.to_owned());
    provenance.started_by = Some(source.clone());
    session.provenance = Some(provenance);
    let mut input = serde_json::to_value(&session).unwrap();
    input["started_by_node_did"] = source.node_did.clone().into();
    input["started_by_session_id"] = source.session_id.clone().into();
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let doc_id = run(
        node,
        format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
    let mut receipt = SessionCreationReceipt {
        node_did: session.node_did.clone(),
        session_id: session.session_id.clone(),
        session_doc_id: doc_id.clone(),
        requester_did: session.requester_did.clone(),
        agent_did: session.agent_did.clone(),
        opening_request_doc_id: opening.to_owned(),
        cause: SessionCreationCause::AgentNew,
        cause_binding: "fixture-call".to_owned(),
        starter: Some(SessionCreationStarter {
            source,
            parent_request_doc_id: parent.to_owned(),
        }),
        signature: Vec::new(),
    };
    receipt.signature = fixture_signer()
        .sign(&receipt.signing_payload())
        .await
        .unwrap();
    let provenance = SessionProvenance {
        creation_receipt: Some(receipt),
        ..session.provenance.unwrap()
    };
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::json!({ "provenance": provenance }),
    )
    .unwrap();
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentSession(docID: "{}", input: {input}) {{ _docID }} }}"#,
            escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    doc_id
}

/// A fixture session with no provenance.
pub fn fixture_session(
    did: &str,
    session_id: &str,
) -> gents_protocol::session::AgentSession {
    gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: did.into(),
        requester_did: Some(did.into()),
        agent_did: "worker".into(),
        created_at: "2026-09-26T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

async fn run(node: &defra_node::Node, mutation: String, collection: &str) -> String {
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    gents_store::graphql::single_mutation_document(&response, &format!("create_{collection}"))
        .unwrap()
        .and_then(|document| document["_docID"].as_str().map(str::to_owned))
        .expect("created doc id")
}

/// One lineage-case row as a `create_AgentRequest` input. Every delivered
/// field is written where its native representation lives; a field with
/// none stops the test.
fn request_input(
    row: &Value,
    did: &str,
    ids: &BTreeMap<String, String>,
    case_ids: &[&str],
    max_retries: u32,
) -> String {
    let quoted = |text: &str| format!("\"{}\"", escape_graphql_string(text));
    let mut input = vec![
        format!("node_did: {}", quoted(did)),
        format!("requester_did: {}", quoted(did)),
        r#"agent_did: "worker", content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", retry_count: 0"#.to_owned(),
        format!("max_retries: {max_retries}"),
    ];
    for (field, value) in row.as_object().expect("case row") {
        match (field.as_str(), value) {
            ("_docID", Value::String(id)) => input.push(format!("request_id: {}", quoted(id))),
            ("session_id", Value::String(id)) => {
                input.push(format!("session_id: {}", quoted(id)));
            }
            ("created_at", Value::Number(millis)) => {
                let at = chrono::DateTime::from_timestamp_millis(millis.as_i64().unwrap())
                    .expect("epoch milliseconds")
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                input.push(format!("created_at: {}", quoted(&at)));
            }
            ("is_public", Value::Bool(public)) => {
                let purpose = if *public {
                    RequestPurpose::PUBLIC
                } else {
                    RequestPurpose::TitleAudit
                };
                input.push(format!("purpose: {}", quoted(purpose.as_str())));
            }
            ("caused_by_parent_request_doc_id", Value::String(cause)) => {
                let native = ids.get(cause).map(String::as_str).unwrap_or_else(|| {
                    assert!(
                        !case_ids.contains(&cause.as_str()),
                        "plan_contradiction: cause {cause} is written after its row"
                    );
                    cause
                });
                input.push(format!(
                    "caused_by_parent_request_doc_id: {}",
                    quoted(native)
                ));
            }
            ("caused_by_parent_tool_call_doc_id", Value::String(call)) => {
                input.push(format!(
                    "caused_by_parent_tool_call_doc_id: {}",
                    quoted(call)
                ));
            }
            // The recorded opener is session provenance, written after the rows.
            ("opening_request_doc_id", _) => {}
            (_, Value::Null) => {}
            _ => panic!("plan_contradiction: case-row field {field} has no native form"),
        }
    }
    input.join(", ")
}

/// Write a lineage case's rows, then one session per `session_id`, into
/// `node`. Returns each row's model `_docID` mapped to its native one.
///
/// DefraDB derives a native `_docID` from document content, so ids cannot
/// follow the model's id order. Rows of one session that share `created_at`
/// are written as one group, re-created with a different `max_retries`
/// (which no reader selects) until their native order matches the model's.
pub async fn seed_lineage_case(
    node: &defra_node::Node,
    case: &Value,
    did: &str,
) -> BTreeMap<String, String> {
    let rows = case["rows"].as_array().expect("case rows");
    let text = |row: &Value, field: &str| row[field].as_str().map(str::to_owned);
    let model_id = |row: &Value| text(row, "_docID").expect("row _docID");
    let case_ids: Vec<String> = rows.iter().map(model_id).collect();
    let case_ids: Vec<&str> = case_ids.iter().map(String::as_str).collect();
    let mut ids = BTreeMap::<String, String>::new();
    for row in rows {
        if ids.contains_key(&model_id(row)) {
            continue;
        }
        let group: Vec<&Value> = rows
            .iter()
            .filter(|other| {
                other["session_id"] == row["session_id"]
                    && other["created_at"] == row["created_at"]
            })
            .collect();
        let mut settled = false;
        for attempt in 0..64 {
            let mut written = Vec::new();
            for member in &group {
                let input = request_input(member, did, &ids, &case_ids, 3 + attempt);
                let mutation = format!(
                    "mutation {{ create_AgentRequest(input: {{ {input} }}) {{ _docID }} }}"
                );
                written.push((model_id(member), run(node, mutation, "AgentRequest").await));
            }
            let mut by_model = written.clone();
            by_model.sort();
            settled = by_model.windows(2).all(|pair| pair[0].1 < pair[1].1);
            if settled {
                ids.extend(written);
                break;
            }
            for (_, native) in written {
                let response = node
                    .execute(&format!(
                        r#"mutation {{ delete_AgentRequest(docID: "{}") {{ _docID }} }}"#,
                        escape_graphql_string(&native)
                    ))
                    .await;
                assert!(!response.has_errors(), "{:?}", response.errors);
            }
        }
        assert!(settled, "no native id order matched after 64 attempts");
    }
    let mapped = |id: &str| ids.get(id).cloned().unwrap_or_else(|| id.to_owned());
    let mut sessions = Vec::<&str>::new();
    for row in rows {
        let session = row["session_id"].as_str().expect("row session_id");
        if sessions.contains(&session) {
            continue;
        }
        sessions.push(session);
        let own: Vec<&Value> = rows
            .iter()
            .filter(|other| other["session_id"].as_str() == Some(session))
            .collect();
        let mut openings: Vec<&str> = own
            .iter()
            .filter_map(|other| other["opening_request_doc_id"].as_str())
            .collect();
        openings.dedup();
        assert!(
            openings.len() <= 1,
            "plan_contradiction: {session} records two opening requests"
        );
        let parent = own
            .iter()
            .find_map(|other| other["caused_by_parent_request_doc_id"].as_str());
        let source = parent.and_then(|parent| {
            rows.iter()
                .find(|other| other["_docID"].as_str() == Some(parent))?["session_id"]
                .as_str()
        });
        // A recorded opening is creation evidence; its receipt names the starter.
        if let (Some(parent), Some(source), Some(opening)) = (parent, source, openings.first())
        {
            create_started_session(
                node,
                fixture_session(did, session),
                gents_protocol::session::SessionReference {
                    node_did: did.to_owned(),
                    session_id: source.to_owned(),
                    requester_did: Some(did.to_owned()),
                },
                &mapped(parent),
                &mapped(opening),
            )
            .await;
            continue;
        }
        let mut provenance = Vec::new();
        if let Some(parent) = parent {
            provenance.push(format!(
                r#"parent_request_doc_id: "{}""#,
                escape_graphql_string(&mapped(parent))
            ));
        }
        if let Some(opening) = openings.first() {
            provenance.push(format!(
                r#"opening_request_doc_id: "{}""#,
                escape_graphql_string(&mapped(opening))
            ));
        }
        let provenance = if provenance.is_empty() {
            String::new()
        } else {
            format!(", provenance: {{ {} }}", provenance.join(", "))
        };
        let session = escape_graphql_string(session);
        let did = escape_graphql_string(did);
        run(
            node,
            format!(
                r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{did}", requester_did: "{did}", agent_did: "worker", created_at: "2026-09-26T00:00:00Z"{provenance} }}) {{ _docID }} }}"#
            ),
            "AgentSession",
        )
        .await;
    }
    ids
}
