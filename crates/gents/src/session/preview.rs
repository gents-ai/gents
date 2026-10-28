use anyhow::Result;
use gents_protocol::session::{SessionObservation, SessionRequestObservation};

pub(super) fn observe_reply_preview(
    observation: &SessionObservation,
    current: &SessionRequestObservation,
    reply: Option<&str>,
) -> Option<String> {
    if observation.latest_request.as_ref().is_some_and(|latest| {
        latest.request_doc_id == current.request_doc_id && latest.request_id == current.request_id
    }) && current.lifecycle_state.is_terminal()
    {
        reply.map(|text| text.chars().take(240).collect())
    } else {
        observation.reply_preview.clone()
    }
}

pub(crate) fn visible_reply_preview(message: &gents_protocol::message::Message) -> Option<String> {
    use gents_protocol::message::{AssistantContent, Message};
    let Message::Assistant { content, .. } = message else {
        return None;
    };
    content
        .iter()
        .filter_map(|part| match part {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .flat_map(str::lines)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            let mut space = false;
            line.chars()
                .filter_map(|character| {
                    if character.is_whitespace() {
                        let repeated = std::mem::replace(&mut space, true);
                        (!repeated).then_some(' ')
                    } else {
                        space = false;
                        Some(character)
                    }
                })
                .take(240)
                .collect()
        })
}

