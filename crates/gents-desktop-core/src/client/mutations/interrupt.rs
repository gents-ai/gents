use gents_protocol::operations::request::{
    RequestInterruptInput, RequestInterruptReceipt, RequestRef,
};
use gents_store::config_client::ConfigAccess;

/// Interrupt exactly `request_id` within `node_did`'s documents over `access`.
pub async fn interrupt_request_on(
    access: &ConfigAccess,
    node_did: &str,
    request_id: &str,
) -> Result<RequestInterruptReceipt, String> {
    gents::operations::request::interrupt(
        access,
        RequestInterruptInput {
            request: RequestRef {
                node_did: node_did.to_owned(),
                requester_did: None,
                request_id: request_id.to_owned(),
            },
        },
    )
    .await
    .map_err(|error| gents_protocol::error_chain::ErrorChain(&error).to_string())
}
