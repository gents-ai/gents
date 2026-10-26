//! Stock usage projection from the runtime's persisted accounting owner.
use std::collections::BTreeSet;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::toolset::{load_session_inference_observation, SessionTokenUsage};
use serde_json::{json, Value};

use crate::caused_sessions::{load_caused_sessions, SessionScope};

fn context_info(
    observation: &gents::toolset::SessionInferenceObservation,
    configured_window: u64,
) -> Result<Value> {
    let Some(context) = observation.latest_context.as_ref() else {
        anyhow::ensure!(
            observation.token_usage.model_calls == 0,
            "context accounting is unavailable for this session's persisted inference calls"
        );
        return Ok(
            json!({"used":0, "total":configured_window, "freeTokens":configured_window,
            "usagePct":0, "usageCategories":[]}),
        );
    };
    let accounting = &context.accounting;
    let used = (accounting.estimated_input_tokens as u64)
        .saturating_add(observation.latest_completion_tokens.unwrap_or(0));
    let total = (accounting.context_window as u64).max(1);
    let components = &accounting.components;
    let mut categories = Vec::new();
    for (label, tokens) in [
        ("Messages (including system)", components.messages as u64),
        ("Tool schemas", components.tool_schemas as u64),
        ("Documents", components.documents as u64),
        (
            "Additional parameters",
            components.additional_parameters as u64,
        ),
        ("Output schema", components.output_schema as u64),
        (
            "Latest generated output",
            observation.latest_completion_tokens.unwrap_or(0),
        ),
    ] {
        if tokens > 0 {
            categories.push(json!({"label":label, "tokens":tokens, "detail":accounting.estimator}));
        }
    }
    Ok(
        json!({"used":used, "total":total, "freeTokens":total.saturating_sub(used),
        "usagePct":((used as u128 * 100) / total as u128).min(100) as u64,
        "toolDefinitionsTokens":components.tool_schemas, "messageTokens":components.messages,
        "autoCompactThresholdPercent":(accounting.compaction_threshold_basis_points / 100).min(100),
        "usageCategories":categories}),
    )
}

pub(super) async fn session_info(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    session: &str,
    model: &str,
    model_name: &str,
    context_window: u64,
) -> Result<Value> {
    let requests = super::sessions::load(node, node_did, agent_id, session).await?;
    let observation =
        load_session_inference_observation(node, node_did, Some(node_did), session).await?;
    let mut context = context_info(&observation, context_window)?;
    let details = match observation.latest_context.as_ref() {
        Some(snapshot) => {
            gents::toolset::load_session_context_details(
                &gents::config_client::ConfigAccess::local(node.clone()),
                node_did,
                Some(node_did),
                session,
                snapshot,
            )
            .await?
        }
        None => None,
    };
    if let Some(details) = details.as_ref() {
        context["systemPromptTokens"] = json!(details.system_prompt_tokens);
        context["messageTokens"] = json!(details.message_tokens);
        context["messageCount"] = json!(details.message_count);
        context["toolDefinitionsCount"] = json!(details.tool_definitions_count);
        context["toolCallCount"] = json!(details.tool_call_count);
    }
    let turns = observation
        .inference_turns
        .context("provider turn coordinates are unavailable for this session")?;
    context["turnCount"] = json!(turns);
    let mut compactions = BTreeSet::new();
    for page in requests.chunks(128) {
        let ids = page
            .iter()
            .map(|request| {
                request
                    .doc_id
                    .as_deref()
                    .context("missing context request identity")
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|id| format!("\"{}\"", escape_graphql_string(id)))
            .collect::<Vec<_>>()
            .join(",");
        let response = graphql_with_transaction_retry(
            node,
            &format!(
                r#"{{
            CompactionEntry(filter: {{request_doc_id: {{_in: [{ids}]}}}}) {{_docID}}
            ProviderContextReduction(filter: {{request_doc_id: {{_in: [{ids}]}}}}) {{_docID}}
        }}"#
            ),
            "Grok context compactions",
        )
        .await?;
        for collection in ["CompactionEntry", "ProviderContextReduction"] {
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get(collection))
                .and_then(Value::as_array)
                .context("missing context compaction rows")?;
            for row in rows {
                compactions.insert((
                    collection,
                    row["_docID"]
                        .as_str()
                        .context("missing compaction identity")?
                        .to_owned(),
                ));
            }
        }
    }
    context["compactionCount"] = json!(compactions.len());
    Ok(json!({"result":{
        "sessionId":session, "cwd":"", "agentName":agent_id, "model":model,
        "modelDisplayName":model_name, "apiBackend":"gents", "turns":turns,
        "turnIndex":turns.saturating_sub(1), "context":context,
        "_meta":{"gents/partialContext":details.is_none(),
            "gents/unavailableContextFields":if details.is_some() {json!([])} else {json!(["systemPromptTokens", "toolDefinitionsCount", "messageCount", "toolCallCount"])},
            "gents/contextAccounting":observation.latest_context}
    }}))
}

#[derive(Default)]
struct Totals {
    input: u64,
    output: u64,
    cached: u64,
    calls: u64,
    duration_ms: u64,
    duration_incomplete: bool,
    incomplete: bool,
}

impl Totals {
    fn add(&mut self, usage: &SessionTokenUsage) {
        self.input = self.input.saturating_add(usage.input_tokens.unwrap_or(0));
        self.output = self.output.saturating_add(usage.output_tokens.unwrap_or(0));
        self.cached = self
            .cached
            .saturating_add(usage.cached_input_tokens.unwrap_or(0));
        self.calls = self.calls.saturating_add(usage.model_calls.max(0) as u64);
        self.duration_ms = self
            .duration_ms
            .saturating_add(usage.api_duration_ms.unwrap_or(0));
        self.duration_incomplete |= usage.model_calls > 0 && usage.api_duration_ms.is_none();
        self.incomplete |= usage.incomplete;
    }

    fn wire(&self, turns: Option<u64>) -> Value {
        json!({"usage":{
            "inputTokens":self.input, "outputTokens":self.output,
            "totalTokens":self.input.saturating_add(self.output), "cachedReadTokens":self.cached,
            "modelCalls":self.calls, "numTurns":turns.unwrap_or(0),
            "apiDurationMs":self.duration_ms,
            "usageIsIncomplete":self.incomplete || self.duration_incomplete || turns.is_none(),
            "costIsPartial":true
        }, "_meta":{"gents/usageScope":"persisted-session-and-caused-sessions",
            "gents/durationIsIncomplete":self.duration_incomplete,
            "gents/unavailableUsageFields":["costUsdTicks", "reasoningTokens", "cacheCreationTokens", "modelUsage"]}})
    }
}

pub(super) async fn session_usage(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    session: &str,
) -> Result<Value> {
    let roots = super::sessions::load(node, node_did, agent_id, session).await?;
    let root = load_session_inference_observation(node, node_did, Some(node_did), session).await?;
    let mut totals = Totals::default();
    totals.add(&root.token_usage);
    totals.incomplete |= roots.iter().any(|row| !row.is_terminal());
    let root_scope = SessionScope {
        node_did: node_did.to_owned(),
        session_id: session.to_owned(),
        requester_did: Some(node_did.to_owned()),
    };
    for caused in load_caused_sessions(node, &[root_scope]).await? {
        totals.incomplete |= !caused.latest.is_terminal();
        let observation = load_session_inference_observation(
            node,
            &caused.scope.node_did,
            caused.scope.requester_did.as_deref(),
            &caused.scope.session_id,
        )
        .await?;
        totals.add(&observation.token_usage);
    }
    Ok(totals.wire(root.inference_turns))
}
