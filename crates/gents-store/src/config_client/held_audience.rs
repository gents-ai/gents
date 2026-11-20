use std::sync::{Arc, RwLock};

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::{
    recovery, CredentialAnswer, CredentialRefusal, DelegationAudience, HeldAudienceChange,
    HomeClock, PresentedCredential, Recovery,
};
use gents_protocol::home_attestation::{HomeNonce, HomeStatusRefusal};

use super::home_status::{self, AuthenticatedHome};
use super::runtime_endpoints::Home;
use super::{ConfigClientError, GraphqlEndpoint, RequestBoundAudience};

/// Reads an endpoint's home's status now, answered to a read carrying
/// `nonce` (`home_attestation::HOME_NONCE_QUERY`); the endpoint keeps what it
/// reports only when the home's node key signed it for that read
/// (`Identity.HeldAudience.reportedOfHome`).
pub trait ResolveAudience: Send + Sync {
    fn resolve<'a>(&'a self, nonce: &'a HomeNonce) -> BoxFuture<'a, Option<serde_json::Value>>;
}

type StatusRead = std::pin::Pin<Box<dyn std::future::Future<Output = Option<Home>> + Send + Sync>>;

/// The device clock a client mints on, in Unix seconds.
#[derive(Clone, Default)]
pub(super) struct DeviceClock(Option<Arc<dyn Fn() -> u64 + Send + Sync>>);

impl DeviceClock {
    pub(super) fn now(&self) -> u64 {
        match &self.0 {
            Some(clock) => clock(),
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        }
    }
}

/// The audience an endpoint mints for and how it reads it again, shared by
/// its clones, so a refusal one of them recovers from is recovered for all.
#[derive(Clone, Default)]
pub(super) struct HeldAudienceCell(Arc<Held>);

#[derive(Default)]
struct Held {
    audience: RwLock<Option<DelegationAudience>>,
    clock: RwLock<Option<HomeClock>>,
    /// Held from a status read's nonce until what it read is kept, so a
    /// read that started earlier never replaces one that started later
    /// (TLA+ `HeldAudienceReads`).
    reads: tokio::sync::Mutex<()>,
    resolver: Option<(String, Arc<dyn ResolveAudience>)>,
    device: DeviceClock,
}

/// How long a status read may hold the cell's status lock; an unanswered
/// read keeps nothing.
const STATUS_READ_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

impl HeldAudienceCell {
    pub(super) fn new(audience: Option<DelegationAudience>) -> Self {
        Self(Arc::new(Held {
            audience: RwLock::new(audience),
            ..Held::default()
        }))
    }

    /// What the endpoint mints a delegation at now: its home's clock as it
    /// read it, and its device's clock.
    pub(super) fn minting_clock(&self) -> (Option<HomeClock>, u64) {
        (
            *self
                .0
                .clock
                .read()
                .unwrap_or_else(|poison| poison.into_inner()),
            self.0.device.now(),
        )
    }

    pub(super) async fn reading(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.reads.lock().await
    }

    fn set_clock(&self, clock: Option<HomeClock>) {
        *self
            .0
            .clock
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = clock;
    }

