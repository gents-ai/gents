use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use gents::pack::{
    archive::{pack_dir, PackArchive},
    install_prepared_document_graph, installed_catalog, load_pack_config,
    prepare_document_pack_install, DriftPolicy, InstalledVia, PackInstallOptions, PackSource,
};
use gents::store::config_client::ConfigAccess;
use gents::{KeyIdentity, NodeIdentity};
use serde_json::json;

#[test]
fn source_metadata_rejects_paths_and_invalid_registry_digests() {
    for name in ["/private/pack.gents", "folder\\pack.gents", ".."] {
        assert!(PackSource::Local {
            file_name: Some(name.into())
        }
        .validate()
        .is_err());
    }
    assert!(PackSource::Local {
        file_name: Some("pack.gents".into())
    }
    .validate()
    .is_ok());
    assert!(PackSource::Registry {
        namespace: "fixture".into(),
        name: "shared_fixture".into(),
        version: "1.0.0".into(),
        artifact_digest: "invalid".into(),
    }
    .validate()
    .is_err());
}

#[tokio::test]
async fn document_install_records_portable_source_and_history() {
    let keys = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(keys.path().join("node.key"), None).unwrap();
    let owner = identity.did().to_string();
    let node = Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    gents::documents::ensure_node(&node, &owner).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs/shared_fixture");
    let (bytes, _) = pack_dir(&fixture).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();
    let config = load_pack_config(
        archive.manifest(),
        &PackInstallOptions {
            node_did: owner.clone(),
        },
        &|path| Ok(archive.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap();
    let mut prepared = prepare_document_pack_install(
        &access,
        &owner,
        archive.manifest(),
        &config,
        &BTreeMap::new(),
        &[],
        InstalledVia::Operator,
        None,
        Some(&archive),
        &|_| None,
    )
    .await
    .unwrap();
    let coordinate = "fixture/shared_fixture";
    let local = PackSource::local(Path::new("/private/folder/shared.pack"));
    for source in [local.clone(), PackSource::Store] {
        prepared.sources.insert(coordinate.into(), source);
        install_prepared_document_graph(
            &access,
            &owner,
            &archive,
            &prepared,
            DriftPolicy::Refuse,
            None,
            false,
            true,
        )
        .await
        .unwrap();
    }
    let catalog = installed_catalog(&access, &owner, None).await.unwrap();
    let record = &catalog[coordinate].record;
    assert_eq!(record["source"], json!({"kind": "store"}));
    assert_eq!(
        record["source_history"][0]["source"],
        serde_json::to_value(local).unwrap()
    );
    assert_eq!(record["source_history"][0]["digest"], archive.digest());
    assert!(!record.to_string().contains("/private/folder"));
    node.shutdown().await;
}

#[tokio::test]
async fn asset_install_keeps_portable_source_across_reinstall() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs/assets_fixture");
    let (bytes, _) = pack_dir(&fixture).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();
    let home = tempfile::tempdir().unwrap();
    let local = PackSource::local(Path::new("/private/folder/assets.pack"));
    for source in [local.clone(), PackSource::Store] {
        gents::pack::plugin_lifecycle::install_with_source(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::new(),
            Some(source),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    }
    let record = gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
        .unwrap()
        .unwrap();
    assert_eq!(record.source, Some(PackSource::Store));
    assert_eq!(record.source_history[0].source, Some(local));
    assert_eq!(record.source_history[0].digest, archive.digest());
    assert!(!serde_json::to_string(&record)
        .unwrap()
        .contains("/private/folder"));
}
