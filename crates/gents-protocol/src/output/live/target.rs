use crate::output::reconstruction::ObservedSegment;
use crate::output::{OutputSource, OutputWriter, TranscriptMessage};

pub struct LiveTarget<'a> {
    pub request_doc_id: &'a str,
    pub source: &'a OutputSource,
    pub writer: &'a OutputWriter,
    pub message_id: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTargetSelection {
    Absent,
    Selected {
        source: OutputSource,
        writer: OutputWriter,
        message_id: Option<String>,
    },
    Conflicted,
}

/// Rust projection of Lean `StreamingResponse.selectTarget`.
pub fn select_live_target(
    request_doc_id: &str,
    execution_generation: &str,
    records: &[ObservedSegment<'_>],
    messages: &[(&str, &TranscriptMessage)],
) -> LiveTargetSelection {
    use crate::rendered_request::CaptureScopeKind;

    let selected = records
        .iter()
        .filter_map(|observed| {
            let OutputSource::ProviderTurn {
                scope,
                turn_index,
                attempt,
            } = &observed.segment.source
            else {
                return None;
            };
            let OutputWriter::RequestExecution {
                execution_generation: writer_generation,
            } = &observed.segment.writer
            else {
                return None;
            };
            (observed.segment.request_doc_id == request_doc_id
                && writer_generation == execution_generation
                && scope.kind == CaptureScopeKind::Inference)
                .then_some((
                    (*scope, *turn_index, *attempt),
                    &observed.segment.source,
                    &observed.segment.writer,
                ))
        })
        .max_by_key(|(coordinate, _, _)| *coordinate);
    let Some((_, source, writer)) = selected else {
        return LiveTargetSelection::Absent;
    };

    let ids = messages
        .iter()
        .filter_map(|(doc_id, message)| {
            (message.request_doc_id.as_deref() == Some(request_doc_id)
                && message.payload_references().iter().any(|reference| {
                    records.iter().any(|observed| {
                        observed.doc_id == reference.close_doc_id
                            && observed.segment.request_doc_id == request_doc_id
                            && observed.segment.source == *source
                            && observed.segment.writer == *writer
                            && observed.segment.close.is_some()
                    })
                }))
            .then_some((*doc_id).to_owned())
        })
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() > 1 {
        return LiveTargetSelection::Conflicted;
    }
    LiveTargetSelection::Selected {
        source: source.clone(),
        writer: writer.clone(),
        message_id: ids.into_iter().next(),
    }
}
