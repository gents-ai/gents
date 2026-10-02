//! A running trial's [`LiveSnapshot`]: read out of its own home while the
//! stages run, and once more when they end.
//!
//! The home's store is locked by this process while the trial runs, so the
//! executor is the only reader that can see inside it; it reports what it
//! reads through the trial's [`crate::eval::runner::StageProgress`].

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use anyhow::Result;
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::eval::runner::embedded::executor::{documents_capture_query, usage_of};
use crate::eval::runner::embedded::observe::request_evidence_from_query_data;
use crate::eval::runner::executor::{Capture, StageSpec};
use crate::eval::runner::progress::{LastToolCall, LiveSnapshot, ToolTally};
use crate::graphql::graphql_with_transaction_retry;
use crate::Collection;

/// The configuration collections every snapshot counts.
const CONFIGURATION: [Collection; 11] = [
    Collection::AgentBehavior,
    Collection::AgentContext,
    Collection::Tools,
    Collection::InferenceProfile,
    Collection::InferenceExecution,
    Collection::SubagentTarget,
    Collection::DatastoreToolSurface,
    Collection::EventSource,
    Collection::Trigger,
    Collection::Task,
    Collection::Schedule,
];

/// What one trial's snapshots read: its home, the collections registered
/// before the subject ran, and the documents captures of every stage.
pub(super) struct LiveObserver<'a> {
    node: &'a std::sync::Arc<EmbeddedNode>,
    trial_did: &'a str,
    installed: BTreeSet<String>,
    started: Instant,
    captures: Vec<(&'a str, &'a str, &'a Value)>,
}

impl<'a> LiveObserver<'a> {
    /// Taken once the pack and fixtures are installed and the runtime is up,
    /// so a later collection is one the subject registered.
    pub(super) async fn new(
        node: &'a std::sync::Arc<EmbeddedNode>,
        trial_did: &'a str,
        stages: &'a [StageSpec],
    ) -> Self {
        let installed = collection_names(node).await.unwrap_or_default();
        let mut captures: Vec<(&str, &str, &Value)> = Vec::new();
        for capture in stages.iter().flat_map(|stage| &stage.captures) {
            if let Capture::Documents {
                name,
                collection,
                filter,
                ..
            } = capture
            {
                // The last stage to declare a name wins, as it does for the
                // goal (`crate::eval::runner::goal::case_goal`).
                captures.retain(|(named, ..)| named != name);
                captures.push((name, collection, filter));
            }
        }
        Self {
            node,
            trial_did,
            installed,
            started: Instant::now(),
            captures,
        }
    }

    /// Read the home now. A collection or capture that cannot be read is
    /// left out rather than counted as empty.
    pub(super) async fn snapshot(&self) -> Result<LiveSnapshot> {
        let registered = collection_names(self.node).await?;
        let schemas: Vec<String> = registered.difference(&self.installed).cloned().collect();
        let mut snapshot = activity(self.node).await?;
        snapshot.observed_at =
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        snapshot.elapsed_secs = self.started.elapsed().as_secs();

        let counted: BTreeSet<&str> = CONFIGURATION
            .iter()
            .map(|collection| collection.graphql_type())
            .chain(self.captures.iter().map(|(_, collection, _)| *collection))
            .chain(schemas.iter().map(String::as_str))
            .filter(|collection| registered.contains(*collection))
            .collect();
        for collection in counted {
            let query = format!("{{ {collection} {{ _docID }} }}");
            if let Some(rows) = count(self.node, &query, collection).await {
                snapshot.documents.insert(collection.to_owned(), rows);
            }
        }
        for (name, collection, filter) in &self.captures {
            if !registered.contains(*collection) {
                continue;
            }
            let Ok(query) = documents_capture_query(collection, filter, &[], self.trial_did) else {
                continue;
            };
            if let Some(rows) = count(self.node, &query, collection).await {
                snapshot.captures.insert((*name).to_owned(), rows);
            }
        }
        snapshot.schemas = schemas;
        Ok(snapshot)
    }
}

async fn collection_names(node: &std::sync::Arc<EmbeddedNode>) -> Result<BTreeSet<String>> {
    ConfigAccess::Local(node.clone()).collection_names().await
}

/// Requests, model turns, tokens and tool calls across the whole home: every
/// row in a trial home is the trial's.
async fn activity(node: &std::sync::Arc<EmbeddedNode>) -> Result<LiveSnapshot> {
    let requests = crate::session::public_request_filter("");
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ {requests} }}) {{ _docID agent_did session_id requester_did }}
            InferenceCall {{ call_id request_doc_id agent_did call_kind queued_at call_seq call_state failure_reason prompt_tokens completion_tokens context_accounting_json }}
            AgentToolCall {{ tool_name lifecycle_state }}
            CompactionEntry {{ agent_did session_id requester_did }}
            ProviderContextReduction {{ agent_did session_id requester_did }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "eval trial live snapshot").await?;
    let data = response.data.unwrap_or(Value::Null);
    let evidence = request_evidence_from_query_data(&data);
    let last_tool = last_tool_call(node).await;
    let usage = usage_of(&evidence.inference_calls);
    let mut tools: BTreeMap<String, ToolTally> = BTreeMap::new();
    let mut failed_tool_calls = 0;
    for call in &evidence.tool_calls {
        let failed = matches!(call.lifecycle_state.as_deref(), Some("failed" | "timedOut"));
        let tally = tools.entry(call.tool_name.clone()).or_default();
        tally.calls += 1;
        if failed {
            tally.failed += 1;
            failed_tool_calls += 1;
        }
    }
    Ok(LiveSnapshot {
        requests: data
            .get("AgentRequest")
            .and_then(Value::as_array)
            .map_or(0, |rows| rows.len() as u64),
        model_turns: evidence.inference_calls.len() as u64,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        session_contexts: super::live_context::session_contexts(&data),
        reported_input_tokens: Some(
            evidence
                .inference_calls
                .iter()
                .filter_map(|call| call.prompt_tokens)
                .fold(0, u64::saturating_add),
        ),
        reported_output_tokens: Some(
            evidence
                .inference_calls
                .iter()
                .filter_map(|call| call.completion_tokens)
                .fold(0, u64::saturating_add),
        ),
        tool_calls: evidence.tool_calls.len() as u64,
        failed_tool_calls,
        tools,
        last_tool,
        ..LiveSnapshot::default()
    })
}

