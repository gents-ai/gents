use super::*;
use gents_session::views::requests::RequestRows;
use gents_session::views::transcript::{MessageIdentities, MessageRows, Scope, SegmentRows};
use gents_store::config_client::ConfigAccess;

/// Bound the rows read for an active request; oversized results fail instead
/// of silently presenting a truncated tip.
const MAX_TIP_REQUEST_ROWS: usize = 2_048;

pub async fn load_session_tip_store_on(
    access: &ConfigAccess,
    request: &AgentRequestRow,
) -> Result<ClientStore> {
    tip_store(read_tip(access, request, false).await?, request)
}

pub async fn load_session_live_store_on(
    access: &ConfigAccess,
    request: &AgentRequestRow,
) -> Result<ClientStore> {
    live_store(read_tip(access, request, true).await?, request)
}

/// The live rows of `request_id` when `observed` holds exactly one such
/// request of `session_id` on `node_did` and the session's requester made it;
/// an empty store otherwise.
pub async fn load_session_request_live_store_on(
    access: &gents_store::config_client::ConfigAccess,
    observed: &ClientStore,
    session_id: &str,
    node_did: &str,
    request_id: &str,
) -> Result<ClientStore> {
    let matches = observed
        .requests
        .iter()
        .filter(|row| {
            row.request_id == request_id
                && row.session_id.as_deref() == Some(session_id)
                && row.node_did.as_deref() == Some(node_did)
        })
        .collect::<Vec<_>>();
    let [request] = matches.as_slice() else {
        return Ok(ClientStore::default());
    };
    let requester = observed
        .sessions
        .iter()
        .find(|row| row.session_id == session_id && row.node_did == node_did)
        .and_then(|session| session.requester_did.as_deref());
    if request.requester_did.as_deref() != requester {
        return Ok(ClientStore::default());
    }
    load_session_live_store_on(access, request).await
}

/// The live content and reasoning of the session's turn request on
/// `node_did` in `observed`, read over `access`; `None` once that request is
/// terminal or while it has no live output.
pub async fn load_session_live_text_on(
    access: &gents_store::config_client::ConfigAccess,
    observed: &ClientStore,
    session_id: &str,
    node_did: &str,
) -> Result<Option<crate::client::canonical_output::CanonicalLiveText>> {
    let Some(request_id) = observed.turn_request_id_for_session_for_node(session_id, node_did)
    else {
        return Ok(None);
    };
    let live =
        load_session_request_live_store_on(access, observed, session_id, node_did, &request_id)
            .await?;
    if live
        .derive_turn_for_request_for_node(&request_id, node_did)
        .is_none_or(|state| state.is_terminal())
    {
        return Ok(None);
    }
    Ok(crate::client::canonical_output::canonical_live_text(
        observed,
        &live,
        session_id,
        Some(node_did),
        &request_id,
    ))
}

fn live_store(data: TipRead, expected: &AgentRequestRow) -> Result<ClientStore> {
    let [request] = data.requests.as_slice() else {
        return Ok(ClientStore::default());
    };
    anyhow::ensure!(
        request.doc_id == expected.doc_id
            && request.request_id == expected.request_id
            && request.node_did == expected.node_did
            && request.session_id == expected.session_id
            && request.requester_did == expected.requester_did,
        "live request scope changed"
    );
    tip_store(data, expected)
}

#[derive(Debug, Clone, Default)]
pub struct RequestPromptOwnership {
    pub by_request_doc_id: std::collections::BTreeMap<String, RequestPromptFact>,
}

#[derive(Debug, Clone)]
pub struct RequestPromptFact {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
    pub materialized: bool,
    pub first_sequence: Option<i64>,
}

fn input_owner(request: &AgentRequestRow) -> Result<(String, String, bool)> {
    let (owner, key) = gents_session::request::lifecycle::input_message_owner(request)
        .context("prompt lookup lacks physical request")?;
    let own = request.doc_id.as_deref() == Some(owner.as_str());
    Ok((owner, key, own))
}

pub async fn load_request_prompt_ownership_on(
    access: &ConfigAccess,
    requests: &[AgentRequestRow],
) -> Result<RequestPromptOwnership> {
    let mut facts = RequestPromptOwnership::default();
    for chunk in requests.chunks(32) {
        let observations = prompt_observations(access, chunk).await?;
        decode_prompt_ownership(&observations, chunk, &mut facts)?;
    }
    Ok(facts)
}

