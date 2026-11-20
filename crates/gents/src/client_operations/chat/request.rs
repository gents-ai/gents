use gents_protocol::request_input::RequestInput;
use gents_session::request::lifecycle::TriggerLineage;

/// The request the runtime holds for a send. Replica observation may still be
/// pending; callers must not resend under a new identity when a subsequent
/// refresh fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SubmitRequestOptions {
    pub input: RequestInput,
    pub trigger_lineage: TriggerLineage,
    pub retry_key: Option<String>,
}

#[cfg(test)]
mod tests;

pub(super) mod prepare;
