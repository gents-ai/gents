use anyhow::{Context, Result};
use gents_desktop_core::client::ClientCore;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RetainedProviderReasoning {
    request_doc_id: String,
    reasoning_by_source: Vec<String>,
}

pub(super) async fn retained_provider_reasoning(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
    request_id: &str,
) -> Result<RetainedProviderReasoning> {
    use gents::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::{
        reconstruction::{reconstruct_stream, ObservedSegment},
        OutputSource, PayloadRef, SourceClose, StreamPayload,
    };

    let node = gents::graphql::escape_graphql_string(node_did);
    let session = gents::graphql::escape_graphql_string(session_id);
    let logical = gents::graphql::escape_graphql_string(request_id);
    let request_query = format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{node}" }}, session_id: {{ _eq: "{session}" }}, request_id: {{ _eq: "{logical}" }} }}, limit: 2) {{ _docID }} }}"#
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        core.node(),
        &request_query,
        "retained reasoning request lookup",
    )
    .await?;
    let requests = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .context("retained reasoning request lookup returned no rows")?;
    anyhow::ensure!(requests.len() == 1, "expected one exact physical request");
    let request_doc_id = requests[0]
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .context("request omitted physical _docID")?
        .to_owned();
    let physical = gents::graphql::escape_graphql_string(&request_doc_id);
    let segment_query = format!(
        r#"{{ AgentOutputSegment(filter: {{ node_did: {{ _eq: "{node}" }}, session_id: {{ _eq: "{session}" }}, request_doc_id: {{ _eq: "{physical}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        core.node(),
        &segment_query,
        "retained reasoning segment lookup",
    )
    .await?;
    let values = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("retained reasoning segment lookup returned no rows")?;
    let rows = values
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    let mut sources = Vec::new();
    for row in &rows {
        if matches!(&row.segment.source, OutputSource::ProviderTurn { .. })
            && !row.segment.source.is_auxiliary_audit()
            && !sources.contains(&row.segment.source)
        {
            sources.push(row.segment.source.clone());
        }
    }
    let mut reasoning_by_source = Vec::new();
    for source in sources {
        let facts = rows
            .iter()
            .filter(|row| row.segment.source == source)
            .collect::<Vec<_>>();
        let closes = facts
            .iter()
            .filter(|row| row.segment.close.is_some())
            .collect::<Vec<_>>();
        anyhow::ensure!(closes.len() == 1, "provider source lacks one exact closure");
        let SourceClose::Closed { stream_bytes, .. } = closes[0].segment.close.as_ref().unwrap()
        else {
            continue;
        };
        let observed = facts
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let mut reasoning = String::new();
        for stream in 0..stream_bytes.len() {
            let reconstructed = reconstruct_stream(
                &observed,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: closes[0].doc_id.clone(),
                    stream: stream as u32,
                },
            )?;
            if matches!(
                reconstructed.declaration.payload,
                StreamPayload::Reasoning | StreamPayload::ReasoningSummary
            ) {
                reasoning.push_str(&reconstructed.text);
            }
        }
        reasoning_by_source.push(reasoning);
    }
    Ok(RetainedProviderReasoning {
        request_doc_id,
        reasoning_by_source,
    })
}
