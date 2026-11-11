use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use gents_pack::pack::{
    archive::{pack_dir, PackArchive},
    install_prepared_document_graph, installed_catalog, load_pack_config,
    prepare_document_pack_install, DriftPolicy, InstalledVia, PackInstallOptions, PackSource,
};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
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
        gents::defra_node::Node::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local_as(node.clone(), owner.parse().unwrap());
    access
        .write(
            "test.pack_source.node",
            &format!(
                "mutation {{ create_Node(input: {{node_did: \"{}\", enabled: true}}) {{_docID}} }}",
                gents_store::graphql::escape_graphql_string(&owner),
            ),
        )
        .await
        .unwrap();
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
        &Default::default(),
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
            &gents_inference::resolved::ProviderProfilePolicy,
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
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn asset_install_keeps_portable_source_across_reinstall() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs/assets_fixture");
    let (bytes, _) = pack_dir(&fixture).unwrap();
    let archive = PackArchive::from_bytes(&bytes).unwrap();
    let home = tempfile::tempdir().unwrap();
    let local = PackSource::local(Path::new("/private/folder/assets.pack"));
    for source in [local.clone(), PackSource::Store] {
        gents_pack::pack::plugin_lifecycle::install(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::new(),
            gents_pack::pack::InstalledVia::Operator,
            source,
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap();
    }
    let record = gents_pack::pack::read_home_install(home.path(), "fixture/assets_fixture")
        .unwrap()
        .unwrap();
    assert_eq!(record.source, Some(PackSource::Store));
    assert_eq!(record.source_history[0].source, Some(local));
    assert_eq!(record.source_history[0].digest, archive.digest());
    assert!(!serde_json::to_string(&record)
        .unwrap()
        .contains("/private/folder"));
}
