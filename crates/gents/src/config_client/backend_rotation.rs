//! Operator switch of a subscription backend between one account and the
//! node's account pool. Agent self-config can never introduce a pool or change
//! its overage choice (`guard_backend_auth`), so this is the only partial write
//! that may; it replaces `auth` alone and validates the whole backend.

use super::{
    load_inference_backend_in_txn, write_inference_backend_in_txn, ConfigAccess, ConfigApplyTxn,
};
use crate::backend_provider::BackendProviderOauthExt;
use crate::document_config::BackendAuth;
use crate::InferenceBackend;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How a backend picks its Claude account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum BackendRotation {
    /// Rotate over every enabled account; `None` keeps the current overage
    /// choice (off when the backend is not yet a pool).
    Pool {
        #[serde(default)]
        #[cfg_attr(feature = "typescript", ts(optional))]
        allow_overage: Option<bool>,
    },
    /// One account: a pool becomes the provider's default account; a backend
    /// already on one account keeps it.
    Single,
}

/// One account of a pool, in the order the rotation walks them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolMember {
    pub credential_id: String,
    pub label: String,
}

/// The backend as written and the accounts it now rotates over.
#[derive(Debug, Clone)]
pub struct RotationOutcome {
    pub backend: InferenceBackend,
    pub members: Vec<PoolMember>,
}

/// Replaces the stored backend's `auth` inside the caller's write transaction,
/// so a caller can first check its own precondition on the stored document.
pub async fn set_backend_rotation_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    backend_id: &str,
    rotation: BackendRotation,
) -> Result<InferenceBackend> {
    let mut backend = load_inference_backend_in_txn(txn, node_did, backend_id)
        .await?
        .with_context(|| format!("Backend {backend_id:?} does not exist on this node."))?;
    anyhow::ensure!(
        backend.auth.uses_node_oauth(),
        "Backend {backend_id:?} does not use a subscription sign-in, so it has no accounts to rotate."
    );
    let current_overage = backend.auth.pool_allow_overage().unwrap_or(false);
    backend.auth = match rotation {
        BackendRotation::Pool { allow_overage } => BackendAuth::NodeOAuthPool {
            allow_overage: allow_overage.unwrap_or(current_overage),
        },
        BackendRotation::Single => match backend.auth {
            BackendAuth::NodeOAuth { account_ref } => BackendAuth::NodeOAuth { account_ref },
            _ => BackendAuth::NodeOAuth { account_ref: None },
        },
    };
    write_inference_backend_in_txn(txn, &backend).await?;
    Ok(backend)
}

/// The accounts a pool backend rotates over, in rotation order; empty for any
/// other backend.
pub async fn backend_pool_members(
    access: &ConfigAccess,
    backend: &InferenceBackend,
) -> Result<Vec<PoolMember>> {
    let (BackendAuth::NodeOAuthPool { .. }, Some(provider)) =
        (&backend.auth, backend.provider_kind.oauth_provider())
    else {
        return Ok(Vec::new());
    };
    Ok(
        crate::oauth_credential::resolve_oauth_pool(access, &backend.node_did, provider)
            .await?
            .iter()
            .map(|credential| PoolMember {
                credential_id: credential.credential_id.clone(),
                label: crate::oauth_credential::effective_account_label(credential),
            })
            .collect(),
    )
}

/// Operator write of one backend's rotation, used by the CLI.
pub async fn set_backend_rotation(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
    rotation: BackendRotation,
) -> Result<RotationOutcome> {
    let backend = access
        .transact_write("config.backend.rotation", |txn| {
            Box::pin(set_backend_rotation_in_txn(
                txn, node_did, backend_id, rotation,
            ))
        })
        .await?;
    let members = backend_pool_members(access, &backend).await?;
    Ok(RotationOutcome { backend, members })
}
