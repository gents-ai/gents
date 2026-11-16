use super::backend_protection::ProtectedBackendStore;
use super::{home, store_key::StoreKey};
use crate::identity::NodeIdentity;
use anyhow::Context;

#[tokio::test]
async fn existing_unbound_store_without_provenance_is_left_untouched() -> anyhow::Result<()> {
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
    let did = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        Ok(
            crate::identity::KeyIdentity::load_or_create(identity_path, None)?
                .did()
                .to_owned(),
        )
    })
    .await??;
    let unbound = gents_protocol::schemas::INFERENCE_BACKEND.replace(
        &format!(
            " @policy(id: \"{}\", resource: \"secret_configuration\")",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        ),
        "",
    );
    let node = super::store_key::persistent_builder(&data, &key)?
        .with_node_identity_did(did.clone())
        .build()
        .await?;
    node.add_collection(&unbound).await?;
    let created = node.execute(r#"mutation { create_InferenceBackend(input:{node_did:"legacy", backend_id:"b", name:"old", provider_kind:"OpenAiCompatible", endpoint:"https://provider.example", auth:{kind:"api_key",key:"legacy-secret"}}) {_docID} }"#).await;
    anyhow::ensure!(
        !created.has_errors(),
        "legacy fixture create failed: {:?}",
        created.errors
    );
    node.shutdown().await?;
    drop(node);
    let cases = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let denied = cases
        .fresh_backend_reopen_cases
        .iter()
        .find(|case| case["marker"] == false && case["store"] == true && case["binding"] == false)
        .unwrap()["allowed"]
        .as_bool()
        .unwrap();
    assert_eq!(
        ProtectedBackendStore::open(&data, &key, &lock)
            .await
            .is_ok(),
        denied
    );
    let node = super::store_key::persistent_builder(&data, &key)?
        .with_node_identity_did(did)
        .build()
        .await?;
    let retained = node.execute("{ InferenceBackend { auth } }").await;
    anyhow::ensure!(
        !retained.has_errors(),
        "legacy fixture read failed: {:?}",
        retained.errors
    );
    let rows = retained.data.context("legacy fixture data missing")?;
    assert_eq!(rows["InferenceBackend"][0]["auth"]["key"], "legacy-secret");
    assert!(node
        .get_collection(gents_protocol::schemas::NODE_NAME)?
        .is_none());
    node.shutdown().await?;
    Ok(())
}

/// A store created before the Agent baseline gained `get-collection` carries
/// the previous lineage marker. It is refused as an older home before any
/// node opens it.
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
    FreshBackendStore::observe(&data, &lock)
        .await?
        .initialize(&key, &node, super::document_acp::DocumentAcp::Local)
        .await?;
    assert!(PROVENANCE_MARKER.ends_with("-v2"));
    std::fs::rename(
        data.join(PROVENANCE_MARKER),
        data.join(".gents-private-backend-v1"),
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
        data.join(".gents-private-backend-v1"),
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
    let key = StoreKey::from_host(&[19; 32])?;
    super::backend_protection::FreshBackendStore::observe(&data, &lock)
        .await?
        .initialize(&key, &identity, super::document_acp::DocumentAcp::Local)
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