pub(crate) async fn request_preview_in_txn<'a>(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &'a crate::AgentRequest,
) -> Result<&'a str> {
    let Some(parent_id) = request.caused_by_parent_request_doc_id.as_deref() else {
        return Ok(&request.content);
    };
    let row = gents_protocol::row::AgentRequestRow {
        request_id: request.request_id.clone(),
        requester_did: request.requester_did.clone(),
        created_at: Some(request.created_at.clone()),
        input: Some(gents_protocol::request_input::RequestInput {
            queue: request.input.queue.clone(),
            goal_continuation: request.input.goal_continuation.clone(),
            ..Default::default()
        }),
        caused_by_parent_request_id: request.caused_by_parent_request_id.clone(),
        caused_by_parent_request_doc_id: request.caused_by_parent_request_doc_id.clone(),
        caused_by_parent_tool_call_doc_id: request.caused_by_parent_tool_call_doc_id.clone(),
        caused_by_trigger_id: request.caused_by_trigger_id.clone(),
        caused_by_trigger_kind: request.caused_by_trigger_kind.clone(),
        ..Default::default()
    };
    if !matches!(
        crate::lifecycle::request_origin(&row),
        crate::lifecycle::RequestOrigin::SessionMessage { .. }
    ) {
        return Ok(&request.content);
    }
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{
        _docID request_id node_did agent_id session_id
    }} }}"#,
        crate::graphql::escape_graphql_string(parent_id)
    );
    let response = txn.execute_local_response(&query).await?;
    let parents =
        crate::graphql::rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")?;
    let [parent] = parents.as_slice() else {
        return Ok(&request.content);
    };
    Ok(
        crate::lifecycle::session_message_display_body_with_content(&row, parent, &request.content)
            .unwrap_or(&request.content),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_reply_projection_follows_the_model() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().session_reply_preview_cases;
        assert_eq!(cases.len(), 96);
        for case in cases {
            let observation = SessionObservation {
                last_activity_at: "2030-01-01T00:00:00Z".into(),
                preview: Some("Question".into()),
                reply_preview: Some("Previous reply".into()),
                latest_request: Some(SessionRequestObservation {
                    request_doc_id: "1".into(),
                    request_id: "2".into(),
                    lifecycle_state:
                        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                }),
            };
            let current = SessionRequestObservation {
                request_doc_id: case["doc"].as_u64().unwrap().to_string(),
                request_id: case["request"].as_u64().unwrap().to_string(),
                lifecycle_state: serde_json::from_value(case["state"].clone()).unwrap(),
            };
            assert_eq!(
                serde_json::to_value(observe_reply_preview(
                    &observation,
                    &current,
                    case["reply"].as_str()
                ))
                .unwrap(),
                case["preview"],
                "{case}"
            );
        }
    }

    #[test]
    fn reply_preview_is_one_bounded_visible_assistant_line() {
        use gents_protocol::message::Message;
        assert_eq!(visible_reply_preview(&Message::user("not a reply")), None);
        assert_eq!(
            visible_reply_preview(&Message::assistant("\n  First   line.\nSecond line.")),
            Some("First line.".into())
        );
        assert_eq!(visible_reply_preview(&Message::assistant(" \n\t")), None);
        let preview = visible_reply_preview(&Message::assistant("界".repeat(300))).unwrap();
        assert_eq!(preview.chars().count(), 240);
        let reasoning = Message::Assistant {
            id: None,
            content: vec![
                gents_protocol::message::AssistantContent::Reasoning(
                    gents_protocol::message::Reasoning::new("Private analysis"),
                ),
                gents_protocol::message::AssistantContent::text("Visible result"),
            ],
        };
        assert_eq!(
            visible_reply_preview(&reasoning),
            Some("Visible result".into())
        );
    }

    #[tokio::test]
    async fn received_preview_uses_only_the_exact_readable_parent_envelope() {
        use crate::ConfigAccess;
        let temp = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(temp.path())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let response = ConfigAccess::write_local(&node, "test.session.preview.parent", r#"mutation {
            create_AgentRequest(input: { request_id: "parent", purpose: "normal", node_did: "did:test:sender",
                agent_id: "did:test:sender:engineer", session_id: "source", content: "Parent request",
                created_at: "2030-01-01T00:00:00Z", lifecycle_state: "completed" }) { _docID }
        }"#).await.unwrap();
        let parent = crate::graphql::mutation_documents(&response, "create_AgentRequest")
            .unwrap()
            .unwrap()[0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let content = "[message agent:engineer session:source node:did:test:sender · at:2030-01-01T00:00:01Z · cause:parent]\n\nInspect the layout.\nKeep the draft.";
        let request = crate::AgentRequest::try_from(gents_protocol::row::AgentRequestRow {
            doc_id: Some("child".into()),
            request_id: "received".into(),
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            node_did: Some("did:test:receiver".into()),
            agent_id: Some("engineer".into()),
            session_id: Some("target".into()),
            requester_did: Some("did:test:sender".into()),
            content: Some(content.into()),
            created_at: Some("2030-01-01T00:00:01Z".into()),
            caused_by_parent_request_doc_id: Some(parent),
            caused_by_parent_request_id: Some("parent".into()),
            caused_by_parent_tool_call_doc_id: Some("tool-doc".into()),
            caused_by_parent_tool_call_id: Some("tool".into()),
            request_hop: Some(1),
            ..Default::default()
        })
        .unwrap();
        let observed =
            ConfigAccess::transact_local_readonly(&node, None, "test.session.preview", |txn| {
                let request = request.clone();
                Box::pin(async move {
                    assert_eq!(
                        request_preview_in_txn(txn, &request).await?,
                        "Inspect the layout.\nKeep the draft."
                    );
                    assert_eq!(request.content, content);
                    let mut spoof = request.clone();
                    spoof.requester_did = Some("did:test:other".into());
                    assert_eq!(request_preview_in_txn(txn, &spoof).await?, content);
                    let mut authored = request.clone();
                    authored.caused_by_parent_request_doc_id = None;
                    authored.caused_by_parent_request_id = None;
                    authored.caused_by_parent_tool_call_doc_id = None;
                    authored.caused_by_parent_tool_call_id = None;
                    authored.request_hop = 0;
                    assert_eq!(request_preview_in_txn(txn, &authored).await?, content);
                    let mut missing = request.clone();
                    missing.caused_by_parent_request_doc_id = Some("missing-parent".into());
                    assert_eq!(request_preview_in_txn(txn, &missing).await?, content);
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        observed.unwrap();
        node.shutdown().await;
    }
}
