use gents_protocol::client_authentication::ClientAuthentication;
use gents_protocol::delegation_audience::{
    DelegationAudience, ReportedDelegationAudience, ReportedHome,
};
use gents_protocol::home_attestation::{HomeAttestation, HomeNonce, HomeStatusRefusal};

/// What a saved home's status reports once it is the home's
/// (`Identity.HeldAudience.reportedOfHome`). Only [`authenticate`] makes one,
/// and a client keeps an audience or clock only from one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedHome {
    authentication: ClientAuthentication,
    reported: ReportedHome,
}

impl AuthenticatedHome {
    pub fn authentication(&self) -> &ClientAuthentication {
        &self.authentication
    }

    pub fn reported(&self) -> &ReportedHome {
        &self.reported
    }

    /// The audience a client mints for at this home, named by its client
    /// authentication when its status names none
    /// (`Identity.HeldAudience.reconcile`): `None` when the home reports no
    /// audience a client can mint for yet, refused when the status contradicts
    /// itself.
    pub(super) fn resolved(mut self) -> Result<Option<Self>, HomeStatusRefusal> {
        match (&self.authentication, &self.reported.audience) {
            (ClientAuthentication::Unavailable, _)
            | (_, ReportedDelegationAudience::Unavailable) => return Ok(None),
            (
                ClientAuthentication::Vera {
                    worker, deployment, ..
                },
                ReportedDelegationAudience::Absent,
            ) => {
                self.reported.audience = ReportedDelegationAudience::Named(DelegationAudience {
                    worker: worker.clone(),
                    deployment: *deployment,
                    genesis_id: None,
                });
            }
            (ClientAuthentication::Local, ReportedDelegationAudience::Absent) => {}
            (
                ClientAuthentication::Vera {
                    worker, deployment, ..
                },
                ReportedDelegationAudience::Named(audience),
            ) if worker == &audience.worker && deployment == &audience.deployment => {}
            (ClientAuthentication::Vera { .. } | ClientAuthentication::Local, _) => {
                return Err(HomeStatusRefusal::Contradictory)
            }
        }
        Ok(Some(self))
    }
}

/// A nonce for one status read.
pub fn fresh_nonce() -> HomeNonce {
    HomeNonce::from_bytes(rand::random())
}

/// What `status`, answered to a read that sent `nonce`, reports for the
/// saved home whose node is `node`: only what that node's key signed for this
/// read (`Identity.HeldAudience.authentic`).
pub fn authenticate(
    status: &serde_json::Value,
    node: &str,
    nonce: &HomeNonce,
) -> Result<AuthenticatedHome, HomeStatusRefusal> {
    let statement = HomeAttestation::of_status(status)?.authenticate(
        node,
        nonce,
        |did, payload, signature| {
            crate::identity::verify_did_signature(did, payload, signature).unwrap_or(false)
        },
    )?;
    Ok(AuthenticatedHome {
        reported: statement.reported(),
        authentication: statement.client_authentication,
    })
}

#[cfg(test)]
mod conformance;

#[cfg(test)]
mod tests;
