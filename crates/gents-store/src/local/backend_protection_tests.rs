use super::backend_protection::ProtectedBackendStore;
use super::{home, store_key::StoreKey};
use crate::identity::NodeIdentity;
use anyhow::Context;

/// A store created before `agent_manager` took only the Node administrators'
/// userset carries the previous lineage marker. It is refused as an older home
/// before any node opens it.
#[tokio::test]
async fn a_store_with_the_previous_lineage_marker_is_refused_unopened() -> anyhow::Result<()> {
    use super::backend_protection::{BackendProtectionError, FreshBackendStore, PROVENANCE_MARKER};
    let directory = tempfile::tempdir()?;
    let home = directory.path().to_owned();
    let data = home.join("data");
    let lock_home = home.clone();
    let lock_data = data.clone();
    let lock = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&lock_data)?;
        Ok::<_, anyhow::Error>(home::lock_store(&lock_home, &lock_data)?)
    })
    .await??;
    let key = StoreKey::from_host(&[7; 32])?;
    let identity_path = home.join("node.key");
    let node = tokio::task::spawn_blocking(
        move || -> anyhow::Result<std::sync::Arc<dyn NodeIdentity>> {
            Ok(std::sync::Arc::new(
                crate::identity::KeyIdentity::load_or_create(identity_path, None)?,
            ))
        },
    )
    .await??;
    let owner_path = home.join("owner.key");
    let owner = tokio::task::spawn_blocking(move || {
        crate::identity::KeyIdentity::load_or_create(owner_path, None)
    })
    .await??;
    FreshBackendStore::observe(&data, &lock)
        .await?
        .initialize(
            &key,
            &node,
            &identity::Did::new(owner.did())?,
            super::document_acp::DocumentAcp::Local,
        )
        .await?;
    assert!(PROVENANCE_MARKER.ends_with("-v3"));
    std::fs::rename(
        data.join(PROVENANCE_MARKER),
        data.join(".gents-private-backend-v2"),
    )?;
    drop(lock);
    // Every file under the home, the store lock's own file included.
    fn snapshot(
        directory: &std::path::Path,
        files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                snapshot(&path, files)?;
            } else {
                files.insert(path.clone(), std::fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut before = std::collections::BTreeMap::new();
    snapshot(&home, &mut before)?;
    assert!(before.contains_key(&home.join("data.lock")));

    let refused = super::backend_protection::refuse_earlier_lineage(&data)
        .expect_err("the previous lineage is not this version's");
    assert!(
        matches!(refused, BackendProtectionError::MissingProvenance),
        "{refused:?}"
    );
    let store = super::storage_backend::incompatible_store(&refused, &data)
        .context("the refusal is reported as an incompatible store")?;
    assert_eq!(
        store.kind,
        gents_protocol::operations::install_repair::IncompatibleStoreKind::UnknownLineage
    );
    assert!(store.kind.made_by_older_version());
    let mut after = std::collections::BTreeMap::new();
    snapshot(&home, &mut after)?;
    assert_eq!(after, before, "the refused home is byte-identical");

    // A store with this version's marker, and one with no marker, pass to
    // the lock.
    std::fs::rename(
        data.join(".gents-private-backend-v2"),
        data.join(PROVENANCE_MARKER),
    )?;
    super::backend_protection::refuse_earlier_lineage(&data)?;
    std::fs::remove_file(data.join(PROVENANCE_MARKER))?;
    super::backend_protection::refuse_earlier_lineage(&data)?;
    Ok(())
}

#[test]
fn copy_check_refusal_matches_the_model() {
    use super::backend_protection::{checked_copy, BackendProtectionError};
    use gents_lean_vocab::families::backend_preflight_copy_check as family;
    family::replay(|input: &family::CopyInput<String>| {
        let verdict = match input.result.as_str() {
            "compatible" => Ok(None),
            "incompatible" => Ok(Some(
                gents_protocol::operations::install_repair::IncompatibleStoreKind::UnknownLineage,
            )),
            "unavailable" => Err(super::store_compat::StoreCompatibilityError::Io {
                path: "copy".into(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            }),
            value => panic!("unknown modeled copy result: {value}"),
        };
        match checked_copy(std::path::Path::new("store"), verdict) {
            Ok(()) => "open",
            Err(BackendProtectionError::Incompatible(_)) => "incompatible",
            Err(BackendProtectionError::CopyCheck(_)) => "unavailable",
            Err(error) => panic!("unexpected copy decision: {error}"),
        }
        .to_owned()
    });
}

fn store_bytes(root: &std::path::Path) -> anyhow::Result<Vec<(std::path::PathBuf, [u8; 32])>> {
    use sha2::{Digest as _, Sha256};
    let mut contents = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            contents.extend(store_bytes(&entry.path())?);
        } else {
            contents.push((
                entry.path(),
                Sha256::digest(std::fs::read(entry.path())?).into(),
            ));
        }
    }
    contents.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(contents)
}

