use std::fmt;
use std::future::Future;
use std::sync::Arc;

use crate::llm::rig_compat::http::{
    self as http_client, HeaderMap, LazyBody, MultipartForm, Request, ReqwestClient, Response,
    StreamingResponse, Transport,
};
use anyhow::{Context, Result};
use bytes::Bytes;
use defra_node::Node as EmbeddedNode;
use gents_loop::provider_call::ProviderCall;

use crate::auth::credential::{
    classify_oauth_auth_error, resolve_oauth_credential, shared_bearer, AccountPick, BearerSource,
    DbCredentialBearer, OAuthAuthProblem, OAuthCredential, OAuthProduct, OAuthRefreshKind,
};

pub trait OAuthHttpPolicy: Clone + Default + Send + Sync + 'static {
    // xAI's proxy uses 403 as a NotEntitled tier gate no refresh fixes; with
    // rotating refresh tokens a force-refresh loop would burn a rotation per
    // request.
    const REJECTION_STATUSES: &'static [u16];

    fn merge_identity_headers(&self, headers: &mut HeaderMap) -> http_client::Result<()> {
        let _ = headers;
        Ok(())
    }

    fn patch_request_body(&self, _call: &ProviderCall, req: Request<Bytes>) -> Request<Bytes> {
        req
    }

    fn send_via<T>(
        &self,
        call: &ProviderCall,
        inner: &T,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static
    where
        T: Clone + Transport + 'static,
    {
        let inner = inner.clone();
        let call = call.clone();
        async move { Transport::send(&inner, &call, req).await }
    }

    fn patch_streaming(
        &self,
        _call: &ProviderCall,
        response: StreamingResponse,
    ) -> StreamingResponse {
        response
    }
}

fn bearer_rejection_status(error: &http_client::Error) -> Option<u16> {
    match error {
        http_client::Error::InvalidStatusCode(status)
        | http_client::Error::InvalidStatusCodeWithMessage(status, _) => Some(status.as_u16()),
        _ => None,
    }
}

pub fn is_bearer_rejection(rejection_statuses: &[u16], error: &http_client::Error) -> bool {
    bearer_rejection_status(error).is_some_and(|status| rejection_statuses.contains(&status))
}

pub fn ensure_event_stream_content_type(headers: &mut HeaderMap) {
    if !headers.contains_key("content-type") {
        headers.insert(
            "content-type",
            http_client::HeaderValue::from_static("text/event-stream"),
        );
    }
}

pub struct BearerAuthHttpClient<S, P, T = ReqwestClient>
where
    S: BearerSource,
    P: OAuthHttpPolicy,
{
    inner: T,
    bearer: Option<Arc<S>>,
    policy: P,
}

impl<S, P, T> Clone for BearerAuthHttpClient<S, P, T>
where
    S: BearerSource,
    P: OAuthHttpPolicy,
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bearer: self.bearer.clone(),
            policy: self.policy.clone(),
        }
    }
}

impl<S, P, T> fmt::Debug for BearerAuthHttpClient<S, P, T>
where
    S: BearerSource,
    P: OAuthHttpPolicy,
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerAuthHttpClient")
            .field("inner", &self.inner)
            .field("bearer_configured", &self.bearer.is_some())
            .finish()
    }
}

impl<S, P, T> Default for BearerAuthHttpClient<S, P, T>
where
    S: BearerSource,
    P: OAuthHttpPolicy,
    T: Default,
{
    fn default() -> Self {
        Self {
            inner: T::default(),
            bearer: None,
            policy: P::default(),
        }
    }
}

impl<S, P> BearerAuthHttpClient<S, P, ReqwestClient>
where
    S: BearerSource,
    P: OAuthHttpPolicy,
{
    pub fn new(bearer: Arc<S>) -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: Some(bearer),
            policy: P::default(),
        }
    }
}

