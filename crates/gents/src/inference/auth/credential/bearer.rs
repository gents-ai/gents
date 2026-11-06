use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use defra_node::Node as EmbeddedNode;
use tokio::sync::Mutex;

use super::*;

const REFRESH_SKEW: Duration = Duration::minutes(5);
const REFRESH_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

pub fn token_is_fresh(expires_at: DateTime<Utc>) -> bool {
    Utc::now() + REFRESH_SKEW < expires_at
}

pub fn apply_refreshed_tokens(credential: &mut OAuthCredential, refreshed: RefreshedTokens) {
    credential.access_token = refreshed.access_token;
    credential.refresh_token = refreshed.refresh_token;
    if refreshed.id_token.is_some() {
        credential.id_token = refreshed.id_token;
    }
    if refreshed.account_id.is_some() {
        credential.account_id = refreshed.account_id;
    }
    if refreshed.plan_type.is_some() {
        credential.chatgpt_plan_type = refreshed.plan_type;
    }
    credential.is_fedramp = refreshed.is_fedramp || credential.is_fedramp;
    credential.access_token_expires_at = refreshed.access_token_expires_at;
    credential.last_refresh = Some(Utc::now());
}

fn provider_account_key(kind: OAuthRefreshKind, credential: &OAuthCredential) -> Option<String> {
    match kind {
        OAuthRefreshKind::ChatGpt => crate::inference::auth::chatgpt_refresh::chatgpt_account_key(
            &credential.access_token,
            credential.id_token.as_deref(),
            credential.account_id.as_deref(),
        ),
        OAuthRefreshKind::Xai => {
            crate::inference::auth::xai_login::xai_account_key(&credential.access_token)
        }
        OAuthRefreshKind::Claude => None,
        OAuthRefreshKind::Gateway => credential.provider_account_key.clone(),
    }
}

/// Live bearers by credential id. Entries are weak: a bearer holds its node, so
/// a strong entry would keep every node that ever bootstrapped OAuth alive for
/// the rest of the process.
pub(super) fn bearer_registry(
) -> &'static std::sync::Mutex<rapidhash::RapidHashMap<String, std::sync::Weak<DbCredentialBearer>>>
{
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<rapidhash::RapidHashMap<String, std::sync::Weak<DbCredentialBearer>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(rapidhash::RapidHashMap::default()))
}

/// The live bearer for this credential on this node, or a new one. A credential
/// id is shared only with the node that holds it, so the same id on another node
/// never reuses a bearer bound to a different node.
pub fn shared_bearer(
    node: &Arc<EmbeddedNode>,
    credential_id: &str,
    make: impl FnOnce() -> DbCredentialBearer,
) -> Arc<DbCredentialBearer> {
    let mut registry = bearer_registry()
        .lock()
        .expect("bearer registry mutex poisoned");
    if let Some(bearer) = registry.get(credential_id).and_then(std::sync::Weak::upgrade) {
        if Arc::ptr_eq(&bearer.node, node) {
            return bearer;
        }
    }
    registry.retain(|_, bearer| bearer.strong_count() > 0);
    let bearer = Arc::new(make());
    registry.insert(credential_id.to_string(), Arc::downgrade(&bearer));
    bearer
}

pub trait BearerSource: Send + Sync {
    fn current_bearer(&self) -> impl Future<Output = Result<String>> + Send;

