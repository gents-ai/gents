//! Cross-session ancestry behind Lean `startedByJob`. Fuel is the caller's
//! `request_hop`: hop 0 reads nothing, and each level reads at most twice.

use std::collections::BTreeSet;

use gents_protocol::job::JobPin;
use serde_json::Value;

use crate::config_client::{ConfigAccess, ConfigClientError};
use crate::graphql::graphql_string_list_literal;

#[derive(Debug, thiserror::Error)]
pub enum AncestryError {
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error("{collection} ancestry query omitted rows")]
    RowsOmitted { collection: String },
    #[error("decoding job_pin")]
    DecodeJobPin(#[source] serde_json::Error),
    #[error("AgentToolCall ancestry row omitted session_id")]
    SessionMissing,
}

type Result<T, E = AncestryError> = std::result::Result<T, E>;

#[cfg(test)]
tokio::task_local! {
    static ANCESTRY_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

pub async fn started_by_job(
    access: &ConfigAccess,
    session_id: &str,
    request_hop: u32,
) -> Result<bool> {
    let mut frontier = vec![session_id.to_owned()];
    let mut seen = BTreeSet::new();
    let mut fuel = request_hop;
    while fuel > 0 {
        fuel -= 1;
        let sessions: Vec<String> = frontier
            .drain(..)
            .filter(|session| seen.insert(session.clone()))
            .collect();
        if sessions.is_empty() {
            break;
        }
        let calls = cross_session_calls(access, &sessions).await?;
        if calls.is_empty() {
            break;
        }
        let list = graphql_string_list_literal(calls.iter().map(String::as_str));
        let document = format!(
            "{{ AgentToolCall(filter: {{ _docID: {{ _in: {list} }} }}) {{ session_id job_pin }} }}"
        );
        let mut parents = Vec::new();
        let mut started = false;
        for row in read(access, &document, "AgentToolCall").await? {
            if let Some(pin) = row.get("job_pin").filter(|value| !value.is_null()) {
                serde_json::from_value::<JobPin>(pin.clone())
                    .map_err(AncestryError::DecodeJobPin)?;
                started = true;
            } else {
                let session = row
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or(AncestryError::SessionMissing)?;
                parents.push(session.to_owned());
            }
        }
        if started {
            return Ok(true);
        }
        parents.sort();
        parents.dedup();
        frontier = parents;
    }
    Ok(false)
}

async fn read(access: &ConfigAccess, document: &str, collection: &str) -> Result<Vec<Value>> {
    #[cfg(test)]
    let _ = ANCESTRY_QUERIES.try_with(|queries| {
        queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    let response = access.execute(document).await?;
    response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| AncestryError::RowsOmitted {
            collection: collection.to_owned(),
        })
}

async fn cross_session_calls(access: &ConfigAccess, sessions: &[String]) -> Result<Vec<String>> {
    let list = graphql_string_list_literal(sessions.iter().map(String::as_str));
    let document = format!(
        "{{ AgentRequest(filter: {{ session_id: {{ _in: {list} }}, caused_by_parent_request_doc_id: {{ _ne: null }}, caused_by_parent_tool_call_doc_id: {{ _ne: null }} }}) {{ caused_by_parent_tool_call_doc_id }} }}"
    );
    let mut calls = Vec::new();
    for row in read(access, &document, "AgentRequest").await? {
        if let Some(call) = row
            .get("caused_by_parent_tool_call_doc_id")
            .and_then(Value::as_str)
        {
            calls.push(call.to_owned());
        }
    }
    calls.sort();
    calls.dedup();
    Ok(calls)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use serde_json::{json, Value};

    use super::{started_by_job, ANCESTRY_QUERIES};
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::write_fixture_job_pin;

    async fn open() -> Arc<EmbeddedNode> {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        node
    }

    async fn insert(node: &std::sync::Arc<EmbeddedNode>, collection: &str, value: Value) -> String {
        ConfigAccess::transact_local_write(node, None, "test.plugin_job_ancestry", |txn| {
            let value = &value;
            Box::pin(async move {
                let query = format!(
                    "mutation($input: {collection}MutationInputArg!) {{create_{collection}(input:$input){{_docID}}}}"
                );
                let response = txn
                    .execute_with_variables(&query, &json!({ "input": value }))
                    .await?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(gents_protocol::graphql::extract_mutation_doc_id(&response, collection)?)
            })
        })
        .await
        .unwrap()
    }

    async fn request(
        node: &std::sync::Arc<EmbeddedNode>,
        request_id: &str,
        session_id: &str,
        parent_request: Option<&str>,
        parent_call: Option<&str>,
    ) -> String {
        let mut value = json!({
            "request_id": request_id,
            "purpose": "normal",
            "node_did": "owner",
            "requester_did": null,
            "session_id": session_id,
            "agent_id": "configured",
            "lifecycle_state": "processing",
        });
        if let Some(parent) = parent_request {
            value["caused_by_parent_request_id"] = json!("parent");
            value["caused_by_parent_request_doc_id"] = json!(parent);
        }
        if let Some(call) = parent_call {
            value["caused_by_parent_tool_call_id"] = json!("call");
            value["caused_by_parent_tool_call_doc_id"] = json!(call);
        }
        insert(node, "AgentRequest", value).await
    }

    async fn tool(
        node: &Arc<EmbeddedNode>,
        tool_call_id: &str,
        session_id: &str,
        request_doc_id: &str,
        job: bool,
    ) -> String {
        let doc = insert(
            node,
            "AgentToolCall",
            json!({
                "request_id": session_id,
                "request_doc_id": request_doc_id,
                "node_did": "owner",
                "requester_did": null,
                "session_id": session_id,
                "tool_call_id": tool_call_id,
                "tool_name": "agent_new",
                "lifecycle_state": "running",
                "await_mode": "background",
            }),
        )
        .await;
        if job {
            write_fixture_job_pin(node, &doc).await.unwrap();
        }
        doc
    }

    async fn counted(
        access: &ConfigAccess,
        session_id: &str,
        hop: u32,
    ) -> (Result<bool, super::AncestryError>, usize) {
        let queries = Arc::new(AtomicUsize::new(0));
        let output = ANCESTRY_QUERIES
            .scope(queries.clone(), started_by_job(access, session_id, hop))
            .await;
        (output, queries.load(Ordering::Relaxed))
    }

    struct Chain {
        node: Arc<EmbeddedNode>,
        child: String,
        grand: String,
        plain_child: String,
        plain_root: String,
    }

    async fn chain() -> Chain {
        let node = open().await;
        let root = request(&node, "root", "root", None, None).await;
        let job = tool(&node, "job", "root", &root, true).await;
        let child = "child\"session".to_owned();
        request(&node, "child", &child, Some(&root), Some(&job)).await;
        request(&node, "child-cont", &child, Some(&root), None).await;
        let mid = request(&node, "mid", "mid", Some(&root), Some(&job)).await;
        let bridge = tool(&node, "bridge", "mid", &mid, false).await;
        let grand = "grand\"session".to_owned();
        let grand_doc = request(&node, "grand", &grand, Some(&mid), Some(&bridge)).await;
        for index in 0..6 {
            request(&node, &format!("c{index}"), &grand, Some(&grand_doc), None).await;
        }
        let plain_root = "plain\"session".to_owned();
        let plain = request(&node, "plain", &plain_root, None, None).await;
        let call = tool(&node, "plain-call", &plain_root, &plain, false).await;
        let plain_child = "plain-child".to_owned();
        request(
            &node,
            "plain-child",
            &plain_child,
            Some(&plain),
            Some(&call),
        )
        .await;
        Chain {
            node,
            child,
            grand,
            plain_child,
            plain_root,
        }
    }

    #[tokio::test]
    async fn direct_job_child_session_is_started_by_job() {
        let chain = chain().await;
        let access = ConfigAccess::local(chain.node.clone());
        assert!(started_by_job(&access, &chain.child, 1).await.unwrap());
        chain.node.shutdown().await;
    }

    #[tokio::test]
    async fn agent_new_grandchild_of_a_job_is_started_by_job() {
        let chain = chain().await;
        let access = ConfigAccess::local(chain.node.clone());
        assert!(started_by_job(&access, &chain.grand, 2).await.unwrap());
        chain.node.shutdown().await;
    }

    #[tokio::test]
    async fn unrelated_session_chain_is_not_started_by_job() {
        let chain = chain().await;
        let access = ConfigAccess::local(chain.node.clone());
        assert!(!started_by_job(&access, &chain.plain_child, 2)
            .await
            .unwrap());
        assert!(!started_by_job(&access, &chain.plain_root, 1).await.unwrap());
        chain.node.shutdown().await;
    }

    #[tokio::test]
    async fn ancestry_walk_issues_at_most_two_queries_per_hop_level() {
        let chain = chain().await;
        let access = ConfigAccess::local(chain.node.clone());
        let (hit, hit_queries) = counted(&access, &chain.grand, 2).await;
        assert!(hit.unwrap());
        assert!(hit_queries <= 2 * (2 + 1), "{hit_queries}");
        let (shallow, shallow_queries) = counted(&access, &chain.grand, 1).await;
        assert!(!shallow.unwrap());
        assert!(shallow_queries <= 2 * (1 + 1), "{shallow_queries}");
        let (miss, miss_queries) = counted(&access, &chain.plain_child, 3).await;
        assert!(!miss.unwrap());
        assert!(miss_queries <= 2 * (3 + 1), "{miss_queries}");
        chain.node.shutdown().await;
    }
}
