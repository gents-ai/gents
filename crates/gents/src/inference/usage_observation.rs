//! Runtime-owned observations of provider limits. No usage observation is persisted.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
pub use gents_loop::account_usage;
use gents_loop::account_usage::{
    usage_from_headers, UsagePlan, UsageReport, UsageSource, READ_SKIP_WINDOW,
};
use rig::http_client::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::documents::{BackendAuth, InferenceBackend};
use crate::inference::auth::credential::{
    resolve_oauth_credential, AccountPick, BearerSource, OAuthCredential,
};
use crate::inference::backends::provider::BackendProviderOauthExt;
use crate::inference::resolved::ResolvedAgent;
use crate::store::config_client::ConfigAccess;

/// Provider clients bind a physical credential row or a keyed backend
/// connection identity. Inventory reads resolve the currently configured
/// account, so a late response cannot populate a replacement's observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageAccount {
    Credential {
        doc_id: Option<String>,
        node_did: String,
        provider: String,
        account_ref: Option<String>,
    },
    Backend {
        node_did: String,
        provider: String,
        backend_id: String,
        connection: String,
    },
}

impl UsageAccount {
    pub fn for_credential(row: &OAuthCredential) -> Self {
        Self::Credential {
            doc_id: row.doc_id.clone(),
            node_did: row.node_did.clone(),
            provider: row.provider.clone(),
            account_ref: row.account_ref.clone(),
        }
    }

    pub fn for_agent(agent: &ResolvedAgent) -> Option<Self> {
        Some(Self::Backend {
            node_did: agent.node_did().to_string(),
            provider: agent.backend_provider_kind.as_str().to_string(),
            backend_id: agent.backend_id.clone()?,
            connection: crate::completion_factory::agent_connection_fingerprint(agent),
        })
    }

