//! `gents accounts`: the subscription accounts signed in on this node, and the
//! backends that use no account, with the profiles that use each.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use gents::backend_provider::BackendProviderOauthExt;
use gents::document_config::InferenceProfile;
use gents::oauth_credential::{backend_account, AccountSummary};
use gents::usage_observation::{
    load_usage, usage_view, AccountUsageRead, UsageAccount, UsageRead, UsageTrigger, UsageView,
};
use gents::{BackendProviderKind, InferenceBackend};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::args::{AccountsCommand, AccountsTargetArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_agent_did, resolve_config_access};

/// One `accounts list` row: a sign-in account, a backend that uses no
/// account, or a subscription backend whose account is not on this node.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AccountRow {
    pub(crate) provider: String,
    pub(crate) label: String,
    pub(crate) identity: Option<String>,
    pub(crate) plan: Option<String>,
    pub(crate) status: String,
    pub(crate) default: bool,
    pub(crate) credential_id: Option<String>,
    pub(crate) account_ref: Option<String>,
    pub(crate) backend_id: Option<String>,
    pub(crate) profiles: Vec<String>,
    /// `None` for a disabled account or backend and an account not on this node.
    pub(crate) usage: Option<UsageView>,
    /// This listing's on-demand read, when the runtime ran one.
    pub(crate) read: Option<UsageRead>,
}

/// A usage read request for the runtime, signed by the operator identity
/// like the enrollment operator commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageReadCommand {
    pub(crate) trigger: UsageTrigger,
    pub(crate) provider: Option<String>,
    pub(crate) signer_did: String,
    pub(crate) issued_at: String,
    pub(crate) nonce: String,
    pub(crate) sig: Vec<u8>,
}

const USAGE_READ_SIGNATURE_DOMAIN: &str = "gents-account-usage-read-v1";

impl UsageReadCommand {
    pub(crate) async fn signed(
        identity: &dyn gents::AgentIdentity,
        trigger: UsageTrigger,
        provider: Option<&str>,
    ) -> Result<Self> {
        let mut command = Self {
            trigger,
            provider: provider.map(str::to_owned),
            signer_did: identity.did().to_owned(),
            issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            sig: Vec::new(),
        };
        command.sig = identity
            .sign(&command.signing_payload())
            .await
            .context("signing the usage read request")?;
        Ok(command)
    }

    pub(crate) fn signing_payload(&self) -> Vec<u8> {
        let trigger = match self.trigger {
            UsageTrigger::Open => "open",
            UsageTrigger::Refresh => "refresh",
        };
        gents_protocol::enrollment::canonical_domain_payload(
            USAGE_READ_SIGNATURE_DOMAIN,
            [
                trigger,
                self.provider.as_deref().unwrap_or_default(),
                &self.signer_did,
                &self.issued_at,
                &self.nonce,
            ],
        )
    }

    /// Issued within the last minute (5 s of clock skew ahead), as the
    /// enrollment operator commands.
    pub(crate) fn validate_at(&self, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
        anyhow::ensure!(self.sig.len() == 64, "invalid usage read signature length");
        let issued = chrono::DateTime::parse_from_rfc3339(&self.issued_at)
            .context("usage read issued_at")?
            .with_timezone(&chrono::Utc);
        anyhow::ensure!(
            issued <= now + chrono::Duration::seconds(5),
            "usage read request is issued in the future"
        );
        anyhow::ensure!(
            now - issued <= chrono::Duration::seconds(60),
            "usage read request expired"
        );
        Ok(())
    }
}

/// The runtime's answer to a [`UsageReadCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UsageReads {
    pub agent_did: String,
    pub reads: Vec<AccountUsageRead>,
}

pub(crate) async fn dispatch(command: AccountsCommand) -> Result<()> {
    match command {
        AccountsCommand::List {
            target,
            output,
            refresh,
        } => {
            let (access, home_dir) =
                resolve_config_access(target.home.as_deref(), target.graphql.as_deref()).await?;
            let did = resolve_agent_did(Some(&home_dir), target.agent_did.as_deref())?;
            let rows = list_with_usage(
                &access,
                &home_dir,
                &did,
                target.provider.as_deref(),
                refresh,
                chrono::Utc::now(),
                &mut std::io::stderr(),
            )
            .await?;
            match output
                .ensure_supported("accounts list", &[OutputFormat::Table, OutputFormat::Json])?
            {
                OutputFormat::Json => print_json(&serde_json::to_value(rows)?),
                _ => {
                    print!("{}", render_table(&rows));
                    Ok(())
                }
            }
        }
        AccountsCommand::Label {
            target,
            account,
            label,
        } => {
            let (access, did) = target_access(&target).await?;
            print_json(
                &label_account(&access, &did, &account, target.provider.as_deref(), &label).await?,
            )
        }
        AccountsCommand::Disable { target, account } => {
            let (access, did) = target_access(&target).await?;
            let result = disable_account(
                &access,
                &did,
                &account,
                target.provider.as_deref(),
                &mut std::io::stderr(),
            )
            .await?;
            print_json(&result)
        }
        AccountsCommand::Remove {
            target,
            account,
            yes,
        } => {
            let (access, did) = target_access(&target).await?;
            warn_profiles(
                &access,
                &did,
                &account,
                target.provider.as_deref(),
                &mut std::io::stderr(),
            )
            .await?;
            let confirmed = yes
                || (std::io::stdin().is_terminal()
                    && std::io::stderr().is_terminal()
                    && crate::interactive_backend::confirm(
                        &format!("Remove account {account:?} from this node?"),
                        false,
                    )
                    .await);
            let result = remove_account(
                &access,
                &did,
                &account,
                target.provider.as_deref(),
                confirmed,
            )
            .await?;
            print_json(&result)
        }
    }
}

async fn target_access(target: &AccountsTargetArgs) -> Result<(crate::CommandAccess, String)> {
    let (access, home_dir) =
        resolve_config_access(target.home.as_deref(), target.graphql.as_deref()).await?;
    let did = resolve_agent_did(Some(&home_dir), target.agent_did.as_deref())?;
    Ok((access, did))
}

