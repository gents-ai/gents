use std::collections::BTreeMap;

use super::plugin_lifecycle::{install, tests::fixture};

type TestError = Box<dyn std::error::Error + Send + Sync>;

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
                |_| async { Ok::<_, TestError>(()) },
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
                        Err::<(), TestError>("rejected local publication".into())
                    },
                );
                assert!(result.is_err());
            }
            "notification_success" => {
                install(home.path(), &next, false, None, &bindings, |_| async {
                    let _guard = crate::plugin::store::try_lock_store(home.path())?;
                    Ok::<_, TestError>(())
                })
                .await
                .unwrap();
            }
            "notification_failure" => {
                let result = install(home.path(), &next, false, None, &bindings, |_| async {
                    let _guard = crate::plugin::store::try_lock_store(home.path())?;
                    Err::<(), TestError>("notification response unavailable".into())
                })
                .await;
                assert!(result.is_err());
            }
            "notification_cancelled" => {
                let mut operation = Box::pin(install(
                    home.path(),
                    &next,
                    false,
                    None,
                    &bindings,
                    |_| async {
                        {
                            let _guard = crate::plugin::store::try_lock_store(home.path())?;
                        }
                        std::future::pending::<()>().await;
                        Ok::<_, TestError>(())
                    },
                ));
                assert!(futures::poll!(operation.as_mut()).is_pending());
                drop(operation);
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
        Ok::<_, TestError>(())
    })
    .await
    .unwrap();
    let prepare_error = super::plugin_lifecycle::remove_local(
        home.path(),
        "fixture/native_echo_pack",
        |_| -> Result<(), TestError> {
            assert!(crate::plugin::install::install_pack_plugins(
                home.path(),
                archive.manifest(),
                archive.digest(),
                |path| archive.asset(path),
                false,
            )
            .is_err());
            Err("cache is in use".into())
        },
        |()| Ok::<_, TestError>(()),
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
        |_| Ok::<_, TestError>(()),
        |_| {
            assert!(crate::plugin::install::install_pack_plugins(
                home.path(),
                archive.manifest(),
                archive.digest(),
                |path| archive.asset(path),
                false,
            )
            .is_err());
            Err::<(), TestError>("cache cleanup failed".into())
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
        Ok::<_, TestError>(())
    })
    .await
    .unwrap();
    assert!(
        super::read_home_install(home.path(), "fixture/native_echo_pack")
            .unwrap()
            .is_some()
    );
}
