use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_store::config_client::{ClientSigningCapability, ConfigClientError, GraphqlEndpoint};

use crate::client::PeerRecord;

fn held(audience: Option<&DelegationAudience>) -> ReportedDelegationAudience {
    audience.map_or(ReportedDelegationAudience::Absent, |audience| {
        ReportedDelegationAudience::Named(audience.clone())
    })
}

/// The User's endpoint at `url`: it presents a delegation for the audience
/// its home reports and the identity token when the home reports none
/// (`Identity.HeldAudience.recordCredential`).
pub fn user_at(
    url: &str,
    capability: &ClientSigningCapability,
    audience: Option<&DelegationAudience>,
) -> Result<GraphqlEndpoint, ConfigClientError> {
    GraphqlEndpoint::as_principal(url, capability.did())
        .with_client_signing_capability(capability.clone())?
        .with_reported_delegation_audience(held(audience))
}

/// The User's endpoint on a saved home, for the audience its record keeps.
pub fn user_for(
    record: &PeerRecord,
    capability: &ClientSigningCapability,
) -> Option<GraphqlEndpoint> {
    user_at(
        record.operator_graphql()?,
        capability,
        record.delegation_audience.as_ref(),
    )
    .ok()
}

/// The User's endpoint at `url` of a home with no saved record, for the
/// audience its public status reports now.
pub async fn user_resolving(
    client: &reqwest::Client,
    url: &str,
    capability: &ClientSigningCapability,
) -> anyhow::Result<GraphqlEndpoint> {
    let status_url = crate::local_runtime::runtime_status_url(url)?;
    let reported = match client.get(&status_url).send().await {
        Ok(response) if response.status().is_success() => response
            .json::<serde_json::Value>()
            .await
            .map_or(ReportedDelegationAudience::Absent, |status| {
                ReportedDelegationAudience::observe(&status)
            }),
        _ => ReportedDelegationAudience::Absent,
    };
    Ok(GraphqlEndpoint::as_principal(url, capability.did())
        .with_client_signing_capability(capability.clone())?
        .with_reported_delegation_audience(reported)?)
}

/// The co-hosted runtime's own Node endpoint. The edge of a Vera home admits
/// the Node's identity token unchanged, and a client mints delegations only
/// for its User (`Identity.UserDelegation.edge`).
pub fn node_at(
    url: &str,
    node_did: &str,
    capability: ClientSigningCapability,
) -> Result<GraphqlEndpoint, ConfigClientError> {
    GraphqlEndpoint::as_principal(url, node_did).with_client_signing_capability(capability)
}

pub fn anonymous_at(url: &str) -> GraphqlEndpoint {
    GraphqlEndpoint::anonymous(url)
}

#[cfg(test)]
mod tests {
    use gents_protocol::delegation_audience::DelegationAudience;

    use super::{user_at, user_for};
    use crate::client::PeerRecord;

    fn audience() -> DelegationAudience {
        DelegationAudience {
            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
            deployment: 7,
        }
    }

    #[tokio::test]
    async fn a_home_that_reports_no_audience_keeps_the_identity_token() {
        let user = tokio::task::spawn_blocking(|| {
            gents_store::test_support::test_identity("home-endpoint-user")
        })
        .await
        .unwrap();
        let capability =
            gents_store::identity::NodeIdentity::client_signing_capability(&user).unwrap();
        let mut record = PeerRecord::new("Home", "iroh://peer", "did:key:home");
        record.graphql = Some("http://127.0.0.1:1/api/v0/graphql".into());

        let local = user_for(&record, &capability).unwrap();
        assert!(!local.mints_delegations());
        assert!(local.authorization().unwrap().is_some());

        record.delegation_audience = Some(audience());
        let vera = user_for(&record, &capability).unwrap();
        assert!(vera.mints_delegations());
        assert_ne!(
            vera.authorization().unwrap(),
            local.authorization().unwrap()
        );

        assert!(
            !user_at(record.graphql.as_deref().unwrap(), &capability, None)
                .unwrap()
                .mints_delegations()
        );
    }
}
