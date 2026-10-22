use std::collections::BTreeMap;
use std::sync::Arc;

use gents::config_client::ConfigAccess;
use gents::graphql::graphql_string_list_literal;
use gents::session::{public_request_filter, session_scope_filter};
use gents::session_origin::{
    group_caused, lineage, single_caused, starting_request, SessionLink, SessionScope,
};
use gents::toolset::is_session_message_tool;
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::types::{
    CausedCallView, CausedRequestView, DesktopSessionProvenanceRequest, LinkedSessionView,
    SessionProvenanceView, TurnSenderView,
};

pub async fn session_provenance_request(
    core: &Arc<ClientCore>,
    request: DesktopSessionProvenanceRequest,
) -> Result<SessionProvenanceView, String> {
    let node_did = request
        .node_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| core.selected_node_did())
        .ok_or("no node selected; pass nodeDid explicitly")?;
    session_provenance(
        core,
        SessionScope {
            node_did,
            session_id: request.session_id,
            requester_did: request.requester_did,
        },
    )
    .await
}

pub async fn session_provenance(
    core: &Arc<ClientCore>,
    scope: SessionScope,
) -> Result<SessionProvenanceView, String> {
    let access = crate::snapshot::node_reads(core, &scope.node_did);
    let fail = |error: anyhow::Error| format!("session provenance: {error:#}");
    let lineage = lineage(&access, &scope).await.map_err(fail)?;
    let calls = caused_calls(&access, &scope).await.map_err(fail)?;
    let opening_request_id = match &lineage.started_by {
        Some(started_by) => opening_request(
            &access,
            &SessionLink {
                scope: scope.clone(),
                cause_request_doc_id: started_by.cause_request_doc_id.clone(),
            },
        )
        .await
        .map_err(fail)?
        .map(|row| row.request_id),
        None => None,
    };
    let senders = turn_senders(&lineage);
    let link = |link: &SessionLink| LinkedSessionView {
        node_did: link.scope.node_did.clone(),
        session_id: link.scope.session_id.clone(),
        requester_did: link.scope.requester_did.clone(),
        cause_request_doc_id: link.cause_request_doc_id.clone(),
    };
    Ok(SessionProvenanceView {
        session_id: scope.session_id.clone(),
        started_by: lineage.started_by.as_ref().map(link),
        started: lineage.started.iter().map(link).collect(),
        sent: lineage.sent.iter().map(link).collect(),
        received: lineage.received.iter().map(link).collect(),
        senders: senders
            .into_iter()
            .map(|(request_id, sender)| TurnSenderView {
                request_id,
                sender: link(sender),
            })
            .collect(),
        opening_request_id,
        calls,
    })
}

