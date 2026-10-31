use rapidhash::RapidHashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::session::load_latest_request_in_txn;
pub(crate) use gents::session::origin::SessionScope;
use gents::session::origin::{
    lineage, load_session, started_by, starting_request, SessionLink, REQUEST_FIELDS,
};
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct CausedSession {
    pub(crate) scope: SessionScope,
    pub(crate) agent_id: String,
    pub(crate) root_session_id: String,
    pub(crate) caused_by_scope: SessionScope,
    pub(crate) depth: u32,
    pub(crate) first: AgentRequestRow,
    pub(crate) latest: AgentRequestRow,
}

fn access(node: &Arc<EmbeddedNode>) -> ConfigAccess {
    ConfigAccess::local(node.clone())
}

#[cfg(unix)]
pub(crate) async fn load_direct_caused_sessions(
    node: &Arc<EmbeddedNode>,
    cause: &AgentRequestRow,
) -> Result<Vec<CausedSession>> {
    let parent = row_scope(cause)?;
    let mut caused = Vec::new();
    for (link, first) in started_by_request(&access(node), cause).await? {
        let latest = load_session_head(node, &link.scope)
            .await?
            .unwrap_or_else(|| first.clone());
        let Some(agent_id) = first.agent_id.clone() else {
            continue;
        };
        caused.push(CausedSession {
            scope: link.scope,
            agent_id,
            root_session_id: parent.session_id.clone(),
            caused_by_scope: parent.clone(),
            depth: 1,
            first,
            latest,
        });
    }
    Ok(caused)
}

pub(crate) async fn load_caused_sessions(
    node: &Arc<EmbeddedNode>,
    roots: &[SessionScope],
) -> Result<Vec<CausedSession>> {
    let access = access(node);
    let mut visited = roots.iter().cloned().collect::<RapidHashSet<_>>();
    let mut frontier = roots
        .iter()
        .map(|scope| (scope.clone(), scope.session_id.clone(), 0_u32))
        .collect::<Vec<_>>();
    let mut caused = Vec::new();
    while let Some((scope, root, depth)) = frontier.pop() {
        for link in lineage(&access, &scope).await?.started {
            if !visited.insert(link.scope.clone()) {
                continue;
            }
            let child = link.scope.clone();
            if let Some(session) = view(node, link, scope.clone(), root.clone(), depth + 1).await? {
                caused.push(session);
                frontier.push((child, root.clone(), depth + 1));
            }
        }
    }
    caused.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.first.created_at.cmp(&right.first.created_at))
            .then_with(|| left.scope.cmp(&right.scope))
    });
    Ok(caused)
}

pub(crate) async fn load_caused_session(
    node: &Arc<EmbeddedNode>,
    session_id: &str,
    is_root: impl Fn(&SessionScope) -> bool,
) -> Result<Option<CausedSession>> {
    let access = access(node);
    let Some(session) = load_session(&access, session_id, None).await? else {
        return Ok(None);
    };
    let mut scope = SessionScope::from(&session);
    let mut chain = Vec::<SessionLink>::new();
    let mut seen = RapidHashSet::from_iter([scope.clone()]);
    loop {
        let Some(link) = started_by(&access, &scope).await? else {
            return Ok(None);
        };
        if !seen.insert(link.scope.clone()) {
            return Ok(None);
        }
        let reached_root = is_root(&link.scope);
        scope = link.scope.clone();
        chain.push(link);
        if reached_root {
            break;
        }
    }
    let depth = u32::try_from(chain.len()).context("caused session chain is too deep")?;
    let root = chain
        .last()
        .map(|link| link.scope.session_id.clone())
        .context("caused session chain is empty")?;
    let parent = chain.swap_remove(0);
    let started = SessionLink {
        scope: SessionScope::from(&session),
        cause_request_doc_id: parent.cause_request_doc_id,
    };
    view(node, started, parent.scope, root, depth).await
}

