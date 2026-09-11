//! Current context is a persisted inference observation, not token spend.
use anyhow::Result;
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;

pub(super) type ContextOrder = (chrono::DateTime<chrono::Utc>, i64, String);

pub(super) struct ContextSample {
    pub order: ContextOrder,
    pub used: u64,
}

/// The turn/descendant owner supplies its authorized physical request.
pub(super) async fn load(
    node: &EmbeddedNode,
    request: &AgentRequestRow,
) -> Result<Option<ContextSample>> {
    let Some(observation) =
        gents::toolset::load_pinned_request_context_observation(node, request).await?
    else {
        return Ok(None);
    };
    let context = observation.context;
    // Without a real dispatch timestamp we cannot order this observation
    // against another request's active context. Do not fabricate one.
    let Some(queued) = context
        .queued_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|time| time.with_timezone(&chrono::Utc))
    else {
        return Ok(None);
    };
    Ok(Some(ContextSample {
        order: (queued, context.call_sequence, context.call_id),
        used: (context.accounting.estimated_input_tokens as u64)
            .saturating_add(observation.completion_tokens.unwrap_or(0)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::graphql::{ensure_no_errors, escape_graphql_string};
    use serde_json::json;

    #[tokio::test]
    async fn context_uses_physical_request_and_rejects_foreign_scope() {
        let dir = tempfile::tempdir().unwrap();
        let node = EmbeddedNode::builder()
            .data_path(dir.path().join("node"))
            .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
            .build()
            .await
            .unwrap();
        gents::schema::ensure_runtime_schemas(&node).await.unwrap();
        let mut requests = Vec::new();
        for content in ["first", "second"] {
            let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "same-label", agent_did: "owner", requester_did: "requester", session_id: "session", behavior_id: "behavior", content: "{content}", lifecycle_state: "pending"}}) {{_docID}} }}"#)).await;
            ensure_no_errors(&response, "seed physical context request").unwrap();
            let doc = gents_protocol::graphql::extract_mutation_doc_id(
                &json!({"data":response.data}),
                "AgentRequest",
            )
            .unwrap();
            requests.push(serde_json::from_value::<AgentRequestRow>(json!({"_docID":doc,"request_id":"same-label","agent_did":"owner","requester_did":"requester","session_id":"session"})).unwrap());
        }
        assert_ne!(requests[0].doc_id, requests[1].doc_id);
        let accounting = json!({
            "accounting_version":1,"turn_index":0,"attempt":0,"estimator":"fixture",
            "components":{"messages":900,"documents":0,"tool_schemas":50,"additional_parameters":0,"output_schema":0},
            "estimated_input_tokens":950,"context_window":10000,"compaction_threshold_basis_points":8000,
            "compaction_threshold_tokens":8000,"compaction_reason":"below_threshold"
        });
        let doc = escape_graphql_string(requests[0].doc_id.as_deref().unwrap());
        let encoded = escape_graphql_string(&accounting.to_string());
        let response = node.execute(&format!(r#"mutation {{create_InferenceCall(input: {{call_id: "context-call", agent_did: "owner", request_id: "same-label", request_doc_id: "{doc}", call_kind: "inference", call_seq: 1, queued_at: "2026-09-04T12:00:00Z", completion_tokens: 25, context_accounting_json: "{encoded}"}}) {{_docID}} }}"#)).await;
        ensure_no_errors(&response, "seed physical inference context").unwrap();
        assert_eq!(load(&node, &requests[0]).await.unwrap().unwrap().used, 975);
        assert!(load(&node, &requests[1]).await.unwrap().is_none());
        assert!(gents::toolset::load_request_context_observation(
            &node,
            "owner",
            Some("requester"),
            "session",
            "same-label"
        )
        .await
        .is_err());
        for field in ["owner", "session", "requester"] {
            let mut foreign = requests[0].clone();
            match field {
                "owner" => foreign.agent_did = Some("foreign".into()),
                "session" => foreign.session_id = Some("foreign".into()),
                _ => foreign.requester_did = None,
            }
            assert!(load(&node, &foreign).await.unwrap().is_none(), "{field}");
        }
        node.shutdown().await;
    }
}
