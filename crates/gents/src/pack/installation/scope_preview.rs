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

    async fn fixture() -> (tempfile::TempDir, String, ConfigAccess, ConfigAccess) {
        use crate::NodeIdentity as _;
        let home = tempfile::tempdir().unwrap();
        let runtime = crate::KeyIdentity::load_or_create(home.path().join("node"), None).unwrap();
        let user = crate::KeyIdentity::load_or_create(home.path().join("user"), None).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let node = Arc::new(
            crate::test_support::node_builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(runtime.did().to_owned())
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let runtime_access = ConfigAccess::local_as(node, runtime.did().parse().unwrap());
        let user_did = identity::Did::new(user.did()).unwrap();
        for permission in [
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DocumentDelete,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            runtime_access
                .grant_node_permission(&user_did, permission)
                .await
                .unwrap();
        }
        let access = ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(
                format!("http://{address}/api/v0/graphql"),
                user.did(),
            )
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
        );
        let value = json!({"node_did": runtime.did()});
        let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
            crate::store::config_client::DesiredStateApplyDocument {
                collection: crate::Collection::Node,
                add: value.clone(),
                update: value,
            },
        ])
        .unwrap();
        access
            .transact_write("test.pack_scope.node", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::store::config_client::apply_desired_state_plan(txn, plan)
                        .await
                        .map(|_| ())
                })
            })
            .await
            .unwrap();
        (home, runtime.did().to_owned(), access, runtime_access)
    }

    #[tokio::test]
    async fn preview_requires_the_installed_artifact_and_matching_schema() {
        let (_identities, owner, access, runtime_access) = fixture().await;
        let owner = owner.as_str();
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
            owner,
            coordinate,
            &stored.header.digest
        )
        .await
        .is_err());

        let config: crate::documents::PackConfig = serde_json::from_value(json!({
            "node": {"node_did": owner}
        }))
        .unwrap();
        let identity = super::super::PackIdentity {
            installation_doc_id: None,
            agent_bindings: BTreeMap::new(),
            coordinate: coordinate.into(),
            version: stored.header.version.clone(),
            digest: stored.header.digest.clone(),
            plugins: vec![],
            dependencies: vec![],
            explicit: true,
        };
        crate::pack::install_pack_documents(
            &access,
            owner,
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
            owner,
            coordinate,
            "sha256:wrong"
        )
        .await
        .is_err());
        assert!(preview_installed_pack_scope(
            &access,
            home.path(),
            owner,
            coordinate,
            &stored.header.digest
        )
        .await
        .is_err());

        let sdl = std::fs::read_to_string(root.join("schemas/fixture_job.graphql")).unwrap();
        runtime_access.add_collection(&sdl).await.unwrap();
        let result = preview_installed_pack_scope(
            &access,
            home.path(),
            owner,
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
        let (_identities, owner, access, runtime_access) = fixture().await;
        let owner = owner.as_str();
        let home = tempfile::tempdir().unwrap();
        let (_fixture, root) =
            crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
        let (bytes, _) = crate::pack::archive::pack_dir(&root).unwrap();
        let stored = PackStore::new(home.path())
            .import(&bytes[..], None)
            .unwrap();
        let coordinate = "fixture/documents_fixture";
        let config: crate::documents::PackConfig = serde_json::from_value(json!({
            "node": {"node_did": owner}
        }))
        .unwrap();
        let identity = super::super::PackIdentity {
            installation_doc_id: None,
            agent_bindings: BTreeMap::new(),
            coordinate: coordinate.into(),
            version: stored.header.version.clone(),
            digest: stored.header.digest.clone(),
            plugins: vec![],
            dependencies: vec![],
            explicit: true,
        };
        crate::pack::install_pack_documents(
            &access,
            owner,
            &identity,
            &config,
            super::super::DriftPolicy::Refuse,
        )
        .await
        .unwrap();
        runtime_access
            .add_collection("type FixtureJob { job_id: Int }")
            .await
            .unwrap();
        let error = preview_installed_pack_scope(
            &access,
            home.path(),
            owner,
            coordinate,
            &stored.header.digest,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("does not match requested schema"));
    }
}