async fn rows(access: &ConfigAccess, query: &str, collection: &str) -> anyhow::Result<Vec<Value>> {
    Ok(access
        .execute(query)
        .await?
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn text(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// The request that opened `link.scope`'s session from `link`'s cause, as
/// `gents::session_origin::starting_request` selects it (C7, Lean
/// `DurableLineage.openingRequest`); `session_provenance` and the lineage
/// cases both read it here.
async fn opening_request(
    access: &ConfigAccess,
    link: &SessionLink,
) -> anyhow::Result<Option<AgentRequestRow>> {
    starting_request(access, link).await
}

async fn caused_calls(
    access: &ConfigAccess,
    scope: &SessionScope,
) -> anyhow::Result<Vec<CausedCallView>> {
    let own = rows(
        access,
        &format!(
            "{{AgentRequest(filter: {{{}}}) {{_docID}}}}",
            public_request_filter(&session_scope_filter(
                &scope.node_did,
                &scope.session_id,
                scope.requester_did.as_deref(),
            ))
        ),
        "AgentRequest",
    )
    .await?;
    let own_doc_ids = own
        .iter()
        .filter_map(|row| row.get("_docID").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if own_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let states = rows(
        access,
        &format!(
            "{{AgentRequest(filter: {{{}}}) {{_docID node_did session_id requester_did request_id created_at lifecycle_state caused_by_parent_tool_call_doc_id}}}}",
            public_request_filter(&format!(
                "caused_by_parent_request_doc_id: {{_in: {}}}, caused_by_parent_tool_call_doc_id: {{_ne: null}}",
                graphql_string_list_literal(own_doc_ids.iter().copied()),
            ))
        ),
        "AgentRequest",
    )
    .await?;
    let caused = group_caused(&states);
    if caused.is_empty() {
        return Ok(Vec::new());
    }
    let state_of = states
        .iter()
        .filter_map(|row| Some((text(row, "_docID")?, row)))
        .collect::<BTreeMap<_, _>>();
    let call_doc_ids = caused.keys().map(String::as_str).collect::<Vec<_>>();
    let mut calls = Vec::new();
    // Bounded lookups, never an empty list literal in a database operation.
    for batch in call_doc_ids.chunks(128) {
        calls.extend(
            rows(
                access,
                &format!(
                    "{{AgentToolCall(docIDs: {}) {{_docID request_id tool_call_id tool_name}}}}",
                    graphql_string_list_literal(batch.iter().copied())
                ),
                "AgentToolCall",
            )
            .await?,
        );
    }
    Ok(calls
        .iter()
        .filter_map(|call| {
            let requests = caused.get(&text(call, "_docID")?)?;
            let session_message = is_session_message_tool(&text(call, "tool_name")?);
            let shown = if session_message {
                single_caused(requests.clone()).into_iter().collect()
            } else {
                requests.clone()
            };
            Some((call, shown))
        })
        .flat_map(|(call, shown)| {
            let state_of = &state_of;
            shown.into_iter().filter_map(move |request| {
                let state = state_of.get(&request.request_doc_id)?;
                Some(CausedCallView {
                    request_id: text(call, "request_id")?,
                    tool_call_id: text(call, "tool_call_id")?,
                    caused: CausedRequestView {
                        request_id: request.request_id,
                        node_did: request.scope.node_did,
                        session_id: request.scope.session_id,
                        requester_did: request.scope.requester_did,
                        lifecycle_state: text(state, "lifecycle_state"),
                        created_at: Some(request.created_at),
                    },
                })
            })
        })
        .collect())
}

fn turn_senders<'a>(
    lineage: &'a gents::session_origin::SessionLineage,
) -> Vec<(String, &'a SessionLink)> {
    let links = lineage
        .started_by
        .iter()
        .chain(lineage.received.iter())
        .collect::<Vec<_>>();
    if links.is_empty() {
        return Vec::new();
    }
    lineage
        .request_causes
        .iter()
        .filter_map(|request| {
            let sender = links
                .iter()
                .find(|link| link.cause_request_doc_id == request.cause_request_doc_id)
                .or_else(|| {
                    let cause_scope = request.cause_scope.as_ref()?;
                    links.iter().find(|link| link.scope == *cause_scope)
                })?;
            Some((request.request_id.clone(), *sender))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use gents::graphql::escape_graphql_string;
    use gents_protocol::request_admission::RequestPurpose;

    async fn run(
        node: &gents::defra_node::EmbeddedNode,
        mutation: String,
        collection: &str,
    ) -> String {
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        gents::graphql::single_mutation_document(&response, &format!("create_{collection}"))
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
    async fn seed_lineage_case(
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
            let mut provenance = Vec::new();
            if let Some(parent) = own
                .iter()
                .find_map(|other| other["caused_by_parent_request_doc_id"].as_str())
            {
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

    #[tokio::test]
    async fn generated_plugin_job_lineage_cases_drive_desktop_opening_request() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["plugin_job_lineage_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        let did = "did:test:node";
        for case in cases {
            let dir = tempfile::tempdir().unwrap();
            let node = Arc::new(
                crate::tests::support::node_builder()
                    .data_path(dir.path().join("node"))
                    .build()
                    .await
                    .unwrap(),
            );
            gents::schema::ensure_runtime_schemas(node.as_ref())
                .await
                .unwrap();
            let ids = seed_lineage_case(&node, case, did).await;
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
                let found = opening_request(&access, &link)
                    .await
                    .unwrap()
                    .and_then(|row| row.doc_id);
                assert_eq!(found, expected, "{}: {query}", case["name"]);
            }
            node.shutdown().await;
        }
    }
}
