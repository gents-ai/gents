use gents_protocol::serve_lifecycle::{
    control::{
        RuntimeControlCommand, RuntimeControlReceipt, RuntimeOperation, RUNTIME_CONTROL_PATH,
        RUNTIME_CONTROL_VERSION,
    },
    preparation::RuntimePreparationRefusal,
};
use gents_store::config_client::GraphqlEndpoint;

/// Bound the receipt read from a recorded runtime before decoding it.
const MAX_CONTROL_RECEIPT: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ControlFailure {
    #[error("{message}")]
    Refused {
        reason: RuntimePreparationRefusal,
        message: String,
    },
    #[error(transparent)]
    Authorization(Box<gents_store::config_client::ConfigClientError>),
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    #[error(transparent)]
    Decode(#[from] serde_json::Error),
    #[error("runtime control receipt is too large")]
    ReceiptTooLarge,
}

impl ControlFailure {
    pub fn reason_in(
        error: &(dyn std::error::Error + 'static),
    ) -> Option<RuntimePreparationRefusal> {
        gents_protocol::error_chain::find_map_source(error, |cause| {
            cause.downcast_ref::<Self>().and_then(Self::reason)
        })
    }

    pub fn reason(&self) -> Option<RuntimePreparationRefusal> {
        match self {
            Self::Refused { reason, .. } => Some(*reason),
            _ => None,
        }
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::Refused {
                reason: RuntimePreparationRefusal::Unsupported,
                ..
            }
        )
    }
}

pub fn refusal(reason: RuntimePreparationRefusal, message: impl Into<String>) -> ControlFailure {
    ControlFailure::Refused {
        reason,
        message: message.into(),
    }
}

pub async fn control(
    client: &reqwest::Client,
    endpoint: &reqwest::Url,
    node_did: &str,
    operation: RuntimeOperation,
    authorization: Option<&GraphqlEndpoint>,
) -> std::result::Result<Option<RuntimeControlReceipt>, ControlFailure> {
    let command = RuntimeControlCommand {
        version: RUNTIME_CONTROL_VERSION,
        node_did: node_did.to_owned(),
        nonce: uuid::Uuid::new_v4().to_string(),
        issued_at: chrono::Utc::now().to_rfc3339(),
        operation,
    };
    let mut url = endpoint.clone();
    url.set_path(RUNTIME_CONTROL_PATH);
    let mut request = client.post(url).json(&command);
    if let Some(authorization) = authorization {
        request = authorization
            .authorize(request)
            .map_err(|error| ControlFailure::Authorization(Box::new(error)))?;
    }
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error)
            if error.is_connect() && matches!(command.operation, RuntimeOperation::Observe) =>
        {
            return Ok(None)
        }
        Err(error) => {
            return Err(refusal(
                RuntimePreparationRefusal::Unavailable,
                format!("Runtime control did not complete. Retry to inspect its state: {error}"),
            ))
        }
    };
    if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
        && matches!(command.operation, RuntimeOperation::Observe)
    {
        return Ok(None);
    }
    if matches!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::NOT_IMPLEMENTED
    ) {
        return Err(refusal(RuntimePreparationRefusal::Unsupported, "The running runtime does not support authenticated updates. Stop it explicitly before preparing this package."));
    }
    if !response.status().is_success() {
        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(refusal(RuntimePreparationRefusal::Credentials, "The runtime did not authorize its recorded operator. Restore operator access before retrying."));
        }
        return Err(refusal(
            RuntimePreparationRefusal::Unavailable,
            format!(
                "Runtime control is unavailable (HTTP {}). Retry after the runtime is ready.",
                response.status().as_u16()
            ),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_CONTROL_RECEIPT {
            return Err(ControlFailure::ReceiptTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let receipt = serde_json::from_slice(&bytes)?;
    gents_store::service::verify_control_receipt(&command, &receipt).map_err(|error| {
        refusal(
            RuntimePreparationRefusal::InvalidHome,
            format!("The live runtime could not authenticate this node: {error:#}"),
        )
    })?;
    Ok(Some(receipt))
}