pub(crate) async fn started_by_request(
    access: &ConfigAccess,
    cause: &AgentRequestRow,
) -> Result<Vec<(SessionLink, AgentRequestRow)>> {
    let cause_doc_id = cause
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("started sessions require a physical request")?;
    let mut started = Vec::new();
    for link in lineage(access, &row_scope(cause)?).await?.started {
        if link.cause_request_doc_id != cause_doc_id {
            continue;
        }
        if let Some(first) = starting_request(access, &link).await? {
            started.push((link, first));
        }
    }
    Ok(started)
}

async fn view(
    node: &Arc<EmbeddedNode>,
    link: SessionLink,
    caused_by_scope: SessionScope,
    root_session_id: String,
    depth: u32,
) -> Result<Option<CausedSession>> {
    let access = access(node);
    let Some(session) = load_session(
        &access,
        &link.scope.session_id,
        link.scope.requester_did.as_deref(),
    )
    .await?
    .filter(|session| SessionScope::from(session) == link.scope) else {
        return Ok(None);
    };
    let Some(first) = starting_request(&access, &link).await? else {
        return Ok(None);
    };
    let latest = load_session_head(node, &link.scope)
        .await?
        .unwrap_or_else(|| first.clone());
    Ok(Some(CausedSession {
        scope: link.scope,
        agent_id: session.agent_id,
        root_session_id,
        caused_by_scope,
        depth,
        first,
        latest,
    }))
}

pub(crate) async fn load_session_head(
    node: &Arc<EmbeddedNode>,
    scope: &SessionScope,
) -> Result<Option<AgentRequestRow>> {
    let head = ConfigAccess::transact_local_write(node, None, "caused_sessions.head", |txn| {
        Box::pin(async move {
            load_latest_request_in_txn(
                txn,
                &scope.node_did,
                &scope.session_id,
                Some(scope.requester_did.as_deref()),
            )
            .await
        })
    })
    .await?;
    let Some(head) = head else {
        return Ok(None);
    };
    let query = format!(
        r#"{{AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {REQUEST_FIELDS} }}}}"#,
        escape_graphql_string(&head.observed.request_doc_id)
    );
    let mut heads = request_rows(&access(node), &query).await?;
    anyhow::ensure!(heads.len() <= 1, "ambiguous caused session head");
    Ok(heads.pop())
}

async fn request_rows(access: &ConfigAccess, query: &str) -> Result<Vec<AgentRequestRow>> {
    access
        .execute(query)
        .await?
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding AgentRequest row"))
        .collect()
}