/// The tool call that started last, with the start of its result on one
/// line; `None` when there is none or it cannot be read.
async fn last_tool_call(node: &std::sync::Arc<EmbeddedNode>) -> Option<LastToolCall> {
    let query = r#"{ AgentToolCall(order: { started_at: DESC }, limit: 1) { _docID agent_did session_id requester_did tool_name lifecycle_state } }"#;
    let response = graphql_with_transaction_retry(node, query, "eval trial last tool call")
        .await
        .map_err(|error| {
            tracing::debug!(error = %format!("{error:#}"), "eval trial last tool call was not read");
        })
        .ok()?;
    let row = response
        .data
        .as_ref()?
        .get("AgentToolCall")?
        .as_array()?
        .first()?
        .clone();
    let text = |key: &str| row.get(key).and_then(Value::as_str).map(str::to_owned);
    let result = crate::tool_call_lifecycle::query::load_tool_call_presentation(
        &ConfigAccess::Local(node.clone()),
        row.get("_docID")?.as_str()?,
        row.get("agent_did")?.as_str()?,
        row.get("session_id")?.as_str()?,
        row.get("requester_did").and_then(Value::as_str),
    )
    .await
    .map_err(|error| {
        tracing::debug!(error = %format!("{error:#}"), "eval last tool presentation was not read");
    })
    .ok()
    .and_then(|read| read.result);
    Some(LastToolCall {
        tool_name: text("tool_name")?,
        state: text("lifecycle_state"),
        result: result.map(|result| one_line(&result, LastToolCall::RESULT_CHARS)),
    })
}

/// `text` with its whitespace collapsed, cut to `max` chars.
fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &collapsed[..at]),
        None => collapsed,
    }
}

async fn count(node: &EmbeddedNode, query: &str, collection: &str) -> Option<u64> {
    match graphql_with_transaction_retry(node, query, "eval trial live count").await {
        Ok(response) => Some(
            response
                .data
                .as_ref()
                .and_then(|data| data.get(collection))
                .and_then(Value::as_array)
                .map_or(0, |rows| rows.len() as u64),
        ),
        Err(error) => {
            tracing::debug!(
                error = %format!("{error:#}"),
                collection,
                "eval trial live count was not read"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_usage_keeps_reported_tokens_visible_without_claiming_an_exact_total() {
        let home = super::super::home::EmbeddedHome::create_temp("live-partial-usage")
            .await
            .unwrap();
        for mutation in [
            r#"mutation { create_InferenceCall(input: { call_id: "done", call_seq: 0, call_state: "completed", prompt_tokens: 112, completion_tokens: 20 }) { _docID } }"#,
            r#"mutation { create_InferenceCall(input: { call_id: "pending", call_seq: 1, call_state: "running" }) { _docID } }"#,
        ] {
            ConfigAccess::write_local(&home.node, "eval.test.live_usage", mutation)
                .await
                .unwrap();
        }
        let live = activity(&home.node).await.unwrap();
        assert_eq!(live.input_tokens, None);
        assert_eq!(live.output_tokens, None);
        assert_eq!(live.reported_input_tokens, Some(112));
        assert_eq!(live.reported_output_tokens, Some(20));
        ConfigAccess::write_local(&home.node, "eval.test.live_usage_complete", r#"mutation { update_InferenceCall(filter: {call_id: {_eq: "pending"}}, input: {call_state: "completed", prompt_tokens: 8, completion_tokens: 3}) { _docID } }"#).await.unwrap();
        let live = activity(&home.node).await.unwrap();
        assert_eq!(live.input_tokens, Some(120));
        assert_eq!(live.output_tokens, Some(23));
        assert_eq!(live.reported_input_tokens, live.input_tokens);
        assert_eq!(live.reported_output_tokens, live.output_tokens);
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn last_tool_reads_canonical_delivery_and_keeps_running_calls_visible() {
        let (node, path, mut tool) =
            crate::tool_call_lifecycle::admission_fixture::published_spawn_parent("eval-last-tool")
                .await;
        let running = last_tool_call(&node).await.expect("running tool");
        assert_eq!(running.state.as_deref(), Some("running"));
        assert_eq!(running.result, None);
        tool.complete("saved\n  the record").await.unwrap();
        let finished = last_tool_call(&node).await.expect("completed tool");
        assert_eq!(finished.state.as_deref(), Some("completed"));
        assert_eq!(finished.result.as_deref(), Some("saved the record"));
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }
}
