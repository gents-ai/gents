use super::*;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::required_rows;

pub(super) async fn load_hydration_selection(
    node: &Arc<EmbeddedNode>,
    request: &HydrationRequest,
) -> Result<HydrationSelection> {
    ConfigAccess::transact_local_readonly(node, None, "session_hydration.selection", |txn| {
        Box::pin(load_hydration_selection_in_txn(txn, request))
    })
    .await
}

/// All origin, segment and effect dependencies use the caller's same read
/// snapshot. Inventory enumeration must additionally establish bounded complete
/// membership and current projection owners before certifying coverage.
pub(super) async fn load_hydration_selection_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &HydrationRequest,
) -> Result<HydrationSelection> {
    let response = pages::load_session_rows(txn, request).await?;

    let mut root_headers = required_rows::<serde_json::Value>(&response, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut root_header_ids = root_headers
        .iter()
        .map(|row| row.doc_id.clone())
        .collect::<Vec<_>>();
    let mut terminal_request_ids = BTreeSet::new();
    for request_row in
        required_rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")?
    {
        if request_row.is_terminal() {
            terminal_request_ids.insert(
                request_row
                    .doc_id
                    .clone()
                    .context("terminal hydration request omitted physical id")?,
            );
            match request_row
                .terminal_output
                .context("terminal hydration request omitted terminal_output")?
            {
                gents_protocol::output::TerminalOutput::Message { message_doc_id } => {
                    let selected = root_headers
                        .iter()
                        .find(|row| row.doc_id == message_doc_id)
                        .context("terminal hydration selection is missing its exact header")?;
                    anyhow::ensure!(
                        selected.message.session_id == request.session_id
                            && selected.message.request_doc_id == request_row.doc_id,
                        "terminal hydration selection does not belong to the exact request"
                    );
                    anyhow::ensure!(
                        selected.message.role == gents_protocol::output::MessageRole::Assistant
                            && matches!(
                                selected.message.publication,
                                gents_protocol::output::MessagePublication::RequestExecution { .. }
                                    | gents_protocol::output::MessagePublication::RequestRecovery { .. }
                            ),
                        "terminal hydration selection is not an eligible assistant header"
                    );
                    root_header_ids.push(message_doc_id);
                }
                gents_protocol::output::TerminalOutput::NoMessage => {}
            }
        }
    }
    root_header_ids.sort();
    root_header_ids.dedup();
    load_origin_headers(
        txn,
        &mut root_headers,
        &request.node_did,
        &request.requester_did,
    )
    .await?;
    let mut output_segments = required_rows::<serde_json::Value>(&response, "AgentOutputSegment")?
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    load_referenced_segments(
        txn,
        &root_headers,
        &mut output_segments,
        &request.node_did,
        &request.requester_did,
    )
    .await?;
    let mut bases = Vec::new();
    let mut effects = Vec::new();
    for row in required_rows::<gents_protocol::row::AgentToolCallRow>(&response, "AgentToolCall")? {
        observe_tool_call(row, &mut bases, &mut effects);
    }
    for collection in [
        gents_protocol::session_hydration::SessionHydrationCollection::AgentRequest,
        gents_protocol::session_hydration::SessionHydrationCollection::CompactionEntry,
    ] {
        let name = hydration_collection_name(collection);
        bases.extend(
            required_rows::<TranscriptRow>(&response, name)?
                .into_iter()
                .filter_map(|row| {
                    Some(ScopedDocument {
                        collection,
                        doc_id: row.doc_id.filter(|value| !value.is_empty())?,
                        requester_did: row.requester_did.unwrap_or_default(),
                        node_did: row.node_did.unwrap_or_default(),
                        session_id: row.session_id.unwrap_or_default(),
                    })
                }),
        );
    }
    load_referenced_bases(
        txn,
        &root_headers,
        &output_segments,
        &mut bases,
        &mut effects,
        &request.node_did,
        &request.requester_did,
    )
    .await?;
    load_effect_children(txn, request, &bases, &mut effects).await?;
    let parents = bases
        .iter()
        .filter(|row| {
            row.collection
                == gents_protocol::session_hydration::SessionHydrationCollection::AgentToolCall
        })
        .map(|row| row.doc_id.as_str())
        .collect::<BTreeSet<_>>();
    let effect_requests = effects
        .iter()
        .filter(|row| {
            crate::tool_call_lifecycle::PluginEffectBinding::decode(
                row.plugin_parent_tool_call_doc_id.as_deref(),
                row.plugin_effect_ordinal,
                row.spawned,
                &row.tool_call_id,
            )
            .ok()
            .flatten()
            .is_some_and(|binding| parents.contains(binding.parent_tool_call_doc_id.as_str()))
        })
        .map(|row| row.request_doc_id.clone())
        .filter(|id| {
            !output_segments
                .iter()
                .any(|segment| &segment.segment.request_doc_id == id)
        })
        .collect();
    load_request_segments(
        txn,
        effect_requests,
        &mut output_segments,
        &request.node_did,
        &request.requester_did,
    )
    .await?;
    let session_bases = bases
        .iter()
        .filter(|row| row.session_id == request.session_id)
        .cloned()
        .collect::<Vec<_>>();
    let mut documents = build_canonical_closure(CanonicalClosureInput {
        root_header_ids: &root_header_ids,
        headers: &root_headers,
        segments: &output_segments,
        base_documents: &bases,
        retained_bases: &session_bases,
        tool_effects: &effects,
        denied_headers: &[],
        denied_segments: &[],
        dependency_denials: &[],
        node_did: &request.node_did,
        requester_did: Some(&request.requester_did),
        session_id: &request.session_id,
    })
    .context("build authorized canonical hydration closure")?;
    for request_id in terminal_request_ids {
        let row = bases
            .iter()
            .find(|row| {
                row.collection
                    == gents_protocol::session_hydration::SessionHydrationCollection::AgentRequest
                    && row.doc_id == request_id
            })
            .context("terminal hydration request missing from authorized base rows")?;
        documents.insert(HydrationDocument {
            collection: row.collection,
            doc_id: row.doc_id.clone(),
            requester_did: row.requester_did.clone(),
            node_did: row.node_did.clone(),
            session_id: row.session_id.clone(),
        });
    }
    let authorized_reference_closure = documents
        .iter()
        .filter(|document| document.session_id != request.session_id)
        .map(|document| SessionHydrationDocumentKey {
            collection: document.collection,
            doc_id: document.doc_id.clone(),
        })
        .collect();

    Ok(HydrationSelection {
        documents,
        authorized_reference_closure,
    })
}

async fn load_origin_headers(
    txn: &ConfigApplyTxn<'_>,
    headers: &mut Vec<crate::session::canonical_rows::TranscriptMessageRow>,
    node_did: &str,
    requester_did: &str,
) -> Result<()> {
    let mut loaded = headers
        .iter()
        .map(|row| row.doc_id.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let next = headers
            .iter()
            .find_map(|row| match &row.message.publication {
                gents_protocol::output::MessagePublication::Fork {
                    origin_message_doc_id,
                } if !loaded.contains(origin_message_doc_id) => Some(origin_message_doc_id.clone()),
                _ => None,
            });
        let Some(doc_id) = next else {
            return Ok(());
        };
        let id = escape_graphql_string(&doc_id);
        let agent = escape_graphql_string(node_did);
        let requester = escape_graphql_string(requester_did);
        let response = txn.execute_local_response(&format!(
            r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{id}" }}, node_did: {{ _eq: "{agent}" }}, requester_did: {{ _eq: "{requester}" }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        )).await?;
        let decoded = required_rows::<serde_json::Value>(&response, "AgentMessage")?
            .iter()
            .map(decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
        anyhow::ensure!(
            decoded.len() == 1,
            "hydration origin is missing, denied, or conflicting"
        );
        loaded.insert(doc_id);
        headers.extend(decoded);
    }
}

async fn load_referenced_segments(
    txn: &ConfigApplyTxn<'_>,
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
    segments: &mut Vec<crate::session::canonical_rows::OutputSegmentRow>,
    node_did: &str,
    requester_did: &str,
) -> Result<()> {
    let request_ids = headers
        .iter()
        .filter_map(|row| row.message.request_doc_id.clone())
        .chain(headers.iter().flat_map(|row| {
            row.message
                .payload_references()
                .into_iter()
                .filter_map(|reference| {
                    segments
                        .iter()
                        .find(|segment| segment.doc_id == reference.close_doc_id)
                        .map(|segment| segment.segment.request_doc_id.clone())
                })
        }))
        .collect::<BTreeSet<_>>();
    load_request_segments(txn, request_ids, segments, node_did, requester_did).await
}

async fn load_request_segments(
    txn: &ConfigApplyTxn<'_>,
    request_ids: BTreeSet<String>,
    segments: &mut Vec<crate::session::canonical_rows::OutputSegmentRow>,
    node_did: &str,
    requester_did: &str,
) -> Result<()> {
    for request_id in request_ids {
        let filter = crate::session::canonical_rows::request_output_segments_filter(&request_id);
        let values = pages::load_rows(
            txn,
            gents_protocol::session_hydration::SessionHydrationCollection::AgentOutputSegment,
            &filter,
        )
        .await?;
        for row in crate::session::canonical_rows::decode_scoped_request_output_segments(
            &values,
            node_did,
            None,
            Some(requester_did),
        )? {
            if !segments.iter().any(|known| known.doc_id == row.doc_id) {
                segments.push(row);
            }
        }
    }
    Ok(())
}

async fn load_referenced_bases(
    txn: &ConfigApplyTxn<'_>,
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
    segments: &[crate::session::canonical_rows::OutputSegmentRow],
    bases: &mut Vec<ScopedDocument>,
    effects: &mut Vec<ToolEffectRow>,
    node_did: &str,
    requester_did: &str,
) -> Result<()> {
    use gents_protocol::output::{MessageBlock, MessagePublication, OutputSource};
    use gents_protocol::session_hydration::SessionHydrationCollection;
    let mut wanted = BTreeSet::new();
    for row in headers {
        if let Some(id) = &row.message.request_doc_id {
            wanted.insert((SessionHydrationCollection::AgentRequest, id.clone()));
        }
        if let MessagePublication::ToolDelivery { tool_call_doc_id } = &row.message.publication {
            wanted.insert((
                SessionHydrationCollection::AgentToolCall,
                tool_call_doc_id.clone(),
            ));
        }
        for block in &row.message.blocks {
            if let MessageBlock::ToolCall {
                tool_call_doc_id, ..
            }
            | MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } = block
            {
                wanted.insert((
                    SessionHydrationCollection::AgentToolCall,
                    tool_call_doc_id.clone(),
                ));
            }
        }
    }
    for row in segments {
        if let OutputSource::ToolCall { tool_call_doc_id } = &row.segment.source {
            wanted.insert((
                SessionHydrationCollection::AgentToolCall,
                tool_call_doc_id.clone(),
            ));
        }
    }
    let agent = escape_graphql_string(node_did);
    let requester = escape_graphql_string(requester_did);
    for (collection, doc_id) in wanted {
        if bases
            .iter()
            .any(|row| row.collection == collection && row.doc_id == doc_id)
            || (collection == SessionHydrationCollection::AgentToolCall
                && effects.iter().any(|row| row.document.doc_id == doc_id))
        {
            continue;
        }
        let name = hydration_collection_name(collection);
        let id = escape_graphql_string(&doc_id);
        let fields = if collection == SessionHydrationCollection::AgentToolCall {
            HYDRATION_TOOL_CALL_FIELDS
        } else {
            "_docID requester_did node_did session_id"
        };
        let response = txn.execute_local_response(&format!(
            r#"{{ {name}(filter: {{ _docID: {{ _eq: "{id}" }}, node_did: {{ _eq: "{agent}" }}, requester_did: {{ _eq: "{requester}" }} }}, limit: 2) {{ {fields} }} }}"#
        )).await?;
        if collection == SessionHydrationCollection::AgentToolCall {
            let found = required_rows::<gents_protocol::row::AgentToolCallRow>(&response, name)?;
            anyhow::ensure!(
                found.len() == 1,
                "hydration provenance is missing, denied, or conflicting"
            );
            for row in found {
                observe_tool_call(row, bases, effects);
            }
            continue;
        }
        let found = required_rows::<TranscriptRow>(&response, name)?;
        anyhow::ensure!(
            found.len() == 1,
            "hydration provenance is missing, denied, or conflicting"
        );
        let row = found.into_iter().next().expect("length checked");
        bases.push(ScopedDocument {
            collection,
            doc_id: row
                .doc_id
                .context("hydration provenance omitted physical id")?,
            requester_did: row.requester_did.unwrap_or_default(),
            node_did: row.node_did.unwrap_or_default(),
            session_id: row.session_id.unwrap_or_default(),
        });
    }
    Ok(())
}

pub(super) const HYDRATION_TOOL_CALL_FIELDS: &str = "_docID tool_call_key requester_did node_did session_id request_doc_id tool_call_id spawned_by_tool_call_doc_id plugin_parent_tool_call_doc_id plugin_effect_ordinal terminal_output";

fn observe_tool_call(
    row: gents_protocol::row::AgentToolCallRow,
    bases: &mut Vec<ScopedDocument>,
    effects: &mut Vec<ToolEffectRow>,
) {
    let Some(doc_id) = row.doc_id.filter(|value| !value.is_empty()) else {
        return;
    };
    let document = ScopedDocument {
        collection: gents_protocol::session_hydration::SessionHydrationCollection::AgentToolCall,
        doc_id,
        requester_did: row.requester_did.unwrap_or_default(),
        node_did: row.node_did.unwrap_or_default(),
        session_id: row.session_id.unwrap_or_default(),
    };
    if row.plugin_parent_tool_call_doc_id.is_none() && row.plugin_effect_ordinal.is_none() {
        bases.push(document);
        return;
    }
    effects.push(ToolEffectRow {
        document,
        plugin_parent_tool_call_doc_id: row.plugin_parent_tool_call_doc_id,
        plugin_effect_ordinal: row.plugin_effect_ordinal,
        spawned: row.spawned_by_tool_call_doc_id.is_some(),
        tool_call_id: row.tool_call_id.unwrap_or_default(),
        request_doc_id: row.request_doc_id.unwrap_or_default(),
        terminal_output: row.terminal_output.map(|terminal| terminal.output),
    });
}

pub(super) async fn load_effect_children(
    txn: &ConfigApplyTxn<'_>,
    request: &HydrationRequest,
    bases: &[ScopedDocument],
    effects: &mut Vec<ToolEffectRow>,
) -> Result<()> {
    let parents = bases
        .iter()
        .filter(|row| {
            row.collection
                == gents_protocol::session_hydration::SessionHydrationCollection::AgentToolCall
                && row.session_id != request.session_id
        })
        .map(|row| row.doc_id.as_str())
        .collect::<BTreeSet<_>>();
    if parents.is_empty() {
        return Ok(());
    }
    let parents: Vec<_> = parents.into_iter().collect();
    let agent = escape_graphql_string(&request.node_did);
    let requester = escape_graphql_string(&request.requester_did);
    let mut ignored = Vec::new();
    for parents in parents.chunks(pages::PAGE_SIZE) {
        let parents = crate::graphql::graphql_string_list_literal(parents.iter().copied());
        let filter = format!(
            r#"plugin_parent_tool_call_doc_id: {{ _in: {parents} }}, node_did: {{ _eq: "{agent}" }}, requester_did: {{ _eq: "{requester}" }}"#
        );
        let values = pages::load_rows(
            txn,
            gents_protocol::session_hydration::SessionHydrationCollection::AgentToolCall,
            &filter,
        )
        .await?;
        for value in values {
            let row: gents_protocol::row::AgentToolCallRow = serde_json::from_value(value)?;
            if !effects
                .iter()
                .any(|known| row.doc_id.as_deref() == Some(&known.document.doc_id))
            {
                observe_tool_call(row, &mut ignored, effects);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

mod pages;
