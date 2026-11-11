use anyhow::{Context, Result};
use gents_protocol::output::rows::decode_scoped_request_output_segments;
use gents_protocol::output::{
    reconstruction::{reconstruct_stream, ObservedSegment},
    OutputOutcome, OutputSource, OutputWriter, PayloadRef, SourceClose, StreamDeclaration,
    StreamPayload,
};
use gents_protocol::plugin_tool_call_limits::{MAX_EFFECTS, MAX_PAYLOAD_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginEffectBinding {
    pub(crate) parent_tool_call_doc_id: String,
    pub(crate) ordinal: u32,
}

impl PluginEffectBinding {
    pub(crate) fn internal_id(parent_tool_call_doc_id: &str, ordinal: u32) -> String {
        format!("plugin-effect:{parent_tool_call_doc_id}:{ordinal}")
    }

    pub(crate) fn decode(
        parent_tool_call_doc_id: Option<&str>,
        ordinal: Option<u32>,
        spawned: bool,
        tool_call_id: &str,
    ) -> anyhow::Result<Option<Self>> {
        match (parent_tool_call_doc_id, ordinal) {
            (None, None) => Ok(None),
            (Some(parent), Some(ordinal))
                if !parent.is_empty()
                    && (1..=MAX_EFFECTS).contains(&ordinal)
                    && !spawned
                    && tool_call_id == Self::internal_id(parent, ordinal) =>
            {
                Ok(Some(Self {
                    parent_tool_call_doc_id: parent.to_owned(),
                    ordinal,
                }))
            }
            _ => anyhow::bail!("invalid plugin effect provenance"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn arguments_from_rows(
    rows: &[serde_json::Value],
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    child: &str,
    generation: &str,
    internal_id: &str,
    name: &str,
) -> Result<(PayloadRef, String)> {
    let records =
        decode_scoped_request_output_segments(rows, node_did, Some(session_id), requester_did)?;
    let source = OutputSource::ToolEffectArguments {
        tool_call_doc_id: child.to_owned(),
    };
    let records = records
        .iter()
        .filter(|row| row.segment.source == source)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        records.len() == 1,
        "plugin effect requires one immutable arguments source"
    );
    let record = records[0];
    anyhow::ensure!(
        record.segment.request_doc_id == request_doc_id
            && matches!(&record.segment.writer, OutputWriter::RequestExecution { execution_generation }
            if execution_generation == generation)
            && matches!(&record.segment.close, Some(SourceClose::Closed { outcome: OutputOutcome::Complete,
            segments: 1, stream_bytes }) if stream_bytes.len() == 1),
        "invalid plugin arguments closure"
    );
    let reference = PayloadRef {
        close_doc_id: record.doc_id.clone(),
        stream: 0,
    };
    let reconstructed = reconstruct_stream(
        &[ObservedSegment {
            doc_id: &record.doc_id,
            segment: &record.segment,
        }],
        &[],
        &[],
        &reference,
    )?;
    anyhow::ensure!(
        reconstructed.declaration
            == StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: internal_id.to_owned(),
                    call_id: None,
                    name: name.to_owned()
                }
            },
        "plugin argument declaration changed tool identity"
    );
    anyhow::ensure!(
        reconstructed.text.len() <= MAX_PAYLOAD_BYTES,
        "plugin arguments exceed their payload bound"
    );
    let _: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&reconstructed.text)
            .context("plugin effect arguments must be a JSON object")?;
    Ok((reference, reconstructed.text))
}

pub fn receipt_source(session: &str, tool: &str) -> OutputSource {
    OutputSource::Authored {
        key: format!("{session}:background-receipt:{tool}"),
    }
}

pub fn receipt_from_rows(
    rows: &[serde_json::Value],
    node: &str,
    session: &str,
    requester: Option<&str>,
    request: &str,
    tool: &str,
) -> Result<Option<String>> {
    let records = decode_scoped_request_output_segments(rows, node, Some(session), requester)?;
    let source = receipt_source(session, tool);
    let matching = records
        .iter()
        .filter(|row| row.segment.source == source)
        .collect::<Vec<_>>();
    anyhow::ensure!(matching.len() <= 1, "ambiguous plugin receipt");
    let Some(row) = matching.first() else {
        return Ok(None);
    };
    anyhow::ensure!(
        row.segment.request_doc_id == request
            && row.segment.writer
                == OutputWriter::ToolExecution {
                    tool_call_doc_id: tool.to_owned()
                }
            && matches!(
                row.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    ..
                })
            ),
        "plugin receipt has invalid ownership or closure"
    );
    let observed = [ObservedSegment {
        doc_id: &row.doc_id,
        segment: &row.segment,
    }];
    let reconstructed = reconstruct_stream(
        &observed,
        &[],
        &[],
        &PayloadRef {
            close_doc_id: row.doc_id.clone(),
            stream: 0,
        },
    )?;
    anyhow::ensure!(
        matches!(reconstructed.declaration.payload, StreamPayload::ToolOutput),
        "plugin receipt is not tool output"
    );
    Ok(Some(reconstructed.text))
}
