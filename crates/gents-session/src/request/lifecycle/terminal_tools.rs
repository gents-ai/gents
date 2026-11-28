use anyhow::{Context, Result};
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::rows::TranscriptMessageRow;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSource, StreamPayload,
};
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow};
use gents_protocol::tool_call::ToolCallState;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::{escape_graphql_string, response_has_documents};
use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ToolAccountingRejection {
    #[error("foreground tool still running at normal request completion")]
    ForegroundRunning,
}

#[derive(Deserialize)]
struct ToolDocument {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_doc_id: String,
    #[serde(rename = "node_did")]
    node_did: String,
    #[serde(flatten)]
    row: AgentToolCallRow,
}

#[derive(Clone, Copy)]
struct AcceptedCallBinding<'a> {
    header: &'a TranscriptMessageRow,
    id: &'a str,
    call_id: &'a Option<String>,
    name: &'a str,
}

struct AccountingIndex<'a> {
    tools_by_id: RapidHashMap<&'a str, &'a ToolDocument>,
    tool_counts: RapidHashMap<&'a str, usize>,
    accepted_calls: RapidHashMap<&'a str, Vec<AcceptedCallBinding<'a>>>,
    deliveries_by_tool: RapidHashMap<&'a str, Vec<&'a TranscriptMessageRow>>,
}

impl<'a> AccountingIndex<'a> {
    fn build(
        tools: &'a [ToolDocument],
        headers: &'a [TranscriptMessageRow],
        generation: &str,
    ) -> Self {
        let mut tools_by_id = RapidHashMap::with_capacity(tools.len());
        let mut tool_counts = RapidHashMap::with_capacity(tools.len());
        for tool in tools {
            tools_by_id.insert(tool.doc_id.as_str(), tool);
            *tool_counts.entry(tool.doc_id.as_str()).or_insert(0) += 1;
        }
        let mut accepted_calls: RapidHashMap<&'a str, Vec<AcceptedCallBinding<'a>>> =
            RapidHashMap::with_capacity(tools.len());
        let mut deliveries_by_tool: RapidHashMap<&'a str, Vec<&'a TranscriptMessageRow>> =
            RapidHashMap::with_capacity(tools.len());
        for header in headers {
            let message = &header.message;
            if message.role == MessageRole::Assistant
                && message.outcome == OutputOutcome::Complete
                && matches!(
                    &message.publication,
                    MessagePublication::RequestExecution { execution_generation }
                        if execution_generation == generation
                )
            {
                for block in &message.blocks {
                    if let MessageBlock::ToolCall {
                        tool_call_doc_id,
                        id,
                        call_id,
                        name,
                        ..
                    } = block
                    {
                        accepted_calls
                            .entry(tool_call_doc_id.as_str())
                            .or_default()
                            .push(AcceptedCallBinding {
                                header,
                                id: id.as_str(),
                                call_id,
                                name: name.as_str(),
                            });
                    }
                }
            }
            if let MessagePublication::ToolDelivery { tool_call_doc_id } = &message.publication {
                deliveries_by_tool
                    .entry(tool_call_doc_id.as_str())
                    .or_default()
                    .push(header);
            }
        }
        Self {
            tools_by_id,
            tool_counts,
            accepted_calls,
            deliveries_by_tool,
        }
    }

    fn directly_bound(&self, tool: &ToolDocument) -> Result<Option<AcceptedCallBinding<'a>>> {
        let Some(bindings) = self.accepted_calls.get(tool.doc_id.as_str()) else {
            return Ok(None);
        };
        let mut found = None;
        for &binding in bindings {
            let message = &binding.header.message;
            if message.request_doc_id.as_deref() != Some(tool.request_doc_id.as_str())
                || message.session_id.as_str() != tool.row.session_id.as_deref().unwrap_or_default()
                || Some(i64::from(message.sequence)) != tool.row.message_sequence
                || message.node_did != tool.node_did
                || message.requester_did != tool.row.requester_did
            {
                continue;
            }
            anyhow::ensure!(
                found.is_none(),
                "tool has ambiguous accepted header membership"
            );
            anyhow::ensure!(
                tool.row.tool_call_id.as_deref() == Some(binding.id)
                    && tool.row.tool_name.as_deref() == Some(binding.name),
                "physical tool disagrees with accepted native intent"
            );
            found = Some(binding);
        }
        Ok(found)
    }
}

