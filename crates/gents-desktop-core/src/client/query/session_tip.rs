use super::*;
use gents::session::canonical_rows::{
    decode_scoped_canonical_rows, decode_scoped_request_output_segments,
    decode_transcript_message_row, AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS,
};

/// An exact prompt-owner lookup is independent of session-history coverage.
/// Only an unfinished request needs live output facts. Keep the existing
/// canonical dependency cap: overflow must not masquerade as complete input
/// to the live target selector or as evidence that a prompt is absent.
const MAX_TIP_REQUEST_ROWS: usize = 2_048;

pub async fn load_session_tip_store(
    node: &EmbeddedNode,
    request: &AgentRequestRow,
) -> Result<ClientStore> {
    let query = tip_query(request, false)?;
    let data = execute_local_graphql_query(node, &query, "session tip").await?;
    tip_store(&data, request)
}

pub async fn load_session_tip_store_on(
    access: &gents::config_client::ConfigAccess,
    request: &AgentRequestRow,
) -> Result<ClientStore> {
    let query = tip_query(request, false)?;
    let data = execute_access_graphql_query(access, &query, "session tip").await?;
    tip_store(&data, request)
}

/// One indexed request read supplies both fresh lifecycle evidence and canonical
/// output. These rows live only for this projection: never merge payloads into
/// the observer or retain a per-request cache. Re-reading the bounded source
/// observes late CRDT twins, gaps and closure-only records as well as appends.
pub async fn load_session_live_store(
    node: &EmbeddedNode,
    request: &AgentRequestRow,
) -> Result<ClientStore> {
    let query = tip_query(request, true)?;
    let data = execute_local_graphql_query(node, &query, "session live output").await?;
    live_store(&data, request)
}

pub async fn load_session_live_store_on(
    access: &gents::config_client::ConfigAccess,
    request: &AgentRequestRow,
) -> Result<ClientStore> {
    let query = tip_query(request, true)?;
    let data = execute_access_graphql_query(access, &query, "session live output").await?;
    live_store(&data, request)
}

fn live_store(data: &Value, expected: &AgentRequestRow) -> Result<ClientStore> {
    let requests: Vec<AgentRequestRow> = parse_query_rows(data, AGENT_REQUEST_NAME)?;
    let [request] = requests.as_slice() else {
        return Ok(ClientStore::default());
    };
    anyhow::ensure!(
        request.doc_id == expected.doc_id
            && request.request_id == expected.request_id
            && request.agent_did == expected.agent_did
            && request.session_id == expected.session_id
            && request.requester_did == expected.requester_did,
        "live request scope changed"
    );
    let mut rows = tip_rows(data, expected)?;
    rows.requests = requests;
    Ok(ClientStore::from_rows(rows))
}

/// Exact scoped prompt observations and compact durable anchors for one page read.
/// Absence is known only for physical requests whose bounded lookup succeeded.
#[derive(Debug, Clone, Default)]
pub struct RequestPromptOwnership {
    pub by_request_doc_id: std::collections::BTreeMap<String, RequestPromptFact>,
}

#[derive(Debug, Clone)]
pub struct RequestPromptFact {
    pub agent_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
    pub materialized: bool,
    pub first_sequence: Option<i64>,
}

pub async fn load_request_prompt_ownership(
    node: &EmbeddedNode,
    requests: &[AgentRequestRow],
) -> Result<RequestPromptOwnership> {
    let mut facts = RequestPromptOwnership::default();
    for chunk in requests.chunks(32) {
        if let Some(query) = prompt_ownership_batch_query(chunk) {
            let data =
                execute_local_graphql_query(node, &query, "request prompt ownership").await?;
            if decode_prompt_ownership_batch(&data, chunk, &mut facts)? {
                continue;
            }
        }
        let query = prompt_ownership_query(chunk)?;
        let data = execute_local_graphql_query(node, &query, "request prompt ownership").await?;
        decode_prompt_ownership(&data, chunk, &mut facts)?;
    }
    Ok(facts)
}

