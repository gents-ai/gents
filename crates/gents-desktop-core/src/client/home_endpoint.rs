use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use std::sync::Arc;

use futures::future::BoxFuture;
use gents_store::config_client::{
    ClientSigningCapability, ConfigClientError, GraphqlEndpoint, ResolveAudience,
};

use crate::client::core::sync_state::ClientSyncStateOwner;
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

/// The User's endpoint at `url` of a saved home, for the audience its record
/// keeps; a request refused for its credential reads the home's public status
/// again (`Identity.HeldAudience.resend`).
pub fn user_on(
    url: &str,
    capability: &ClientSigningCapability,
    record: &PeerRecord,
) -> Result<GraphqlEndpoint, ConfigClientError> {
    Ok(
        user_at(url, capability, record.delegation_audience.as_ref())?.with_audience_resolver(
            Arc::new(HomeStatus {
                record: record.clone(),
                saved: None,
            }),
        ),
    )
}

/// The User's endpoint on a saved home's operator GraphQL (`user_on`).
pub fn user_for(
    record: &PeerRecord,
    capability: &ClientSigningCapability,
) -> Option<GraphqlEndpoint> {
    user_on(record.operator_graphql()?, capability, record).ok()
}

/// `user_on`, keeping what the status reports with the saved record.
pub(in crate::client) fn user_saved(
    url: &str,
    capability: &ClientSigningCapability,
    record: &PeerRecord,
    saved: &ClientSyncStateOwner,
) -> Result<GraphqlEndpoint, ConfigClientError> {
    Ok(
        user_at(url, capability, record.delegation_audience.as_ref())?.with_audience_resolver(
            Arc::new(HomeStatus {
                record: record.clone(),
                saved: Some(saved.clone()),
            }),
        ),
    )
}

struct HomeStatus {
    record: PeerRecord,
    saved: Option<ClientSyncStateOwner>,
}

impl ResolveAudience for HomeStatus {
    fn resolve(&self) -> BoxFuture<'_, Option<ReportedDelegationAudience>> {
        Box::pin(async move {
            let Some(saved) = &self.saved else {
                return crate::local_runtime::public_delegation_audience(&self.record)
                    .await
                    .ok()
                    .flatten();
            };
            let current = saved
                .records()
                .into_iter()
                .find(|record| record.peer_id == self.record.peer_id)?;
            let reported = crate::local_runtime::public_delegation_audience(&current)
                .await
                .ok()
                .flatten()?;
            if let Err(error) = saved.observe_delegation_audience(&current, &reported).await {
                tracing::warn!(
                    directory_peer_id = %current.peer_id,
                    error = %format!("{error:#}"),
                    "failed to keep the audience a home reports"
                );
            }
            Some(reported)
        })
    }
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

    fn presented_for(request: &wiremock::Request) -> Option<u64> {
        use base64::Engine as _;
        let bearer = request.headers.get("authorization")?.to_str().ok()?;
        let claims = bearer.strip_prefix("Bearer ")?.split('.').nth(1)?;
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims)
                .ok()?,
        )
        .ok()?;
        claims["aud"].as_str()?.strip_prefix("vera:")?.parse().ok()
    }

    #[tokio::test]
    async fn a_refused_request_learns_the_audience_and_keeps_it_with_the_record() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(|request: &wiremock::Request| {
                if request.headers.get("authorization").is_none() {
                    return ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "node_did": "did:key:home",
                        "delegation_audience": audience(),
                    }));
                }
                match presented_for(request) {
                    Some(7) => ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({ "node_did": "did:key:home" })),
                    Some(_) => ResponseTemplate::new(403),
                    None => ResponseTemplate::new(401)
                        .set_body_json(serde_json::json!({ "code": "delegationRequired" })),
                }
            })
            .mount(&server)
            .await;
        let user = tokio::task::spawn_blocking(|| {
            gents_store::test_support::test_identity("home-endpoint-learns")
        })
        .await
        .unwrap();
        let capability =
            gents_store::identity::NodeIdentity::client_signing_capability(&user).unwrap();
        let mut record = PeerRecord::new("Home", "iroh://peer", "did:key:home");
        record.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        let (_dir, saved) = crate::client::core::sync_state::ClientSyncStateOwner::for_test(
            vec![record.clone()],
            vec![],
        )
        .await;
        let endpoint = super::user_saved(
            record.graphql.as_deref().unwrap(),
            &capability,
            &record,
            &saved,
        )
        .unwrap();
        let status_url = format!("{}/status", server.uri());
        let client = reqwest::Client::new();
        let response = endpoint
            .send_resolving(|endpoint| endpoint.authorize(client.get(&status_url)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(endpoint.mints_delegations());
        assert_eq!(
            saved.records()[0].delegation_audience,
            Some(audience()),
            "the record keeps what the status reported"
        );
    }
}
