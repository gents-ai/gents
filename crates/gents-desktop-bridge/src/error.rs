use gents_store::client_error::{BridgeError, BridgeErrorCode};

pub fn control_send_error(
    error: gents_toolcalls::call::control::client::send::ControlSendError,
) -> BridgeError {
    use gents_toolcalls::call::control::client::send::ControlSendError;
    let code = match &error {
        ControlSendError::NotAuthorized
        | ControlSendError::PermissionDenied
        | ControlSendError::Rejected { .. } => BridgeErrorCode::PermissionDenied,
        ControlSendError::NotFound => BridgeErrorCode::NotFound,
        ControlSendError::RuntimeRequired => BridgeErrorCode::Unsupported,
        ControlSendError::Unavailable(_) | ControlSendError::Unconfirmed { .. } => {
            BridgeErrorCode::Unknown
        }
    };
    BridgeError::new(
        code,
        gents_protocol::error_chain::ErrorChain(&error).to_string(),
    )
}
