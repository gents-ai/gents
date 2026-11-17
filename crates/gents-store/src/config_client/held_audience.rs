use std::sync::{Arc, RwLock};

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::{
    resend, CredentialAnswer, CredentialRefusal, DelegationAudience, HeldAudienceChange,
    PresentedCredential, ReportedDelegationAudience,
};

use super::{ConfigClientError, GraphqlEndpoint};

/// Reads what an endpoint's home reports of its delegation audience now, and
/// keeps it wherever its client keeps it (`Identity.HeldAudience`).
pub trait ResolveAudience: Send + Sync {
    fn resolve(&self) -> BoxFuture<'_, Option<ReportedDelegationAudience>>;
}

/// The audience an endpoint mints for and how it reads it again, shared by
/// its clones, so a refusal one of them recovers from is recovered for all.
#[derive(Clone, Default)]
pub(super) struct HeldAudienceCell(Arc<Held>);

#[derive(Default)]
struct Held {
    audience: RwLock<Option<DelegationAudience>>,
    resolver: Option<Arc<dyn ResolveAudience>>,
}

impl HeldAudienceCell {
    pub(super) fn new(audience: Option<DelegationAudience>) -> Self {
        Self(Arc::new(Held {
            audience: RwLock::new(audience),
            resolver: None,
        }))
    }

    pub(super) fn get(&self) -> Option<DelegationAudience> {
        self.0
            .audience
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn set(&self, audience: Option<DelegationAudience>) {
        *self
            .0
            .audience
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = audience;
    }

    fn resolving(&self, resolver: Arc<dyn ResolveAudience>) -> Self {
        Self(Arc::new(Held {
            audience: RwLock::new(self.get()),
            resolver: Some(resolver),
        }))
    }
}

impl PartialEq for HeldAudienceCell {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
            && match (&self.0.resolver, &other.0.resolver) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

impl Eq for HeldAudienceCell {}

impl std::fmt::Debug for HeldAudienceCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldAudienceCell")
            .field("audience", &self.get())
            .field("resolves", &self.0.resolver.is_some())
            .finish()
    }
}

impl GraphqlEndpoint {
    /// Read the home's status again when a request is refused for its
    /// credential (`Identity.HeldAudience.resend`).
    pub fn with_audience_resolver(mut self, resolver: Arc<dyn ResolveAudience>) -> Self {
        self.delegation_audience = self.delegation_audience.resolving(resolver);
        self
    }

    fn presented_credential(&self) -> PresentedCredential {
        if self.delegated_authorization.is_some()
            || self.principal_did.is_none()
            || self.client_signing_capability.is_none()
        {
            PresentedCredential::None
        } else if self.mints_delegations() {
            PresentedCredential::Delegation
        } else {
            PresentedCredential::IdentityToken
        }
    }

    /// Read the home's status again after its node answered a request this
    /// endpoint sent with `status`, and keep the audience it reports. `None`
    /// when the answer refuses no credential this endpoint presented
    /// (`Identity.HeldAudience.refusalOf`) or no status could be read.
    pub(super) async fn reread_after_refusal(
        &self,
        status: reqwest::StatusCode,
    ) -> Option<(CredentialRefusal, HeldAudienceChange)> {
        let refusal = CredentialRefusal::of(
            CredentialAnswer::of_status(status.as_u16()),
            self.presented_credential(),
        );
        if refusal == CredentialRefusal::Other {
            return None;
        }
        let resolver = self.delegation_audience.0.resolver.clone()?;
        // A task of its own keeps a caller's future `Sync`, as a tool's
        // definition future must be, whatever future the resolver returns.
        let reported = tokio::spawn(async move { resolver.resolve().await })
            .await
            .ok()??;
        let held = self.delegation_audience.get();
        let change = reported.change_for(held.as_ref());
        self.delegation_audience
            .set(change.clone().held_after(held));
        Some((refusal, change))
    }

    /// Whether a request this endpoint sent and its node answered with
    /// `status` is to be sent once more, after the home's status was read
    /// again and the audience it reports kept.
    pub async fn recovers_refusal(&self, status: reqwest::StatusCode) -> bool {
        self.reread_after_refusal(status)
            .await
            .is_some_and(|(refusal, change)| resend(false, refusal, &change))
    }

