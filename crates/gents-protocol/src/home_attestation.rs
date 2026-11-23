use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client_authentication::ClientAuthentication;
use crate::delegation_audience::{
    DelegationAudience, ReportedDelegationAudience, ReportedHome, STATUS_NODE_DID_FIELD,
    STATUS_PROVIDER_TIME_FIELD,
};

/// The query parameter a client sends its nonce in when it reads its home's
/// status to keep what it reports (`Identity.HeldAudience.authentic`).
pub const HOME_NONCE_QUERY: &str = "home_nonce";

pub const STATUS_HOME_ATTESTATION_FIELD: &str = "home_attestation";

pub const HOME_ATTESTATION_VERSION: u8 = 1;

const HOME_STATUS_SIGNATURE_DOMAIN: &str = "gents-home-status-signature-v1";

const HOME_NONCE_BYTES: usize = 32;

/// The nonce a client sends with one status read: 32 bytes, unpadded base64url.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeNonce(String);

impl HomeNonce {
    pub fn from_bytes(bytes: [u8; HOME_NONCE_BYTES]) -> Self {
        Self(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn parse(text: &str) -> Result<Self, HomeStatusRefusal> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| HomeStatusRefusal::Malformed)?;
        let bytes: [u8; HOME_NONCE_BYTES] =
            bytes.try_into().map_err(|_| HomeStatusRefusal::Malformed)?;
        let nonce = Self::from_bytes(bytes);
        if nonce.0 == text {
            Ok(nonce)
        } else {
            Err(HomeStatusRefusal::Malformed)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a home's status says of its audience, as the home signs it
/// (`Identity.HeldAudience.Reading`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttestedAudience {
    Absent,
    Named { audience: DelegationAudience },
    Unavailable,
}

impl From<ReportedDelegationAudience> for AttestedAudience {
    fn from(reported: ReportedDelegationAudience) -> Self {
        match reported {
            ReportedDelegationAudience::Absent => Self::Absent,
            ReportedDelegationAudience::Named(audience) => Self::Named { audience },
            ReportedDelegationAudience::Unavailable => Self::Unavailable,
        }
    }
}

impl From<AttestedAudience> for ReportedDelegationAudience {
    fn from(attested: AttestedAudience) -> Self {
        match attested {
            AttestedAudience::Absent => Self::Absent,
            AttestedAudience::Named { audience } => Self::Named(audience),
            AttestedAudience::Unavailable => Self::Unavailable,
        }
    }
}

/// What a home signs of a status read (`Identity.HeldAudience.Statement`):
/// the nonce its client sent, the node, how clients authenticate to it, its
/// delegation audience and the provider's time its clients mint at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeStatement {
    pub version: u8,
    pub nonce: String,
    pub node_did: String,
    pub client_authentication: ClientAuthentication,
    pub delegation_audience: AttestedAudience,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_time: Option<u64>,
}

impl HomeStatement {
    /// What `status` says, as its home signs it for the read that sent
    /// `nonce`.
    pub fn of_status(status: &Value, nonce: &HomeNonce) -> Self {
        Self {
            version: HOME_ATTESTATION_VERSION,
            nonce: nonce.as_str().to_owned(),
            node_did: status
                .get(STATUS_NODE_DID_FIELD)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            client_authentication: status
                .get(crate::client_authentication::STATUS_FIELD)
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or(ClientAuthentication::Unavailable),
            delegation_audience: ReportedDelegationAudience::observe(status).into(),
            provider_time: status
                .get(STATUS_PROVIDER_TIME_FIELD)
                .and_then(Value::as_u64),
        }
    }

    /// The bytes the home's node key signs: the home status domain first, so
    /// no other signature of that key is taken for one.
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        let (authentication, worker, deployment) = match &self.client_authentication {
            ClientAuthentication::Local => ("local", "", String::new()),
            ClientAuthentication::Vera {
                worker,
                deployment,
                shared_household,
            } => (
                if *shared_household {
                    "vera-shared"
                } else {
                    "vera"
                },
                worker.as_str(),
                deployment.to_string(),
            ),
            ClientAuthentication::Unavailable => ("unavailable", "", String::new()),
        };
        let (reading, audience_worker, audience_deployment, genesis) =
            match &self.delegation_audience {
                AttestedAudience::Absent => ("absent", "", String::new(), None),
                AttestedAudience::Named { audience } => (
                    "named",
                    audience.worker.as_str(),
                    audience.deployment.to_string(),
                    audience.genesis_id.as_deref(),
                ),
                AttestedAudience::Unavailable => ("unavailable", "", String::new(), None),
            };
        let provider_time = self.provider_time.map(|time| time.to_string());
        crate::enrollment::canonical_domain_payload(
            HOME_STATUS_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.nonce,
                &self.node_did,
                authentication,
                worker,
                &deployment,
                reading,
                audience_worker,
                &audience_deployment,
                if genesis.is_some() { "1" } else { "0" },
                genesis.unwrap_or_default(),
                if provider_time.is_some() { "1" } else { "0" },
                provider_time.as_deref().unwrap_or_default(),
            ],
        )
    }

    pub fn reported(&self) -> ReportedHome {
        ReportedHome {
            audience: self.delegation_audience.clone().into(),
            provider_time: self.provider_time,
        }
    }
}