pub async fn load_request_prompt_ownership_on(
    access: &gents::config_client::ConfigAccess,
    requests: &[AgentRequestRow],
) -> Result<RequestPromptOwnership> {
    let mut facts = RequestPromptOwnership::default();
    for chunk in requests.chunks(32) {
        if let Some(query) = prompt_ownership_batch_query(chunk) {
            let data =
                execute_access_graphql_query(access, &query, "request prompt ownership").await?;
            if decode_prompt_ownership_batch(&data, chunk, &mut facts)? {
                continue;
            }
        }
        let query = prompt_ownership_query(chunk)?;
        let data = execute_access_graphql_query(access, &query, "request prompt ownership").await?;
        decode_prompt_ownership(&data, chunk, &mut facts)?;
    }
    Ok(facts)
}

fn prompt_ownership_batch_query(requests: &[AgentRequestRow]) -> Option<String> {
    let first = requests.first()?;
    let agent = escape_graphql_string(first.agent_did.as_deref()?);
    let session = escape_graphql_string(first.session_id.as_deref()?);
    let requester = first
        .requester_did
        .as_deref()
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let ids = requests
        .iter()
        .map(|request| {
            if request.agent_did != first.agent_did
                || request.session_id != first.session_id
                || request.requester_did != first.requester_did
            {
                return None;
            }
            Some(format!(
                "\"{}\"",
                escape_graphql_string(request.doc_id.as_deref()?)
            ))
        })
        .collect::<Option<Vec<_>>>()?
        .join(",");
    let limit = MAX_TIP_REQUEST_ROWS + 1;
    Some(format!(
        r#"query DesktopRequestPromptOwnership {{
        AgentMessage(filter: {{agent_did: {{_eq: "{agent}"}}, session_id: {{_eq: "{session}"}},
            requester_did: {{_eq: {requester}}}, request_doc_id: {{_in: [{ids}]}}}},
            order: {{sequence: ASC}}, limit: {limit}) {{
            _docID request_doc_id agent_did session_id requester_did message_key role sequence
        }}
    }}"#
    ))
}

/// Only a complete bounded read can establish missing prompts. Oversized
/// histories and tied anchors retain the exact per-request lookup path.
fn decode_prompt_ownership_batch(
    data: &Value,
    requests: &[AgentRequestRow],
    facts: &mut RequestPromptOwnership,
) -> Result<bool> {
    let rows = data
        .get(AGENT_MESSAGE_NAME)
        .and_then(Value::as_array)
        .context("prompt batch omitted rows")?;
    if rows.len() > MAX_TIP_REQUEST_ROWS {
        return Ok(false);
    }
    let mut observations = serde_json::Map::new();
    for (index, request) in requests.iter().enumerate() {
        let doc = request
            .doc_id
            .as_deref()
            .context("prompt lookup lacks physical request")?;
        let matching = rows
            .iter()
            .filter(|row| row["request_doc_id"].as_str() == Some(doc))
            .collect::<Vec<_>>();
        if matching.len() > 1 && matching[0]["sequence"] == matching[1]["sequence"] {
            return Ok(false);
        }
        let key = format!("authored:{doc}:prompt");
        observations.insert(
            format!("p{index}"),
            Value::Array(
                matching
                    .iter()
                    .filter(|row| {
                        row["message_key"].as_str() == Some(key.as_str()) && row["role"] == "user"
                    })
                    .take(2)
                    .map(|row| (*row).clone())
                    .collect(),
            ),
        );
        observations.insert(
            format!("a{index}"),
            Value::Array(
                matching
                    .first()
                    .map(|row| (*row).clone())
                    .into_iter()
                    .collect(),
            ),
        );
    }
    decode_prompt_ownership(&Value::Object(observations), requests, facts)?;
    Ok(true)
}

fn prompt_ownership_query(requests: &[AgentRequestRow]) -> Result<String> {
    let mut fields = Vec::new();
    for (index, request) in requests.iter().enumerate() {
        let doc = request
            .doc_id
            .as_deref()
            .context("prompt lookup lacks physical request")?;
        let agent = request
            .agent_did
            .as_deref()
            .context("prompt lookup lacks principal")?;
        let session = request
            .session_id
            .as_deref()
            .context("prompt lookup lacks session")?;
        let requester = request
            .requester_did
            .as_deref()
            .map(|did| format!("\"{}\"", escape_graphql_string(did)))
            .unwrap_or_else(|| "null".into());
        let scope = format!(
            r#"request_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}"#,
            escape_graphql_string(doc),
            escape_graphql_string(agent),
            escape_graphql_string(session)
        );
        let key = escape_graphql_string(&format!("authored:{doc}:prompt"));
        fields.push(format!(r#"p{index}: AgentMessage(filter: {{ {scope}, message_key: {{ _eq: "{key}" }}, role: {{ _eq: "user" }} }}, limit: 2) {{ _docID request_doc_id agent_did session_id requester_did message_key role sequence }}
        a{index}: AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}, limit: 1) {{ _docID request_doc_id agent_did session_id requester_did message_key sequence }}"#));
    }
    Ok(format!(
        "query DesktopRequestPromptOwnership {{ {} }}",
        fields.join("\n")
    ))
}