#[tokio::test]
async fn an_uncheckable_copy_leaves_the_original_store_untouched() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let home = directory.path().to_owned();
    let data = home.join("data");
    tokio::fs::create_dir(&data).await?;
    let (lock_home, lock_data) = (home.clone(), data.clone());
    let lock =
        tokio::task::spawn_blocking(move || home::lock_store(&lock_home, &lock_data)).await??;
    let identity_path = home.join("node.key");
    let identity = tokio::task::spawn_blocking(
        move || -> anyhow::Result<std::sync::Arc<dyn NodeIdentity>> {
            Ok(std::sync::Arc::new(
                crate::identity::KeyIdentity::load_or_create(identity_path, None)?,
            ))
        },
    )
    .await??;
    let owner_path = home.join("owner.key");
    let owner = tokio::task::spawn_blocking(move || {
        crate::identity::KeyIdentity::load_or_create(owner_path, None)
            .map(|key| key.did().to_owned())
    })
    .await??;
    let key = StoreKey::from_host(&[19; 32])?;
    super::backend_protection::FreshBackendStore::observe(&data, &lock)
        .await?
        .initialize(
            &key,
            &identity,
            &identity::Did::new(owner)?,
            super::document_acp::DocumentAcp::Local,
        )
        .await?;
    ProtectedBackendStore::open(&data, &key, &lock)
        .await?
        .preflight(&key, &identity, super::document_acp::DocumentAcp::Local)
        .await?;
    tokio::fs::remove_file(data.join(super::store_compat::COMPATIBILITY_STAMP)).await?;
    tokio::fs::write(data.join("MANIFEST"), b"unreadable fixture manifest").await?;
    let original = data.clone();
    let before = tokio::task::spawn_blocking(move || store_bytes(&original)).await??;
    let error = ProtectedBackendStore::open(&data, &key, &lock)
        .await?
        .preflight(&key, &identity, super::document_acp::DocumentAcp::Local)
        .await
        .err()
        .context("a failed copy must refuse the original")?;
    assert!(
        matches!(
            error,
            super::backend_protection::BackendProtectionError::CopyCheck(_)
        ),
        "{error}"
    );
    let after = tokio::task::spawn_blocking(move || store_bytes(&data)).await??;
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test]
async fn diag_fresh_store_reopens_at_once() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("warn,workers::worker=error,storage::backends::shared=error,workers::pool::report=error").with_test_writer().try_init();
    let lanes: usize = std::env::var("DIAG_LANES").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let results = futures::future::join_all((0..lanes).map(diag_lane)).await;
    for result in results {
        result?;
    }
    Ok(())
}

async fn diag_lane(lane: usize) -> anyhow::Result<()> {
    let rounds: usize = std::env::var("DIAG_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    for round in 0..rounds {
        let directory = tempfile::tempdir()?;
        let home = directory.path().to_owned();
        let data = home.join("data");
        let lock_home = home.clone();
        let lock_data = data.clone();
        let lock = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&lock_data)?;
            Ok::<_, anyhow::Error>(home::lock_store(&lock_home, &lock_data)?)
        })
        .await??;
        let key = StoreKey::from_host(&[7; 32])?;
        let identity_path = home.join("node.key");
        let node = tokio::task::spawn_blocking(
            move || -> anyhow::Result<std::sync::Arc<dyn NodeIdentity>> {
                Ok(std::sync::Arc::new(
                    crate::identity::KeyIdentity::load_or_create(identity_path, None)?,
                ))
            },
        )
        .await??;
        let owner_path = home.join("owner.key");
        let owner = tokio::task::spawn_blocking(move || {
            crate::identity::KeyIdentity::load_or_create(owner_path, None)
        })
        .await??;
        super::backend_protection::FreshBackendStore::observe(&data, &lock)
            .await?
            .initialize(&key, &node, &identity::Did::new(owner.did())?, super::document_acp::DocumentAcp::Local)
            .await?;
        let result = match ProtectedBackendStore::open(&data, &key, &lock)
            .await?
            .preflight(&key, &node, super::document_acp::DocumentAcp::Local)
            .await
        {
            Ok(preflight) => preflight
                .start_node_with_projection(
                    &key,
                    node.clone(),
                    |builder| builder,
                    std::sync::Arc::new(DiagNoTools),
                    Vec::new(),
                    &crate::node_extension::Extensions::default(),
                )
                .await
                .map(|(served, _)| served),
            Err(error) => Err(error),
        };
        if let Ok(served) = &result {
            let _ = served.shutdown().await;
        }
        eprintln!("DIAG lane {lane} round {round}: {}", match &result { Ok(_) => "ok".to_owned(), Err(e) => format!("ERR {e}") });
    }
    Ok(())
}

