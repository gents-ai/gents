use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde::Serialize;

use crate::pack::store::PackStore;
use crate::runtime::agent::p2p_reconcile::templates::admit_app_collections;
use crate::store::config_client::{preview_schema_install, ConfigAccess};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledPackScopePreview {
    pub coordinate: String,
    pub version: String,
    pub digest: String,
    pub collection_contracts: BTreeMap<String, String>,
}

/// Resolve the installed artifact and compare its declared schemas with the
/// current node. The caller must authenticate the operator before invoking
/// this function. A preview neither grants access nor proves document ACP.
pub async fn preview_installed_pack_scope(
    access: &ConfigAccess,
    home: &Path,
    owner: &str,
    coordinate: &str,
    expected_digest: &str,
) -> Result<InstalledPackScopePreview> {
    let installed = super::read_installed_pack(access, owner, coordinate)
        .await?
        .with_context(|| format!("pack {coordinate:?} is not installed on this node"))?;
    ensure!(
        installed.digest == expected_digest,
        "installed pack changed; preview again"
    );
    let archive = PackStore::new(home).open(&installed.digest)?;
    let header = archive.header();
    ensure!(
        header.coordinate == installed.coordinate && header.version == installed.version,
        "installed pack identity does not match its stored artifact"
    );

    let mut contracts = BTreeMap::new();
    for path in super::document_pack_schema_paths(archive.manifest())? {
        let sdl = std::str::from_utf8(archive.asset(path)?)
            .with_context(|| format!("schema {path:?} is not UTF-8"))?;
        let plan = preview_schema_install(access, sdl)
            .await
            .with_context(|| format!("checking installed schema {path:?}"))?;
        plan.require_satisfied()?;
        for (name, contract) in plan.collection_contracts {
            ensure!(
                contracts.insert(name.clone(), contract).is_none(),
                "pack declares collection {name:?} more than once"
            );
        }
    }
    ensure!(
        admit_app_collections(contracts.keys().cloned().collect()).is_some(),
        "pack declares no application collections or includes a protocol collection"
    );
    ensure!(
        super::read_installed_pack(access, owner, coordinate).await? == Some(installed.clone()),
        "installed pack changed; preview again"
    );
    Ok(InstalledPackScopePreview {
        coordinate: installed.coordinate,
        version: installed.version,
        digest: installed.digest,
        collection_contracts: contracts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn preview_requires_the_installed_artifact_and_matching_schema() {
        const OWNER: &str = "did:key:zPackScopeOwner";
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node);
        let home = tempfile::tempdir().unwrap();
        let (_fixture, root) =
            crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
        let (bytes, _) = crate::pack::archive::pack_dir(&root).unwrap();
        let stored = PackStore::new(home.path())
            .import(&bytes[..], None)
            .unwrap();
        let coordinate = "fixture/documents_fixture";
        assert!(preview_installed_pack_scope(
            &access,
            home.path(),
            OWNER,
            coordinate,
            &stored.header.digest
        )
        .await
        .is_err());

        let config: crate::documents::PackConfig = serde_json::from_value(json!({
            "node": {"node_did": OWNER}
        }))
        .unwrap();
        let identity = super::super::PackIdentity {
            coordinate: coordinate.into(),
            version: stored.header.version.clone(),
            digest: stored.header.digest.clone(),
            plugins: vec![],
            dependencies: vec![],
            explicit: true,
        };
        crate::pack::install_pack_documents(
            &access,
            OWNER,
            &identity,
            &config,
            super::super::DriftPolicy::Refuse,
        )
        .await
        .unwrap();
        assert!(preview_installed_pack_scope(
            &access,
            home.path(),
            "did:key:another",
            coordinate,
            &stored.header.digest
        )
        .await
        .is_err());
        assert!(preview_installed_pack_scope(
            &access,
            home.path(),
            OWNER,
            coordinate,
            "sha256:wrong"
        )
        .await
        .is_err());
        assert!(preview_installed_pack_scope(
            &access,
            home.path(),
            OWNER,
            coordinate,
            &stored.header.digest
        )
        .await
        .is_err());

        let sdl = std::fs::read_to_string(root.join("schemas/fixture_job.graphql")).unwrap();
        access.add_collection(&sdl).await.unwrap();
        let result = preview_installed_pack_scope(
            &access,
            home.path(),
            OWNER,
            coordinate,
            &stored.header.digest,
        )
        .await
        .unwrap();
        assert_eq!(result.coordinate, coordinate);
        assert_eq!(result.version, stored.header.version);
        assert_eq!(result.digest, stored.header.digest);
        assert_eq!(
            result.collection_contracts.keys().collect::<Vec<_>>(),
            vec!["FixtureJob"]
        );
    }

    #[tokio::test]
    async fn preview_rejects_a_different_published_schema() {
        const OWNER: &str = "did:key:zPackScopeOwner";
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node);
        let home = tempfile::tempdir().unwrap();
        let (_fixture, root) =
            crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
        let (bytes, _) = crate::pack::archive::pack_dir(&root).unwrap();
        let stored = PackStore::new(home.path())
            .import(&bytes[..], None)
            .unwrap();
        let coordinate = "fixture/documents_fixture";
        let config: crate::documents::PackConfig = serde_json::from_value(json!({
            "node": {"node_did": OWNER}
        }))
        .unwrap();
        let identity = super::super::PackIdentity {
            coordinate: coordinate.into(),
            version: stored.header.version.clone(),
            digest: stored.header.digest.clone(),
            plugins: vec![],
            dependencies: vec![],
            explicit: true,
        };
        crate::pack::install_pack_documents(
            &access,
            OWNER,
            &identity,
            &config,
            super::super::DriftPolicy::Refuse,
        )
        .await
        .unwrap();
        access
            .add_collection("type FixtureJob { job_id: Int }")
            .await
            .unwrap();
        let error = preview_installed_pack_scope(
            &access,
            home.path(),
            OWNER,
            coordinate,
            &stored.header.digest,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("does not match requested schema"));
    }
}
