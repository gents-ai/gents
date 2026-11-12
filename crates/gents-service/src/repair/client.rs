use std::path::Path;

use anyhow::{Context, Result};
use gents_desktop_core::client::{
    client_store_from_other_version, client_store_key_missing, load_peer_records,
    retire_stale_local_peers, set_aside_unreadable, set_aside_unreadable_peer_directory,
    DesktopPaths,
};
use gents_desktop_core::home_reset::{plan_home_reset, ClientBinding, HomeResetDisposition};
use gents_desktop_core::preference::{
    read_home_identity, StoredManagedServer, MANAGED_SERVER_CONFIG,
};
use gents_protocol::operations::install_repair::{
    Blocker, ClientObservation, Finding, IncompatibleStore, IncompatibleStoreKind, RefusalAction,
    RefusalObservation, RepairClientInput, RepairClientReceipt, RepairClientScope,
};

use super::ClientExclusion;

pub fn classify_binding(o: &ClientObservation) -> ClientBinding {
    if !o.reviewed {
        if o.stale_local_peer {
            ClientBinding::Orphaned
        } else {
            ClientBinding::Unbound
        }
    } else if o.configured_matches {
        ClientBinding::BoundToHome
    } else if o.configured_same_did || o.reviewed_home_same_did {
        ClientBinding::Unbound
    } else if o.reviewed_home_present {
        ClientBinding::Orphaned
    } else if o.reviewed_init_present {
        ClientBinding::Unbound
    } else {
        ClientBinding::Orphaned
    }
}

async fn preference(paths: &DesktopPaths) -> Result<Option<StoredManagedServer>> {
    let path = paths.root().join(MANAGED_SERVER_CONFIG);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).context("reading managed server settings"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("reading managed server settings"),
    }
}

pub async fn binding_from_paths(
    paths: &DesktopPaths,
    node_home: Option<&Path>,
) -> Result<ClientBinding> {
    let Some(node_home) = node_home else {
        return Ok(ClientBinding::Unbound);
    };
    let configured = read_home_identity(node_home).await;
    let peers = load_peer_records(paths.peer_directory_path())
        .await
        .unwrap_or_default();
    let local = peers
        .iter()
        .find(|peer| peer.source.as_deref() == Some("local-standard"));
    let stored = preference(paths).await?;
    let reviewed = stored.as_ref().and_then(|p| p.reviewed_for.as_ref());
    let reviewed_home = match reviewed {
        Some(reviewed) => read_home_identity(Path::new(&reviewed.node_home)).await,
        None => None,
    };
    let reviewed_init_present = match reviewed {
        Some(reviewed) => {
            tokio::fs::try_exists(Path::new(&reviewed.node_home).join("init.json")).await?
        }
        None => false,
    };
    Ok(classify_binding(&ClientObservation {
        reviewed: reviewed.is_some(),
        configured_matches: configured
            .as_ref()
            .zip(reviewed)
            .is_some_and(|(c, r)| &c.reviewed == r),
        configured_same_did: configured
            .as_ref()
            .zip(reviewed)
            .is_some_and(|(c, r)| c.reviewed.node_did == r.node_did),
        reviewed_home_same_did: reviewed_home
            .as_ref()
            .zip(reviewed)
            .is_some_and(|(c, r)| c.reviewed.node_did == r.node_did),
        reviewed_home_present: reviewed_home.is_some(),
        reviewed_init_present,
        stale_local_peer: configured.as_ref().zip(local).is_some_and(|(c, p)| {
            p.local_node_home.as_deref() == Some(c.reviewed.node_home.as_str())
                && p.node_did != c.reviewed.node_did
        }),
    }))
}

async fn archive_store(
    input: &RepairClientInput,
    paths: &DesktopPaths,
    kind: Option<IncompatibleStoreKind>,
) -> Result<std::path::PathBuf> {
    let input = input.clone();
    let paths = paths.clone();
    tokio::task::spawn_blocking(move || {
        let plan = plan_home_reset(
            None,
            None,
            kind.map(|kind| IncompatibleStore {
                kind,
                data_path: paths.node_data_dir().to_path_buf(),
            }),
            &paths,
            &input.user_home,
            if kind.is_none() {
                ClientBinding::Orphaned
            } else {
                ClientBinding::Unbound
            },
        )
        .map_err(|e| anyhow::anyhow!(e.message))?;
        let plan = if kind.is_some() {
            plan.keeping_managed_server_preference()
        } else {
            plan
        };
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let backup = plan
            .backup_path(&stamp)
            .map_err(|e| anyhow::anyhow!(e.message))?;
        plan.preflight(&backup)
            .map_err(|e| anyhow::anyhow!(e.message))?;
        plan.retire(HomeResetDisposition::Archive, Some(&backup))
            .map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(backup)
    })
    .await?
}