fn row_scope(row: &AgentRequestRow) -> Result<SessionScope> {
    Ok(SessionScope {
        node_did: row
            .node_did
            .clone()
            .context("AgentRequest row omitted node_did")?,
        session_id: row
            .session_id
            .clone()
            .context("AgentRequest row omitted session_id")?,
        requester_did: row.requester_did.clone(),
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::BTreeMap;

    use gents::store::graphql::escape_graphql_string;
    use gents_protocol::request_admission::RequestPurpose;
    use serde_json::Value;

    /// The DID of [`fixture_signer`]; sessions it starts carry receipts it signed.
    pub(crate) const FIXTURE_DID: &str = "did:key:z6MkvDqGT54cXesYGvABpF1UapVNwjCqRcafi4Px6Thv5T3Z";

    /// A fixed test key, so fixture rows can name its DID as a constant.
    pub(crate) fn fixture_signer() -> gents::KeyIdentity {
        use gents::NodeIdentity as _;
        use std::io::Write as _;
        // A named temporary file is created private to its owner, as a key must be.
        let mut key = tempfile::NamedTempFile::new().unwrap();
        key.write_all(&crypto::ed25519_key_from_seed(&[7; 32]).unwrap())
            .unwrap();
        let signer = gents::KeyIdentity::load_existing(key.path(), None).unwrap();
        assert_eq!(signer.did(), FIXTURE_DID);
        signer
    }

    /// Write `session` as creation writes a started session: the immutable
    /// starter index at create, then the receipt [`fixture_signer`] signs for
    /// the created physical document, binding `source`, `parent` and `opening`.
    pub(crate) async fn create_started_session(
        node: &gents::defra_node::EmbeddedNode,
        mut session: gents_protocol::session::AgentSession,
        source: gents_protocol::session::SessionReference,
        parent: &str,
        opening: &str,
    ) -> String {
        use gents::NodeIdentity as _;
        use gents_protocol::session::{
            SessionCreationCause, SessionCreationReceipt, SessionCreationStarter, SessionProvenance,
        };
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
            agent_id: session.agent_id.clone(),
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
    pub(crate) fn fixture_session(
        did: &str,
        session_id: &str,
    ) -> gents_protocol::session::AgentSession {
        gents_protocol::session::AgentSession {
            session_id: session_id.into(),
            node_did: did.into(),
            requester_did: Some(did.into()),
            agent_id: "worker".into(),
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

    async fn run(
        node: &gents::defra_node::EmbeddedNode,
        mutation: String,
        collection: &str,
    ) -> String {
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        gents::store::graphql::single_mutation_document(&response, &format!("create_{collection}"))
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
            r#"agent_id: "worker", content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", retry_count: 0"#.to_owned(),
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
    pub(crate) async fn seed_lineage_case(
        node: &gents::defra_node::EmbeddedNode,
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
                    r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{did}", requester_did: "{did}", agent_id: "worker", created_at: "2026-09-26T00:00:00Z"{provenance} }}) {{ _docID }} }}"#
                ),
                "AgentSession",
            )
            .await;
        }
        ids
    }

    /// A parent request in `parent_session` and two child sessions it started,
    /// each opened by a request naming the same tool call (of no session-message
    /// kind). Returns the parent request's `_docID`.
    pub(crate) async fn seed_one_call_two_sessions(
        node: &gents::defra_node::EmbeddedNode,
        did: &str,
        parent_session: &str,
    ) -> String {
        let request = |request_id: &str, session_id: &str, cause: &str| {
            format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "{did}", requester_did: "{did}", agent_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:01Z", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            )
        };
        let parent = run(
            node,
            request("parent-request", parent_session, ""),
            "AgentRequest",
        )
        .await;
        for child in ["child-a", "child-b"] {
            let cause = format!(
                r#"caused_by_parent_request_id: "parent-request", caused_by_parent_request_doc_id: "{parent}", caused_by_parent_tool_call_id: "call-1", caused_by_parent_tool_call_doc_id: "call-doc","#
            );
            let opening = run(
                node,
                request(&format!("{child}-request"), child, &cause),
                "AgentRequest",
            )
            .await;
            create_started_session(
                node,
                fixture_session(did, child),
                gents_protocol::session::SessionReference {
                    node_did: did.to_owned(),
                    session_id: parent_session.to_owned(),
                    requester_did: Some(did.to_owned()),
                },
                &parent,
                &opening,
            )
            .await;
        }
        parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generated_plugin_job_lineage_cases_drive_caused_session_readers() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let did = fixtures::FIXTURE_DID;
        for case in snapshot["plugin_job_lineage_cases"].as_array().unwrap() {
            let dir = tempfile::tempdir().unwrap();
            let node = Arc::new(
                crate::shared::test_support::node_builder()
                    .data_path(dir.path().join("node"))
                    .build()
                    .await
                    .unwrap(),
            );
            gents::store::schema::ensure_runtime_schemas(node.as_ref())
                .await
                .unwrap();
            let ids = fixtures::seed_lineage_case(&node, case, did).await;
            let access = ConfigAccess::local(node.clone());
            for query in case["queries"].as_array().unwrap() {
                let cause = query["cause_request_doc_id"].as_str().unwrap();
                let link = SessionLink {
                    scope: SessionScope {
                        node_did: did.into(),
                        session_id: query["session_id"].as_str().unwrap().into(),
                        requester_did: Some(did.into()),
                    },
                    cause_request_doc_id: ids.get(cause).cloned().unwrap_or(cause.into()),
                };
                let expected = query["expected_opening_doc_id"]
                    .as_str()
                    .map(|id| ids[id].clone());
                let found = starting_request(&access, &link)
                    .await
                    .unwrap()
                    .and_then(|row| row.doc_id);
                assert_eq!(found, expected, "{}: {query}", case["name"]);
            }
            node.shutdown().await;
        }
    }
}