    /// Keep what the home's own status says, read now, and say what it
    /// changed (`Identity.HeldAudience.observe`).
    pub(super) fn observe(&self, home: &AuthenticatedHome) -> HeldAudienceChange {
        let reported = home.reported();
        let held = self.get();
        let (held_clock, now) = self.minting_clock();
        let read = reported.provider_time.map(|provider_at_read| HomeClock {
            provider_at_read,
            clock_at_read: now,
        });
        let moved =
            gents_protocol::delegation_audience::clock_moved(held_clock.as_ref(), read.as_ref());
        let change = reported.audience.change_with_clock(held.as_ref(), moved);
        if matches!(
            change,
            HeldAudienceChange::Learned(_)
                | HeldAudienceChange::Changed(_)
                | HeldAudienceChange::Reclocked
        ) {
            self.set_clock(read);
        } else if change == HeldAudienceChange::Forgotten {
            self.set_clock(None);
        }
        self.set(change.clone().held_after(held));
        change
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

    fn resolving(&self, node: String, resolver: Arc<dyn ResolveAudience>) -> Self {
        let (clock, _) = self.minting_clock();
        Self(Arc::new(Held {
            audience: RwLock::new(self.get()),
            clock: RwLock::new(clock),
            reads: tokio::sync::Mutex::new(()),
            resolver: Some((node, resolver)),
            device: self.0.device.clone(),
        }))
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn on_device_clock(&self, device: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        let (clock, _) = self.minting_clock();
        Self(Arc::new(Held {
            audience: RwLock::new(self.get()),
            clock: RwLock::new(clock),
            reads: tokio::sync::Mutex::new(()),
            resolver: self.0.resolver.clone(),
            device: DeviceClock(Some(device)),
        }))
    }
}

impl PartialEq for HeldAudienceCell {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
            && match (&self.0.resolver, &other.0.resolver) {
                (None, None) => true,
                (Some((left_node, left)), Some((right_node, right))) => {
                    left_node == right_node && Arc::ptr_eq(left, right)
                }
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
    /// Read the status of the home whose node is `node` through `resolver`
    /// when a request is refused for its credential
    /// (`Identity.HeldAudience.resend`).
    pub fn with_audience_resolver(
        mut self,
        node: impl Into<String>,
        resolver: Arc<dyn ResolveAudience>,
    ) -> Self {
        self.delegation_audience = self.delegation_audience.resolving(node.into(), resolver);
        self
    }

    /// Mint on `device`, in Unix seconds, in place of this device's clock.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    pub fn with_device_clock_for_test(
        mut self,
        device: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        self.delegation_audience = self.delegation_audience.on_device_clock(device);
        self
    }

    /// The read is boxed so the futures awaiting it carry no nesting of
    /// what it reports.
    fn read_status(&self) -> StatusRead {
        // A task of its own keeps a caller's future `Sync`, as a tool's
        // definition future must be, whatever future the read returns.
        let mut read = match self.delegation_audience.0.resolver.clone() {
            Some((node, resolver)) => tokio::spawn(async move {
                let nonce = home_status::fresh_nonce();
                let status = resolver.resolve(&nonce).await?;
                match home_status::authenticate(&status, &node, &nonce)
                    .and_then(|home| home.resolved())
                {
                    Ok(home) => home.map(Ok),
                    Err(refusal) => Some(Err(refusal)),
                }
            }),
            None => {
                let endpoint = self.clone();
                tokio::spawn(async move { endpoint.fetch_runtime_home().await })
            }
        };
        Box::pin(async move {
            match tokio::time::timeout(STATUS_READ_LIMIT, &mut read).await {
                Ok(answered) => answered.ok().flatten(),
                Err(_) => {
                    read.abort();
                    None
                }
            }
        })
    }

    /// Read the home's status and keep what it reports when it is the
    /// home's, holding the cell's status lock from the read's nonce until it
    /// is kept (TLA+ `HeldAudienceReads`); `None` when no status could be
    /// read.
    async fn reread_status(&self) -> Option<Result<HeldAudienceChange, HomeStatusRefusal>> {
        let _reading = self.delegation_audience.reading().await;
        self.read_and_keep().await
    }

    /// [`Self::reread_status`] for a caller holding the status lock.
    async fn read_and_keep(&self) -> Option<Result<HeldAudienceChange, HomeStatusRefusal>> {
        Some(match self.read_status().await? {
            Ok(home) => Ok(self.keep_status(&home).await),
            Err(refusal) => Err(refusal),
        })
    }

    async fn keep_status(&self, home: &AuthenticatedHome) -> HeldAudienceChange {
        match self.delegation_audience.0.resolver {
            Some(_) => self.delegation_audience.observe(home),
            None => self.keep_runtime_status(home).await,
        }
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
    /// endpoint sent with `answer`, and keep the audience and clock it
    /// reports when it is the home's; otherwise why it is not. `None` when
    /// the answer refuses no credential this endpoint presented
    /// (`Identity.HeldAudience.refusalOf`) or no status could be read.
    pub(super) async fn reread_after_refusal(
        &self,
        answer: CredentialAnswer,
    ) -> Option<(
        CredentialRefusal,
        Result<HeldAudienceChange, HomeStatusRefusal>,
    )> {
        let refusal = CredentialRefusal::of(answer, self.presented_credential());
        if refusal == CredentialRefusal::Other {
            return None;
        }
        Some((refusal, self.reread_status().await?))
    }

    /// The audience this endpoint mints request-bound delegations for, with
    /// the deployment's genesis identity, and the provider's time it issues
    /// them at; the home's status is read again first when the held audience
    /// names no genesis or no clock of its home is held
    /// (`Identity.HeldAudience.rereadBeforeRequestBound`). `None` when the
    /// endpoint mints no delegations.
    pub(super) async fn request_bound_audience(
        &self,
    ) -> Result<Option<RequestBoundAudience>, ConfigClientError> {
        self.runtime_authentication().await?;
        if !self.mints_delegations() {
            return Ok(None);
        }
        let reread = || {
            let (clock, _) = self.delegation_audience.minting_clock();
            gents_protocol::delegation_audience::reread_before_request_bound(
                self.delegation_audience.get().as_ref(),
                clock.is_some(),
            )
        };
        if reread() {
            let _reading = self.delegation_audience.reading().await;
            if reread() {
                if let Some(Err(refusal)) = self.read_and_keep().await {
                    return Err(ConfigClientError::HomeStatusUnauthenticated(refusal));
                }
            }
        }
        let (clock, now) = self.delegation_audience.minting_clock();
        match self.delegation_audience.get() {
            Some(audience) if audience.genesis_id.is_some() => Ok(Some(RequestBoundAudience {
                audience,
                issued_at: gents_protocol::delegation_audience::issue_at(clock.as_ref(), now),
            })),
            _ => Err(super::DelegationError::AudienceUnavailable.into()),
        }
    }

    /// The provider's time a client dates what it signs at, in Unix seconds
    /// (`Identity.UserDelegation.storedCommandWindow`); the home's status is
    /// read first when this endpoint mints delegations and holds no clock of
    /// its home.
    pub(super) async fn home_time(&self) -> u64 {
        let unclocked =
            || self.mints_delegations() && self.delegation_audience.minting_clock().0.is_none();
        if unclocked() {
            let _reading = self.delegation_audience.reading().await;
            if unclocked() {
                self.read_and_keep().await;
            }
        }
        let (clock, now) = self.delegation_audience.minting_clock();
        gents_protocol::delegation_audience::issue_at(clock.as_ref(), now)
    }

    /// Whether a request this endpoint sent and its node answered with
    /// `answer` is to be sent once more, after the home's status was read
    /// again and the audience and clock it reports kept
    /// (`Identity.HeldAudience.recovery`); a status that is not the home's
    /// fails the request.
    pub async fn recovers(&self, answer: CredentialAnswer) -> Result<bool, ConfigClientError> {
        let Some((refusal, read)) = self.reread_after_refusal(answer).await else {
            return Ok(false);
        };
        let (authenticated, record) = match &read {
            Ok(record) => (true, record.clone()),
            Err(_) => (false, HeldAudienceChange::Unchanged),
        };
        match (recovery(false, refusal, authenticated, &record), read) {
            (Recovery::Resend, _) => Ok(true),
            (Recovery::Unauthenticated, Err(refusal)) => {
                Err(ConfigClientError::HomeStatusUnauthenticated(refusal))
            }
            _ => Ok(false),
        }
    }

    /// `recovers` for an answer's HTTP status.
    pub async fn recovers_refusal(
        &self,
        status: reqwest::StatusCode,
    ) -> Result<bool, ConfigClientError> {
        self.recovers(CredentialAnswer::of_status(status.as_u16()))
            .await
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
        Ok(self.send_counting_resends(build).await?.0)
    }

    /// `send_resolving`, and whether the request was sent once more.
    pub(super) async fn send_counting_resends<F>(
        &self,
        build: F,
    ) -> Result<(Result<reqwest::Response, reqwest::Error>, bool), ConfigClientError>
    where
        F: Fn(&Self) -> Result<reqwest::RequestBuilder, ConfigClientError>,
    {
        self.runtime_authentication().await?;
        let response = build(self)?.send().await;
        let recovered = match &response {
            Ok(answered) => self.recovers_refusal(answered.status()).await?,
            Err(_) => false,
        };
        if recovered {
            Ok((build(self)?.send().await, true))
        } else {
            Ok((response, false))
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

    /// The home's status, signed by its node key for each read.
    struct Status {
        node: crate::identity::KeyIdentity,
        reports: ReportedDelegationAudience,
        provider_time: Option<u64>,
        reads: AtomicUsize,
    }

    impl ResolveAudience for Status {
        fn resolve<'a>(
            &'a self,
            nonce: &'a gents_protocol::home_attestation::HomeNonce,
        ) -> BoxFuture<'a, Option<serde_json::Value>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Some(
                    crate::test_support::signed_home_status(
                        &self.node,
                        &self.reports,
                        self.provider_time,
                        nonce,
                    )
                    .await,
                )
            })
        }
    }

