use gents_protocol::operations::request::{
    MessageSendBody, MessageSendInput, MessageSendRoute, RequestInterruptInput,
    RequestInterruptReceipt, RequestOperationError, RequestReceipt,
};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

pub async fn send(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    input: MessageSendInput,
) -> Result<RequestReceipt, RequestOperationError> {
    let (route, body) = input.into_parts();
    send_with(access, signer, route, std::future::ready(Ok(body))).await
}

pub async fn send_with<M>(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    route: MessageSendRoute,
    message: M,
) -> Result<RequestReceipt, RequestOperationError>
where
    M: std::future::Future<Output = Result<MessageSendBody, RequestOperationError>>,
{
    let has_session = route
        .session_id
        .as_deref()
        .is_some_and(|session| !session.trim().is_empty());
    gents_session::operations::request::send_with(
        access,
        signer,
        route,
        |node_did, session_id| async move {
            let mut body = message.await?;
            if has_session && body.input.goal_assignment.is_none() {
                body.input = gents_automation::goal::prepare_client_goal_input(
                    access,
                    signer,
                    &node_did,
                    &session_id,
                    body.input,
                )
                .await
                .map_err(RequestOperationError::passthrough)?;
            }
            Ok(body)
        },
    )
    .await
}

pub async fn interrupt(
    access: &ConfigAccess,
    input: RequestInterruptInput,
) -> Result<RequestInterruptReceipt, RequestOperationError> {
    gents_session::operations::request::interrupt(access, input).await
}
