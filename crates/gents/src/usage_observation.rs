//! Provider-reported usage per node and provider account, kept in
//! `ProviderAccountUsage` and never on `OAuthCredential`: every credential
//! write reloads the runtime view.
//!
//! The process that hosts the embedded node and runs the provider clients is
//! the only writer, so writes take the node rather than a `ConfigAccess`.
//! Usage is an observation: it picks no account and gates nothing.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
pub use gents_loop::account_usage;
use gents_loop::account_usage::{
    usage_from_headers, UsagePlan, UsageReport, UsageSource, READ_SKIP_WINDOW, REWRITE_AFTER,
};
use gents_protocol::schemas::PROVIDER_ACCOUNT_USAGE_NAME as COLLECTION;
use rig::http_client::HeaderMap;
use serde_json::{json, Value};

use crate::backend_provider::BackendProviderOauthExt;
use crate::config::ResolvedBehavior;
use crate::config_client::ConfigAccess;
use crate::document_config::InferenceBackend;
use crate::graphql::escape_graphql_string;
use crate::oauth_credential::{
    resolve_oauth_credential, AccountPick, BearerSource, OAuthCredential,
};

/// The provider account usage belongs to, per agent. A credential account's
/// key is resolved when usage is written or read, so a key filled after the
/// client was built is used from the next response on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageAccount {
    Credential {
        agent_did: String,
        provider: String,
        account_ref: Option<String>,
    },
    Backend {
        agent_did: String,
        provider: String,
        backend_id: String,
    },
}

impl UsageAccount {
    pub fn for_credential(row: &OAuthCredential) -> Self {
        Self::Credential {
            agent_did: row.agent_did.clone(),
            provider: row.provider.clone(),
            account_ref: row.account_ref.clone(),
        }
    }

    /// An API-key backend's account; `None` without a backend id.
    pub fn for_behavior(behavior: &ResolvedBehavior) -> Option<Self> {
        Some(Self::Backend {
            agent_did: behavior.agent_did().to_string(),
            provider: behavior.backend_provider_kind.as_str().to_string(),
            backend_id: behavior.backend_id.clone()?,
        })
    }