async fn prompt_observations(access: &ConfigAccess, requests: &[AgentRequestRow]) -> Result<Value> {
    let mut views = Vec::new();
    for request in requests {
        let (owner, key, own) = input_owner(request)?;
        let scope = Scope::Session(gents_protocol::session::SessionScope {
            node_did: request
                .node_did
                .clone()
                .context("prompt lookup lacks node")?,
            session_id: request
                .session_id
                .clone()
                .context("prompt lookup lacks session")?,
            requester_did: request.requester_did.clone(),
        });
        let mut prompt = MessageRows::new(scope.clone());
        prompt.request_doc_ids = Some(vec![owner.clone()]);
        prompt.message_key = Some(key.clone());
        prompt.role = Some(gents_protocol::output::MessageRole::User);
        prompt.limit = Some(2);
        let mut anchor = MessageRows::new(scope);
        anchor.request_doc_ids = Some(vec![owner]);
        anchor.limit = Some(1);
        if !own {
            anchor.message_key = Some(key);
            anchor.role = Some(gents_protocol::output::MessageRole::User);
        }
        views.push(MessageIdentities(prompt));
        views.push(MessageIdentities(anchor));
    }
    let rows = access.read_views(&views).await?;
    let mut observations = serde_json::Map::new();
    for (index, rows) in rows.into_iter().enumerate() {
        let key = if index % 2 == 0 {
            format!("p{}", index / 2)
        } else {
            format!("a{}", index / 2)
        };
        observations.insert(key, serde_json::to_value(rows)?);
    }
    Ok(Value::Object(observations))
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
        let (owner, key, _) = input_owner(request)?;
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
                row["request_doc_id"].as_str() == Some(owner.as_str())
                    && row["node_did"].as_str() == request.node_did.as_deref()
                    && row["session_id"].as_str() == request.session_id.as_deref()
                    && row["requester_did"].as_str() == request.requester_did.as_deref(),
                "prompt observation crossed physical request scope"
            );
        }
        if let Some(row) = prompt.first() {
            anyhow::ensure!(
                row["message_key"].as_str() == Some(key.as_str())
                    && row["role"].as_str() == Some("user"),
                "prompt observation is not the exact authored owner"
            );
        }
        facts.by_request_doc_id.insert(
            doc.clone(),
            RequestPromptFact {
                node_did: request
                    .node_did
                    .clone()
                    .context("prompt lookup lacks node")?,
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
                        !gents_session::request::lifecycle::is_runtime_control_message(
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

struct TipRead {
    requests: Vec<AgentRequestRow>,
    messages: Vec<gents_session::views::transcript::MessageRow>,
    segments: Vec<gents_session::views::transcript::SegmentRow>,
}

async fn read_tip(
    access: &ConfigAccess,
    request: &AgentRequestRow,
    include_request: bool,
) -> Result<TipRead> {
    let doc = request
        .doc_id
        .clone()
        .context("tip request lacks physical identity")?;
    let scope = Scope::Session(gents_protocol::session::SessionScope {
        node_did: request.node_did.clone().context("tip request lacks node")?,
        session_id: request
            .session_id
            .clone()
            .context("tip request lacks session")?,
        requester_did: request.requester_did.clone(),
    });
    let live = include_request
        || request
            .lifecycle_state
            .is_some_and(|state| !state.is_terminal());
    let mut headers = MessageRows::new(scope.clone());
    headers.request_doc_ids = Some(vec![doc.clone()]);
    headers.limit = Some((MAX_TIP_REQUEST_ROWS + 1) as u64);
    if !live {
        headers.message_key = Some(format!("authored:{doc}:prompt"));
    }
    let mut segments = SegmentRows::new(scope.clone());
    segments.request_doc_ids = Some(vec![doc.clone()]);
    segments.limit = Some((MAX_TIP_REQUEST_ROWS + 1) as u64);
    segments.provider_live_only = include_request;
    if include_request {
        let requests = RequestRows::new(scope).doc_ids(vec![doc]).limit(2);
        let (requests, messages, segments) =
            access.read_views(&(requests, headers, segments)).await?;
        Ok(TipRead {
            requests,
            messages,
            segments,
        })
    } else if live {
        let (messages, segments) = access.read_views(&(headers, segments)).await?;
        Ok(TipRead {
            requests: Vec::new(),
            messages,
            segments,
        })
    } else {
        Ok(TipRead {
            requests: Vec::new(),
            messages: access.read_view(&headers).await?,
            segments: Vec::new(),
        })
    }
}

fn tip_store(data: TipRead, request: &AgentRequestRow) -> Result<ClientStore> {
    let node = request
        .node_did
        .as_deref()
        .context("tip request lacks node")?;
    let session = request
        .session_id
        .as_deref()
        .context("tip request lacks session")?;
    anyhow::ensure!(
        data.messages.len() <= MAX_TIP_REQUEST_ROWS && data.segments.len() <= MAX_TIP_REQUEST_ROWS,
        "active request exceeds bounded session-tip read of {MAX_TIP_REQUEST_ROWS} rows"
    );
    let messages = data
        .messages
        .into_iter()
        .filter(|row| {
            row.message.node_did == node
                && row.message.session_id == session
                && row.message.requester_did == request.requester_did
        })
        .map(|row| row.into_canonical())
        .collect();
    let segments = data
        .segments
        .into_iter()
        .filter(|row| {
            row.segment.node_did == node
                && row.segment.session_id == session
                && row.segment.requester_did == request.requester_did
        })
        .map(|row| row.into_canonical())
        .collect();
    Ok(ClientStore::from_rows(ClientStoreRows {
        requests: data.requests,
        transcript_messages: messages,
        output_segments: segments,
        ..Default::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use gents_store::config_client::ConfigAccess;

    #[tokio::test]
    async fn reopening_completed_request_reads_only_its_prompt_and_active_reads_stay_request_scoped(
    ) {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::client::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        for request in ["completed", "active"] {
            let count = if request == "completed" { 120 } else { 1 };
            let mutations = (0..count)
                .map(|sequence| {
                    let storage_sequence = if request == "active" {
                        1_000 + sequence
                    } else {
                        sequence
                    };
                    let key = if sequence == 0 {
                        format!("authored:{request}:prompt")
                    } else {
                        format!("{request}:{sequence}")
                    };
                    let key = escape_graphql_string(&key);
                    let request = escape_graphql_string(request);
                    format!(
                        r#"m{sequence}: create_AgentMessage(input: {{
                    message_key: "{key}", session_id: "session", node_did: "agent",
                    request_doc_id: "{request}", requester_did: null,
                    publication: {{kind: "request_execution", execution_generation: "generation"}},
                    outcome: "complete", sequence: {storage_sequence}, role: "user", blocks: null,
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
        ConfigAccess::write_local(&node, "test.tip_history", r#"mutation {
            create_AgentOutputSegment(input: {node_did:"agent", session_id:"session",
                request_doc_id:"completed", source:{kind:"not_a_source"}, payload:"historic"}) { _docID }
        }"#).await.unwrap();
        for (id, state) in [
            ("completed", RequestLifecycleState::Completed),
            ("active", RequestLifecycleState::Processing),
        ] {
            let request = AgentRequestRow {
                doc_id: Some(id.into()),
                request_id: id.into(),
                node_did: Some("agent".into()),
                session_id: Some("session".into()),
                lifecycle_state: Some(state),
                ..Default::default()
            };
            let tip = load_session_tip_store_on(&fixture_view_access(&node), &request)
                .await
                .unwrap();
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
            node_did: Some("agent".into()),
            session_id: Some("session".into()),
            ..Default::default()
        };
        let mut requests = vec![scoped("completed"), scoped("interrupted")];
        let ownership = load_request_prompt_ownership_on(&fixture_view_access(&node), &requests)
            .await
            .unwrap();
        let exact = prompt_observations(&fixture_view_access(&node), &requests)
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
        let other_scope =
            load_request_prompt_ownership_on(&fixture_view_access(&node), &requests[..1])
                .await
                .unwrap();
        assert!(!other_scope.by_request_doc_id["completed"].materialized);
        assert_eq!(
            other_scope.by_request_doc_id["completed"].first_sequence,
            None
        );
        let control_anchor = serde_json::json!({"p0":[], "a0":[{
            "request_doc_id":"completed", "node_did":"agent", "session_id":"session",
            "requester_did":"different-requester", "sequence":0,
            "message_key":"background-completion-notification:result:done"
        }]});
        let mut control_facts = RequestPromptOwnership::default();
        decode_prompt_ownership(&control_anchor, &requests[..1], &mut control_facts).unwrap();
        assert_eq!(
            control_facts.by_request_doc_id["completed"].first_sequence,
            None
        );
        let malformed = serde_json::json!({"p0":[], "a0":[{"request_doc_id":"foreign", "node_did":"agent", "session_id":"session", "requester_did":"different-requester", "sequence":0}]});
        assert!(decode_prompt_ownership(
            &malformed,
            &requests[..1],
            &mut RequestPromptOwnership::default()
        )
        .is_err());
        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_queued_or_folded_submission_loads_the_running_turns_open_output() {
        use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
        use gents_protocol::request_lifecycle::RequestLifecycleState;

        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::client::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        ConfigAccess::write_local(
            &node,
            "test.tip_open_stream",
            r#"mutation { create_AgentOutputSegment(input: {
                node_did: "agent", session_id: "session", request_doc_id: "doc-head",
                requester_did: "reader",
                source: {kind: "authored", key: "stream"}, ordinal: 0,
                writer: {kind: "request_execution", execution_generation: "generation-1"},
                runs: [{stream: 0, bytes: 5, declaration: {block_index: 0, part_index: 0, payload: {kind: "text"}}}],
                payload: "hello", created_at: "2026-09-30T00:00:00Z"
            }) { _docID } }"#,
        )
        .await
        .unwrap();
        let row = |id: &str, state, requester: &str| AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some(format!("doc-{id}")),
            request_id: id.into(),
            node_did: Some("agent".into()),
            session_id: Some("session".into()),
            requester_did: Some(requester.into()),
            lifecycle_state: Some(state),
            ..Default::default()
        };
        let queue = |after: &str| {
            Some(RequestInput {
                queue: Some(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::User,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: Some(after.into()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            })
        };
        let queued = AgentRequestRow {
            input: queue("head"),
            ..row("queued", RequestLifecycleState::Pending, "reader")
        };
        let folded = AgentRequestRow {
            input: queue("head"),
            superseded_by_request: Some("head".into()),
            superseded_by_request_doc_id: Some("doc-head".into()),
            failure_reason: Some(gents_session::request::lifecycle::FOLDED_REASON.into()),
            ..row("folded", RequestLifecycleState::Superseded, "reader")
        };
        let foreign = AgentRequestRow {
            input: queue("head"),
            ..row("foreign", RequestLifecycleState::Pending, "other")
        };
        let store = ClientStore::from_rows(ClientStoreRows {
            requests: vec![
                row("head", RequestLifecycleState::Processing, "reader"),
                queued.clone(),
                folded,
                foreign,
            ],
            ..Default::default()
        });

        let untargeted = load_session_tip_store_on(&fixture_view_access(&node), &queued)
            .await
            .unwrap();
        assert!(untargeted.output_segments.is_empty());
        for submitted in ["queued", "folded"] {
            let tip = store
                .session_tip_request("session", Some("agent"), Some("reader"), submitted)
                .expect("submitted row in scope");
            assert_eq!(tip.request_id, "head", "{submitted}");
            let loaded = load_session_tip_store_on(&fixture_view_access(&node), &tip)
                .await
                .unwrap();
            assert_eq!(loaded.output_segments.len(), 1, "{submitted}");
            assert_eq!(loaded.output_segments[0].segment.payload, "hello");
        }
        let foreign_tip = store
            .session_tip_request("session", Some("agent"), Some("other"), "foreign")
            .expect("foreign row in its own scope");
        assert_eq!(foreign_tip.request_id, "foreign");
        node.shutdown().await.unwrap();
    }
}

#[cfg(test)]
fn fixture_view_access(node: &std::sync::Arc<EmbeddedNode>) -> ConfigAccess {
    ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    )
}

#[cfg(test)]
mod live_tests;