    pub fn for_backend(node_did: &str, backend: &InferenceBackend) -> Self {
        match (backend.provider_kind.oauth_provider(), &backend.auth) {
            (Some(provider), BackendAuth::NodeOAuth { account_ref }) => Self::Credential {
                doc_id: None,
                node_did: node_did.to_string(),
                provider: provider.to_string(),
                account_ref: account_ref.clone(),
            },
            _ => Self::Backend {
                node_did: node_did.to_string(),
                provider: backend.provider_kind.as_str().to_string(),
                backend_id: backend.backend_id.clone(),
                connection: crate::inference::admission::backend_connection_fingerprint(
                    &backend.backend_fields(),
                ),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageObservation {
    pub report: UsageReport,
    pub observed_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
    pub read_error: Option<String>,
}

impl UsageObservation {
    fn merge(self, other: UsageObservation) -> UsageObservation {
        let (read_at, read_error) = if other.read_at > self.read_at {
            (other.read_at, other.read_error)
        } else {
            (self.read_at, self.read_error)
        };
        let report = self.report.merge(other.report);
        UsageObservation {
            observed_at: report.observed_at(),
            report,
            read_at,
            read_error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageView {
    pub windows: Vec<WindowView>,
    pub plan: Option<String>,
    pub note: Option<String>,
    pub read_at: Option<DateTime<Utc>>,
    pub read_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowView {
    pub label: String,
    pub window_minutes: Option<i64>,
    pub used_pct: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub resets_in_secs: Option<i64>,
    pub source: UsageSource,
    pub observed_at: DateTime<Utc>,
    pub age_secs: i64,
    pub last_known: bool,
}

pub fn usage_view(
    stored: Option<&UsageObservation>,
    kind: crate::BackendProviderKind,
    now: DateTime<Utc>,
) -> UsageView {
    use crate::BackendProviderKind as Kind;
    let empty = UsageObservation::default();
    let stored = stored.unwrap_or(&empty);
    let visible = account_usage::visible_windows(&stored.report, now);
    let windows: Vec<_> = visible
        .iter()
        .map(|(window, freshness)| WindowView {
            label: window.label.clone(),
            window_minutes: window.window_minutes,
            used_pct: window.used_pct,
            resets_at: window.resets_at,
            resets_in_secs: window.resets_at.map(|at| (at - now).num_seconds()),
            source: window.source,
            observed_at: window.observed_at,
            age_secs: (now - window.observed_at).num_seconds(),
            last_known: *freshness == account_usage::Freshness::Stale,
        })
        .collect();
    let read_ok = stored.read_at.is_some() && stored.read_error.is_none();
    let note = match kind {
        _ if !windows.is_empty() => None,
        Kind::OpenAiCompatible if stored.report.is_empty() && stored.read_at.is_none() => {
            Some("not reported")
        }
        Kind::OpenRouter if read_ok && stored.report.windows.is_empty() => {
            Some("no cap on this key")
        }
        _ => Some("unknown"),
    };
    UsageView {
        windows,
        plan: stored.report.plan.as_ref().map(|plan| plan.name.clone()),
        note: note.map(str::to_owned),
        read_at: stored.read_at,
        read_error: stored.read_error.clone(),
    }
}

/// One cache belongs to one runtime and is shared with its clients and local views.
/// Keys include the physical credential or the existing keyed connection fingerprint;
/// a late response from a replaced sign-in or edited backend cannot populate its successor.
#[derive(Debug, Default)]
pub struct UsageCache {
    endpoints: UsageEndpoints,
    entries: tokio::sync::Mutex<rapidhash::RapidHashMap<CacheKey, UsageObservation>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    node_did: String,
    provider: String,
    binding: String,
}

impl UsageCache {
    pub fn with_endpoints(endpoints: UsageEndpoints) -> Self {
        Self {
            endpoints,
            ..Default::default()
        }
    }

    pub fn endpoints(&self) -> &UsageEndpoints {
        &self.endpoints
    }

    async fn load(&self, target: &Target) -> Option<UsageObservation> {
        self.entries.lock().await.get(&target.key).cloned()
    }

    async fn record(
        &self,
        target: &Target,
        report: UsageReport,
        read: Option<(DateTime<Utc>, Option<String>)>,
    ) {
        if report.is_empty() && read.is_none() {
            return;
        }
        let observation = UsageObservation {
            observed_at: report.observed_at(),
            report,
            read_at: read.as_ref().map(|(at, _)| *at),
            read_error: read.and_then(|(_, error)| error),
        };
        let mut entries = self.entries.lock().await;
        let previous = entries.remove(&target.key).unwrap_or_default();
        entries.insert(target.key.clone(), previous.merge(observation));
    }
}

struct Target {
    credential_doc_id: Option<String>,
    key: CacheKey,
    plan: Option<UsagePlan>,
}

async fn target(access: &ConfigAccess, account: &UsageAccount) -> Result<Option<Target>> {
    match account {
        UsageAccount::Backend {
            node_did,
            provider,
            connection,
            ..
        } => Ok(Some(Target {
            credential_doc_id: None,
            key: CacheKey {
                node_did: node_did.clone(),
                provider: provider.clone(),
                binding: connection.clone(),
            },
            plan: None,
        })),
        UsageAccount::Credential {
            doc_id,
            node_did,
            provider,
            account_ref,
        } => {
            let pick = AccountPick::Reference(account_ref.as_deref());
            let Some(row) = resolve_oauth_credential(access, node_did, provider, pick).await?
            else {
                return Ok(None);
            };
            credential_target(node_did, provider, doc_id.as_deref(), row)
        }
    }
}

fn credential_target(
    node_did: &str,
    provider: &str,
    doc_id: Option<&str>,
    row: OAuthCredential,
) -> Result<Option<Target>> {
    if doc_id.is_some_and(|expected| row.doc_id.as_deref() != Some(expected)) {
        return Ok(None);
    }
    let binding = row.doc_id.clone().context("sign-in without _docID")?;
    Ok(Some(Target {
        credential_doc_id: row.doc_id.clone(),
        key: CacheKey {
            node_did: node_did.to_string(),
            provider: provider.to_string(),
            binding,
        },
        plan: row.chatgpt_plan_type.map(|name| UsagePlan {
            name,
            observed_at: row.last_refresh.unwrap_or_default(),
        }),
    }))
}

pub async fn load_usage(
    cache: &UsageCache,
    access: &ConfigAccess,
    account: &UsageAccount,
) -> Result<Option<UsageObservation>> {
    match target(access, account).await? {
        Some(target) => Ok(cache.load(&target).await),
        None => Ok(None),
    }
}

pub async fn record_usage(
    cache: &UsageCache,
    node: &Arc<EmbeddedNode>,
    account: &UsageAccount,
    report: UsageReport,
) -> Result<()> {
    write(cache, node, account, report, None).await
}

async fn write(
    cache: &UsageCache,
    node: &Arc<EmbeddedNode>,
    account: &UsageAccount,
    report: UsageReport,
    read: Option<(DateTime<Utc>, Option<String>)>,
) -> Result<()> {
    if let Some(target) = target(&ConfigAccess::local(node.clone()), account).await? {
        cache.record(&target, report, read).await;
    }
    Ok(())
}

pub struct UsageReporter {
    cache: Arc<UsageCache>,
    node: Arc<EmbeddedNode>,
    pub(crate) account: UsageAccount,
}

impl std::fmt::Debug for UsageReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageReporter").finish_non_exhaustive()
    }
}

impl UsageReporter {
    pub(crate) fn new(
        cache: Arc<UsageCache>,
        node: Arc<EmbeddedNode>,
        account: UsageAccount,
    ) -> Arc<Self> {
        Arc::new(Self {
            cache,
            node,
            account,
        })
    }

    /// The response never waits on credential resolution; cache writes cannot affect admission.
    pub(crate) fn observe(self: &Arc<Self>, headers: &HeaderMap, source: UsageSource) {
        let headers = headers
            .iter()
            .filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?)));
        let report = usage_from_headers(headers, source, Utc::now());
        if report.is_empty() {
            return;
        }
        let reporter = self.clone();
        tokio::spawn(async move {
            if let Err(error) =
                record_usage(&reporter.cache, &reporter.node, &reporter.account, report).await
            {
                tracing::warn!(error = %error, "recording provider usage failed");
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageTrigger {
    Cached,
    Open,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "reason", rename_all = "snake_case")]
pub enum UsageRead {
    Read,
    SkippedRecent,
    NotReported,
    Disabled,
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEndpoints {
    pub grok_billing: String,
    pub claude_usage: String,
}

impl Default for UsageEndpoints {
    fn default() -> Self {
        Self {
            grok_billing: format!(
                "{}/billing?format=credits",
                crate::inference::clients::xai_grok::XAI_GROK_OAUTH_BASE_URL
            ),
            claude_usage: "https://api.anthropic.com/api/oauth/usage".to_string(),
        }
    }
}

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type UsageParser = fn(&Value, DateTime<Utc>) -> Option<UsageReport>;

fn should_read(trigger: UsageTrigger, now: DateTime<Utc>, previous: Option<DateTime<Utc>>) -> bool {
    match trigger {
        UsageTrigger::Cached => false,
        UsageTrigger::Refresh => true,
        UsageTrigger::Open => previous.is_none_or(|previous| now - previous >= READ_SKIP_WINDOW),
    }
}

pub async fn read_account_usage(
    cache: &UsageCache,
    node: Arc<EmbeddedNode>,
    node_did: &str,
    backend: &InferenceBackend,
    trigger: UsageTrigger,
    endpoints: &UsageEndpoints,
    now: DateTime<Utc>,
) -> Result<UsageRead> {
    use crate::inference::backends::provider::BackendProviderKind as Kind;
    let kind = backend.provider_kind;
    if !backend.enabled {
        return Ok(UsageRead::Disabled);
    }
    if matches!(kind, Kind::OpenAiCompatible | Kind::AnthropicApiKey) {
        return Ok(UsageRead::NotReported);
    }
    let api_key = match kind {
        Kind::OpenRouter => match backend.auth.resolve_api_key() {
            Ok(Some(key)) => Some(key),
            _ => return Ok(unavailable("no key")),
        },
        _ => None,
    };
    let access = ConfigAccess::local(node.clone());
    let mut account = UsageAccount::for_backend(node_did, backend);
    let Some(target) = target(&access, &account).await? else {
        return Ok(unavailable("no enabled account"));
    };
    if let UsageAccount::Credential { doc_id, .. } = &mut account {
        *doc_id = target.credential_doc_id.clone();
    }
    let last_read = cache.load(&target).await.and_then(|stored| stored.read_at);
    if !should_read(trigger, now, last_read) {
        return Ok(UsageRead::SkippedRecent);
    }

    let http = reqwest::Client::builder().timeout(READ_TIMEOUT).build()?;
    let (request, parse): (reqwest::RequestBuilder, UsageParser) = match api_key {
        Some(key) => (
            http.get(format!("{}/key", backend.endpoint.trim_end_matches('/')))
                .bearer_auth(key),
            account_usage::openrouter_key,
        ),
        None => {
            let provider = kind.oauth_provider().context("backend has no usage read")?;
            let bootstrap = crate::inference::auth::http::bootstrap_oauth_client(
                node.clone(),
                node_did,
                provider,
                crate::inference::backends::health::oauth::oauth_refresh_kind(kind),
                crate::inference::backends::health::oauth::oauth_product(kind),
                AccountPick::Reference(backend.auth.oauth_account_ref()),
            )
            .await;
            let Ok((bearer, credential)) = bootstrap else {
                return Ok(unavailable("sign-in expired"));
            };
            let Ok(token) = bearer.current_bearer().await else {
                return Ok(unavailable("sign-in expired"));
            };
            let (request, parse): (reqwest::RequestBuilder, UsageParser) = match kind {
                Kind::ChatGptCodex => (
                    http.get(codex_usage_url(&backend.endpoint)).headers(
                        crate::inference::clients::chatgpt_codex::build_chatgpt_codex_headers(
                            credential.account_id.as_deref(),
                            credential.is_fedramp,
                        )?,
                    ),
                    account_usage::codex_usage,
                ),
                Kind::XaiGrokOAuth => (
                    http.get(&endpoints.grok_billing).headers(
                        crate::inference::clients::xai_grok::build_xai_grok_oauth_headers()?,
                    ),
                    account_usage::grok_billing,
                ),
                Kind::ClaudeCliSubscription => (
                    http.get(&endpoints.claude_usage).header(
                        "anthropic-beta",
                        crate::inference::clients::claude_messages::OAUTH_BETA,
                    ),
                    account_usage::claude_oauth_usage,
                ),
                // A new provider kind must fail to compile here rather than
                // send its bearer to another provider.
                Kind::OpenAiCompatible | Kind::OpenRouter | Kind::AnthropicApiKey => {
                    return Ok(UsageRead::NotReported);
                }
            };
            (request.bearer_auth(token), parse)
        }
    };

    let outcome = match request.send().await {
        Err(_) => Err("unreachable".to_string()),
        Ok(response) => match response.status().as_u16() {
            200..=299 => response
                .json::<Value>()
                .await
                .ok()
                .and_then(|body| parse(&body, now))
                .ok_or_else(|| "malformed".to_string()),
            401 | 403 => Err("unauthorized".to_string()),
            429 => Err("throttled".to_string()),
            status => Err(format!("http {status}")),
        },
    };
    match outcome {
        Ok(report) => {
            write(cache, &node, &account, report, Some((now, None))).await?;
            Ok(UsageRead::Read)
        }
        Err(error) => {
            write(
                cache,
                &node,
                &account,
                UsageReport::default(),
                Some((now, Some(error.clone()))),
            )
            .await?;
            Ok(UsageRead::Unavailable(error))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUsageRead {
    pub label: String,
    pub provider: String,
    pub account_ref: Option<String>,
    pub backend_id: Option<String>,
    pub outcome: UsageRead,
    pub usage: Option<UsageView>,
}

impl AccountUsageRead {
    pub fn matches_backend(&self, backend: &InferenceBackend) -> bool {
        match &self.backend_id {
            Some(id) => *id == backend.backend_id,
            None => backend
                .provider_kind
                .oauth_provider()
                .is_some_and(|provider| {
                    provider == self.provider
                        && backend.auth.oauth_account_ref() == self.account_ref.as_deref()
                }),
        }
    }
}

pub async fn read_node_usage(
    cache: &UsageCache,
    node: Arc<EmbeddedNode>,
    node_did: &str,
    trigger: UsageTrigger,
    provider: Option<&str>,
    endpoints: &UsageEndpoints,
    now: DateTime<Utc>,
) -> Result<Vec<AccountUsageRead>> {
    let access = ConfigAccess::local(node.clone());
    let accounts = crate::inference::auth::credential::list_accounts(&access, node_did).await?;
    let backends = access
        .transact_readonly("usage_observation.read_node", |txn| {
            Box::pin(async move {
                crate::store::config_client::list_inference_backends_in_txn(txn, node_did).await
            })
        })
        .await?;
    let mut seen = rapidhash::RapidHashSet::default();
    let mut jobs = Vec::new();
    for backend in &backends {
        let (entry, account_enabled) =
            match crate::inference::auth::credential::backend_account(backend, &accounts) {
                Some(None) => continue,
                Some(Some(account)) => {
                    if !backend.enabled
                        || provider.is_some_and(|provider| provider != account.provider)
                        || !seen.insert(account.credential_id.as_str())
                    {
                        continue;
                    }
                    (account_entry(account), account.enabled)
                }
                None if provider.is_some_and(|provider| {
                    !provider.eq_ignore_ascii_case(backend.provider_kind.as_str())
                }) =>
                {
                    continue
                }
                None => {
                    let entry = AccountUsageRead {
                        label: backend.name.clone(),
                        provider: backend.provider_kind.as_str().to_string(),
                        account_ref: None,
                        backend_id: Some(backend.backend_id.clone()),
                        outcome: UsageRead::Disabled,
                        usage: None,
                    };
                    (entry, true)
                }
            };
        jobs.push((entry, account_enabled, backend.clone()));
    }
    for account in &accounts {
        if provider.is_some_and(|provider| provider != account.provider)
            || seen.contains(account.credential_id.as_str())
        {
            continue;
        }
        let backend = crate::inference::auth::credential::preset_account_backend(
            node_did,
            &account.provider,
            account.account_ref.as_deref(),
            account.label.clone(),
        )?;
        jobs.push((account_entry(account), account.enabled, backend));
    }
    let reads = jobs.into_iter().map(|(entry, account_enabled, backend)| {
        let node = node.clone();
        async move {
            if !account_enabled {
                return entry;
            }
            let outcome = read_account_usage(
                cache,
                node.clone(),
                node_did,
                &backend,
                trigger,
                endpoints,
                now,
            )
            .await
            .unwrap_or_else(|_| unavailable("account unavailable"));
            let stored = usage_for_backend(cache, &ConfigAccess::local(node), node_did, &backend)
                .await
                .ok()
                .flatten();
            AccountUsageRead {
                outcome,
                usage: Some(usage_view(stored.as_ref(), backend.provider_kind, now)),
                ..entry
            }
        }
    });
    Ok(futures::future::join_all(reads).await)
}

fn account_entry(account: &crate::inference::auth::credential::AccountSummary) -> AccountUsageRead {
    AccountUsageRead {
        label: account.label.clone(),
        provider: account.provider.clone(),
        account_ref: account.account_ref.clone(),
        backend_id: None,
        outcome: UsageRead::Disabled,
        usage: None,
    }
}

fn unavailable(reason: &str) -> UsageRead {
    UsageRead::Unavailable(reason.to_string())
}

fn codex_usage_url(endpoint: &str) -> String {
    let base = crate::inference::clients::chatgpt_codex::normalize_endpoint(endpoint);
    format!(
        "{}/wham/usage",
        base.strip_suffix("/codex").unwrap_or(&base)
    )
}

pub async fn usage_for_backend(
    cache: &UsageCache,
    access: &ConfigAccess,
    node_did: &str,
    backend: &InferenceBackend,
) -> Result<Option<UsageObservation>> {
    let account = UsageAccount::for_backend(node_did, backend);
    let Some(target) = target(access, &account).await? else {
        return Ok(None);
    };
    let mut stored = cache.load(&target).await;
    if let Some(plan) = target.plan {
        let stored = stored.get_or_insert_with(UsageObservation::default);
        stored.report.plan.get_or_insert(plan);
    }
    Ok(stored)
}

pub(crate) struct ProviderLimitsTool {
    pub node: Arc<EmbeddedNode>,
    pub node_did: String,
    pub cache: Arc<UsageCache>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderLimitsArgs {}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ProviderLimitsError(#[from] anyhow::Error);

impl crate::inference::llm::tool::Tool for ProviderLimitsTool {
    const NAME: &'static str = "provider_limits";
    type Error = ProviderLimitsError;
    type Args = ProviderLimitsArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> crate::inference::llm::tool::ToolDefinition {
        crate::inference::llm::tool::ToolDefinition {
            name: Self::NAME.into(),
            description: "Read this node's provider limits, resets and freshness. Contacts usage endpoints when the last read is older than five minutes; selects no account.".into(),
            parameters: serde_json::json!({"type":"object", "properties":{}, "additionalProperties":false}),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<String, Self::Error> {
        let reads = read_node_usage(
            &self.cache,
            self.node.clone(),
            &self.node_did,
            UsageTrigger::Open,
            None,
            self.cache.endpoints(),
            Utc::now(),
        )
        .await?;
        let accounts: Vec<_> = reads
            .into_iter()
            .map(|read| {
                serde_json::json!({
                    "label": read.label, "provider": read.provider, "limits": read.usage, "read": read.outcome,
                })
            })
            .collect();
        serde_json::to_string(&serde_json::json!({"accounts": accounts}))
            .map_err(|error| ProviderLimitsError(error.into()))
    }
}

#[cfg(test)]
#[path = "usage_observation/tests.rs"]
mod tests;