    /// The account `backend` names for `agent_did` (account = backend).
    fn for_backend(agent_did: &str, backend: &InferenceBackend) -> Self {
        match backend.provider_kind.oauth_provider() {
            Some(provider) => Self::Credential {
                agent_did: agent_did.to_string(),
                provider: provider.to_string(),
                account_ref: backend.auth.oauth_account_ref().map(str::to_string),
            },
            None => Self::Backend {
                agent_did: agent_did.to_string(),
                provider: backend.provider_kind.as_str().to_string(),
                backend_id: backend.backend_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredUsage {
    pub report: UsageReport,
    pub observed_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
    pub read_error: Option<String>,
}

impl StoredUsage {
    fn merge(self, other: StoredUsage) -> StoredUsage {
        let (read_at, read_error) = if other.read_at > self.read_at {
            (other.read_at, other.read_error)
        } else {
            (self.read_at, self.read_error)
        };
        let report = self.report.merge(other.report);
        StoredUsage {
            observed_at: report.observed_at(),
            report,
            read_at,
            read_error,
        }
    }

    /// Only what was observed at or after `since`.
    fn since(mut self, since: DateTime<Utc>) -> StoredUsage {
        let report = &mut self.report;
        report.windows.retain(|window| window.observed_at >= since);
        report.credits = report.credits.take().filter(|c| c.observed_at >= since);
        report.plan = report.plan.take().filter(|plan| plan.observed_at >= since);
        self.observed_at = report.observed_at();
        if self.read_at.is_some_and(|read_at| read_at < since) {
            self.read_at = None;
            self.read_error = None;
        }
        self
    }
}

/// Where an account's usage is stored.
struct Target {
    agent_did: String,
    provider: String,
    /// Written here: the provider account key, else `reference`.
    key: String,
    /// `ref:<account_ref>` or `ref:original`, DID-free, for usage seen before
    /// the key is known. A new first sign-in after every account was removed
    /// reuses the reference, so its observations from before the sign-in's
    /// `connected_at` belong to the removed account.
    reference: Option<String>,
    connected_at: Option<DateTime<Utc>>,
    /// The plan the sign-in carries, for a report that has none.
    plan: Option<UsagePlan>,
}

/// `None` for a credential account with no enabled sign-in: disabled and
/// removed accounts get no usage written or loaded.
async fn target(access: &ConfigAccess, account: &UsageAccount) -> Result<Option<Target>> {
    match account {
        UsageAccount::Backend {
            agent_did,
            provider,
            backend_id,
        } => Ok(Some(Target {
            agent_did: agent_did.clone(),
            provider: provider.clone(),
            key: backend_id.clone(),
            reference: None,
            connected_at: None,
            plan: None,
        })),
        UsageAccount::Credential {
            agent_did,
            provider,
            account_ref,
        } => {
            let pick = AccountPick::Reference(account_ref.as_deref());
            let Some(row) = resolve_oauth_credential(access, agent_did, provider, pick).await?
            else {
                return Ok(None);
            };
            let reference = format!("ref:{}", account_ref.as_deref().unwrap_or("original"));
            Ok(Some(Target {
                agent_did: agent_did.clone(),
                provider: provider.clone(),
                key: row
                    .provider_account_key
                    .unwrap_or_else(|| reference.clone()),
                reference: Some(reference),
                connected_at: row.connected_at,
                plan: row.chatgpt_plan_type.map(|name| UsagePlan {
                    name,
                    observed_at: row.last_refresh.unwrap_or_default(),
                }),
            }))
        }
    }
}

fn row_query(target: &Target, key: &str) -> String {
    format!(
        r#"{{ {COLLECTION}(filter: {{ agent_did: {{ _eq: "{}" }}, provider: {{ _eq: "{}" }}, usage_key: {{ _eq: "{}" }} }}, limit: 1) {{ _docID report read_at read_error }} }}"#,
        escape_graphql_string(&target.agent_did),
        escape_graphql_string(&target.provider),
        escape_graphql_string(key),
    )
}

fn stored_row(response: &Value) -> Result<Option<(String, StoredUsage)>> {
    let Some(row) = response["data"][COLLECTION]
        .as_array()
        .and_then(|rows| rows.first())
    else {
        return Ok(None);
    };
    let doc_id = row["_docID"].as_str().context("usage row without _docID")?;
    // A report this build cannot read is replaced by the next observation.
    let report: UsageReport = serde_json::from_value(row["report"].clone()).unwrap_or_default();
    let read_at = row["read_at"]
        .as_str()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc));
    Ok(Some((
        doc_id.to_string(),
        StoredUsage {
            observed_at: report.observed_at(),
            report,
            read_at,
            read_error: row["read_error"].as_str().map(str::to_string),
        },
    )))
}

async fn load(access: &ConfigAccess, target: &Target) -> Result<Option<StoredUsage>> {
    let mut keys = vec![target.key.as_str()];
    keys.extend(target.reference.as_deref().filter(|key| *key != target.key));
    let mut loaded: Option<StoredUsage> = None;
    for key in keys {
        let response = access.execute(&row_query(target, key)).await?;
        let Some((_, mut stored)) = stored_row(&response)? else {
            continue;
        };
        if let Some(since) = target
            .connected_at
            .filter(|_| Some(key) == target.reference.as_deref())
        {
            stored = stored.since(since);
        }
        loaded = Some(match loaded {
            Some(loaded) => loaded.merge(stored),
            None => stored,
        });
    }
    Ok(loaded.filter(|stored| !stored.report.is_empty() || stored.read_at.is_some()))
}

/// Stored usage of `account`; `None` when nothing is stored or the account
/// has no enabled sign-in.
pub async fn load_usage(
    access: &ConfigAccess,
    account: &UsageAccount,
) -> Result<Option<StoredUsage>> {
    match target(access, account).await? {
        Some(target) => load(access, &target).await,
        None => Ok(None),
    }
}

/// Merge `report` into `account`'s stored usage.
pub async fn record_usage(
    node: &Arc<EmbeddedNode>,
    account: &UsageAccount,
    report: UsageReport,
) -> Result<()> {
    write(node, account, report, None).await
}

/// One read-merge-write transaction. `read` is an on-demand read's time and
/// error; header observations leave both as stored. Unchanged values are not
/// rewritten until the report is [`REWRITE_AFTER`] newer, which bounds
/// updates, and their replication, to about one a minute per account.
async fn write(
    node: &Arc<EmbeddedNode>,
    account: &UsageAccount,
    report: UsageReport,
    read: Option<(DateTime<Utc>, Option<String>)>,
) -> Result<()> {
    if report.is_empty() && read.is_none() {
        return Ok(());
    }
    let access = ConfigAccess::Local(node.clone());
    let Some(target) = target(&access, account).await? else {
        return Ok(());
    };
    let query = row_query(&target, &target.key);
    access
        .transact("usage_observation.record", |txn| {
            let (target, query, report, read) = (&target, &query, report.clone(), read.clone());
            Box::pin(async move {
                let (doc_id, stored) = match stored_row(&txn.execute(query).await?)? {
                    Some((doc_id, stored)) => (Some(doc_id), stored),
                    None => (None, StoredUsage::default()),
                };
                let merged = stored.report.clone().merge(report.clone());
                let recent = report
                    .observed_at()
                    .zip(stored.observed_at)
                    .is_some_and(|(new, old)| new - old < REWRITE_AFTER);
                if read.is_none() && recent && merged.same_values(&stored.report) {
                    return Ok(());
                }
                let mut input = json!({ "report": merged });
                if let Some(observed_at) = merged.observed_at() {
                    input["observed_at"] = json!(observed_at);
                }
                if let Some((read_at, read_error)) = read {
                    input["read_at"] = json!(read_at);
                    input["read_error"] = json!(read_error);
                }
                let response = match doc_id {
                    Some(doc_id) => {
                        txn.execute_with_variables(
                            &format!(
                                r#"mutation($input: {COLLECTION}MutationInputArg!) {{ update_{COLLECTION}(docID: "{}", input: $input) {{ _docID }} }}"#,
                                escape_graphql_string(&doc_id)
                            ),
                            &json!({ "input": input }),
                        )
                        .await?
                    }
                    None => {
                        input["agent_did"] = json!(target.agent_did);
                        input["provider"] = json!(target.provider);
                        input["usage_key"] = json!(target.key);
                        txn.execute_with_variables(
                            &format!(
                                "mutation($input: {COLLECTION}MutationInputArg!) {{ create_{COLLECTION}(input: $input) {{ _docID }} }}"
                            ),
                            &json!({ "input": input }),
                        )
                        .await?
                    }
                };
                anyhow::ensure!(
                    response.get("errors").is_none_or(Value::is_null),
                    "writing provider usage failed: {}",
                    response["errors"]
                );
                Ok(())
            })
        })
        .await
}

/// Records the usage headers of one provider client's responses for the
/// account that client serves.
pub struct UsageReporter {
    node: Arc<EmbeddedNode>,
    pub(crate) account: UsageAccount,
}

impl std::fmt::Debug for UsageReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageReporter").finish_non_exhaustive()
    }
}

