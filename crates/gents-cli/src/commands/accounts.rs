use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use gents::backend_provider::BackendProviderOauthExt;
use gents::document_config::InferenceProfile;
use gents::oauth_credential::{backend_account, AccountSummary};
use gents::usage_observation::{usage_view, AccountUsageRead, UsageRead, UsageTrigger, UsageView};
use gents::{BackendProviderKind, InferenceBackend};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::args::{AccountsCommand, AccountsTargetArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access, resolve_node_did};

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
    pub(crate) usage: Option<UsageView>,
    pub(crate) read: Option<UsageRead>,
}

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
        identity: &dyn gents::NodeIdentity,
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
            UsageTrigger::Cached => "cached",
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UsageReads {
    pub node_did: String,
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
            let did = resolve_node_did(Some(&home_dir), target.node_did.as_deref())?;
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

async fn target_access(
    target: &AccountsTargetArgs,
) -> Result<(gents::config_client::ConfigAccess, String)> {
    let (access, home_dir) =
        resolve_config_access(target.home.as_deref(), target.graphql.as_deref()).await?;
    let did = resolve_node_did(Some(&home_dir), target.node_did.as_deref())?;
    Ok((access, did))
}

pub(crate) struct Snapshot {
    pub(crate) accounts: Vec<AccountSummary>,
    pub(crate) backends: Vec<InferenceBackend>,
    pub(crate) profiles: Vec<InferenceProfile>,
}

pub(crate) async fn snapshot(access: &ConfigAccess, node_did: &str) -> Result<Snapshot> {
    let accounts = gents::oauth_credential::list_accounts(access, node_did).await?;
    let (backends, profiles) = access
        .transact_write("accounts.config", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>((
                    gents::config_client::list_inference_backends_in_txn(txn, node_did).await?,
                    gents::config_client::list_inference_profiles_in_txn(txn, node_did).await?,
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

pub(crate) async fn account_rows(
    access: &ConfigAccess,
    node_did: &str,
    provider: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<AccountRow>> {
    let snapshot = snapshot(access, node_did).await?;
    let mut rows = Vec::new();
    for account in snapshot
        .accounts
        .iter()
        .filter(|account| provider.is_none_or(|provider| account.provider == provider))
    {
        let kind = BackendProviderKind::ALL
            .into_iter()
            .find(|kind| kind.oauth_provider() == Some(account.provider.as_str()));
        let usage = kind
            .filter(|_| account.enabled)
            .map(|kind| usage_view(None, kind, now));
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
            Some(usage_view(None, backend.provider_kind, now))
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

/// An unresponsive runtime must not leave the interactive usage request pending
/// indefinitely after the signed command has been sent.
pub async fn request_usage_reads(
    identity: &dyn gents::NodeIdentity,
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

pub(crate) fn attach_reads(rows: &mut [AccountRow], reads: &UsageReads, node_did: &str) {
    if reads.node_did != node_did {
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
            row.usage = read.usage.clone();
        }
    }
}

pub(crate) async fn list_with_usage(
    access: &ConfigAccess,
    home: &std::path::Path,
    node_did: &str,
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
    let mut rows = account_rows(access, node_did, provider, now).await?;
    if let Some(reads) = reads {
        attach_reads(&mut rows, &reads, node_did);
    }
    Ok(rows)
}

pub(crate) fn short_duration(secs: i64) -> String {
    let secs = secs.max(0); // a replicated clock may run ahead
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
            usage.note.as_deref().unwrap_or("unknown").to_owned(),
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
    node_did: &str,
    account: &str,
    provider: Option<&str>,
) -> Result<AccountSummary> {
    Ok(snapshot(access, node_did)
        .await?
        .pick(account, provider)?
        .clone())
}

pub(crate) async fn label_account(
    access: &ConfigAccess,
    node_did: &str,
    account: &str,
    provider: Option<&str>,
    label: &str,
) -> Result<Value> {
    let account = resolve_account(access, node_did, account, provider).await?;
    gents::oauth_credential::set_account_label(access, node_did, &account.credential_id, label)
        .await?;
    Ok(json!({
        "credential_id": account.credential_id,
        "provider": account.provider,
        "label": gents::oauth_credential::validate_account_label(label)?,
    }))
}

async fn warn_profiles(
    access: &ConfigAccess,
    node_did: &str,
    account: &str,
    provider: Option<&str>,
    warnings: &mut impl Write,
) -> Result<()> {
    let snapshot = snapshot(access, node_did).await?;
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
    node_did: &str,
    account: &str,
    provider: Option<&str>,
    warnings: &mut impl Write,
) -> Result<Value> {
    warn_profiles(access, node_did, account, provider, warnings).await?;
    let snapshot = snapshot(access, node_did).await?;
    let account = snapshot.pick(account, provider)?;
    gents::oauth_credential::set_account_enabled(access, node_did, &account.credential_id, false)
        .await?;
    Ok(json!({
        "credential_id": account.credential_id,
        "provider": account.provider,
        "label": account.label,
        "enabled": false,
        "profiles": snapshot.account_profiles(account),
    }))
}

pub async fn remove_account(
    access: &ConfigAccess,
    node_did: &str,
    account: &str,
    provider: Option<&str>,
    confirmed: bool,
) -> Result<Value> {
    let snapshot = snapshot(access, node_did).await?;
    let account = snapshot.pick(account, provider)?.clone();
    anyhow::ensure!(
        confirmed,
        "removing an account asks for confirmation; pass --yes when not running in a terminal"
    );
    let (deleted, kept) = access
        .transact_write("accounts.remove", |txn| {
            let account = account.clone();
            Box::pin(async move {
                gents::oauth_credential::remove_account_in_txn(
                    txn,
                    node_did,
                    &account.credential_id,
                )
                .await?;
                let profiles =
                    gents::config_client::list_inference_profiles_in_txn(txn, node_did).await?;
                let (kept, deleted): (Vec<_>, Vec<_>) =
                    gents::config_client::list_inference_backends_in_txn(txn, node_did)
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
                    node_did,
                    &deleted,
                )
                .await?;
                Ok::<_, anyhow::Error>((deleted, kept))
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

pub(crate) async fn probe_each_account<F, Fut>(
    access: &ConfigAccess,
    node_did: &str,
    provider: &str,
    probe: F,
) -> Result<Vec<String>>
where
    F: Fn(gents::oauth_credential::OAuthCredential) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let mut blocks = Vec::new();
    let mut failed = Vec::new();
    for account in gents::oauth_credential::list_accounts(access, node_did)
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
            node_did,
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