fn decode_prompt_ownership(
    data: &Value,
    requests: &[AgentRequestRow],
    facts: &mut RequestPromptOwnership,
) -> Result<()> {
    for (index, request) in requests.iter().enumerate() {
        let doc = request
            .doc_id
            .as_ref()
            .context("prompt lookup lacks physical request")?;
        let prompt = data
            .get(format!("p{index}"))
            .and_then(Value::as_array)
            .context("prompt lookup omitted rows")?;
        let anchor = data
            .get(format!("a{index}"))
            .and_then(Value::as_array)
            .context("prompt anchor lookup omitted rows")?;
        anyhow::ensure!(
            prompt.len() <= 1 && anchor.len() <= 1,
            "prompt ownership is physically ambiguous"
        );
        for row in prompt.iter().chain(anchor.iter()) {
            anyhow::ensure!(
                row["request_doc_id"].as_str() == Some(doc.as_str())
                    && row["agent_did"].as_str() == request.agent_did.as_deref()
                    && row["session_id"].as_str() == request.session_id.as_deref()
                    && row["requester_did"].as_str() == request.requester_did.as_deref(),
                "prompt observation crossed physical request scope"
            );
        }
        if let Some(row) = prompt.first() {
            anyhow::ensure!(
                row["message_key"].as_str() == Some(format!("authored:{doc}:prompt").as_str())
                    && row["role"].as_str() == Some("user"),
                "prompt observation is not the exact authored owner"
            );
        }
        facts.by_request_doc_id.insert(
            doc.clone(),
            RequestPromptFact {
                agent_did: request
                    .agent_did
                    .clone()
                    .context("prompt lookup lacks principal")?,
                session_id: request
                    .session_id
                    .clone()
                    .context("prompt lookup lacks session")?,
                requester_did: request.requester_did.clone(),
                materialized: !prompt.is_empty(),
                first_sequence: anchor
                    .first()
                    .filter(|row| {
                        let input = request.input.clone().unwrap_or_default();
                        !gents::lifecycle::is_runtime_control_message(
                            &input,
                            row["message_key"].as_str().unwrap_or_default(),
                        )
                    })
                    .map(|row| {
                        row["sequence"]
                            .as_i64()
                            .context("prompt anchor lacks sequence")
                    })
                    .transpose()?,
            },
        );
    }
    Ok(())
}

fn tip_query(request: &AgentRequestRow, include_request: bool) -> Result<String> {
    let doc = escape_graphql_string(
        request
            .doc_id
            .as_deref()
            .context("tip request lacks physical identity")?,
    );
    let agent = escape_graphql_string(
        request
            .agent_did
            .as_deref()
            .context("tip request lacks principal")?,
    );
    let session = escape_graphql_string(
        request
            .session_id
            .as_deref()
            .context("tip request lacks session")?,
    );
    let requester = request
        .requester_did
        .as_deref()
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .unwrap_or_else(|| "null".into());
    // DefraDB's capped cardinality estimates can tie once a request exceeds
    // 1024 segments and choose an agent-wide index. One indexed predicate
    // fixes the scan to this physical request; the canonical row owner checks the
    // remaining namespace before decoding. ACP stays with ConfigAccess.
    let scope = format!(r#"request_doc_id: {{ _eq: "{doc}" }}"#);
    let live = include_request
        || request
            .lifecycle_state
            .is_some_and(|state| !state.is_terminal());
    let limit = MAX_TIP_REQUEST_ROWS + 1;
    let header_filter = if live {
        scope.clone()
    } else {
        let key = escape_graphql_string(&format!(
            "authored:{}:prompt",
            request.doc_id.as_deref().unwrap()
        ));
        format!(r#"{scope}, message_key: {{ _eq: "{key}" }}"#)
    };
    let segments = if live {
        format!(
            r#"AgentOutputSegment(filter: {{ {scope} }}, limit: {limit}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}"#
        )
    } else {
        String::new()
    };
    let request_fields = if include_request {
        format!(
            r#"AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, agent_did: {{ _eq: "{agent}" }}, session_id: {{ _eq: "{session}" }}, requester_did: {{ _eq: {requester} }} }}, limit: 2) {{ {AGENT_REQUEST_FIELDS} }}"#
        )
    } else {
        String::new()
    };
    Ok(format!(
        r#"query DesktopSessionTip {{
        {request_fields}
        AgentMessage(filter: {{ {header_filter} }}, limit: {limit}) {{ {AGENT_MESSAGE_FIELDS} }}
        {segments}
    }}"#
    ))
}