impl UsageReporter {
    pub(crate) fn new(node: Arc<EmbeddedNode>, account: UsageAccount) -> Arc<Self> {
        Arc::new(Self { node, account })
    }

    /// Parses now and writes in its own task, so a response never waits on
    /// the node's write gate. The spawned task does not inherit the caller's
    /// transaction (a task-local), so the write is never nested in it.
    // ponytail: one task per response, unbounded; most write nothing (no
    // rewrite of unchanged values). Coalesce per account if the write gate
    // shows contention.
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
            if let Err(error) = record_usage(&reporter.node, &reporter.account, report).await {
                tracing::warn!(error = %error, "recording provider usage failed");
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTrigger {
    /// The account list was opened.
    Open,
    /// The user asked for fresh usage.
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageRead {
    Read,
    SkippedRecent,
    /// Claude's usage endpoint is read only on an explicit refresh.
    SkippedUntilRefresh,
    /// The provider has no usage read.
    NotReported,
    Disabled,
    Unavailable(String),
}

/// Usage endpoints that do not follow from the backend endpoint.
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
                crate::xai_grok_oauth::XAI_GROK_OAUTH_BASE_URL
            ),
            claude_usage: "https://api.anthropic.com/api/oauth/usage".to_string(),
        }
    }
}

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type UsageParser = fn(&Value, DateTime<Utc>) -> Option<UsageReport>;