    struct Contradictory {
        node: crate::identity::KeyIdentity,
        reads: AtomicUsize,
    }

    impl ResolveAudience for Contradictory {
        fn resolve<'a>(
            &'a self,
            nonce: &'a gents_protocol::home_attestation::HomeNonce,
        ) -> BoxFuture<'a, Option<serde_json::Value>> {
            use crate::identity::NodeIdentity as _;
            use gents_protocol::home_attestation::{
                AttestedAudience, HomeAttestation, HomeStatement, HOME_ATTESTATION_VERSION,
                STATUS_HOME_ATTESTATION_FIELD,
            };
            self.reads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let statement = HomeStatement {
                    version: HOME_ATTESTATION_VERSION,
                    nonce: nonce.as_str().to_owned(),
                    node_did: self.node.did().to_owned(),
                    client_authentication:
                        gents_protocol::client_authentication::ClientAuthentication::Vera {
                            worker: audience(2).worker,
                            deployment: 2,
                        },
                    delegation_audience: AttestedAudience::Named {
                        audience: DelegationAudience {
                            genesis_id: Some("0x03".into()),
                            ..audience(3)
                        },
                    },
                    provider_time: Some(1_000),
                };
                let signature = self.node.sign(&statement.signing_payload()).await.unwrap();
                Some(serde_json::json!({
                    "node_did": self.node.did(),
                    STATUS_HOME_ATTESTATION_FIELD: HomeAttestation::signed(statement, &signature),
                }))
            })
        }
    }

    #[tokio::test]
    async fn a_signed_status_that_contradicts_itself_is_refused_not_read_as_absent() {
        use crate::identity::NodeIdentity as _;
        let node = home_key().await;
        let user =
            tokio::task::spawn_blocking(|| crate::test_support::test_identity("contradicted-user"))
                .await
                .unwrap();
        let home = node.did().to_owned();
        let status = Arc::new(Contradictory {
            node,
            reads: AtomicUsize::new(0),
        });
        let held = DelegationAudience {
            genesis_id: Some("0x02".into()),
            ..audience(2)
        };
        let endpoint =
            GraphqlEndpoint::as_principal("http://127.0.0.1:1/api/v0/graphql", user.did())
                .with_client_signing_capability(user.client_signing_capability().unwrap())
                .unwrap()
                .with_reported_delegation_audience(ReportedDelegationAudience::Named(held))
                .unwrap()
                .with_audience_resolver(home, status.clone());
        let refused = endpoint.request_bound_audience().await;
        assert!(
            matches!(
                refused,
                Err(
                    crate::config_client::ConfigClientError::HomeStatusUnauthenticated(
                        gents_protocol::home_attestation::HomeStatusRefusal::Contradictory
                    )
                )
            ),
            "{refused:?}"
        );
        assert_eq!(status.reads.load(Ordering::SeqCst), 1);
        assert!(refused
            .unwrap_err()
            .to_string()
            .contains("update Gents on the home"));
    }

    async fn home_key() -> crate::identity::KeyIdentity {
        tokio::task::spawn_blocking(|| crate::test_support::test_identity("held-home"))
            .await
            .unwrap()
    }

    fn audience(deployment: u64) -> DelegationAudience {
        DelegationAudience {
            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
            deployment,
            genesis_id: None,
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
            .with_audience_resolver(
                crate::identity::NodeIdentity::did(&status.node),
                status.clone(),
            )
    }

    async fn status(reports: ReportedDelegationAudience) -> Arc<Status> {
        Arc::new(Status {
            node: home_key().await,
            reports,
            provider_time: None,
            reads: AtomicUsize::new(0),
        })
    }

    /// What answers at the home's address naming the home's node and the
    /// audience the node there takes, without the home's signature for the
    /// read: unsigned, signed by another key, or the home's own status
    /// signed for an earlier read.
    #[derive(Clone, Copy, Debug)]
    enum Forgery {
        Unsigned,
        OtherKey,
        Replayed,
    }

    struct Impostor {
        home: crate::identity::KeyIdentity,
        other: crate::identity::KeyIdentity,
        forgery: Forgery,
        reports: DelegationAudience,
        reads: AtomicUsize,
    }

    impl ResolveAudience for Impostor {
        fn resolve<'a>(
            &'a self,
            nonce: &'a gents_protocol::home_attestation::HomeNonce,
        ) -> BoxFuture<'a, Option<serde_json::Value>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let mut status = serde_json::json!({
                    "node_did": crate::identity::NodeIdentity::did(&self.home),
                    "client_authentication": {
                        "kind": "vera",
                        "worker": self.reports.worker,
                        "deployment": self.reports.deployment,
                    },
                    "delegation_audience": self.reports,
                });
                match self.forgery {
                    Forgery::Unsigned => {}
                    Forgery::OtherKey => {
                        crate::identity::home_status::attest(Some(&self.other), nonce, &mut status)
                            .await;
                    }
                    Forgery::Replayed => {
                        let earlier =
                            gents_protocol::home_attestation::HomeNonce::from_bytes([9; 32]);
                        crate::identity::home_status::attest(
                            Some(&self.home),
                            &earlier,
                            &mut status,
                        )
                        .await;
                    }
                }
                Some(status)
            })
        }
    }

    #[tokio::test]
    async fn a_status_the_home_did_not_sign_changes_no_record_and_mints_nothing() {
        for forgery in [Forgery::Unsigned, Forgery::OtherKey, Forgery::Replayed] {
            for held in [None, Some(audience(1))] {
                let server = node_taking(2).await;
                let impostor = Arc::new(Impostor {
                    home: home_key().await,
                    other: home_key().await,
                    forgery,
                    reports: audience(2),
                    reads: AtomicUsize::new(0),
                });
                let user =
                    tokio::task::spawn_blocking(|| crate::test_support::test_identity("held-user"))
                        .await
                        .unwrap();
                let capability =
                    crate::identity::NodeIdentity::client_signing_capability(&user).unwrap();
                let endpoint = GraphqlEndpoint::as_principal(&server.url, capability.did())
                    .with_client_signing_capability(capability)
                    .unwrap()
                    .with_reported_delegation_audience(match &held {
                        Some(audience) => ReportedDelegationAudience::Named(audience.clone()),
                        None => ReportedDelegationAudience::Absent,
                    })
                    .unwrap()
                    .with_audience_resolver(
                        crate::identity::NodeIdentity::did(&impostor.home),
                        impostor.clone(),
                    );
                let answered = super::super::graphql::query_with_options(
                    &endpoint,
                    "query { Node { _docID } }",
                    None,
                    gents_protocol::graphql::GraphqlRequestOptions {
                        max_attempts: 1,
                        ..Default::default()
                    },
                )
                .await;
                assert!(
                    matches!(
                        answered,
                        Err(crate::config_client::ConfigClientError::HomeStatusUnauthenticated(_))
                    ),
                    "{forgery:?} {held:?}: {answered:?}"
                );
                assert_eq!(impostor.reads.load(Ordering::SeqCst), 1);
                assert_eq!(
                    server.presented(),
                    vec![held.as_ref().map(|audience| audience.deployment)],
                    "{forgery:?}: nothing is minted for the audience the home did not sign"
                );
                assert_eq!(endpoint.delegation_audience.get(), held, "{forgery:?}");
            }
        }
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
        let status = status(ReportedDelegationAudience::Named(audience(1))).await;
        let endpoint = endpoint(&server, ReportedDelegationAudience::Absent, &status).await;
        assert!(query(&endpoint).await);
        assert_eq!(server.presented().len(), 2);
        assert!(endpoint.clone().mints_delegations(), "its clones keep it");
    }

    #[tokio::test]
    async fn a_client_recovers_from_a_restart_under_another_audience() {
        let server = node_taking(2).await;
        let status = status(ReportedDelegationAudience::Named(audience(2))).await;
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
            let status = status(reports).await;
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
        let status = status(ReportedDelegationAudience::Named(audience(2))).await;
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
            let status = status(reports).await;
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
    async fn a_transaction_whose_home_status_is_not_signed_fails_saying_so_and_runs_once() {
        for forgery in [Forgery::Unsigned, Forgery::OtherKey, Forgery::Replayed] {
            let server = transacting_node(2).await;
            let impostor = Arc::new(Impostor {
                home: home_key().await,
                other: home_key().await,
                forgery,
                reports: audience(2),
                reads: AtomicUsize::new(0),
            });
            let user =
                tokio::task::spawn_blocking(|| crate::test_support::test_identity("held-user"))
                    .await
                    .unwrap();
            let capability =
                crate::identity::NodeIdentity::client_signing_capability(&user).unwrap();
            let endpoint = GraphqlEndpoint::as_principal(&server.url, capability.did())
                .with_client_signing_capability(capability)
                .unwrap()
                .with_reported_delegation_audience(ReportedDelegationAudience::Named(audience(1)))
                .unwrap()
                .with_audience_resolver(
                    crate::identity::NodeIdentity::did(&impostor.home),
                    impostor.clone(),
                );
            let runs = AtomicUsize::new(0);
            let committed = crate::config_client::ConfigAccess::Graphql(endpoint.clone())
                .transact_readonly("held_audience.unsigned", |txn| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        txn.execute("query { Node { _docID } }").await?;
                        Ok::<_, crate::config_client::ConfigClientError>(())
                    })
                })
                .await;
            assert!(
                matches!(
                    committed,
                    Err(crate::config_client::ConfigClientError::HomeStatusUnauthenticated(_))
                ),
                "{forgery:?}: {committed:?}"
            );
            assert_eq!(runs.load(Ordering::SeqCst), 1, "{forgery:?}");
            assert_eq!(server.presented(), vec![Some(1)], "{forgery:?}");
            assert_eq!(endpoint.delegation_audience.get(), Some(audience(1)));
        }
    }

    #[tokio::test]
    async fn a_transaction_refused_again_is_run_only_twice() {
        let server = transacting_node(3).await;
        let status = status(ReportedDelegationAudience::Named(audience(2))).await;
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
            let status = status(ReportedDelegationAudience::Absent).await;
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
        let status = status(ReportedDelegationAudience::Named(audience(2))).await;
        let endpoint = endpoint(
            &server,
            ReportedDelegationAudience::Named(audience(1)),
            &status,
        )
        .await;
        assert!(!query(&endpoint).await);
        assert_eq!(server.presented().len(), 2);
    }

    fn claim(headers: &HeaderMap, name: &str) -> Option<u64> {
        let bearer = headers.get("authorization")?.to_str().ok()?;
        let claims = bearer.strip_prefix("Bearer ")?.split('.').nth(1)?;
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims)
                .ok()?,
        )
        .ok()?;
        claims[name].as_u64()
    }

    /// A node whose clock is `provider_time`: it refuses a mutation whose
    /// delegation does not hold then with DefraDB's window refusal, and
    /// records each delegation's expiry.
    async fn node_at(provider_time: u64) -> (String, Arc<Mutex<Vec<Option<u64>>>>) {
        let expiries = Arc::new(Mutex::new(Vec::new()));
        let seen = expiries.clone();
        let router = axum::Router::new().fallback(move |headers: HeaderMap| {
            let seen = seen.clone();
            async move {
                let expires = claim(&headers, "exp");
                seen.lock().unwrap().push(expires);
                let holds = claim(&headers, "nbf").is_some_and(|nbf| nbf <= provider_time)
                    && expires.is_some_and(|exp| provider_time <= exp);
                axum::Json(if holds {
                    serde_json::json!({"data": {"create_Node": [{"_docID": "bae-1"}]}})
                } else {
                    serde_json::json!({"errors": [{
                        "message": "verified write refused",
                        "extensions": {
                            "code": gents_protocol::delegation_audience::DELEGATION_OUTSIDE_WINDOW_CODE,
                            "providerTime": provider_time,
                        },
                    }]})
                })
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        (format!("http://{address}/api/v0/graphql"), expiries)
    }

    #[tokio::test]
    async fn a_delegation_refused_for_its_window_is_minted_again_at_the_home_clock() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let provider_time = now + 600;
        let (url, expiries) = node_at(provider_time).await;
        let status = Arc::new(Status {
            node: home_key().await,
            reports: ReportedDelegationAudience::Named(audience(1)),
            provider_time: Some(provider_time),
            reads: AtomicUsize::new(0),
        });
        let server = Node {
            url,
            presented: Arc::default(),
        };
        let endpoint = endpoint(
            &server,
            ReportedDelegationAudience::Named(audience(1)),
            &status,
        )
        .await;
        let created = super::super::graphql::auto_commit(
            &endpoint,
            "mutation { create_Node(input: {}) { _docID } }",
            None,
        )
        .await;
        assert!(created.is_ok(), "{created:?}");
        assert_eq!(status.reads.load(Ordering::SeqCst), 1);
        let expiries = expiries.lock().unwrap().clone();
        assert_eq!(expiries.len(), 2);
        assert!(expiries[1].unwrap() >= provider_time + 120, "{expiries:?}");
    }
}

#[cfg(test)]
mod ordering_tests;