fn diag_lock_held(data: &std::path::Path) -> bool {
    match regolith::Db::open_read_only(data, regolith::Options::default()) {
        Ok(_) => false,
        Err(error) => error.to_string().contains("already locked"),
    }
}

async fn diag_blocked_worker(deadline: std::time::Duration, hold: std::time::Duration, label: &str) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let data = directory.path().join("data");
    let mut config = workers::Config::default();
    config.workers = workers::Workers::Count(1);
    config.shutdown_deadline = deadline;
    let key_path = directory.path().join("node.key");
    let did = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        Ok(crate::identity::KeyIdentity::load_or_create(key_path, None)?.did().to_owned())
    })
    .await??;
    let node = defra_node::Node::builder()
        .with_node_identity_did(did)
        .data_path(&data)
        .with_storage_backend(defra_node::StorageBackend::Regolith)
        .with_workers(config)
        .build()
        .await?;
    crate::schema::ensure_runtime_schemas(&node).await?;
    let weak_db = std::sync::Arc::downgrade(node.database());
    let weak_store = std::sync::Arc::downgrade(node.database().store());
    let pool = node.database().pool().expect("node pool").clone();
    let (entered, started) = std::sync::mpsc::channel();
    let _blocked = pool
        .submit(workers::Target::LeastLoaded, move || async move {
            let _ = entered.send(());
            std::thread::sleep(hold);
        })
        .await?;
    tokio::task::spawn_blocking(move || started.recv()).await??;
    let begun = std::time::Instant::now();
    let shutdown = node.shutdown().await;
    let took = begun.elapsed();
    drop(pool);
    drop(node);
    let held = diag_lock_held(&data);
    eprintln!(
        "SEAM {label}: shutdown={shutdown:?} took={took:?} held_after_drop={held} db_strong={} store_strong={}",
        weak_db.strong_count(),
        weak_store.strong_count()
    );
    let waited = std::time::Instant::now();
    while diag_lock_held(&data) && waited.elapsed() < std::time::Duration::from_secs(20) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    eprintln!("SEAM {label}: released after a further {:?}; db_strong={} store_strong={}", waited.elapsed(), weak_db.strong_count(), weak_store.strong_count());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn diag_seam_blocked_worker() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("warn,workers::worker=error,storage::backends::shared=error,workers::pool::report=error").with_test_writer().try_init();
    use std::time::Duration;
    diag_blocked_worker(Duration::from_millis(200), Duration::from_secs(8), "stall-8s-deadline-200ms").await?;
    diag_blocked_worker(Duration::from_secs(2), Duration::from_secs(12), "stall-12s-deadline-2s").await?;
    Ok(())
}

struct DiagNoTools;

impl crate::local::ToolSourcePolicy for DiagNoTools {
    fn node_operation_sources(
        &self,
        _: &serde_json::Value,
        _: &[serde_json::Value],
    ) -> anyhow::Result<Vec<crate::local::ProjectedOperationSource>> {
        Ok(Vec::new())
    }

    fn plugin_refs(
        &self,
        _: &serde_json::Value,
    ) -> anyhow::Result<Vec<crate::local::ProjectedPluginRef>> {
        Ok(Vec::new())
    }

    fn selected_tools(&self, _: &str, _: &serde_json::Value) -> Option<String> {
        None
    }
}
