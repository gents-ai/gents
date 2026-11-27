use super::backend_protection::ProtectedBackendStore;
use super::{home, store_key::StoreKey};
use crate::identity::NodeIdentity;
use anyhow::Context;

/// A store created before `agent_manager` took only the Node administrators'
/// userset carries the previous lineage marker. It is refused as an older home
/// before any node opens it.
#[tokio::test]
async fn a_store_with_the_previous_lineage_marker_is_refused_unopened() -> anyhow::Result<()> {
    use super::backend_protection::{BackendProtectionError, PROVENANCE_MARKER};
    let directory = tempfile::tempdir()?;
    let home = directory.path().to_owned();
    let data = fresh_protected_store(&home).await?;
    assert!(PROVENANCE_MARKER.ends_with("-v3"));
    let manifest = data.join(super::store_migration::STORE_MANIFEST_FILE);
    let manifest_bytes = std::fs::read(&manifest)?;
    std::fs::remove_file(&manifest)?;
    std::fs::rename(
        data.join(PROVENANCE_MARKER),
        data.join(".gents-private-backend-v2"),
    )?;
    let before = store_bytes(&home)?;
    assert!(before
        .iter()
        .any(|(path, _)| path == &home.join("data.lock")));

    let refused = super::backend_protection::refuse_other_lineage(&data)
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
    assert_eq!(
        store_bytes(&home)?,
        before,
        "the refused home is byte-identical"
    );

    // A store with this version's marker and manifest, and one with no marker,
    // pass to the lock.
    std::fs::write(&manifest, &manifest_bytes)?;
    std::fs::rename(
        data.join(".gents-private-backend-v2"),
        data.join(PROVENANCE_MARKER),
    )?;
    super::backend_protection::refuse_other_lineage(&data)?;
    std::fs::remove_file(data.join(PROVENANCE_MARKER))?;
    super::backend_protection::refuse_other_lineage(&data)?;
    Ok(())
}

async fn fresh_protected_store(home: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let data = home.join("data");
    let (lock_home, lock_data) = (home.to_owned(), data.clone());
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
        .initialize(
            &key,
            &node,
            &identity::Did::new(owner.did())?,
            super::document_acp::DocumentAcp::Local,
        )
        .await?;
    drop(lock);
    Ok(data)
}

/// A store a newer release made carries a later lineage marker. It is refused
/// unopened, but never as an older home: that release may still want it.
#[tokio::test]
async fn a_store_from_a_newer_release_is_never_reported_as_older() -> anyhow::Result<()> {
    use super::backend_protection::PROVENANCE_MARKER;
    let directory = tempfile::tempdir()?;
    let home = directory.path().to_owned();
    let data = fresh_protected_store(&home).await?;
    assert!(PROVENANCE_MARKER.ends_with("-v3"));
    std::fs::rename(
        data.join(PROVENANCE_MARKER),
        data.join(".gents-private-backend-v4"),
    )?;
    let before = store_bytes(&home)?;
    assert!(before
        .iter()
        .any(|(path, _)| path == &home.join("data.lock")));

    let refused = super::backend_protection::refuse_other_lineage(&data)
        .expect_err("a newer lineage is not this version's");
    let store = super::storage_backend::incompatible_store(&refused, &data)
        .context("the refusal is reported as an incompatible store")?;
    assert_eq!(
        store_bytes(&home)?,
        before,
        "the refused home is byte-identical"
    );
    assert!(
        !store.kind.is_older(),
        "a newer store is offered for deletion: kind {:?}, is_older {}, made_by_older_version {}",
        store.kind,
        store.kind.is_older(),
        store.kind.made_by_older_version()
    );
    Ok(())
}

#[tokio::test]
async fn a_home_with_a_store_from_a_newer_release_is_never_reported_as_older() -> anyhow::Result<()>
{
    use super::backend_protection::PROVENANCE_MARKER;
    let directory = tempfile::tempdir()?;
    let home = directory.path().to_owned();
    let data = fresh_protected_store(&home).await?;
    std::fs::rename(
        data.join(PROVENANCE_MARKER),
        data.join(".gents-private-backend-v4"),
    )?;
    assert!(data.join("MANIFEST").is_file(), "the home holds a store");

    let kind = refused_unchanged(&home).await?;
    assert!(
        !kind.is_older(),
        "a newer store is offered for deletion: kind {kind:?}, is_older {}, made_by_older_version {}",
        kind.is_older(),
        kind.made_by_older_version()
    );
    Ok(())
}

/// What `check_home_store` refuses the home as, asserting the look leaves the
/// home byte-identical.
async fn refused_unchanged(
    home: &std::path::Path,
) -> anyhow::Result<gents_protocol::operations::install_repair::IncompatibleStoreKind> {
    use super::store_compat::{check_home_store, StoreCompatibility};
    let before = store_bytes(home)?;
    let verdict = check_home_store(home).await?;
    assert_eq!(store_bytes(home)?, before, "the home is byte-identical");
    match verdict {
        StoreCompatibility::Refused(store) => Ok(store.kind),
        verdict => anyhow::bail!("the home is refused, not {verdict:?}"),
    }
}

/// A home whose store has an earlier lineage's marker, or no marker, is
/// reported as older, and the look leaves it byte-identical.
#[tokio::test]
async fn a_home_with_an_earlier_or_no_lineage_marker_is_reported_as_older() -> anyhow::Result<()> {
    use super::backend_protection::PROVENANCE_MARKER;
    use gents_protocol::operations::install_repair::IncompatibleStoreKind;
    let directory = tempfile::tempdir()?;
    let home = directory.path().to_owned();
    let data = fresh_protected_store(&home).await?;
    let earlier = data.join(".gents-private-backend-v2");
    std::fs::remove_file(data.join(super::store_migration::STORE_MANIFEST_FILE))?;
    std::fs::rename(data.join(PROVENANCE_MARKER), &earlier)?;
    assert_eq!(
        refused_unchanged(&home).await?,
        IncompatibleStoreKind::UnknownLineage,
        "an earlier marker"
    );
    std::fs::remove_file(&earlier)?;
    assert_eq!(
        refused_unchanged(&home).await?,
        IncompatibleStoreKind::UnknownLineage,
        "no marker"
    );
    Ok(())
}

#[test]
fn lineage_refusal_matches_the_model() {
    use super::backend_protection::{lineage_refusal, BackendProtectionError};
    use gents_lean_vocab::families::backend_preflight_lineage_refusal as family;
    family::replay(|input: &family::LineageInput| {
        lineage_refusal(input.current, &input.names, input.before_lock).map(|refusal| {
            match refusal {
                BackendProtectionError::MissingProvenance => "older",
                BackendProtectionError::NewerLineage => "newer",
                error => panic!("unexpected lineage refusal: {error}"),
            }
            .to_owned()
        })
    });
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
