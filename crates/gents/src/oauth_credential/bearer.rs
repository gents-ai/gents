use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
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
        OAuthRefreshKind::ChatGpt => crate::chatgpt_oauth_refresh::chatgpt_account_key(
            &credential.access_token,
            credential.id_token.as_deref(),
            credential.account_id.as_deref(),
        ),
        OAuthRefreshKind::Xai => crate::xai_oauth_login::xai_account_key(&credential.access_token),
        OAuthRefreshKind::Claude => None,
    }
}

pub(super) fn get_or_insert_arc<T>(
    registry: &std::sync::Mutex<rapidhash::RapidHashMap<String, Arc<T>>>,
    key: &str,
    make: impl FnOnce() -> T,
) -> Arc<T> {
    let mut map = registry.lock().expect("bearer registry mutex poisoned");
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(make()))
        .clone()
}

pub(super) fn bearer_registry(
) -> &'static std::sync::Mutex<rapidhash::RapidHashMap<String, Arc<DbCredentialBearer>>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<rapidhash::RapidHashMap<String, Arc<DbCredentialBearer>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(rapidhash::RapidHashMap::default()))
}

pub fn shared_bearer(
    credential_id: &str,
    make: impl FnOnce() -> DbCredentialBearer,
) -> Arc<DbCredentialBearer> {
    get_or_insert_arc(bearer_registry(), credential_id, make)
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
        let Some(credential) =
            lookup_oauth_credential_by_id(self.node.as_ref(), &self.credential_id).await?
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
        let mut last_error = None;
        let mut delay_ms = 200u64;
        for attempt in 0..3u32 {
            match crate::config_client::ConfigAccess::write_local(
                &self.node,
                "oauth_credential.refresh",
                &mutation,
            )
            .await
            {
                Ok(response) => {
                    let written = response
                        .get("data")
                        .and_then(|data| data.get("update_OAuthCredential"))
                        .is_some_and(crate::graphql::response_has_documents);
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
        refresh_token: &str,
    ) -> Result<RefreshedTokens, OAuthAuthProblem> {
        match self.refresh_kind {
            OAuthRefreshKind::ChatGpt => {
                crate::chatgpt_oauth_refresh::refresh_chatgpt_token(refresh_token, &self.http).await
            }
            OAuthRefreshKind::Claude => {
                crate::claude_oauth_refresh::refresh_claude_token(refresh_token, &self.http).await
            }
            OAuthRefreshKind::Xai => {
                crate::xai_oauth_refresh::refresh_xai_token(refresh_token, &self.http).await
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

        let refreshed = match self.refresh_tokens(&credential.refresh_token).await {
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