fn tip_store(data: &Value, request: &AgentRequestRow) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(tip_rows(data, request)?))
}

fn bounded_tip_rows<'a>(data: &'a Value, root: &str) -> Result<&'a [Value]> {
    let rows = data
        .get(root)
        .and_then(Value::as_array)
        .with_context(|| format!("session tip omitted {root} rows"))?;
    anyhow::ensure!(
        rows.len() <= MAX_TIP_REQUEST_ROWS,
        "active request exceeds bounded session-tip read of {MAX_TIP_REQUEST_ROWS} rows"
    );
    Ok(rows)
}

fn tip_rows(data: &Value, request: &AgentRequestRow) -> Result<ClientStoreRows> {
    let agent = request
        .agent_did
        .as_deref()
        .context("tip request lacks principal")?;
    let session = request
        .session_id
        .as_deref()
        .context("tip request lacks session")?;
    let messages = decode_scoped_canonical_rows(
        bounded_tip_rows(data, AGENT_MESSAGE_NAME)?,
        agent,
        Some(session),
        request.requester_did.as_deref(),
        decode_transcript_message_row,
    )?;
    let segments = if data.get(AGENT_OUTPUT_SEGMENT_NAME).is_some() {
        decode_scoped_request_output_segments(
            bounded_tip_rows(data, AGENT_OUTPUT_SEGMENT_NAME)?,
            agent,
            Some(session),
            request.requester_did.as_deref(),
        )?
    } else {
        Vec::new()
    };
    Ok(ClientStoreRows {
        transcript_messages: messages,
        output_segments: segments,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::config_client::ConfigAccess;
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    #[tokio::test]
    async fn reopening_completed_request_reads_only_its_prompt_and_active_reads_stay_request_scoped(
    ) {
        let node = defra_node::NodeBuilder::default().build().await.unwrap();
        crate::client::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        for request in ["completed", "active"] {
            let count = if request == "completed" { 120 } else { 1 };
            let mutations = (0..count)
                .map(|sequence| {
                    let key = if sequence == 0 {
                        format!("authored:{request}:prompt")
                    } else {
                        format!("{request}:{sequence}")
                    };
                    let key = escape_graphql_string(&key);
                    let request = escape_graphql_string(request);
                    format!(
                        r#"m{sequence}: create_AgentMessage(input: {{
                    message_key: "{key}", session_id: "session", agent_did: "agent",
                    request_doc_id: "{request}", requester_did: null,
                    publication: {{kind: "request_execution", execution_generation: "generation"}},
                    outcome: "complete", sequence: {sequence}, role: "user", blocks: null,
                    created_at: "2026-09-30T00:00:00Z"
                }}) {{ _docID }}"#
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            ConfigAccess::write_local(
                &node,
                "test.tip_seed",
                &format!("mutation {{ {mutations} }}"),
            )
            .await
            .unwrap();
        }
        // A historic payload is deliberately undecodable: selecting the whole
        // session would fail even though neither tip needs these bytes.
        ConfigAccess::write_local(&node, "test.tip_history", r#"mutation {
            create_AgentOutputSegment(input: {agent_did:"agent", session_id:"session",
                request_doc_id:"completed", source:{kind:"not_a_source"}, payload:"historic"}) { _docID }
        }"#).await.unwrap();
        for (id, state) in [
            ("completed", RequestLifecycleState::Completed),
            ("active", RequestLifecycleState::Processing),
        ] {
            let request = AgentRequestRow {
                doc_id: Some(id.into()),
                request_id: id.into(),
                agent_did: Some("agent".into()),
                session_id: Some("session".into()),
                lifecycle_state: Some(state),
                ..Default::default()
            };
            let tip = load_session_tip_store(&node, &request).await.unwrap();
            assert_eq!(tip.transcript_messages.len(), 1);
            assert_eq!(
                tip.transcript_messages[0].message.message_key,
                format!("authored:{id}:prompt")
            );
            assert!(tip.output_segments.is_empty());
        }
        let scoped = |id: &str| AgentRequestRow {
            doc_id: Some(id.into()),
            request_id: id.into(),
            agent_did: Some("agent".into()),
            session_id: Some("session".into()),
            ..Default::default()
        };
        let mut requests = vec![scoped("completed"), scoped("interrupted")];
        let ownership = load_request_prompt_ownership(&node, &requests)
            .await
            .unwrap();
        let exact = execute_local_graphql_query(
            &node,
            &prompt_ownership_query(&requests).unwrap(),
            "exact prompt ownership comparison",
        )
        .await
        .unwrap();
        let mut exact_facts = RequestPromptOwnership::default();
        decode_prompt_ownership(&exact, &requests, &mut exact_facts).unwrap();
        for (doc, fact) in &ownership.by_request_doc_id {
            let expected = &exact_facts.by_request_doc_id[doc];
            assert_eq!(fact.materialized, expected.materialized);
            assert_eq!(fact.first_sequence, expected.first_sequence);
        }
        assert!(ownership.by_request_doc_id["completed"].materialized);
        assert_eq!(
            ownership.by_request_doc_id["completed"].first_sequence,
            Some(0)
        );
        assert!(!ownership.by_request_doc_id["interrupted"].materialized);
        assert_eq!(
            ownership.by_request_doc_id["interrupted"].first_sequence,
            None
        );
        requests[0].requester_did = Some("different-requester".into());
        assert!(prompt_ownership_batch_query(&requests).is_none());
        let other_scope = load_request_prompt_ownership(&node, &requests[..1])
            .await
            .unwrap();
        assert!(!other_scope.by_request_doc_id["completed"].materialized);
        assert_eq!(
            other_scope.by_request_doc_id["completed"].first_sequence,
            None
        );
        let control_anchor = serde_json::json!({"p0":[], "a0":[{
            "request_doc_id":"completed", "agent_did":"agent", "session_id":"session",
            "requester_did":"different-requester", "sequence":0,
            "message_key":"background-completion-notification:result:done"
        }]});
        let mut control_facts = RequestPromptOwnership::default();
        decode_prompt_ownership(&control_anchor, &requests[..1], &mut control_facts).unwrap();
        assert_eq!(
            control_facts.by_request_doc_id["completed"].first_sequence,
            None
        );
        let malformed = serde_json::json!({"p0":[], "a0":[{"request_doc_id":"foreign", "agent_did":"agent", "session_id":"session", "requester_did":"different-requester", "sequence":0}]});
        assert!(decode_prompt_ownership(
            &malformed,
            &requests[..1],
            &mut RequestPromptOwnership::default()
        )
        .is_err());
        node.shutdown().await;
    }

    #[test]
    fn incomplete_prompt_batches_do_not_establish_absence_or_select_tied_anchors() {
        let requests = [AgentRequestRow {
            doc_id: Some("request".into()),
            agent_did: Some("agent".into()),
            session_id: Some("session".into()),
            ..Default::default()
        }];
        let row = serde_json::json!({
            "request_doc_id": "request", "agent_did": "agent", "session_id": "session",
            "requester_did": null, "message_key": "authored:request:prompt",
            "role": "user", "sequence": 0
        });
        for rows in [
            vec![row.clone(); MAX_TIP_REQUEST_ROWS + 1],
            vec![row.clone(); 2],
        ] {
            let mut facts = RequestPromptOwnership::default();
            assert!(!decode_prompt_ownership_batch(
                &serde_json::json!({"AgentMessage": rows}),
                &requests,
                &mut facts,
            )
            .unwrap());
            assert!(facts.by_request_doc_id.is_empty());
        }
        let mut later = row.clone();
        later["sequence"] = serde_json::json!(1);
        assert!(decode_prompt_ownership_batch(
            &serde_json::json!({"AgentMessage": [row, later]}),
            &requests,
            &mut RequestPromptOwnership::default(),
        )
        .is_err());
    }
}

#[cfg(test)]
mod live_tests;
