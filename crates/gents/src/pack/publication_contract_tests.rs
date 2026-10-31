use std::collections::BTreeMap;

use super::plugin_lifecycle::{install, tests::fixture};

#[tokio::test]
async fn generated_local_publication_survives_notification_outcomes() {
    let contract = crate::lean_vocab_test::lean_contract_snapshot();
    for case in contract.plugin_resource_cases["publication"]
        .as_array()
        .unwrap()
    {
        let home = tempfile::tempdir().unwrap();
        let (_previous_dir, _, previous) = fixture("1.0.0", false);
        if case["previous"].as_bool().unwrap() {
            install(
                home.path(),
                &previous,
                false,
                None,
                &BTreeMap::new(),
                |_| async { Ok(()) },
            )
            .await
            .unwrap();
        }
        let (_next_dir, _, next) = fixture("2.0.0", false);
        let bindings = BTreeMap::new();
        match case["outcome"].as_str().unwrap() {
            "local_failure" => {
                let result = crate::plugin::install::install_pack_plugins_with_bindings(
                    home.path(),
                    next.manifest(),
                    next.digest(),
                    |path| next.asset(path),
                    false,
                    None,
                    &bindings,
                    |_| {
                        assert!(crate::plugin::store::try_lock_store(home.path()).is_err());
                        anyhow::bail!("rejected local publication")
                    },
                );
                assert!(result.is_err());
            }
            "notification_success" => {
                let notify_home = home.path().to_owned();
                install(
                    home.path(),
                    &next,
                    false,
                    None,
                    &bindings,
                    move |_| async move {
                        assert!(crate::plugin::store::try_lock_store(&notify_home).is_err());
                        Ok(())
                    },
                )
                .await
                .unwrap();
            }
            "notification_failure" => {
                let notify_home = home.path().to_owned();
                let result = install(
                    home.path(),
                    &next,
                    false,
                    None,
                    &bindings,
                    move |_| async move {
                        assert!(crate::plugin::store::try_lock_store(&notify_home).is_err());
                        Err(super::PluginStoreChangeError::Unconfirmed(anyhow::anyhow!(
                            "notification response unavailable"
                        )))
                    },
                )
                .await;
                assert!(result.is_err());
            }
            "notification_cancelled" => {
                let reached = std::sync::Arc::new(tokio::sync::Notify::new());
                let release = std::sync::Arc::new(tokio::sync::Notify::new());
                let caller_home = home.path().to_owned();
                let notify_home = caller_home.clone();
                let reached_notify = reached.clone();
                let release_notify = release.clone();
                let operation =
                    tokio::spawn(async move {
                        install(&caller_home, &next, false, None, &bindings, move |_| async move {
                        assert!(crate::plugin::store::try_lock_store(&notify_home).is_err());
                        reached_notify.notify_one();
                        release_notify.notified().await;
                        Ok(())
                    }).await
                    });
                tokio::time::timeout(std::time::Duration::from_secs(20), reached.notified())
                    .await
                    .unwrap();
                operation.abort();
                assert!(operation.await.unwrap_err().is_cancelled());
                assert!(crate::plugin::store::try_lock_store(home.path()).is_err());
                release.notify_one();
                let wait_home = home.path().to_owned();
                let guard = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    tokio::task::spawn_blocking(move || {
                        crate::plugin::store::lock_store(&wait_home, true)
                    }),
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap();
                drop(guard);
            }
            other => panic!("unknown modeled publication outcome {other}"),
        }
        let expected_version = match case["expected"].as_str() {
            Some("new") => Some("2.0.0"),
            Some("previous") => Some("1.0.0"),
            None => None,
            other => panic!("unknown modeled visible installation {other:?}"),
        };
        let pin = super::read_home_install(home.path(), "fixture/native_echo_pack").unwrap();
        assert_eq!(
            pin.as_ref().map(|pin| pin.version.as_str()),
            expected_version,
            "{case}"
        );
        let records = crate::plugin::store::list_records(home.path()).unwrap();
        assert_eq!(
            records.len(),
            usize::from(expected_version.is_some()),
            "{case}"
        );
        if let Some(expected) = expected_version {
            assert_eq!(records[0].version, expected, "{case}");
        }
        assert!(
            crate::plugin::store::try_lock_store(home.path()).is_ok(),
            "{case}"
        );
    }
}