/// Why a home answered a status read carrying a nonce without signing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum UnsignedHomeStatus {
    #[error("its runtime has not started yet")]
    RuntimeNotReady,
    #[error("it holds no node key it can sign with")]
    NodeKeyUnavailable,
}

/// What a home answers a status read carrying a nonce with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum HomeAttestation {
    Signed {
        statement: HomeStatement,
        /// Unpadded base64url.
        signature: String,
    },
    Unsigned {
        reason: UnsignedHomeStatus,
    },
}

impl HomeAttestation {
    pub fn signed(statement: HomeStatement, signature: &[u8]) -> Self {
        Self::Signed {
            statement,
            signature: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature),
        }
    }

    pub fn of_status(status: &Value) -> Result<Self, HomeStatusRefusal> {
        let attestation = status
            .get(STATUS_HOME_ATTESTATION_FIELD)
            .ok_or(HomeStatusRefusal::NotSigned)?;
        serde_json::from_value(attestation.clone()).map_err(|_| HomeStatusRefusal::Malformed)
    }

    /// The statement, when it is the home's whose node is `node`, signed for
    /// the read that sent `nonce` (`Identity.HeldAudience.authentic`);
    /// `verify` says whether a signature verifies under a node's key.
    pub fn authenticate<V>(
        self,
        node: &str,
        nonce: &HomeNonce,
        verify: V,
    ) -> Result<HomeStatement, HomeStatusRefusal>
    where
        V: FnOnce(&str, &[u8], &[u8]) -> bool,
    {
        let (statement, signature) = match self {
            Self::Signed {
                statement,
                signature,
            } => (statement, signature),
            Self::Unsigned { reason } => return Err(HomeStatusRefusal::Unsigned(reason)),
        };
        if statement.version != HOME_ATTESTATION_VERSION {
            return Err(HomeStatusRefusal::OtherVersion(statement.version));
        }
        if statement.node_did != node {
            return Err(HomeStatusRefusal::OtherNode);
        }
        if statement.nonce != nonce.as_str() {
            return Err(HomeStatusRefusal::OtherNonce);
        }
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| HomeStatusRefusal::Malformed)?;
        if verify(node, &statement.signing_payload(), &signature) {
            Ok(statement)
        } else {
            Err(HomeStatusRefusal::BadSignature)
        }
    }
}

/// Why a status is not the saved home's (`Identity.HeldAudience.authentic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HomeStatusRefusal {
    #[error("the home signs no status; update Gents on the home")]
    NotSigned,
    #[error("the home did not sign its status: {0}")]
    Unsigned(UnsignedHomeStatus),
    #[error("the home's status signature is malformed")]
    Malformed,
    #[error("the home's status names another node than the saved home")]
    OtherNode,
    #[error("the home's status was signed for another read")]
    OtherNonce,
    #[error("the home's status is signed under attestation version {0}; update Gents on this device or on the home")]
    OtherVersion(u8),
    #[error("the home's status is not signed by the saved home's node key")]
    BadSignature,
    /// Its client authentication and its audience name different homes
    /// (`Identity.HeldAudience.reconcile`).
    #[error("the home's signed status contradicts itself; update Gents on the home")]
    Contradictory,
}

#[cfg(test)]
mod household_tests {
    use super::*;

    #[test]
    fn shared_household_selection_is_signed() {
        let nonce = HomeNonce::from_bytes([7; 32]);
        let mut status = serde_json::json!({
            "node_did": "did:key:device",
            "client_authentication": {"kind": "vera", "worker": "worker", "deployment": 9},
        });
        let original = HomeStatement::of_status(&status, &nonce);
        status["client_authentication"]["shared_household"] = false.into();
        assert_eq!(HomeStatement::of_status(&status, &nonce), original);
        assert!(serde_json::to_value(&original.client_authentication)
            .unwrap()
            .get("shared_household")
            .is_none());
        status["client_authentication"]["shared_household"] = true.into();
        assert_ne!(
            HomeStatement::of_status(&status, &nonce).signing_payload(),
            original.signing_payload()
        );
    }
}
