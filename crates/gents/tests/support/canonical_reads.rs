//! Reading a canonical message through the live, non-transactional reader.

/// Reconstructs `header`, yielding `None` while the message is not yet wholly
/// readable.
///
/// The live reader answers from independent reads of a node that may still be
/// receiving a session's facts, so `ReconstructionError::is_incomplete` names
/// the states a reader may not yet resolve. Every other error is a defect.
pub async fn live_canonical_message(
    access: &gents_store::config_client::ConfigAccess,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Option<(
    gents_protocol::output::TranscriptMessage,
    gents_protocol::message::Message,
)> {
    match gents_session::session::load_canonical_message(access, header_doc_id, node_did, requester_did)
        .await
    {
        Ok(message) => Some(message),
        Err(error)
            if error
                .downcast_ref::<gents_protocol::output::ReconstructionError>()
                .is_some_and(gents_protocol::output::ReconstructionError::is_incomplete) =>
        {
            None
        }
        Err(error) => panic!("reconstruct canonical message {header_doc_id}: {error:#}"),
    }
}
