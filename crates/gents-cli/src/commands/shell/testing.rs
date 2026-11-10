use gents_desktop_core::inference::AgentView;
use gents_shell::sample::Tone;
use gents_shell::source::{FieldKind, Form, MenuAct, MenuRow, Update};

/// `pairs` as the shell sends a form's values.
pub(super) fn sent(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let sent = pairs.iter();
    sent.map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Each of `form`'s fields' key and the value it shows.
pub(super) fn shown_values(form: &Form) -> Vec<(&str, &str)> {
    let shown = form.fields.iter();
    shown
        .map(|field| (field.key.as_str(), field.value.as_str()))
        .collect()
}

/// The keys of `form`'s fields of `kind`.
pub(super) fn keys_of<'a>(form: &'a Form, kind: &FieldKind) -> Vec<&'a str> {
    let fields = form.fields.iter().filter(|field| field.kind == *kind);
    fields.map(|field| field.key.as_str()).collect()
}

/// The `(value, label)` pairs `form`'s field `key` chooses among, none for a field that is no
/// choice.
pub(super) fn options<'a>(form: &'a Form, key: &str) -> Vec<(&'a str, &'a str)> {
    let field = form.fields.iter().find(|field| field.key == key);
    match field.map(|field| &field.kind) {
        Some(FieldKind::Choice(pairs)) => pairs
            .iter()
            .map(|(value, label)| (value.as_str(), label.as_str()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Each field `form` shows only while another holds a value: its key, the other's and the value.
pub(super) fn conditions(form: &Form) -> Vec<(&str, &str, &str)> {
    let shown = form.fields.iter().filter_map(|field| {
        let (key, values) = field.when.as_ref()?;
        Some(
            values
                .iter()
                .map(move |value| (field.key.as_str(), key.as_str(), value.as_str())),
        )
    });
    shown.flatten().collect()
}

pub(super) fn agent(id: &str, name: &str, enabled: bool, is_default: bool) -> AgentView {
    AgentView {
        creation_nonce: String::new(),
        agent_did: id.to_string(),
        node_did: "owner".to_string(),
        display_name: name.to_string(),
        description: None,
        context_id: None,
        inference_profile_id: None,
        enabled,
        is_default,
        tags: Vec::new(),
        created_at: None,
    }
}

pub(super) fn shown_rows(rows: &[MenuRow]) -> Vec<(&str, Tone)> {
    rows.iter()
        .map(|row| (row.text.as_str(), row.tone))
        .collect()
}

/// An Ask's title and its rows' text, tone and enter act.
pub(super) fn question(update: Update) -> (String, Vec<(String, Tone, Option<MenuAct>)>) {
    let Update::Ask { title, rows } = update else {
        panic!("{update:?}");
    };
    let rows = rows.into_iter().map(|row| (row.text, row.tone, row.enter));
    (title, rows.collect())
}

/// Where a fixture's output is written: its node, session, requester and request.
#[derive(Clone)]
pub(super) struct At {
    pub(super) node: String,
    pub(super) session: String,
    pub(super) requester: Option<String>,
    pub(super) request: String,
    pub(super) generation: String,
}

/// One whole stream `payload` of `source`, closed complete.
pub(super) fn closed(
    at: &At,
    source: gents_protocol::output::OutputSource,
    writer: gents_protocol::output::OutputWriter,
    payload: gents_protocol::output::StreamPayload,
    text: &str,
) -> gents_protocol::output::OutputSegment {
    use gents_protocol::output::{OutputOutcome, SegmentRun, SourceClose, StreamDeclaration};
    let bytes = u32::try_from(text.len()).unwrap();
    gents_protocol::output::OutputSegment {
        node_did: at.node.clone(),
        requester_did: at.requester.clone(),
        session_id: at.session.clone(),
        request_doc_id: at.request.clone(),
        source,
        writer,
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload,
            }),
        }],
        payload: text.to_string(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![u64::from(bytes)],
        }),
        created_at: "2026-11-05T12:00:00Z".to_string(),
    }
}

/// The request's execution as a segment's writer.
pub(super) fn execution(at: &At) -> gents_protocol::output::OutputWriter {
    let execution_generation = at.generation.clone();
    gents_protocol::output::OutputWriter::RequestExecution {
        execution_generation,
    }
}

/// A provider turn of the request.
pub(super) fn turn_source(turn: u32) -> gents_protocol::output::OutputSource {
    gents_protocol::output::OutputSource::ProviderTurn {
        scope: format!("inference.{}", turn + 1).parse().unwrap(),
        turn_index: turn,
        attempt: 0,
    }
}

/// A message of `role` at `sequence` whose `blocks` reference closed streams.
pub(super) fn header(
    at: &At,
    key: &str,
    sequence: u32,
    role: gents_protocol::output::MessageRole,
    publication: gents_protocol::output::MessagePublication,
    blocks: Vec<gents_protocol::output::MessageBlock>,
) -> gents_protocol::output::TranscriptMessage {
    gents_protocol::output::TranscriptMessage {
        message_key: key.to_string(),
        session_id: at.session.clone(),
        node_did: at.node.clone(),
        requester_did: at.requester.clone(),
        request_doc_id: Some(at.request.clone()),
        publication,
        outcome: gents_protocol::output::OutputOutcome::Complete,
        sequence,
        role,
        native_id: None,
        native_request_id: None,
        blocks,
        created_at: "2026-11-05T12:00:00Z".to_string(),
    }
}

/// The whole of the stream that `close` ends.
pub(super) fn presented(close: &str) -> gents_protocol::output::PresentedPayload {
    gents_protocol::output::PresentedPayload {
        output: gents_protocol::output::PayloadRef {
            close_doc_id: close.to_string(),
            stream: 0,
        },
        presentation: gents_protocol::output::PayloadPresentation::Full,
    }
}

/// A text message's stream and its header, whose block references the stream's close,
/// `close`.
pub(super) fn text_message(
    at: &At,
    sequence: u32,
    role: gents_protocol::output::MessageRole,
    text: &str,
    close: &str,
) -> (
    gents_protocol::output::OutputSegment,
    gents_protocol::output::TranscriptMessage,
) {
    use gents_protocol::output::{MessageBlock, MessagePublication, OutputSource, StreamPayload};
    let key = format!("{}:{sequence}", at.request);
    let source = OutputSource::Authored { key: key.clone() };
    let segment = closed(at, source, execution(at), StreamPayload::Text, text);
    let publication = MessagePublication::RequestExecution {
        execution_generation: at.generation.clone(),
    };
    let blocks = vec![MessageBlock::Text {
        text: presented(close),
    }];
    let message = header(at, &key, sequence, role, publication, blocks);
    (segment, message)
}