#[tokio::test]
async fn home_removal_excludes_reinstall_until_pin_and_records_are_removed() {
    let home = tempfile::tempdir().unwrap();
    let (_dir, _, archive) = fixture("1.0.0", false);
    let bindings = BTreeMap::new();
    install(home.path(), &archive, false, None, &bindings, |_| async {
        Ok(())
    })
    .await
    .unwrap();
    let prepare_error = super::plugin_lifecycle::remove_local(
        home.path(),
        "fixture/native_echo_pack",
        |_| -> anyhow::Result<()> {
            assert!(crate::plugin::install::install_pack_plugins(
                home.path(),
                archive.manifest(),
                archive.digest(),
                |path| archive.asset(path),
                false,
            )
            .is_err());
            anyhow::bail!("cache is in use")
        },
        |()| Ok(()),
    );
    assert!(prepare_error.is_err());
    assert!(
        super::read_home_install(home.path(), "fixture/native_echo_pack")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        crate::plugin::store::list_records(home.path())
            .unwrap()
            .len(),
        1
    );

    let removal = super::plugin_lifecycle::remove_local(
        home.path(),
        "fixture/native_echo_pack",
        |_| Ok(()),
        |_| {
            assert!(crate::plugin::install::install_pack_plugins(
                home.path(),
                archive.manifest(),
                archive.digest(),
                |path| archive.asset(path),
                false,
            )
            .is_err());
            anyhow::bail!("cache cleanup failed")
        },
    )
    .unwrap();
    assert!(removal.cache_error.is_some());
    assert!(
        super::read_home_install(home.path(), "fixture/native_echo_pack")
            .unwrap()
            .is_none()
    );
    assert!(crate::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    install(home.path(), &archive, false, None, &bindings, |_| async {
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        super::read_home_install(home.path(), "fixture/native_echo_pack")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn non_pin_notification_rejection_restores_previous_plugin_and_home_record() {
    let home = tempfile::tempdir().unwrap();
    let (_old_dir, _, old) = tokio::task::spawn_blocking(|| fixture("1.0.0", false))
        .await
        .unwrap();
    install(
        home.path(),
        &old,
        false,
        None,
        &BTreeMap::new(),
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    let old_home = super::read_home_install(home.path(), "fixture/native_echo_pack")
        .unwrap()
        .unwrap();
    let old_plugin =
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    let (next_dir, _, _) = tokio::task::spawn_blocking(|| fixture("2.0.0", false))
        .await
        .unwrap();
    let bytes = tokio::task::spawn_blocking(|| {
        crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
            br#"{"replacement":true}"#,
        ))
    })
    .await
    .unwrap();
    std::fs::write(
        next_dir.path().join("source/plugins/native_echo.afb"),
        bytes,
    )
    .unwrap();
    let (packed, _) = crate::pack::archive::pack_dir(&next_dir.path().join("source")).unwrap();
    let next = crate::pack::archive::PackArchive::from_bytes(&packed).unwrap();
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = crate::store::config_client::ConfigAccess::local(node.clone());
    access.write("test.dangling_agent", r#"mutation { create_Agent(input: {agent_id: "broken", node_did: "did:test:owner", context_id: "missing", inference_profile_id: "profile"}) { _docID } }"#).await.unwrap();
    let notify_access = access.clone();
    let notify_home = home.path().to_owned();
    let original_digest = old_plugin.digest.clone();
    let error = install(
        home.path(),
        &next,
        false,
        Some("did:test:owner"),
        &BTreeMap::new(),
        move |identity| async move {
            let staged =
                crate::plugin::store::read_record(&notify_home, "fixture", "native_echo").unwrap();
            assert_ne!(staged.digest, original_digest);
            super::record_plugin_store_change(
                &notify_access,
                "did:test:owner",
                &notify_home,
                &identity.coordinate,
                Some(&identity),
            )
            .await
        },
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("references missing context"),
        "{error:#}"
    );
    assert_eq!(
        super::read_home_install(home.path(), "fixture/native_echo_pack").unwrap(),
        Some(old_home)
    );
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap(),
        old_plugin
    );
    node.shutdown().await;
}
