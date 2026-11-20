use gents_protocol::home_attestation::{
    HomeAttestation, HomeNonce, HomeStatement, UnsignedHomeStatus, STATUS_HOME_ATTESTATION_FIELD,
};

use super::NodeIdentity;

pub async fn attest(
    identity: Option<&dyn NodeIdentity>,
    nonce: &HomeNonce,
    status: &mut serde_json::Value,
) {
    let attestation = match identity {
        None => HomeAttestation::Unsigned {
            reason: UnsignedHomeStatus::RuntimeNotReady,
        },
        Some(identity) => {
            let statement = HomeStatement::of_status(status, nonce);
            match identity.sign(&statement.signing_payload()).await {
                Ok(signature) => HomeAttestation::signed(statement, &signature),
                Err(error) => {
                    tracing::warn!(error = %error, "the node key could not sign the home's status");
                    HomeAttestation::Unsigned {
                        reason: UnsignedHomeStatus::NodeKeyUnavailable,
                    }
                }
            }
        }
    };
    if let Some(map) = status.as_object_mut() {
        map.insert(
            STATUS_HOME_ATTESTATION_FIELD.to_owned(),
            serde_json::json!(attestation),
        );
    }
}