impl<S, P, T> BearerAuthHttpClient<S, P, T>
where
    S: BearerSource,
    P: OAuthHttpPolicy,
{
    pub fn with_inner(bearer: Arc<S>, inner: T) -> Self {
        Self {
            inner,
            bearer: Some(bearer),
            policy: P::default(),
        }
    }

    pub fn with_policy(mut self, policy: P) -> Self {
        self.policy = policy;
        self
    }

    async fn fresh_auth_header(&self) -> http_client::Result<http_client::HeaderValue> {
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            http_client::Error::Instance(
                anyhow::anyhow!("BearerAuthHttpClient used without a configured BearerSource")
                    .into(),
            )
        })?;
        let token = bearer
            .current_bearer()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        http_client::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| http_client::Error::Instance(anyhow::Error::from(error).into()))
    }

    async fn prepare(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        let req = self.policy.patch_request_body(call, req);
        let value = self.fresh_auth_header().await?;
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("authorization", value);
        self.policy.merge_identity_headers(&mut parts.headers)?;
        Ok(Request::from_parts(parts, body))
    }

    fn bearer_to_invalidate<X>(&self, result: &http_client::Result<X>) -> Option<Arc<S>> {
        match result {
            Err(error) if is_bearer_rejection(P::REJECTION_STATUSES, error) => self.bearer.clone(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub async fn prepare_for_test(
        &self,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        self.prepare(&ProviderCall::default(), req).await
    }
}

impl<S, P, T> Transport for BearerAuthHttpClient<S, P, T>
where
    S: BearerSource + 'static,
    P: OAuthHttpPolicy,
    T: Clone + Transport + fmt::Debug + 'static,
{
    fn send(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        let inner = self.inner.clone();
        let this = self.clone();
        let call = call.clone();
        async move {
            let req = this.prepare(&call, req).await?;
            let result = this.policy.send_via(&call, &inner, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            result
        }
    }

    fn send_multipart(
        &self,
        call: &ProviderCall,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        let inner = self.inner.clone();
        let this = self.clone();
        let call = call.clone();
        async move {
            let value = this.fresh_auth_header().await?;
            let (mut parts, body) = req.into_parts();
            parts.headers.insert("authorization", value);
            this.policy.merge_identity_headers(&mut parts.headers)?;
            let req = Request::from_parts(parts, body);
            let result = Transport::send_multipart(&inner, &call, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            result
        }
    }

    fn send_streaming(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send {
        let inner = self.inner.clone();
        let this = self.clone();
        let call = call.clone();
        async move {
            let req = this.prepare(&call, req).await?;
            let result = Transport::send_streaming(&inner, &call, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            let mut response = result?;
            ensure_event_stream_content_type(response.headers_mut());
            Ok(this.policy.patch_streaming(&call, response))
        }
    }
}

pub async fn bootstrap_oauth_client(
    node: Arc<EmbeddedNode>,
    node_did: &str,
    provider: &str,
    refresh_kind: OAuthRefreshKind,
    product: OAuthProduct,
    pick: AccountPick<'_>,
) -> Result<(Arc<DbCredentialBearer>, OAuthCredential)> {
    bootstrap_oauth_client_typed(node, node_did, provider, refresh_kind, product, pick)
        .await
        .map_err(Into::into)
}

pub(crate) async fn bootstrap_oauth_client_typed(
    node: Arc<EmbeddedNode>,
    node_did: &str,
    provider: &str,
    refresh_kind: OAuthRefreshKind,
    product: OAuthProduct,
    pick: AccountPick<'_>,
) -> std::result::Result<
    (Arc<DbCredentialBearer>, OAuthCredential),
    crate::auth::credential::OAuthCredentialError,
> {
    let access = crate::auth::credential::runtime_credential_access(&node, node_did)?;
    let credential = resolve_oauth_credential(&access, node_did, provider, pick)
        .await
        .with_context(|| format!("loading OAuthCredential for node {node_did}"))?
        .ok_or_else(|| crate::auth::credential::OAuthCredentialAuthError {
            problem: OAuthAuthProblem::Missing,
            message: classify_oauth_auth_error(
                &product,
                node_did,
                provider,
                &OAuthAuthProblem::Missing,
            ),
        })?;
    let credential_id = credential.credential_id.clone();
    let provider = provider.to_string();
    let bearer = shared_bearer(&node, &credential_id, || {
        DbCredentialBearer::new(
            node.clone(),
            node_did,
            provider,
            credential_id.clone(),
            true,
            refresh_kind,
            product,
        )
        .with_cache_seed(credential.clone())
    });
    bearer.adopt_document(&credential).await;
    Ok((bearer, credential))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_rebuilt_client_does_not_serve_a_removed_sign_ins_cached_token() {
        use crate::auth::claude::{
            credential_from_login_tokens, ClaudeLoginTokens, CLAUDE_OAUTH_PRODUCT,
            CLAUDE_OAUTH_PROVIDER,
        };
        use crate::auth::credential::{
            remove_account_in_txn, store_sign_in, AccountPick, OAuthRefreshKind, SignInResult,
        };
        use gents_store::config_client::{ConfigAccess, NodePermission};
        use gents_store::identity::NodeIdentity;
        let (db, user, access) = crate::test_support::signed_test_node_without_agent(
            "rebuilt-oauth-client",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let node = db.node.clone();
        let did = db.node_identity.did();
        let runtime = ConfigAccess::local_as(node.clone(), did.parse().unwrap());
        runtime
            .grant_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentDelete)
            .await
            .unwrap();
        let sign_in = |who: &str, expires_in| {
            credential_from_login_tokens(
                did,
                CLAUDE_OAUTH_PROVIDER,
                &ClaudeLoginTokens {
                    access_token: format!("access-{who}"),
                    refresh_token: format!("refresh-{who}"),
                    expires_in: Some(expires_in),
                    scope: None,
                    account_id: Some(format!("label-{who}")),
                    organization_uuid: Some("org-1".into()),
                    account_uuid: Some(format!("account-{who}")),
                },
                chrono::Utc::now(),
            )
        };
        let bootstrap = || {
            bootstrap_oauth_client(
                node.clone(),
                did,
                CLAUDE_OAUTH_PROVIDER,
                OAuthRefreshKind::Claude,
                CLAUDE_OAUTH_PRODUCT,
                AccountPick::Reference(None),
            )
        };
        let a = store_sign_in(&access, sign_in("a", 7200), None)
            .await
            .unwrap();
        let (bearer, _) = bootstrap().await.unwrap();
        assert_eq!(bearer.current_bearer().await.unwrap(), "access-a");

        runtime.write(
            "test.same_document",
            &format!(
                r#"mutation {{ update_OAuthCredential(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ access_token: "access-a-stored" }}) {{ _docID }} }}"#,
                a.doc_id
            ),
        )
        .await
        .unwrap();
        let (bearer, _) = bootstrap().await.unwrap();
        assert_eq!(bearer.current_bearer().await.unwrap(), "access-a");

        let credential_id = a.credential.credential_id.as_str();
        access
            .transact_write("test.remove_account", |txn| {
                Box::pin(async move { remove_account_in_txn(txn, did, credential_id).await })
            })
            .await
            .unwrap();
        let b = store_sign_in(&access, sign_in("b", 3600), None)
            .await
            .unwrap();
        assert_eq!(b.result, SignInResult::Added);
        assert_eq!(b.credential.credential_id, a.credential.credential_id);
        assert_ne!(b.doc_id, a.doc_id);
        let (bearer, _) = bootstrap().await.unwrap();
        assert_eq!(bearer.current_bearer().await.unwrap(), "access-b");
    }
}
