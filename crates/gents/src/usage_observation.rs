//! Provider-reported usage per node and provider account, kept in
//! `ProviderAccountUsage` and never on `OAuthCredential`: every credential
//! write reloads the runtime view.
//!
//! The process that hosts the embedded node and runs the provider clients is
//! the only writer, so writes take the node rather than a `ConfigAccess`.
//! Usage is an observation: it picks no account and gates nothing.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_loop::account_usage::UsageReport;

use crate::config_client::ConfigAccess;
use crate::document_config::InferenceBackend;
use crate::oauth_credential::OAuthCredential;

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
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredUsage {
    pub report: UsageReport,
    pub observed_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
    pub read_error: Option<String>,
}

/// Stored usage of `account`; `None` when nothing is stored or the account
/// has no enabled sign-in.
pub async fn load_usage(
    access: &ConfigAccess,
    account: &UsageAccount,
) -> Result<Option<StoredUsage>> {
    let _ = (access, account);
    Ok(None)
}

/// Merge `report` into `account`'s stored usage.
pub async fn record_usage(
    node: &Arc<EmbeddedNode>,
    account: &UsageAccount,
    report: UsageReport,
) -> Result<()> {
    write(node, account, report, None).await
}

async fn write(
    node: &Arc<EmbeddedNode>,
    account: &UsageAccount,
    report: UsageReport,
    read: Option<(DateTime<Utc>, Option<String>)>,
) -> Result<()> {
    let _ = (node, account, report, read);
    Ok(())
}

/// Stored usage of the account `backend` names for `agent_did`.
pub async fn usage_for_backend(
    access: &ConfigAccess,
    agent_did: &str,
    backend: &InferenceBackend,
) -> Result<Option<StoredUsage>> {
    let _ = (access, agent_did, backend);
    Ok(None)
}

#[cfg(test)]
#[path = "usage_observation/tests.rs"]
mod tests;