pub async fn execute(
    input: &RepairClientInput,
    lifecycle: &dyn ClientExclusion,
    receipt: &mut RepairClientReceipt,
) -> Result<(), Blocker> {
    let result = if let RepairClientScope::Store { kind } = input.scope {
        let action = super::planning::refusal_action(&RefusalObservation {
            fresh: true,
            missing_key: kind == IncompatibleStoreKind::MissingStoreKey,
            explicit_home: false,
            older_store: kind.is_older(),
            newer_store: !kind.is_older(),
        });
        if action != RefusalAction::Archive {
            return Err(Blocker::StoreRefused {
                kind,
                message: IncompatibleStore {
                    kind,
                    data_path: DesktopPaths::from_root(input.root.clone())
                        .node_data_dir()
                        .to_path_buf(),
                }
                .to_string(),
            });
        }
        async {
            let _excluded = lifecycle.exclude().await?;
            let paths = DesktopPaths::from_root(input.root.clone());
            let kept_at = archive_store(input, &paths, Some(kind)).await?;
            receipt
                .findings
                .push(Finding::ClientStoreSetAside { kept_at });
            Ok::<_, anyhow::Error>(())
        }
        .await
    } else {
        execute_inner(input, lifecycle, receipt).await
    };
    result.map_err(|error| Blocker::HomeUnreadable {
        home: input.root.clone(),
        reason: gents_protocol::error_chain::ErrorChain(error.as_ref()).to_string(),
    })
}

async fn execute_inner(
    input: &RepairClientInput,
    lifecycle: &dyn ClientExclusion,
    receipt: &mut RepairClientReceipt,
) -> Result<()> {
    let paths = DesktopPaths::from_root(input.root.clone());
    if matches!(input.scope, RepairClientScope::All) {
        if let Some(kept_at) =
            set_aside_unreadable_peer_directory(paths.peer_directory_path()).await?
        {
            receipt
                .findings
                .push(Finding::ClientConnectionsSetAside { kept_at });
        }
        let pref_path = paths.root().join(MANAGED_SERVER_CONFIG);
        if let Ok(bytes) = tokio::fs::read(&pref_path).await {
            if serde_json::from_slice::<StoredManagedServer>(&bytes)
                .is_err_and(|e| e.classify() == serde_json::error::Category::Data)
            {
                let kept_at = set_aside_unreadable(&pref_path).await?;
                receipt
                    .findings
                    .push(Finding::ClientPreferenceSetAside { kept_at });
            }
        }
    }
    let binding = binding_from_paths(&paths, input.node_home.as_deref()).await?;
    let stale = match input.node_home.as_deref() {
        Some(home) => {
            let current = read_home_identity(home).await;
            let present = tokio::fs::try_exists(home.join("init.json")).await?;
            if present && current.is_none() {
                None
            } else {
                let current = current.map(|h| h.reviewed.node_did);
                load_peer_records(paths.peer_directory_path())
                    .await?
                    .iter()
                    .any(|peer| peer.is_stale_local_peer(home, current.as_deref()))
                    .then_some(current)
            }
        }
        None => None,
    };
    let mut exclusion = None;
    if stale.is_some() || binding == ClientBinding::Orphaned {
        exclusion = Some(lifecycle.exclude().await?);
        if let (Some(current), Some(home)) = (stale, input.node_home.as_deref()) {
            let retired = retire_stale_local_peers(
                paths.peer_directory_path(),
                home,
                current.as_deref(),
                &paths.retired_local_peers_path(),
            )
            .await?;
            receipt.local_node_lost |= retired > 0;
            receipt.local_peers += retired;
        }
        if binding == ClientBinding::Orphaned {
            receipt.archive = Some(archive_store(input, &paths, None).await?);
            receipt.local_node_lost = true;
        }
    }
    if matches!(input.scope, RepairClientScope::Orphaned) {
        return Ok(());
    }
    let kind = if client_store_from_other_version(&paths).await? {
        Some(IncompatibleStoreKind::OtherVersionSchema)
    } else if client_store_key_missing(&paths).await? {
        Some(IncompatibleStoreKind::MissingStoreKey)
    } else {
        None
    };
    if let Some(kind) = kind {
        if exclusion.is_none() {
            exclusion = Some(lifecycle.exclude().await?);
        }
        let kept_at = archive_store(input, &paths, Some(kind)).await?;
        receipt
            .findings
            .push(Finding::ClientStoreSetAside { kept_at });
    }
    drop(exclusion);
    Ok(())
}