    fn invalidate(&self) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn is_oauth(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshPersist {
    Written,
    Superseded,
}

/// Bounds each token-endpoint exchange, not the persist that follows it: a hung endpoint must
/// fail refresh instead of stalling callers, while cancelling after a response could lose a
/// rotated refresh token.
const REFRESH_REQUEST_TIMEOUT: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_secs(5)
} else {
    std::time::Duration::from_secs(30)
};

pub struct DbCredentialBearer {
    node: Arc<EmbeddedNode>,
    node_did: String,
    provider: String,
    credential_id: String,
    http: reqwest::Client,
    cache: Mutex<Option<OAuthCredential>>,
    refresh_lock: Mutex<Option<(std::time::Instant, OAuthAuthProblem)>>,
    is_owner: bool,
    force_refresh: AtomicBool,
    refresh_kind: OAuthRefreshKind,
    product: OAuthProduct,
}

impl DbCredentialBearer {
    pub fn new(
        node: Arc<EmbeddedNode>,
        node_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
        refresh_kind: OAuthRefreshKind,
        product: OAuthProduct,
    ) -> Self {
        Self::with_cache(
            node,
            node_did,
            provider,
            credential_id,
            is_owner,
            None,
            refresh_kind,
            product,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_cache(
        node: Arc<EmbeddedNode>,
        node_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
        cache_seed: Option<OAuthCredential>,
        refresh_kind: OAuthRefreshKind,
        product: OAuthProduct,
    ) -> Self {
        Self {
            node,
            node_did: node_did.into(),
            provider: provider.into(),
            credential_id: credential_id.into(),
            http: reqwest::Client::builder()
                .timeout(REFRESH_REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client with a timeout builds like Client::new()"),
            cache: Mutex::new(cache_seed),
            refresh_lock: Mutex::new(None),
            is_owner,
            force_refresh: AtomicBool::new(false),
            refresh_kind,
            product,
        }
    }

    async fn load_credential(&self) -> std::result::Result<OAuthCredential, OAuthCredentialError> {
        let access = runtime_credential_access(&self.node, &self.node_did)?;
        let Some(credential) =
            lookup_oauth_credential_by_id_on(&access, &self.credential_id).await?
        else {
            let label = self.cached().await.map_or_else(
                || self.product.name.to_owned(),
                |cached| effective_account_label(&cached),
            );
            return Err(OAuthCredentialAuthError {
                problem: OAuthAuthProblem::Missing,
                message: format!(
                    "{} {MOVE_OFF_ACCOUNT} Signing in again adds a new account; it does not restore \
                     this one.",
                    account_failure(&self.product, &label, "removed from this node"),
                ),
            }
            .into());
        };
        if !credential.enabled {
            return Err(OAuthCredentialAuthError {
                problem: OAuthAuthProblem::WrongMode {
                    found_mode: "disabled".into(),
                },
                message: format!(
                    "{}\n{}\n{MOVE_OFF_ACCOUNT}",
                    account_failure(
                        &self.product,
                        &effective_account_label(&credential),
                        "disabled"
                    ),
                    classify_oauth_auth_error(
                        &self.product,
                        &self.node_did,
                        &self.provider,
                        &OAuthAuthProblem::WrongMode {
                            found_mode: "disabled".to_string(),
                        },
                    )
                ),
            }
            .into());
        }
        Ok(credential)
    }

    async fn cached(&self) -> Option<OAuthCredential> {
        self.cache.lock().await.clone()
    }

    async fn cache_credential(&self, credential: &OAuthCredential) {
        *self.cache.lock().await = Some(credential.clone());
    }

    pub(crate) async fn adopt_document(&self, stored: &OAuthCredential) {
        let mut cache = self.cache.lock().await;
        if cache
            .as_ref()
            .is_some_and(|cached| cached.doc_id != stored.doc_id)
        {
            *cache = Some(stored.clone());
        }
    }

    /// Persist rotated fields only while the stored refresh token still matches the one read.
    /// A sign-in, removal, or replacement since that read must not be overwritten or revived.
    async fn persist_with_retry(
        &self,
        credential: &OAuthCredential,
        stored_refresh_token: &str,
        key_filled: bool,
    ) -> Result<RefreshPersist> {
        let mutation = refresh_update_mutation(credential, stored_refresh_token, key_filled);
        let access = runtime_credential_access(&self.node, &self.node_did)?;
        let mut last_error = None;
        let mut delay_ms = 200u64;
        for attempt in 0..3u32 {
            match access.write("oauth_credential.refresh", &mutation).await {
                Ok(response) => {
                    let written = response
                        .get("data")
                        .and_then(|data| data.get("update_OAuthCredential"))
                        .is_some_and(crate::store::graphql::response_has_documents);
                    return Ok(if written {
                        RefreshPersist::Written
                    } else {
                        RefreshPersist::Superseded
                    });
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2;
                    }
                }
            }
        }
        Err(last_error
            .expect("persist_with_retry ran at least one failing attempt")
            .into())
    }

    fn auth_error(
        &self,
        credential: &OAuthCredential,
        problem: &OAuthAuthProblem,
    ) -> OAuthCredentialAuthError {
        let state = match problem {
            OAuthAuthProblem::Expired => "signed out (expired or revoked)",
            OAuthAuthProblem::NotEntitled => "not entitled",
            _ => "unusable",
        };
        OAuthCredentialAuthError {
            problem: problem.clone(),
            message: format!(
                "{}\n{}\n{MOVE_OFF_ACCOUNT}",
                account_failure(&self.product, &effective_account_label(credential), state),
                classify_oauth_auth_error(&self.product, &self.node_did, &self.provider, problem),
            ),
        }
    }

    async fn refresh_tokens(
        &self,
        credential: &OAuthCredential,
    ) -> Result<RefreshedTokens, OAuthAuthProblem> {
        let refresh_token = credential.refresh_token.as_str();
        match self.refresh_kind {
            OAuthRefreshKind::ChatGpt => {
                crate::inference::auth::chatgpt_refresh::refresh_chatgpt_token(
                    refresh_token,
                    &self.http,
                )
                .await
            }
            OAuthRefreshKind::Claude => {
                crate::inference::auth::claude_refresh::refresh_claude_token(
                    refresh_token,
                    &self.http,
                )
                .await
            }
            OAuthRefreshKind::Xai => {
                crate::inference::auth::xai_refresh::refresh_xai_token(refresh_token, &self.http)
                    .await
            }
            OAuthRefreshKind::Gateway => {
                crate::claude_gateway_login::refresh_gateway_token(
                    credential.account_id.as_deref(),
                    refresh_token,
                    &self.http,
                )
                .await
            }
        }
    }
}

impl DbCredentialBearer {
    pub(crate) async fn current_bearer_typed(
        &self,
    ) -> std::result::Result<String, OAuthCredentialError> {
        let forced = self.force_refresh.load(Ordering::SeqCst);

        if !forced {
            if let Some(cred) = self.cached().await {
                if token_is_fresh(cred.access_token_expires_at) {
                    return Ok(cred.access_token);
                }
            }
        }

        let mut last_failure = self.refresh_lock.lock().await;

        let forced = self.force_refresh.load(Ordering::SeqCst);

        if !forced {
            if let Some(cred) = self.cached().await {
                if token_is_fresh(cred.access_token_expires_at) {
                    return Ok(cred.access_token);
                }
            }
        }

        let mut credential = match self.cached().await {
            Some(cred) => cred,
            None => {
                let cred = self.load_credential().await?;
                self.cache_credential(&cred).await;
                cred
            }
        };
        if !forced && token_is_fresh(credential.access_token_expires_at) {
            return Ok(credential.access_token);
        }

        if !self.is_owner {
            self.force_refresh.store(false, Ordering::SeqCst);
            return Ok(credential.access_token);
        }

        let db_credential = self.load_credential().await?;
        let stored_refresh_token = db_credential.refresh_token.clone();
        if db_credential.doc_id != credential.doc_id
            || db_credential.access_token_expires_at >= credential.access_token_expires_at
        {
            credential = db_credential;
            if !forced && token_is_fresh(credential.access_token_expires_at) {
                self.cache_credential(&credential).await;
                return Ok(credential.access_token);
            }
        }

        if let Some((failed_at, problem)) = last_failure.as_ref() {
            if failed_at.elapsed() < REFRESH_FAILURE_COOLDOWN {
                return Err(self.auth_error(&credential, problem).into());
            }
        }

        let refreshed = match self.refresh_tokens(&credential).await {
            Ok(refreshed) => refreshed,
            Err(problem) => {
                let error = self.auth_error(&credential, &problem);
                *last_failure = Some((std::time::Instant::now(), problem));
                return Err(error.into());
            }
        };
        *last_failure = None;
        apply_refreshed_tokens(&mut credential, refreshed);
        let key_filled = credential.provider_account_key.is_none() && {
            credential.provider_account_key = provider_account_key(self.refresh_kind, &credential);
            credential.provider_account_key.is_some()
        };

        self.cache_credential(&credential).await;
        self.force_refresh.store(false, Ordering::SeqCst);
        match self
            .persist_with_retry(&credential, &stored_refresh_token, key_filled)
            .await
        {
            Ok(RefreshPersist::Written) => {}
            Ok(RefreshPersist::Superseded) => {
                *self.cache.lock().await = None;
                tracing::debug!(
                    node_did = %self.node_did,
                    credential_id = %self.credential_id,
                    product = self.product.name,
                    "the stored account changed while its token refreshed; the stored row stands \
                     and this refreshed token is served once"
                )
            }
            Err(error) => tracing::error!(
                node_did = %self.node_did,
                credential_id = %self.credential_id,
                product = self.product.name,
                %error,
                "failed to persist rotated OAuth token to DefraDB after retries; serving \
                 the rotated token from memory. It must be re-persisted before this process exits \
                 or the rotated refresh token will be lost."
            ),
        }
        Ok(credential.access_token)
    }
}

impl BearerSource for DbCredentialBearer {
    async fn current_bearer(&self) -> Result<String> {
        self.current_bearer_typed().await.map_err(Into::into)
    }

    async fn invalidate(&self) {
        self.force_refresh.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod native_access_tests {
    use super::*;
    use crate::store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
    use crate::NodeIdentity;

    #[tokio::test]
    async fn runtime_refresh_uses_native_writer_grant() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let user = crate::KeyIdentity::load_or_create(dir.path().join("user"), None)?;
        let runtime = crate::KeyIdentity::load_or_create(dir.path().join("runtime"), None)?;
        let agent = crate::KeyIdentity::load_or_create(dir.path().join("agent"), None)?;
        let sibling = crate::KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
        let outsider = crate::KeyIdentity::load_or_create(dir.path().join("outsider"), None)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .data_path(dir.path().join("node"))
                .build()
                .await?,
        );
        node.add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        node.add_collection(gents_protocol::schemas::NODE).await?;
        node.add_collection(gents_protocol::schemas::OAUTH_CREDENTIAL)
            .await?;
        let access = |actor: &crate::KeyIdentity| -> anyhow::Result<ConfigAccess> {
            Ok(ConfigAccess::Graphql(
                GraphqlEndpoint::as_principal(
                    format!("http://{address}/api/v0/graphql"),
                    actor.did(),
                )
                .with_client_signing_capability(actor.client_signing_capability()?)?,
            ))
        };
        let user_access = access(&user)?;
        let runtime_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        for actor in [&user, &agent, &sibling, &outsider] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
            ] {
                runtime_access
                    .grant_node_permission(&identity::Did::new(actor.did())?, permission)
                    .await?;
            }
        }
        runtime_access
            .grant_node_permission(
                &identity::Did::new(user.did())?,
                NodePermission::DacRelationDelete,
            )
            .await?;
        let parent = user_access
            .write(
                "test.refresh.node",
                &format!(
                    r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                    crate::store::graphql::escape_graphql_string(runtime.did()),
                ),
            )
            .await?;
        let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
        user_access
            .add_actor_relationship(
                "Node",
                &parent,
                "runtime_reader",
                &identity::Did::new(runtime.did())?,
            )
            .await?;
        let mut credential = OAuthCredential {
            doc_id: None,
            credential_id: "test-refresh".into(),
            node_did: runtime.did().into(),
            provider: crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER.into(),
            access_token: "before".into(),
            refresh_token: "before-refresh".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: Utc::now(),
            last_refresh: None,
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        let doc = upsert_oauth_credential_on(&user_access, &credential).await?;
        assert_eq!(
            upsert_oauth_credential_on(&user_access, &credential).await?,
            doc
        );
        let signed_in = crate::inference::auth::credential::store_sign_in(
            &user_access,
            credential.clone(),
            Some("primary"),
        )
        .await?;
        assert_eq!(signed_in.doc_id, doc);
        crate::inference::auth::credential::set_account_enabled(
            &user_access,
            runtime.did(),
            &credential.credential_id,
            false,
        )
        .await?;
        crate::inference::auth::credential::set_account_enabled(
            &user_access,
            runtime.did(),
            &credential.credential_id,
            true,
        )
        .await?;
        crate::inference::auth::credential::set_account_label(
            &user_access,
            runtime.did(),
            &credential.credential_id,
            "renamed",
        )
        .await?;
        let configured = user_access
            .execute("{ OAuthCredential { _docID label enabled } }")
            .await?;
        assert_eq!(configured["data"]["OAuthCredential"][0]["label"], "renamed");
        assert_eq!(configured["data"]["OAuthCredential"][0]["enabled"], true);
        credential.doc_id = Some(doc.clone());
        credential.access_token = "after".into();
        credential.refresh_token = "after-refresh".into();
        let bearer = DbCredentialBearer::new(
            node.clone(),
            runtime.did(),
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            "test-refresh",
            true,
            OAuthRefreshKind::Claude,
            crate::inference::auth::claude::CLAUDE_OAUTH_PRODUCT,
        );
        assert!(matches!(
            bearer
                .persist_with_retry(&credential, "before-refresh", false)
                .await?,
            RefreshPersist::Written
        ));
        let query = "{ OAuthCredential { access_token refresh_token } }";
        for actor in [&agent, &sibling, &outsider] {
            let visible = access(actor)?.execute(query).await?;
            assert!(gents_protocol::graphql::graphql_rows_from_response(
                &visible,
                "OAuthCredential"
            )
            .is_empty());
        }
        let denied = access(&outsider)?.write("test.oauth.denied", &format!(
            r#"mutation {{ update_OAuthCredential(filter: {{_docID: {{_eq: "{}"}}}}, input: {{access_token: "stolen"}}) {{_docID}} }}"#,
            crate::store::graphql::escape_graphql_string(&doc),
        )).await;
        assert!(
            denied.is_err()
                || denied
                    .ok()
                    .and_then(|value| gents_protocol::graphql::extract_mutation_doc_id(
                        &value,
                        "OAuthCredential"
                    )
                    .ok())
                    .is_none()
        );
        let stored = user_access.execute(query).await?;
        assert_eq!(
            stored["data"]["OAuthCredential"][0]["refresh_token"],
            "after-refresh"
        );
        user_access
            .delete_actor_relationship(
                "OAuthCredential",
                &doc,
                "runtime_writer",
                &identity::Did::new(runtime.did())?,
            )
            .await?;
        credential.access_token = "revoked".into();
        credential.refresh_token = "revoked-refresh".into();
        let result = bearer
            .persist_with_retry(&credential, "after-refresh", false)
            .await;
        assert!(!matches!(result, Ok(RefreshPersist::Written)));
        let stored = user_access.execute(query).await?;
        assert_eq!(
            stored["data"]["OAuthCredential"][0]["refresh_token"],
            "after-refresh"
        );
        drop(bearer);
        drop(user_access);
        drop(runtime_access);
        node.shutdown().await?;
        drop(node);
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let restarted = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .data_path(dir.path().join("node"))
                .build()
                .await?,
        );
        let user_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?,
        );
        assert_eq!(
            user_access.execute(query).await?["data"]["OAuthCredential"][0]["refresh_token"],
            "after-refresh"
        );
        let bearer = DbCredentialBearer::new(
            restarted.clone(),
            runtime.did(),
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            "test-refresh",
            true,
            OAuthRefreshKind::Claude,
            crate::inference::auth::claude::CLAUDE_OAUTH_PRODUCT,
        );
        assert!(!matches!(
            bearer
                .persist_with_retry(&credential, "after-refresh", false)
                .await,
            Ok(RefreshPersist::Written)
        ));
        restarted.shutdown().await?;
        Ok(())
    }
}
