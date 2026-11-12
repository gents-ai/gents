use gents_protocol::operations::request::{
    MessageSendInput, RequestInterruptInput, RequestInterruptReceipt, RequestOperationError,
    RequestReceipt,
};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

pub async fn send(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    mut input: MessageSendInput,
) -> Result<RequestReceipt, RequestOperationError> {
    let session_id = input
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(session_id) = session_id {
        if input.input.goal_assignment.is_none() {
            input.input = gents_automation::goal::prepare_client_goal_input(
                access,
                signer,
                input.node_did.trim(),
                &session_id,
                std::mem::take(&mut input.input),
            )
            .await
            .map_err(RequestOperationError::passthrough)?;
        }
    }
    gents_session::operations::request::send(access, signer, input).await
}

pub async fn interrupt(
    access: &ConfigAccess,
    input: RequestInterruptInput,
) -> Result<RequestInterruptReceipt, RequestOperationError> {
    gents_session::operations::request::interrupt(access, input).await
}