/// The principal's accounts and the configuration that uses them.
pub(crate) struct Snapshot {
    pub(crate) accounts: Vec<AccountSummary>,
    pub(crate) backends: Vec<InferenceBackend>,
    pub(crate) profiles: Vec<InferenceProfile>,
}

pub(crate) async fn snapshot(access: &ConfigAccess, agent_did: &str) -> Result<Snapshot> {
    let accounts = gents::oauth_credential::list_accounts(access, agent_did).await?;
    let (backends, profiles) = access
        .transact("accounts.config", |txn| {
            Box::pin(async move {
                Ok((
                    gents::config_client::list_inference_backends_in_txn(txn, agent_did).await?,
                    gents::config_client::list_inference_profiles_in_txn(txn, agent_did).await?,
                ))
            })
        })
        .await?;
    Ok(Snapshot {
        accounts,
        backends,
        profiles,
    })
}

impl Snapshot {
    fn profiles_on(&self, backend_ids: &[&str]) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|profile| backend_ids.contains(&profile.backend_id.as_str()))
            .map(|profile| profile.profile_id.clone())
            .collect()
    }

    /// Profiles whose backend runs on `account`.
    fn account_profiles(&self, account: &AccountSummary) -> Vec<String> {
        let serving: Vec<_> = self
            .backends
            .iter()
            .filter(|backend| {
                backend_account(backend, &self.accounts)
                    .flatten()
                    .is_some_and(|found| found.credential_id == account.credential_id)
            })
            .map(|backend| backend.backend_id.as_str())
            .collect();
        self.profiles_on(&serving)
    }

    /// The one account `needle` (label, `credential_id` or `account_ref`)
    /// names, among `provider`'s when given.
    pub(crate) fn pick(&self, needle: &str, provider: Option<&str>) -> Result<&AccountSummary> {
        let candidates: Vec<_> = self
            .accounts
            .iter()
            .filter(|account| provider.is_none_or(|provider| account.provider == provider))
            .filter(|account| {
                account.label == needle
                    || account.credential_id == needle
                    || account.account_ref.as_deref() == Some(needle)
            })
            .collect();
        match candidates.as_slice() {
            [one] => Ok(one),
            [] if self
                .backends
                .iter()
                .any(|backend| backend.name == needle || backend.backend_id == needle) =>
            {
                anyhow::bail!(
                    "{needle:?} is a backend, not a signed-in account; manage it with `gents config backend`"
                )
            }
            [] => anyhow::bail!(
                "no account {needle:?} on this node; `gents accounts list` shows them"
            ),
            many => anyhow::bail!(
                "{needle:?} names several accounts: {}; pass --provider or one of these ids",
                many.iter()
                    .map(|account| account.credential_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

fn provider_kind_name(backend: &InferenceBackend) -> String {
    serde_json::to_value(backend.provider_kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn stored_view(
    access: &ConfigAccess,
    account: &UsageAccount,
    kind: BackendProviderKind,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<UsageView> {
    Ok(usage_view(
        load_usage(access, account).await?.as_ref(),
        kind,
        now,
    ))
}

pub(crate) async fn account_rows(
    access: &ConfigAccess,
    agent_did: &str,
    provider: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<AccountRow>> {
    let snapshot = snapshot(access, agent_did).await?;
    let mut rows = Vec::new();
    for account in snapshot
        .accounts
        .iter()
        .filter(|account| provider.is_none_or(|provider| account.provider == provider))
    {
        let kind = BackendProviderKind::ALL
            .into_iter()
            .find(|kind| kind.oauth_provider() == Some(account.provider.as_str()));
        let usage = match kind {
            Some(kind) if account.enabled => {
                let usage_account = UsageAccount::Credential {
                    doc_id: None,
                    agent_did: agent_did.to_owned(),
                    provider: account.provider.clone(),
                    account_ref: account.account_ref.clone(),
                };
                Some(stored_view(access, &usage_account, kind, now).await?)
            }
            _ => None,
        };
        rows.push(AccountRow {
            provider: account.provider.clone(),
            label: account.label.clone(),
            identity: account.identity.clone(),
            plan: account.plan.clone(),
            status: if account.enabled {
                "enabled"
            } else {
                "disabled"
            }
            .to_owned(),
            default: account.default,
            credential_id: Some(account.credential_id.clone()),
            account_ref: account.account_ref.clone(),
            backend_id: None,
            profiles: snapshot.account_profiles(account),
            usage,
            read: None,
        });
    }
    if provider.is_some() {
        return Ok(rows);
    }
    for backend in &snapshot.backends {
        let status = match backend_account(backend, &snapshot.accounts) {
            Some(Some(_)) => continue,
            Some(None) => gents::oauth_credential::AccountState::Missing.as_str(),
            None if backend.enabled => "enabled",
            None => "disabled",
        };
        let usage = if status == "enabled" {
            let usage_account = UsageAccount::Backend {
                agent_did: agent_did.to_owned(),
                provider: backend.provider_kind.as_str().to_owned(),
                backend_id: backend.backend_id.clone(),
            };
            Some(stored_view(access, &usage_account, backend.provider_kind, now).await?)
        } else {
            None
        };
        rows.push(AccountRow {
            provider: provider_kind_name(backend),
            label: backend.name.clone(),
            identity: None,
            plan: None,
            status: status.to_owned(),
            default: false,
            credential_id: None,
            account_ref: backend.auth.oauth_account_ref().map(str::to_owned),
            backend_id: Some(backend.backend_id.clone()),
            profiles: snapshot.profiles_on(&[backend.backend_id.as_str()]),
            usage,
            read: None,
        });
    }
    Ok(rows)
}

/// Asks the runtime behind `graphql` to read usage now.
pub async fn request_usage_reads(
    identity: &dyn gents::AgentIdentity,
    graphql: &gents::config_client::GraphqlEndpoint,
    trigger: UsageTrigger,
    provider: Option<&str>,
) -> Result<UsageReads> {
    let command = UsageReadCommand::signed(identity, trigger, provider).await?;
    let mut url = reqwest::Url::parse(graphql.url()).context("parsing runtime GraphQL endpoint")?;
    url.set_path("/accounts/usage/read");
    url.set_query(None);
    url.set_fragment(None);
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(url)
        .json(&command)
        .send()
        .await
        .context("asking the runtime to read usage")?;
    let status = response.status();
    if !status.is_success() {
        let body: Value = response.json().await.unwrap_or_default();
        anyhow::bail!(
            "runtime refused the usage read ({status}): {}",
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    response
        .json()
        .await
        .context("decoding the runtime's usage reads")
}

/// Labels rows with the reads the runtime ran for `agent_did`.
pub(crate) fn attach_reads(rows: &mut [AccountRow], reads: &UsageReads, agent_did: &str) {
    if reads.agent_did != agent_did {
        return;
    }
    for read in &reads.reads {
        let row = rows.iter_mut().find(|row| match &read.backend_id {
            Some(backend_id) => row.backend_id.as_ref() == Some(backend_id),
            None => {
                row.backend_id.is_none()
                    && row.provider == read.provider
                    && row.account_ref == read.account_ref
            }
        });
        if let Some(row) = row {
            row.read = Some(read.outcome.clone());
        }
    }
}

/// The account rows, after the runtime read usage when one is running.
/// Reads run only in the runtime, which holds the sign-ins' refresh owner.
pub(crate) async fn list_with_usage(
    access: &ConfigAccess,
    home: &std::path::Path,
    agent_did: &str,
    provider: Option<&str>,
    refresh: bool,
    now: chrono::DateTime<chrono::Utc>,
    warnings: &mut impl Write,
) -> Result<Vec<AccountRow>> {
    let reads = match access {
        ConfigAccess::Graphql(graphql) => {
            let trigger = if refresh {
                UsageTrigger::Refresh
            } else {
                UsageTrigger::Open
            };
            let requested = async {
                let identity =
                    crate::commands::p2p::enrollment_admin::resolve_home_identity(Some(home))?;
                request_usage_reads(identity.as_ref(), graphql, trigger, provider).await
            }
            .await;
            match requested {
                Ok(reads) => Some(reads),
                Err(error) if refresh => return Err(error),
                Err(error) => {
                    writeln!(warnings, "usage not read: {error:#}")?;
                    None
                }
            }
        }
        ConfigAccess::Local(_) => {
            anyhow::ensure!(
                !refresh,
                "usage refresh runs in the runtime; start it with `gents serve` or list without --refresh"
            );
            None
        }
    };
    let mut rows = account_rows(access, agent_did, provider, now).await?;
    if let Some(reads) = reads {
        attach_reads(&mut rows, &reads, agent_did);
    }
    Ok(rows)
}

/// `<1m`, `45m`, `2h13m`, `5d3h`.
pub(crate) fn short_duration(secs: i64) -> String {
    // A row replicated from a host whose clock runs ahead has a negative age.
    let secs = secs.max(0);
    let (days, hours, minutes) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "<1m".to_owned(),
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h{minutes}m"),
        (days, 0, _) => format!("{days}d"),
        (days, hours, _) => format!("{days}d{hours}h"),
    }
}

/// WINDOW, USED, RESETS, SOURCE and AGE: one line per visible window.
fn usage_cells(row: &AccountRow) -> Vec<[String; 5]> {
    let dash = || "-".to_owned();
    let Some(usage) = &row.usage else {
        return vec![std::array::from_fn(|_| dash())];
    };
    let reason = match &row.read {
        Some(UsageRead::Unavailable(reason)) => Some(reason.as_str()),
        _ => usage.read_error.as_deref(),
    }
    .map(|reason| format!("(read: {reason})"));
    if usage.windows.is_empty() {
        return vec![[
            dash(),
            usage.note.unwrap_or("unknown").to_owned(),
            dash(),
            reason.unwrap_or_else(dash),
            dash(),
        ]];
    }
    usage
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let label = match window
                .window_minutes
                .map(|minutes| short_duration(minutes * 60))
            {
                Some(short) if short != window.label => format!("{} ({short})", window.label),
                _ => window.label.clone(),
            };
            let resets = window.resets_at.map_or_else(dash, |at| {
                format!(
                    "{} (in {})",
                    at.format("%Y-%m-%d %H:%MZ"),
                    short_duration(window.resets_in_secs.unwrap_or_default())
                )
            });
            let source = match window.source {
                gents::usage_observation::account_usage::UsageSource::Header => "headers",
                gents::usage_observation::account_usage::UsageSource::Endpoint => "read",
                gents::usage_observation::account_usage::UsageSource::Error => "rejected",
            };
            let source = match (&reason, index) {
                (Some(reason), 0) => format!("{source} {reason}"),
                _ => source.to_owned(),
            };
            let mut age = short_duration(window.age_secs);
            if window.last_known {
                age.push_str(", last known");
            }
            [
                label,
                format!("{}%", window.used_pct.round()),
                resets,
                source,
                age,
            ]
        })
        .collect()
}

pub(crate) fn render_table(rows: &[AccountRow]) -> String {
    let headers = [
        "PROVIDER", "LABEL", "IDENTITY", "PLAN", "STATUS", "DEFAULT", "PROFILES", "WINDOW", "USED",
        "RESETS", "SOURCE", "AGE",
    ];
    let cell = |value: Option<&str>| {
        value
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("-")
            .to_owned()
    };
    let mut lines: Vec<Vec<String>> = vec![headers.map(str::to_owned).to_vec()];
    for row in rows {
        let account = [
            row.provider.clone(),
            row.label.clone(),
            cell(row.identity.as_deref()),
            cell(row.plan.as_deref()),
            row.status.clone(),
            if row.default { "yes" } else { "-" }.to_owned(),
            cell(Some(&row.profiles.join(","))),
        ];
        for (index, usage) in usage_cells(row).into_iter().enumerate() {
            let lead = if index == 0 {
                account.clone()
            } else {
                Default::default()
            };
            lines.push(lead.into_iter().chain(usage).collect());
        }
    }
    let mut widths = vec![0; headers.len()];
    for line in &lines {
        for (width, value) in widths.iter_mut().zip(line) {
            *width = (*width).max(value.chars().count());
        }
    }
    lines
        .iter()
        .map(|cells| {
            let mut line = cells
                .iter()
                .zip(&widths)
                .map(|(value, &width)| format!("{value:<width$}"))
                .collect::<Vec<_>>()
                .join("  ");
            line.truncate(line.trim_end().len());
            line + "\n"
        })
        .collect()
}

pub(crate) async fn resolve_account(
    access: &ConfigAccess,
    agent_did: &str,
    account: &str,
    provider: Option<&str>,
) -> Result<AccountSummary> {
    Ok(snapshot(access, agent_did)
        .await?
        .pick(account, provider)?
        .clone())
}

pub(crate) async fn label_account(
    access: &ConfigAccess,
    agent_did: &str,
    account: &str,
    provider: Option<&str>,
    label: &str,
) -> Result<Value> {
    let account = resolve_account(access, agent_did, account, provider).await?;
    gents::oauth_credential::set_account_label(access, agent_did, &account.credential_id, label)
        .await?;
    Ok(json!({
        "credential_id": account.credential_id,
        "provider": account.provider,
        "label": gents::oauth_credential::validate_account_label(label)?,
    }))
}

/// Say which profiles fail their next turn once `account` stops.
async fn warn_profiles(
    access: &ConfigAccess,
    agent_did: &str,
    account: &str,
    provider: Option<&str>,
    warnings: &mut impl Write,
) -> Result<()> {
    let snapshot = snapshot(access, agent_did).await?;
    let profiles = snapshot.account_profiles(snapshot.pick(account, provider)?);
    if !profiles.is_empty() {
        writeln!(
            warnings,
            "These profiles use this account and fail their next turn until moved to another \
             backend: {}",
            profiles.join(", ")
        )?;
    }
    Ok(())
}

pub(crate) async fn disable_account(
    access: &ConfigAccess,
    agent_did: &str,
    account: &str,
    provider: Option<&str>,
    warnings: &mut impl Write,
) -> Result<Value> {
    warn_profiles(access, agent_did, account, provider, warnings).await?;
    let snapshot = snapshot(access, agent_did).await?;
    let account = snapshot.pick(account, provider)?;
    gents::oauth_credential::set_account_enabled(access, agent_did, &account.credential_id, false)
        .await?;
    Ok(json!({
        "credential_id": account.credential_id,
        "provider": account.provider,
        "label": account.label,
        "enabled": false,
        "profiles": snapshot.account_profiles(account),
    }))
}

/// Delete the account's row and, in the same transaction, the backends
/// sign-in created for it (its reference) that no profile uses. A backend
/// with no reference is never deleted: it is the provider's own backend.
pub async fn remove_account(
    access: &ConfigAccess,
    agent_did: &str,
    account: &str,
    provider: Option<&str>,
    confirmed: bool,
) -> Result<Value> {
    let snapshot = snapshot(access, agent_did).await?;
    let account = snapshot.pick(account, provider)?.clone();
    anyhow::ensure!(
        confirmed,
        "removing an account asks for confirmation; pass --yes when not running in a terminal"
    );
    let (deleted, kept) = access
        .transact("accounts.remove", |txn| {
            let account = account.clone();
            Box::pin(async move {
                gents::oauth_credential::remove_account_in_txn(
                    txn,
                    agent_did,
                    &account.credential_id,
                )
                .await?;
                let profiles =
                    gents::config_client::list_inference_profiles_in_txn(txn, agent_did).await?;
                let (kept, deleted): (Vec<_>, Vec<_>) =
                    gents::config_client::list_inference_backends_in_txn(txn, agent_did)
                        .await?
                        .into_iter()
                        .filter(|backend| {
                            account.account_ref.is_some()
                                && backend_account(backend, std::slice::from_ref(&account))
                                    .flatten()
                                    .is_some()
                        })
                        .map(|backend| backend.backend_id)
                        .partition(|backend_id| {
                            profiles
                                .iter()
                                .any(|profile| &profile.backend_id == backend_id)
                        });
                crate::config_import::apply_delete_collection(
                    txn,
                    gents::Collection::InferenceBackend,
                    agent_did,
                    &deleted,
                )
                .await?;
                Ok((deleted, kept))
            })
        })
        .await?;
    Ok(json!({
        "removed": account.credential_id,
        "provider": account.provider,
        "label": account.label,
        "profiles": snapshot.account_profiles(&account),
        "deleted_backends": deleted,
        "kept_backends": kept,
    }))
}

/// Run `probe` on each enabled account of `provider`, each resolved by its
/// own reference, and return one block per account, label first. A disabled
/// account is listed, not probed. A failing probe does not stop the others;
/// the result is then an error that carries every block and names the
/// failures.
pub(crate) async fn probe_each_account<F, Fut>(
    access: &ConfigAccess,
    agent_did: &str,
    provider: &str,
    probe: F,
) -> Result<Vec<String>>
where
    F: Fn(gents::oauth_credential::OAuthCredential) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let mut blocks = Vec::new();
    let mut failed = Vec::new();
    for account in gents::oauth_credential::list_accounts(access, agent_did)
        .await?
        .into_iter()
        .filter(|account| account.provider == provider)
    {
        if !account.enabled {
            blocks.push(format!("{}: disabled, not probed", account.label));
            continue;
        }
        let probed = match gents::oauth_credential::resolve_oauth_credential(
            access,
            agent_did,
            provider,
            gents::oauth_credential::AccountPick::Reference(account.account_ref.as_deref()),
        )
        .await?
        {
            Some(credential) => probe(credential).await,
            None => Err(anyhow::anyhow!("the account is no longer stored")),
        };
        match probed {
            Ok(text) => blocks.push(format!("{}\n{text}", account.label)),
            Err(error) => {
                blocks.push(format!("{}: probe failed: {error:#}", account.label));
                failed.push(account.label);
            }
        }
    }
    anyhow::ensure!(
        failed.is_empty(),
        "{}\n\nprobe failed for: {}",
        blocks.join("\n\n"),
        failed.join(", ")
    );
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::document_config::BackendAuth;
    use gents::oauth_credential::{list_oauth_credentials_on, upsert_oauth_credential_on};
    use gents::{BackendProviderKind, InferenceBackend};
    use serde_json::json;
    use std::sync::Arc;

    const DID: &str = "did:key:z6MkTestAccounts";
    const CHATGPT: &str = "chatgpt-codex";
    const CLAUDE: &str = "claude-subscription";
    const GROK: &str = "xai-oauth";

    async fn seed_account(
        access: &ConfigAccess,
        provider: &str,
        account_ref: Option<&str>,
        label: Option<&str>,
    ) {
        let mut credential = gents::claude_oauth::credential_from_login_tokens(
            DID,
            provider,
            &gents::claude_oauth::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: "refresh-SECRET".into(),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("identity-{}", account_ref.unwrap_or("original"))),
                organization_uuid: None,
                account_uuid: None,
            },
            chrono::Utc::now(),
        );
        if let Some(account_ref) = account_ref {
            credential.credential_id = format!("{provider}:{DID}:{account_ref}");
            credential.account_ref = Some(account_ref.to_owned());
            credential.connected_at = Some(chrono::Utc::now());
        }
        credential.label = label.map(str::to_owned);
        upsert_oauth_credential_on(access, &credential)
            .await
            .unwrap();
    }

    fn backend(
        backend_id: &str,
        name: &str,
        provider_kind: BackendProviderKind,
        auth: BackendAuth,
    ) -> InferenceBackend {
        let mut value = json!({
            "agent_did": DID,
            "backend_id": backend_id,
            "name": name,
            "provider_kind": provider_kind,
            "endpoint": "http://127.0.0.1:1/v1",
            "auth": auth,
        });
        if provider_kind == BackendProviderKind::ClaudeCliSubscription {
            value["endpoint"] = json!("claude-cli://subscription");
        }
        serde_json::from_value(value).unwrap()
    }

    async fn seed_backend(access: &ConfigAccess, backend: InferenceBackend) {
        gents::config_client::write_inference_backend_document(access, &backend)
            .await
            .unwrap();
    }

    async fn seed_profile(access: &ConfigAccess, profile_id: &str, backend_id: &str) {
        let profile = serde_json::from_value(json!({
            "agent_did": DID,
            "profile_id": profile_id,
            "backend_id": backend_id,
            "model_name": "model-x",
        }))
        .unwrap();
        gents::config_client::write_inference_profile_document(access, &profile)
            .await
            .unwrap();
    }

    fn oauth(account_ref: Option<&str>) -> BackendAuth {
        BackendAuth::PrincipalOAuth {
            account_ref: account_ref.map(str::to_owned),
        }
    }

    /// Two accounts per sign-in provider, two API-key and two endpoint
    /// backends, and profiles on some of them.
    async fn seeded() -> ConfigAccess {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node);
        seed_account(&access, CHATGPT, None, None).await;
        seed_account(&access, CHATGPT, Some("acct-c2"), Some("ChatGPT 2")).await;
        seed_account(&access, CLAUDE, None, Some("Personal")).await;
        seed_account(&access, CLAUDE, Some("acct-l2"), Some("Work")).await;
        seed_account(&access, GROK, None, None).await;
        seed_account(&access, GROK, Some("acct-g2"), Some("Grok 2")).await;
        use BackendProviderKind::*;
        for backend in [
            backend("claude", "Claude", ClaudeCliSubscription, oauth(None)),
            backend(
                "claude-acct-l2",
                "Work",
                ClaudeCliSubscription,
                oauth(Some("acct-l2")),
            ),
            backend(
                "chatgpt-acct-c2",
                "ChatGPT 2",
                ChatGptCodex,
                oauth(Some("acct-c2")),
            ),
            backend(
                "openai",
                "OpenAI",
                OpenAiCompatible,
                BackendAuth::ApiKey {
                    key: "key-SECRET".into(),
                },
            ),
            backend(
                "openrouter",
                "OpenRouter",
                OpenRouter,
                BackendAuth::ApiKey {
                    key: "key-SECRET".into(),
                },
            ),
            backend(
                "local",
                "Local server",
                OpenAiCompatible,
                BackendAuth::Unauthenticated,
            ),
            backend(
                "lab",
                "Lab server",
                OpenAiCompatible,
                BackendAuth::Unauthenticated,
            ),
        ] {
            seed_backend(&access, backend).await;
        }
        seed_profile(&access, "default-profile", "claude").await;
        seed_profile(&access, "work-profile", "claude-acct-l2").await;
        seed_profile(&access, "api-profile", "openai").await;
        access
    }

    fn row<'a>(rows: &'a [AccountRow], label: &str) -> &'a AccountRow {
        rows.iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("no row {label}: {rows:#?}"))
    }

    async fn stored_ids(access: &ConfigAccess) -> Vec<String> {
        list_oauth_credentials_on(access, DID)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.credential_id)
            .collect()
    }

    async fn backend_ids(access: &ConfigAccess) -> Vec<String> {
        let ConfigAccess::Local(node) = access else {
            unreachable!()
        };
        gents::backend_registry::list_all_backends(node)
            .await
            .unwrap()
            .into_iter()
            .map(|backend| backend.backend_id)
            .collect()
    }

    #[tokio::test]
    async fn list_shows_every_account_and_backend() {
        let access = seeded().await;
        let rows = account_rows(&access, DID, None, chrono::Utc::now())
            .await
            .unwrap();
        let mut labels: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
        labels.sort();
        assert_eq!(
            labels,
            [
                "ChatGPT",
                "ChatGPT 2",
                "Grok",
                "Grok 2",
                "Lab server",
                "Local server",
                "OpenAI",
                "OpenRouter",
                "Personal",
                "Work"
            ]
        );
        assert_eq!(row(&rows, "Personal").profiles, ["default-profile"]);
        assert_eq!(row(&rows, "Work").profiles, ["work-profile"]);
        assert_eq!(row(&rows, "OpenAI").profiles, ["api-profile"]);
        assert!(row(&rows, "ChatGPT 2").profiles.is_empty());
        let defaults: Vec<_> = rows
            .iter()
            .filter(|row| row.default)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(defaults.len(), 3);
        for label in ["ChatGPT", "Personal", "Grok"] {
            assert!(defaults.contains(&label), "{defaults:?}");
        }
        assert_eq!(
            row(&rows, "Personal").identity.as_deref(),
            Some("identity-original")
        );

        let json = serde_json::to_string(&rows).unwrap();
        let table = render_table(&rows);
        for text in [&json, &table] {
            assert!(!text.contains("SECRET"), "{text}");
        }
        for heading in [
            "PROVIDER", "LABEL", "IDENTITY", "PLAN", "STATUS", "DEFAULT", "PROFILES",
        ] {
            assert!(table.contains(heading), "{table}");
        }
        assert!(
            table.contains("work-profile") && table.contains("Lab server"),
            "{table}"
        );

        let claude_only = account_rows(&access, DID, Some(CLAUDE), chrono::Utc::now())
            .await
            .unwrap();
        let mut labels: Vec<_> = claude_only.iter().map(|row| row.label.as_str()).collect();
        labels.sort();
        assert_eq!(labels, ["Personal", "Work"]);
    }

    #[tokio::test]
    async fn a_backend_whose_account_is_elsewhere_is_listed_as_missing() {
        let access = seeded().await;
        seed_backend(
            &access,
            backend(
                "grok-remote",
                "Remote Grok",
                BackendProviderKind::XaiGrokOAuth,
                oauth(Some("acct-other")),
            ),
        )
        .await;
        let rows = account_rows(&access, DID, None, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(rows.len(), 11);
        let missing = row(&rows, "Remote Grok");
        assert_eq!(missing.status, "account not on this node");
        assert_eq!(missing.backend_id.as_deref(), Some("grok-remote"));
        assert!(render_table(&rows).contains("account not on this node"));
    }

    #[tokio::test]
    async fn a_shared_label_is_ambiguous_until_narrowed() {
        let access = seeded().await;
        gents::oauth_credential::set_account_label(
            &access,
            DID,
            &format!("{GROK}:{DID}:acct-g2"),
            "Work",
        )
        .await
        .unwrap();
        let error = resolve_account(&access, DID, "Work", None)
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains(&format!("{CLAUDE}:{DID}:acct-l2")), "{text}");
        assert!(text.contains(&format!("{GROK}:{DID}:acct-g2")), "{text}");
        assert!(text.contains("--provider"), "{text}");
        let by_provider = resolve_account(&access, DID, "Work", Some(CLAUDE))
            .await
            .unwrap();
        assert_eq!(by_provider.account_ref.as_deref(), Some("acct-l2"));
        let by_id = resolve_account(&access, DID, &format!("{GROK}:{DID}:acct-g2"), None)
            .await
            .unwrap();
        assert_eq!(by_id.provider, GROK);
        let by_ref = resolve_account(&access, DID, "acct-g2", None)
            .await
            .unwrap();
        assert_eq!(by_ref.provider, GROK);

        // Two accounts of one provider with one label (concurrent adds).
        seed_account(&access, CHATGPT, Some("acct-c3"), Some("ChatGPT 2")).await;
        let error = resolve_account(&access, DID, "ChatGPT 2", Some(CHATGPT))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("acct-c3"), "{error}");
        assert!(resolve_account(&access, DID, "acct-c3", None).await.is_ok());
    }

    #[tokio::test]
    async fn a_backend_label_points_to_config_backend() {
        let access = seeded().await;
        let error = resolve_account(&access, DID, "OpenAI", None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("gents config backend"),
            "{error}"
        );
        let error = resolve_account(&access, DID, "nothing-here", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nothing-here"), "{error}");
    }

    #[tokio::test]
    async fn labels_rename_and_refuse_duplicates() {
        let access = seeded().await;
        let result = label_account(&access, DID, "Work", None, "Office")
            .await
            .unwrap();
        assert_eq!(result["label"], "Office");
        let rows = account_rows(&access, DID, None, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(row(&rows, "Office").profiles, ["work-profile"]);
        let ConfigAccess::Local(node) = &access else {
            unreachable!()
        };
        let renamed = gents::backend_registry::lookup_backend(node, DID, "claude-acct-l2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.name, "Office");
        assert!(label_account(&access, DID, "Office", None, "Personal")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn disable_names_the_profiles_and_keeps_the_row() {
        let access = seeded().await;
        let mut warnings = Vec::new();
        let result = disable_account(&access, DID, "Work", None, &mut warnings)
            .await
            .unwrap();
        assert_eq!(result["profiles"], json!(["work-profile"]));
        let warnings = String::from_utf8(warnings).unwrap();
        assert!(warnings.contains("work-profile"), "{warnings}");
        let rows = account_rows(&access, DID, None, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(row(&rows, "Work").status, "disabled");
        assert!(stored_ids(&access)
            .await
            .contains(&format!("{CLAUDE}:{DID}:acct-l2")));
    }

    #[tokio::test]
    async fn remove_deletes_the_row_and_an_unnamed_backend_but_keeps_a_named_one() {
        let access = seeded().await;
        let result = remove_account(&access, DID, "ChatGPT 2", None, true)
            .await
            .unwrap();
        assert_eq!(result["deleted_backends"], json!(["chatgpt-acct-c2"]));
        assert!(!stored_ids(&access)
            .await
            .contains(&format!("{CHATGPT}:{DID}:acct-c2")));
        assert!(!backend_ids(&access)
            .await
            .contains(&"chatgpt-acct-c2".to_string()));

        let result = remove_account(&access, DID, "Work", None, true)
            .await
            .unwrap();
        assert_eq!(result["profiles"], json!(["work-profile"]));
        assert_eq!(result["kept_backends"], json!(["claude-acct-l2"]));
        assert!(!stored_ids(&access)
            .await
            .contains(&format!("{CLAUDE}:{DID}:acct-l2")));
        assert!(backend_ids(&access)
            .await
            .contains(&"claude-acct-l2".to_string()));
    }

    #[tokio::test]
    async fn removing_the_original_keeps_the_no_reference_backend() {
        let access = seeded().await;
        remove_account(&access, DID, "Personal", None, true)
            .await
            .unwrap();
        assert!(!stored_ids(&access)
            .await
            .contains(&format!("{CLAUDE}:{DID}")));
        assert!(backend_ids(&access).await.contains(&"claude".to_string()));
    }

    #[tokio::test]
    async fn probe_prints_one_block_per_account_by_its_own_reference() {
        let access = seeded().await;
        gents::oauth_credential::set_account_enabled(
            &access,
            DID,
            &format!("{CLAUDE}:{DID}:acct-l2"),
            false,
        )
        .await
        .unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let blocks = probe_each_account(&access, DID, CLAUDE, |credential| {
            seen.lock().unwrap().push(credential.credential_id.clone());
            async move {
                Ok(format!(
                    "probed {}",
                    credential.account_ref.as_deref().unwrap_or("original")
                ))
            }
        })
        .await
        .unwrap();
        assert_eq!(seen.into_inner().unwrap(), [format!("{CLAUDE}:{DID}")]);
        assert_eq!(blocks.len(), 2);
        let personal = blocks
            .iter()
            .find(|block| block.starts_with("Personal"))
            .unwrap();
        assert!(personal.contains("probed original"), "{personal}");
        let work = blocks
            .iter()
            .find(|block| block.starts_with("Work"))
            .unwrap();
        assert!(work.contains("disabled, not probed"), "{work}");
    }

    #[tokio::test]
    async fn probe_failures_do_not_stop_the_others() {
        let access = seeded().await;
        let seen = std::sync::Mutex::new(Vec::new());
        let error = probe_each_account(&access, DID, GROK, |credential| {
            seen.lock().unwrap().push(credential.account_ref.clone());
            async move {
                match credential.account_ref.as_deref() {
                    Some("acct-g2") => anyhow::bail!("HTTP 401"),
                    _ => Ok("models: 3".to_string()),
                }
            }
        })
        .await
        .unwrap_err();
        let mut seen = seen.into_inner().unwrap();
        seen.sort();
        assert_eq!(seen, [None, Some("acct-g2".to_string())]);
        let text = format!("{error:#}");
        assert!(
            text.contains("Grok 2") && text.contains("HTTP 401"),
            "{text}"
        );
        assert!(text.contains("models: 3"), "{text}");
    }

    #[tokio::test]
    async fn remove_off_a_terminal_needs_yes() {
        let access = seeded().await;
        let error = remove_account(&access, DID, "Grok 2", None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--yes"), "{error}");
        assert!(stored_ids(&access)
            .await
            .contains(&format!("{GROK}:{DID}:acct-g2")));
    }

    fn usage_window(
        label: &str,
        used_pct: f64,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> gents::usage_observation::account_usage::UsageWindow {
        gents::usage_observation::account_usage::UsageWindow {
            label: label.to_owned(),
            window_minutes: Some(300),
            used_pct,
            resets_at: None,
            source: gents::usage_observation::account_usage::UsageSource::Header,
            observed_at,
        }
    }

    async fn record(
        access: &ConfigAccess,
        provider: &str,
        account_ref: Option<&str>,
        windows: Vec<gents::usage_observation::account_usage::UsageWindow>,
    ) {
        let ConfigAccess::Local(node) = access else {
            unreachable!()
        };
        gents::usage_observation::record_usage(
            node,
            &gents::usage_observation::UsageAccount::Credential {
                doc_id: None,
                agent_did: DID.to_owned(),
                provider: provider.to_owned(),
                account_ref: account_ref.map(str::to_owned),
            },
            gents::usage_observation::account_usage::UsageReport {
                windows,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    /// Personal at 42% of a 5h window that resets in 2h13m, seen 4m ago;
    /// Work last seen 40m ago; ChatGPT 2 with a Codex primary window.
    async fn seeded_usage() -> (ConfigAccess, chrono::DateTime<chrono::Utc>) {
        let access = seeded().await;
        let now = chrono::Utc::now();
        let mut personal = usage_window("5h", 42.0, now - chrono::Duration::minutes(4));
        personal.resets_at = Some(now + chrono::Duration::minutes(133));
        record(&access, CLAUDE, None, vec![personal]).await;
        record(
            &access,
            CLAUDE,
            Some("acct-l2"),
            vec![usage_window(
                "5h",
                10.0,
                now - chrono::Duration::minutes(40),
            )],
        )
        .await;
        record(
            &access,
            CHATGPT,
            Some("acct-c2"),
            vec![usage_window("primary", 12.0, now)],
        )
        .await;
        (access, now)
    }

    #[tokio::test]
    async fn usage_columns_rows_carry_their_accounts_usage() {
        let (access, now) = seeded_usage().await;
        let rows = account_rows(&access, DID, None, now).await.unwrap();
        let personal = row(&rows, "Personal").usage.as_ref().expect("usage");
        assert_eq!(personal.windows[0].used_pct, 42.0);
        assert_eq!(
            row(&rows, "OpenAI").usage.as_ref().expect("usage").note,
            Some("not reported")
        );
        assert_eq!(
            row(&rows, "OpenRouter").usage.as_ref().expect("usage").note,
            Some("unknown")
        );
        let chatgpt = row(&rows, "ChatGPT 2").usage.as_ref().expect("usage");
        assert_eq!(chatgpt.windows[0].label, "primary");
        assert!(render_table(&rows).contains("primary (5h)"));
    }

    #[tokio::test]
    async fn usage_columns_table_has_window_used_resets_source_age() {
        let (access, now) = seeded_usage().await;
        let rows = account_rows(&access, DID, None, now).await.unwrap();
        let table = render_table(&rows);
        for text in [
            "WINDOW",
            "USED",
            "RESETS",
            "SOURCE",
            "AGE",
            "42%",
            "(in 2h13m)",
            "headers",
            "4m",
            "40m, last known",
            "not reported",
        ] {
            assert!(table.contains(text), "{text}: {table}");
        }
        let json = serde_json::to_string(&rows).unwrap();
        for text in [&json, &table] {
            assert!(!text.contains("SECRET"), "{text}");
        }
    }

    #[tokio::test]
    async fn usage_columns_disabled_account_shows_no_usage() {
        let (access, now) = seeded_usage().await;
        gents::oauth_credential::set_account_enabled(
            &access,
            DID,
            &format!("{CLAUDE}:{DID}:acct-l2"),
            false,
        )
        .await
        .unwrap();
        let rows = account_rows(&access, DID, None, now).await.unwrap();
        assert_eq!(row(&rows, "Work").usage, None);
        let table = render_table(&rows);
        let work = table
            .lines()
            .find(|line| line.contains("Work") && line.contains("disabled"))
            .expect("work line");
        assert!(!work.contains("10%"), "{work}");
    }

    fn usage_read(
        provider: &str,
        account_ref: Option<&str>,
        backend_id: Option<&str>,
        outcome: UsageRead,
    ) -> AccountUsageRead {
        AccountUsageRead {
            provider: provider.to_owned(),
            account_ref: account_ref.map(str::to_owned),
            backend_id: backend_id.map(str::to_owned),
            outcome,
        }
    }

    #[tokio::test]
    async fn usage_reads_post_the_trigger_to_the_runtime() {
        use axum::{routing::post, Json, Router};
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn gents::AgentIdentity> = Arc::new(
            gents::KeyIdentity::load_or_create(temp.path().join("home.key"), None).unwrap(),
        );
        let recorded = Arc::new(std::sync::Mutex::new(None::<Value>));
        let app = Router::new().route(
            "/accounts/usage/read",
            post({
                let recorded = recorded.clone();
                move |Json(body): Json<Value>| async move {
                    *recorded.lock().unwrap() = Some(body);
                    Json(json!({
                        "agent_did": DID,
                        "reads": [{
                            "provider": CLAUDE,
                            "account_ref": null,
                            "backend_id": null,
                            "outcome": { "outcome": "read" },
                        }],
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let reads = request_usage_reads(
            identity.as_ref(),
            &gents::config_client::GraphqlEndpoint::anonymous(format!("{origin}/api/v0/graphql")),
            UsageTrigger::Refresh,
            Some(CLAUDE),
        )
        .await
        .unwrap();

        assert_eq!(reads.agent_did, DID);
        assert_eq!(
            reads.reads,
            [usage_read(CLAUDE, None, None, UsageRead::Read)]
        );
        let body = recorded.lock().unwrap().clone().expect("posted");
        assert_eq!(body["trigger"], "refresh");
        assert_eq!(body["provider"], CLAUDE);
        assert_eq!(body["signer_did"], identity.did());
        let command: UsageReadCommand = serde_json::from_value(body).unwrap();
        assert!(identity
            .verify(identity.did(), &command.signing_payload(), &command.sig)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn usage_reads_refresh_without_a_runtime_is_an_error() {
        let access = seeded().await;
        let home = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let now = chrono::Utc::now();

        let error = list_with_usage(&access, home.path(), DID, None, true, now, &mut warnings)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("gents serve"), "{error}");

        let rows = list_with_usage(&access, home.path(), DID, None, false, now, &mut warnings)
            .await
            .unwrap();
        assert_eq!(rows.len(), 10);
        assert!(warnings.is_empty());
    }

    #[tokio::test]
    async fn usage_reads_outcomes_label_their_rows() {
        let access = seeded().await;
        let mut rows = account_rows(&access, DID, None, chrono::Utc::now())
            .await
            .unwrap();
        let mut reads = UsageReads {
            agent_did: "did:key:z6MkSomeoneElse".to_owned(),
            reads: vec![
                usage_read(
                    GROK,
                    Some("acct-g2"),
                    None,
                    UsageRead::Unavailable("throttled".to_owned()),
                ),
                usage_read(CLAUDE, None, None, UsageRead::SkippedRecent),
                usage_read("OpenRouter", None, Some("openrouter"), UsageRead::Read),
            ],
        };
        attach_reads(&mut rows, &reads, DID);
        assert!(rows.iter().all(|row| row.read.is_none()));

        reads.agent_did = DID.to_owned();
        attach_reads(&mut rows, &reads, DID);
        assert_eq!(
            row(&rows, "Grok 2").read,
            Some(UsageRead::Unavailable("throttled".to_owned()))
        );
        assert_eq!(row(&rows, "Personal").read, Some(UsageRead::SkippedRecent));
        assert_eq!(row(&rows, "OpenRouter").read, Some(UsageRead::Read));
        assert_eq!(row(&rows, "Grok").read, None);

        let table = render_table(&rows);
        let line = |label: &str| {
            table
                .lines()
                .find(|line| line.contains(label))
                .unwrap_or_else(|| panic!("{label}: {table}"))
                .to_owned()
        };
        assert!(line("Grok 2").contains("(read: throttled)"), "{table}");
    }

    #[test]
    fn usage_read_command_is_valid_for_one_minute_and_five_seconds_ahead() {
        let now = chrono::Utc::now();
        let command = |offset: i64| UsageReadCommand {
            trigger: UsageTrigger::Open,
            provider: None,
            signer_did: DID.to_owned(),
            issued_at: (now + chrono::Duration::seconds(offset))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            nonce: "nonce-a".to_owned(),
            sig: vec![0; 64],
        };
        assert!(command(-59).validate_at(now).is_ok());
        assert!(command(-61).validate_at(now).is_err());
        assert!(command(6).validate_at(now).is_err());
    }

    #[test]
    fn short_duration_of_a_future_observation_is_under_a_minute() {
        assert_eq!(short_duration(-180), "<1m");
    }

    #[tokio::test]
    async fn usage_reads_a_runtime_without_the_route_names_the_status() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(temp.path().join("home.key"), None).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, axum::Router::new()).await });

        let error = request_usage_reads(
            &identity,
            &gents::config_client::GraphqlEndpoint::anonymous(format!("{origin}/api/v0/graphql")),
            UsageTrigger::Open,
            None,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("404"), "{error:#}");
    }
}
