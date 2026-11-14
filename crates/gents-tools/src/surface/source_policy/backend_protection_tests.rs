use anyhow::Context;
use gents_store::identity::NodeIdentity;
use gents_store::local::backend_protection::{FreshBackendStore, ProtectedBackendStore};
use gents_store::local::{home, store_key::StoreKey};

#[tokio::test]
async fn only_the_fresh_store_and_its_key_can_reopen_protected_backend() -> anyhow::Result<()> {
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
    let other_key = StoreKey::from_host(&[8; 32])?;
    let identity_path = home.join("node.key");
    let runtime = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok(std::sync::Arc::new(
            gents_store::identity::KeyIdentity::load_or_create(identity_path, None)?,
        ))
    })
    .await??;
    let did = runtime.did().to_owned();
    let cases = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let allowed = |marker, store, binding| {
        cases
            .fresh_backend_reopen_cases
            .iter()
            .find(|case| {
                case["marker"] == marker && case["store"] == store && case["binding"] == binding
            })
            .unwrap()["allowed"]
            .as_bool()
            .unwrap()
    };
    assert_eq!(
        ProtectedBackendStore::open(&data, &key, &lock)
            .await
            .is_ok(),
        allowed(false, true, true)
    );
    FreshBackendStore::observe(&data, &lock)
        .await?
        .initialize(&key, Some(&did))
        .await?;
    assert_eq!(
        ProtectedBackendStore::open(&data, &other_key, &lock)
            .await
            .is_ok(),
        allowed(false, true, true)
    );
    assert_eq!(
        ProtectedBackendStore::open(&data, &key, &lock)
            .await
            .is_ok(),
        allowed(true, true, true)
    );
    assert!(FreshBackendStore::observe(&data, &lock).await.is_err());
    let protected = ProtectedBackendStore::open(&data, &key, &lock)
        .await?
        .preflight(&key, Some(&did))
        .await?;
    let (node, admission) = protected
        .start_node_with_projection(
            &key,
            runtime,
            None,
            None,
            std::sync::Arc::new(crate::surface::ToolCeiling::meta_only()),
            Vec::new(),
            &gents_session::test_support::no_lifecycle_plugins(),
        )
        .await?;
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    for entry in gents_migration::DEFAULT_BASELINE.iter() {
        let collection = node
            .get_collection(entry.name)?
            .with_context(|| format!("missing fresh baseline collection {}", entry.name))?;
        assert_eq!(
            Some(collection.version_id.as_str()),
            entry.expected_version,
            "fresh protected baseline lineage for {}",
            entry.name
        );
    }
    assert_eq!(
        admission.validate(&node).await.is_ok(),
        allowed(true, true, true)
    );
    let foreign = std::sync::Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(did)
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    assert_eq!(
        admission.validate(&foreign).await.is_ok(),
        allowed(true, false, true)
    );
    foreign.shutdown().await?;
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_node_reopened_with_nac_disabled_is_refused_at_startup() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::node_provisioning_runtime_nac::cases::<String>() {
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
        let key = StoreKey::from_host(&[9; 32])?;
        let identity_path = home.join("node.key");
        let runtime = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(std::sync::Arc::new(
                gents_store::identity::KeyIdentity::load_or_create(identity_path, None)?,
            ))
        })
        .await??;
        let did = runtime.did().to_owned();
        FreshBackendStore::observe(&data, &lock)
            .await?
            .initialize(&key, Some(&did))
            .await?;
        let (node, admission) = ProtectedBackendStore::open(&data, &key, &lock)
            .await?
            .preflight(&key, Some(&did))
            .await?
            .start_node_with_projection(
                &key,
                runtime,
                None,
                None,
                std::sync::Arc::new(crate::surface::ToolCeiling::meta_only()),
                Vec::new(),
                &gents_session::test_support::no_lifecycle_plugins(),
            )
            .await?;
        match case.input.status.as_str() {
            "enabled" => {}
            "disabled_temporarily" => {
                let owner = defra_core::RequestContext::default().with_identity(Some(did.clone()));
                node.disable_nac_as(&owner).await?;
            }
            other => panic!("no production start reaches NAC status {other}"),
        }
        let admitted = match admission.validate(&node).await {
            Ok(()) => true,
            Err(
                gents_store::local::backend_protection::BackendProtectionError::NodeAcpNotEnabled {
                    ..
                },
            ) => false,
            Err(error) => panic!("{:?} refused for another reason: {error}", case.input),
        };
        node.shutdown().await?;
        drop(node);
        let restarted = match ProtectedBackendStore::open(&data, &key, &lock)
            .await?
            .preflight(&key, Some(&did))
            .await
        {
            Ok(_) => true,
            Err(
                error @ gents_store::local::backend_protection::BackendProtectionError::NodeAcpNotEnabled {
                    ..
                },
            ) => {
                assert!(error.to_string().contains("re-enable NAC"), "{error}");
                false
            }
            Err(error) => panic!("{:?} preflight refused for another reason: {error}", case.input),
        };
        assert_eq!(
            admitted, restarted,
            "{:?}: the running node and its restart disagree",
            case.input
        );
        case.check(
            gents_lean_vocab::families::node_provisioning_runtime_nac::NacOutput {
                may_start: admitted,
            },
        );
    }
    Ok(())
}
