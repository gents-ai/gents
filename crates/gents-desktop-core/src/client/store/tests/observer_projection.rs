use crate::client::{ClientStore, ClientStoreRows};
use gents::session::canonical_rows::TranscriptMessageRow;
use gents_protocol::output::{MessagePublication, MessageRole, OutputOutcome, TranscriptMessage};

fn header(id: &str) -> TranscriptMessageRow {
    TranscriptMessageRow {
        doc_id: id.into(),
        message: TranscriptMessage {
            message_key: id.into(),
            session_id: "session-1".into(),
            agent_did: "did:agent:1".into(),
            requester_did: None,
            request_doc_id: None,
            publication: MessagePublication::RequestExecution {
                execution_generation: "test".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 1,
            role: MessageRole::Assistant,
            native_id: None,
            blocks: Vec::new(),
            created_at: "2026-04-21T00:00:00Z".into(),
        },
    }
}

#[test]
fn observer_projection_drops_canonical_payload_facts() {
    let store = ClientStore::from_rows(ClientStoreRows {
        transcript_messages: vec![header("header")],
        ..ClientStoreRows::default()
    });
    let observer = store.into_observer_projection();
    assert!(observer.transcript_messages.is_empty());
    assert!(observer.output_segments.is_empty());
    assert!(observer.transcript("session-1").messages.is_empty());
}