/// Reads `backend`'s usage from its provider on demand. Runs in the runtime:
/// an OAuth account's bearer comes from the refresh owner, so an expired
/// sign-in is renewed here like a request would renew it (one credential
/// write, one runtime view reload). Every skip happens before any bearer is
/// touched, so a skipped read never refreshes.
// ponytail: the skip check is not atomic, so two concurrent Opens of one
// account can both read upstream; add a per-account in-flight guard if it shows.
pub async fn read_account_usage(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    backend: &InferenceBackend,
    trigger: UsageTrigger,
    endpoints: &UsageEndpoints,
    now: DateTime<Utc>,
) -> Result<UsageRead> {
    use crate::backend_provider::BackendProviderKind as Kind;
    let kind = backend.provider_kind;
    if !backend.enabled {
        return Ok(UsageRead::Disabled);
    }
    match (kind, trigger) {
        (Kind::OpenAiCompatible, _) => return Ok(UsageRead::NotReported),
        (Kind::ClaudeCliSubscription, UsageTrigger::Open) => {
            return Ok(UsageRead::SkippedUntilRefresh)
        }
        _ => {}
    }
    let api_key = match kind {
        Kind::OpenRouter => match backend.auth.resolve_api_key() {
            Ok(Some(key)) => Some(key),
            _ => return Ok(unavailable("no key")),
        },
        _ => None,
    };
    let access = ConfigAccess::Local(node.clone());
    let account = UsageAccount::for_backend(agent_did, backend);
    let Some(target) = target(&access, &account).await? else {
        return Ok(unavailable("no enabled account"));
    };
    let last_read = load(&access, &target)
        .await?
        .and_then(|stored| stored.read_at);
    if last_read.is_some_and(|read_at| now - read_at < READ_SKIP_WINDOW) {
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
            let bootstrap = crate::oauth_http::bootstrap_oauth_client(
                node.clone(),
                agent_did,
                provider,
                crate::backend_health::oauth_refresh_kind(kind),
                crate::backend_health::oauth_product(kind),
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
                        crate::chatgpt_codex::build_chatgpt_codex_headers(
                            credential.account_id.as_deref(),
                            credential.is_fedramp,
                        )?,
                    ),
                    account_usage::codex_usage,
                ),
                Kind::XaiGrokOAuth => (
                    http.get(&endpoints.grok_billing)
                        .headers(crate::xai_grok_oauth::build_xai_grok_oauth_headers()?),
                    account_usage::grok_billing,
                ),
                _ => (
                    http.get(&endpoints.claude_usage)
                        .header("anthropic-beta", crate::claude_messages::OAUTH_BETA),
                    account_usage::claude_oauth_usage,
                ),
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
            write(&node, &account, report, Some((now, None))).await?;
            Ok(UsageRead::Read)
        }
        Err(error) => {
            write(
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

fn unavailable(reason: &str) -> UsageRead {
    UsageRead::Unavailable(reason.to_string())
}

/// `/wham/usage` on the backend's host: the Responses base without its
/// trailing `/codex` (default `https://chatgpt.com/backend-api/wham/usage`).
fn codex_usage_url(endpoint: &str) -> String {
    let base = crate::chatgpt_codex::normalize_endpoint(endpoint);
    format!(
        "{}/wham/usage",
        base.strip_suffix("/codex").unwrap_or(&base)
    )
}

/// Stored usage of the account `backend` names for `agent_did`, with the
/// sign-in's plan when the report has none.
pub async fn usage_for_backend(
    access: &ConfigAccess,
    agent_did: &str,
    backend: &InferenceBackend,
) -> Result<Option<StoredUsage>> {
    let account = UsageAccount::for_backend(agent_did, backend);
    let Some(target) = target(access, &account).await? else {
        return Ok(None);
    };
    let mut stored = load(access, &target).await?;
    if let Some(plan) = target.plan {
        let stored = stored.get_or_insert_with(StoredUsage::default);
        stored.report.plan.get_or_insert(plan);
    }
    Ok(stored)
}

#[cfg(test)]
#[path = "usage_observation/tests.rs"]
mod tests;