    /// Send the request `build` makes, and once more when its refusal is
    /// recovered (`recovers_refusal`).
    pub async fn send_resolving<F>(
        &self,
        build: F,
    ) -> Result<Result<reqwest::Response, reqwest::Error>, ConfigClientError>
    where
        F: Fn(&Self) -> Result<reqwest::RequestBuilder, ConfigClientError>,
    {
        let response = build(self)?.send().await;
        match &response {
            Ok(answered) if self.recovers_refusal(answered.status()).await => {
                Ok(build(self)?.send().await)
            }
            _ => Ok(response),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use base64::Engine as _;
    use futures::future::BoxFuture;
    use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
    use std::sync::Mutex;

    use axum::http::{HeaderMap, StatusCode};

    use super::ResolveAudience;
    use crate::config_client::GraphqlEndpoint;

    struct Status {
        reports: ReportedDelegationAudience,
        reads: AtomicUsize,
    }

    impl ResolveAudience for Status {
        fn resolve(&self) -> BoxFuture<'_, Option<ReportedDelegationAudience>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Some(self.reports.clone()) })
        }
    }

    fn audience(deployment: u64) -> DelegationAudience {
        DelegationAudience {
            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
            deployment,
        }
    }

    fn presented_for(headers: &HeaderMap) -> Option<u64> {
        let bearer = headers.get("authorization")?.to_str().ok()?;
        let token = bearer.strip_prefix("Bearer ")?;
        let claims = token.split('.').nth(1)?;
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims)
                .ok()?,
        )
        .ok()?;
        claims["aud"].as_str()?.strip_prefix("vera:")?.parse().ok()
    }

    /// A node that takes only a delegation for its deployment: 401 to an
    /// identity token, 403 to a delegation for another deployment.
    struct Node {
        url: String,
        presented: Arc<Mutex<Vec<Option<u64>>>>,
    }

    impl Node {
        fn presented(&self) -> Vec<Option<u64>> {
            self.presented.lock().unwrap().clone()
        }
    }

    async fn node_taking(deployment: u64) -> Node {
        let presented = Arc::new(Mutex::new(Vec::new()));
        let seen = presented.clone();
        let router = axum::Router::new().fallback(move |headers: HeaderMap| {
            let seen = seen.clone();
            async move {
                let audience = presented_for(&headers);
                seen.lock().unwrap().push(audience);
                match audience {
                    Some(presented) if presented == deployment => (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"data": {"Node": []}})),
                    ),
                    Some(_) => (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({}))),
                    None => (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({}))),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        Node {
            url: format!("http://{address}/api/v0/graphql"),
            presented,
        }
    }

    async fn endpoint(
        server: &Node,
        held: ReportedDelegationAudience,
        status: &Arc<Status>,
    ) -> GraphqlEndpoint {
        let user = tokio::task::spawn_blocking(|| crate::test_support::test_identity("held-user"))
            .await
            .unwrap();
        let capability = crate::identity::NodeIdentity::client_signing_capability(&user).unwrap();
        GraphqlEndpoint::as_principal(&server.url, capability.did())
            .with_client_signing_capability(capability)
            .unwrap()
            .with_reported_delegation_audience(held)
            .unwrap()
            .with_audience_resolver(status.clone())
    }

    fn status(reports: ReportedDelegationAudience) -> Arc<Status> {
        Arc::new(Status {
            reports,
            reads: AtomicUsize::new(0),
        })
    }

    async fn query(endpoint: &GraphqlEndpoint) -> bool {
        super::super::graphql::query_with_options(
            endpoint,
            "query { Node { _docID } }",
            None,
            gents_protocol::graphql::GraphqlRequestOptions {
                max_attempts: 1,
                ..Default::default()
            },
        )
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn a_client_that_knew_no_audience_learns_it_from_the_refusal() {
        let server = node_taking(1).await;
        let status = status(ReportedDelegationAudience::Named(audience(1)));
        let endpoint = endpoint(&server, ReportedDelegationAudience::Absent, &status).await;
        assert!(query(&endpoint).await);
        assert_eq!(server.presented().len(), 2);
        assert!(endpoint.clone().mints_delegations(), "its clones keep it");
    }

    #[tokio::test]
    async fn a_client_recovers_from_a_restart_under_another_audience() {
        let server = node_taking(2).await;
        let status = status(ReportedDelegationAudience::Named(audience(2)));
        let endpoint = endpoint(
            &server,
            ReportedDelegationAudience::Named(audience(1)),
            &status,
        )
        .await;
        assert!(query(&endpoint).await);
        assert_eq!(server.presented(), vec![Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn a_refusal_the_status_does_not_change_is_sent_once() {
        for (node, held, reports) in [
            (
                2,
                ReportedDelegationAudience::Absent,
                ReportedDelegationAudience::Absent,
            ),
            (
                2,
                ReportedDelegationAudience::Named(audience(1)),
                ReportedDelegationAudience::Named(audience(1)),
            ),
            (
                2,
                ReportedDelegationAudience::Absent,
                ReportedDelegationAudience::Unavailable,
            ),
        ] {
            let server = node_taking(node).await;
            let status = status(reports);
            let endpoint = endpoint(&server, held, &status).await;
            assert!(!query(&endpoint).await);
            assert_eq!(server.presented().len(), 1);
            assert_eq!(status.reads.load(Ordering::SeqCst), 1);
        }
    }

    /// A node whose transactions outlive no restart: it opens one for any
    /// caller and refuses a statement inside it that presents a delegation
    /// for another deployment, as the runtime that replaced it would.
    async fn transacting_node(deployment: u64) -> Node {
        let presented = Arc::new(Mutex::new(Vec::new()));
        let seen = presented.clone();
        let router = axum::Router::new().fallback(
            move |method: axum::http::Method, uri: axum::http::Uri, headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    let ok = |body| (StatusCode::OK, axum::Json(body));
                    if uri.path().ends_with("/tx") {
                        return ok(serde_json::json!({"id": 7}));
                    }
                    if uri.path().contains("/tx/") || method == axum::http::Method::DELETE {
                        return ok(serde_json::json!({}));
                    }
                    let audience = presented_for(&headers);
                    seen.lock().unwrap().push(audience);
                    if audience == Some(deployment) {
                        ok(serde_json::json!({"data": {"Node": []}}))
                    } else {
                        (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({})))
                    }
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        Node {
            url: format!("http://{address}/api/v0/graphql"),
            presented,
        }
    }

    async fn transact(endpoint: GraphqlEndpoint) -> (bool, usize) {
        let runs = AtomicUsize::new(0);
        let access = crate::config_client::ConfigAccess::Graphql(endpoint);
        let committed = access
            .transact_readonly("held_audience.unit", |txn| {
                runs.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    txn.execute("query { Node { _docID } }").await?;
                    Ok::<_, crate::config_client::ConfigClientError>(())
                })
            })
            .await
            .is_ok();
        (committed, runs.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_transaction_refused_after_a_restart_runs_again_whole_once() {
        let server = transacting_node(2).await;
        let status = status(ReportedDelegationAudience::Named(audience(2)));
        let endpoint = endpoint(
            &server,
            ReportedDelegationAudience::Named(audience(1)),
            &status,
        )
        .await;
        assert_eq!(transact(endpoint).await, (true, 2));
        assert_eq!(server.presented(), vec![Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn a_transaction_whose_refusal_the_status_does_not_change_runs_once() {
        for reports in [
            ReportedDelegationAudience::Named(audience(1)),
            ReportedDelegationAudience::Absent,
            ReportedDelegationAudience::Unavailable,
        ] {
            let server = transacting_node(2).await;
            let status = status(reports);
            let endpoint = endpoint(
                &server,
                ReportedDelegationAudience::Named(audience(1)),
                &status,
            )
            .await;
            assert_eq!(transact(endpoint).await, (false, 1));
            assert_eq!(status.reads.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn a_transaction_refused_again_is_run_only_twice() {
        let server = transacting_node(3).await;
        let status = status(ReportedDelegationAudience::Named(audience(2)));
        let endpoint = endpoint(
            &server,
            ReportedDelegationAudience::Named(audience(1)),
            &status,
        )
        .await;
        assert_eq!(transact(endpoint).await, (false, 2));
    }

    async fn edge_answering(body: serde_json::Value) -> String {
        let router = axum::Router::new().fallback(move || {
            let body = body.clone();
            async move { (StatusCode::UNAUTHORIZED, axum::Json(body)) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        format!("http://{address}/api/v0/graphql")
    }

    #[tokio::test]
    async fn an_identity_token_the_edge_refuses_is_told_as_an_update_of_gents() {
        for (body, update) in [
            (
                serde_json::json!({
                    "code": gents_protocol::delegation_audience::DELEGATION_REQUIRED_CODE,
                    "error": "refused",
                }),
                true,
            ),
            (serde_json::json!({ "error": "refused" }), false),
        ] {
            let server = Node {
                url: edge_answering(body).await,
                presented: Arc::default(),
            };
            let status = status(ReportedDelegationAudience::Absent);
            let endpoint = endpoint(&server, ReportedDelegationAudience::Absent, &status).await;
            let told = crate::config_client::post_graphql(&endpoint, "query { Node { _docID } }")
                .await
                .unwrap_err()
                .to_string();
            assert_eq!(
                told == crate::local::document_acp::DocumentAcpError::DelegationRequired
                    .to_string(),
                update,
                "{told}"
            );
            assert_eq!(told.contains("Next:"), !update, "{told}");
        }
    }

    #[tokio::test]
    async fn a_second_refusal_is_told() {
        let server = node_taking(3).await;
        let status = status(ReportedDelegationAudience::Named(audience(2)));
        let endpoint = endpoint(
            &server,
            ReportedDelegationAudience::Named(audience(1)),
            &status,
        )
        .await;
        assert!(!query(&endpoint).await);
        assert_eq!(server.presented().len(), 2);
    }
}
