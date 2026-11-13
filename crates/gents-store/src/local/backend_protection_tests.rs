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