struct DerivedValidationCache {
    root_messages: RapidHashMap<String, gents_protocol::message::Message>,
    validated_args: RapidHashMap<String, String>,
}

impl DerivedValidationCache {
    fn new() -> Self {
        Self {
            root_messages: RapidHashMap::new(),
            validated_args: RapidHashMap::new(),
        }
    }
}

async fn validated_root_arguments(
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
    cache: &mut DerivedValidationCache,
    root: &ToolDocument,
    binding: AcceptedCallBinding<'_>,
) -> Result<String> {
    if let Some(args) = cache.validated_args.get(root.doc_id.as_str()) {
        return Ok(args.clone());
    }
    let header_doc_id = &binding.header.doc_id;
    if !cache.root_messages.contains_key(header_doc_id.as_str()) {
        let (_, message) = reader.load_message(header_doc_id).await?;
        cache.root_messages.insert(header_doc_id.clone(), message);
    }
    let message = &cache.root_messages[header_doc_id.as_str()];
    let gents_protocol::message::Message::Assistant { content, .. } = message else {
        anyhow::bail!("accepted tool publication is not an assistant message");
    };
    let tool_call_id = root
        .row
        .tool_call_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .context("accepted root tool is missing native tool_call_id")?;
    let tool_name = root
        .row
        .tool_name
        .as_deref()
        .context("accepted root tool is missing tool_name")?;
    let matching = content
        .iter()
        .filter_map(|item| match item {
            gents_protocol::message::AssistantContent::ToolCall(call)
                if call.id == tool_call_id && call.function.name == tool_name =>
            {
                Some(&call.function.arguments)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matching.len() == 1,
        "accepted native message does not uniquely bind tool arguments"
    );
    let serialized =
        serde_json::to_string(matching[0]).context("serializing accepted native tool arguments")?;
    cache
        .validated_args
        .insert(root.doc_id.clone(), serialized.clone());
    Ok(serialized)
}

async fn validate_derived_chain_arguments(
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
    cache: &mut DerivedValidationCache,
    chain_from_root: &[&ToolDocument],
    binding: AcceptedCallBinding<'_>,
    request_id: &str,
    session: &str,
    generation: &str,
) -> Result<()> {
    let mut parent_args =
        validated_root_arguments(reader, cache, chain_from_root[0], binding).await?;
    for pair in chain_from_root.windows(2) {
        let (parent, child) = (pair[0], pair[1]);
        if let Some(existing) = cache.validated_args.get(child.doc_id.as_str()) {
            parent_args = existing.clone();
            continue;
        }
        let child_call_id = child
            .row
            .tool_call_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .context("derived tool is missing native tool_call_id")?;
        let child_name = child
            .row
            .tool_name
            .as_deref()
            .context("derived tool is missing tool_name")?;
        let args = if child.row.plugin_parent_tool_call_doc_id.is_some() {
            anyhow::ensure!(
                child.row.tool_call_key == child_call_id
                    && binding.header.message.outcome == OutputOutcome::Complete,
                "plugin effect lacks exact accepted provenance"
            );
            let source = OutputSource::ToolEffectArguments {
                tool_call_doc_id: child.doc_id.clone(),
            };
            let segments = reader
                .load_cached_request_source_rows(request_id, session, &source)
                .await?;
            crate::session::plugin_effect_rows::arguments_from_decoded_rows(
                &segments,
                request_id,
                crate::session::plugin_effect_rows::PluginEffectCall {
                    tool_call_doc_id: &child.doc_id,
                    tool_call_id: child_call_id,
                    tool_name: child_name,
                    execution_generation: generation,
                },
            )?
            .1
        } else {
            anyhow::ensure!(
                parent.row.tool_name.as_deref()
                    == Some(gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME)
                    && child.row.await_mode.as_deref() == Some("background"),
                "spawned tool lacks its spawn_process owner"
            );
            let input: crate::session::tool_output_rows::BackgroundToolArgs =
                serde_json::from_str(&parent_args)?;
            anyhow::ensure!(
                input.tool_name == child_name,
                "spawned tool differs from admitted target"
            );
            serde_json::to_string(&input.args)?
        };
        cache
            .validated_args
            .insert(child.doc_id.clone(), args.clone());
        parent_args = args;
    }
    Ok(())
}

async fn validate_derived_completed_read(
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
    index: &AccountingIndex<'_>,
    tool: &ToolDocument,
    request_id: &str,
    session: &str,
) -> Result<()> {
    let state_str = tool
        .row
        .lifecycle_state
        .as_deref()
        .context("owned tool missing lifecycle")?;
    let lifecycle_state = ToolCallState::from_persisted(state_str).with_context(|| {
        format!(
            "AgentToolCall {} has invalid lifecycle state {state_str}",
            tool.doc_id
        )
    })?;
    let delivery_rows = index
        .deliveries_by_tool
        .get(tool.doc_id.as_str())
        .map_or(&[][..], Vec::as_slice);
    if tool.row.plugin_parent_tool_call_doc_id.is_some() {
        anyhow::ensure!(
            delivery_rows.is_empty(),
            "plugin effect must not publish a transcript invocation reply"
        );
        if let Some(payload) = &tool.row.terminal_output {
            anyhow::ensure!(
                lifecycle_state.is_terminal(),
                "nonterminal plugin effect carries terminal output"
            );
            anyhow::ensure!(
                payload.output.stream == 0,
                "plugin terminal output must select stream zero"
            );
            let source = OutputSource::ToolCall {
                tool_call_doc_id: tool.doc_id.clone(),
            };
            let segments = reader
                .load_cached_request_source_rows(request_id, session, &source)
                .await?;
            anyhow::ensure!(
                segments.iter().any(|row| {
                    row.doc_id == payload.output.close_doc_id
                        && row.segment.source == source
                        && row.segment.close.is_some()
                }),
                "canonical payload close does not belong to the expected tool source"
            );
            let observed = segments
                .iter()
                .map(|row| ObservedSegment {
                    doc_id: &row.doc_id,
                    segment: &row.segment,
                })
                .collect::<Vec<_>>();
            let output = reconstruct_stream(&observed, &[], &[], &payload.output)?;
            anyhow::ensure!(
                matches!(output.declaration.payload, StreamPayload::ToolOutput),
                "plugin terminal output selects a non-tool-output stream"
            );
            crate::session::tool_presentation::render_presentation(
                &output.text,
                &payload.presentation,
            )?;
        } else if tool.row.await_mode.as_deref() == Some("background")
            && !lifecycle_state.is_terminal()
        {
            let receipt_source =
                crate::session::plugin_effect_rows::receipt_source(session, &tool.doc_id);
            let segments = reader
                .load_cached_request_source_rows(request_id, session, &receipt_source)
                .await?;
            crate::session::plugin_effect_rows::receipt_from_decoded_rows(
                &segments,
                session,
                request_id,
                &tool.doc_id,
            )?;
        } else {
            anyhow::ensure!(
                !lifecycle_state.is_terminal()
                    || (lifecycle_state == ToolCallState::Cancelled
                        && tool.row.started_at.is_none()),
                "terminal plugin effect is missing its canonical presentation"
            );
        }
        return Ok(());
    }
    let has_reply = delivery_rows.iter().any(|row| {
        crate::tool_call::query::result::is_invocation_reply(&row.message, &tool.doc_id)
    });
    anyhow::ensure!(
        !has_reply && delivery_rows.len() <= 1,
        "spawned process must not fabricate a native ToolResult delivery or duplicate its notification"
    );
    Ok(())
}

pub async fn account_tools_in_txn(
    txn: &ConfigApplyTxn<'_>,
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
    request: &AgentRequestRow,
    headers: &[TranscriptMessageRow],
    generation: &str,
    completed: bool,
    timestamp: &str,
) -> Result<()> {
    let request_id = request
        .doc_id
        .as_deref()
        .context("request has no physical identity")?;
    let agent = request
        .node_did
        .as_deref()
        .context("request missing agent")?;
    let session = request
        .session_id
        .as_deref()
        .context("request missing session")?;
    let escaped_request = escape_graphql_string(request_id);
    let response = {
        use gents_protocol::document_query::{
            fields::agent_tool_call as fields, session_scope, Filter, Query, Selection,
        };
        let rendered = Query::select(
            Selection::doc_id()
                .field(fields::TOOL_CALL_KEY)
                .field(fields::REQUEST_DOC_ID)
                .field(fields::NODE_DID)
                .field(fields::REQUESTER_DID)
                .field(fields::SESSION_ID)
                .field(fields::MESSAGE_SEQUENCE)
                .field(fields::TOOL_CALL_ID)
                .field(fields::TOOL_NAME)
                .field(fields::LIFECYCLE_STATE)
                .field(fields::AWAIT_MODE)
                .field(fields::STARTED_AT)
                .field(fields::DEADLINE_AT)
                .field(fields::SPAWNED_BY_TOOL_CALL_DOC_ID)
                .field(fields::PLUGIN_PARENT_TOOL_CALL_DOC_ID)
                .field(fields::PLUGIN_EFFECT_ORDINAL)
                .field(fields::TERMINAL_OUTPUT),
        )
        .filter(session_scope(
            agent,
            session,
            request.requester_did.as_deref(),
        ))
        .filter(Filter::eq(fields::REQUEST_DOC_ID, request_id))
        .render();
        txn.run_local_response(&rendered).await?
    };
    let tools: Vec<ToolDocument> = gents_store::graphql::rows(&response, "AgentToolCall")?;
    let index = AccountingIndex::build(&tools, headers, generation);
    for (tool_call_doc_id, bindings) in &index.accepted_calls {
        let _ = bindings;
        anyhow::ensure!(
            index
                .tool_counts
                .get(tool_call_doc_id)
                .copied()
                .unwrap_or(0)
                == 1,
            "accepted tool lifecycle is missing or ambiguous"
        );
    }
    let timestamp = escape_graphql_string(timestamp);
    let mut derived_cache = DerivedValidationCache::new();
    for tool in &tools {
        let spawned = tool.row.spawned_by_tool_call_doc_id.as_deref();
        let plugin_parent = crate::session::plugin_effect_rows::PluginEffectBinding::decode(
            tool.row.plugin_parent_tool_call_doc_id.as_deref(),
            tool.row.plugin_effect_ordinal,
            spawned.is_some(),
            tool.row.tool_call_id.as_deref().unwrap_or_default(),
        )?;
        let mut root = tool;
        let mut chain = vec![tool];
        let mut seen = RapidHashSet::default();
        let mut live_background_ancestor = false;
        while let Some(parent_id) = root
            .row
            .plugin_parent_tool_call_doc_id
            .as_deref()
            .or(root.row.spawned_by_tool_call_doc_id.as_deref())
        {
            anyhow::ensure!(
                seen.insert(root.doc_id.as_str()),
                "cyclic tool invocation ancestry"
            );
            let parent = index
                .tools_by_id
                .get(parent_id)
                .copied()
                .context("derived tool has no parent in its request")?;
            anyhow::ensure!(
                parent.request_doc_id == root.request_doc_id
                    && parent.node_did == root.node_did
                    && parent.row.requester_did == root.row.requester_did
                    && parent.row.session_id == root.row.session_id
                    && parent.row.message_sequence == root.row.message_sequence,
                "derived tool crosses its parent scope"
            );
            if root.row.spawned_by_tool_call_doc_id.is_some() {
                anyhow::ensure!(
                    parent.row.tool_name.as_deref() == Some("spawn_process")
                        && root.row.await_mode.as_deref() == Some("background"),
                    "invalid spawned invocation"
                );
            }
            live_background_ancestor |= parent.row.await_mode.as_deref() == Some("background")
                && parent.row.lifecycle_state.as_deref() == Some("running");
            root = parent;
            chain.push(root);
        }
        let accepted = index.directly_bound(root)?;
        if root.doc_id != tool.doc_id {
            if let Some(binding) = accepted {
                chain.reverse();
                validate_derived_chain_arguments(
                    reader,
                    &mut derived_cache,
                    &chain,
                    binding,
                    request_id,
                    session,
                    generation,
                )
                .await?;
                if completed {
                    validate_derived_completed_read(reader, &index, tool, request_id, session)
                        .await?;
                }
            }
        }
        let Some(accepted_binding) = accepted else {
            continue;
        };
        let state = tool
            .row
            .lifecycle_state
            .as_deref()
            .context("owned tool missing lifecycle")?;
        anyhow::ensure!(
            matches!(
                state,
                "pending" | "running" | "completed" | "failed" | "timedOut" | "cancelled"
            ),
            "owned tool has invalid lifecycle"
        );
        if live_background_ancestor {
            continue;
        }
        if completed && state != "pending" {
            if state == "running" && tool.row.await_mode.as_deref() != Some("background") {
                return Err(ToolAccountingRejection::ForegroundRunning.into());
            }
            if spawned.is_none()
                && plugin_parent.is_none()
                && (state == "running" || tool.row.started_at.is_some())
            {
                let tool_deliveries = index
                    .deliveries_by_tool
                    .get(tool.doc_id.as_str())
                    .map_or(&[][..], Vec::as_slice);
                let deliveries = tool_deliveries
                    .iter()
                    .copied()
                    .filter(|header| {
                        super::terminal_binding::is_exact_invocation_reply(
                            &header.message,
                            request_id,
                            session,
                            &tool.doc_id,
                            accepted_binding.id,
                            accepted_binding.call_id,
                        )
                    })
                    .collect::<Vec<_>>();
                if deliveries.len() != 1 {
                    let candidates = tool_deliveries
                        .iter()
                        .map(|header| format!("{}:{:?}", header.doc_id, header.message.blocks))
                        .collect::<Vec<_>>();
                    anyhow::bail!(
                        "started invocation has no unique durable reply: tool_doc_id={}, accepted_id={}, accepted_call_id={:?}, exact_replies={}, physical_delivery_candidates={:?}",
                        tool.doc_id,
                        accepted_binding.id,
                        accepted_binding.call_id,
                        deliveries.len(),
                        candidates
                    );
                }
                reader.load_message(&deliveries[0].doc_id).await?;
            }
        }
        let input = if state == "pending" {
            format!(
                r#"lifecycle_state: "cancelled", cancel_cause: "interrupted", completed_at: "{timestamp}""#
            )
        } else if state == "running" && !completed {
            format!(r#"stuck_since: "{timestamp}""#)
        } else {
            continue;
        };
        let tool_id = escape_graphql_string(&tool.doc_id);
        let state = escape_graphql_string(state);
        let response = txn.execute_local_response(&format!(r#"mutation {{
            update_AgentToolCall(filter: {{ _docID: {{ _eq: "{tool_id}" }},
                request_doc_id: {{ _eq: "{escaped_request}" }}, lifecycle_state: {{ _eq: "{state}" }} }},
                input: {{ {input} }}) {{ _docID }}
        }}"#)).await?;
        anyhow::ensure!(
            response
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentToolCall"))
                .is_some_and(response_has_documents),
            "owned tool accounting lost lifecycle CAS"
        );
    }
    Ok(())
}
