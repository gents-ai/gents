use super::*;

fn unfinished_home(temp: &Path) -> (PathBuf, gents_store::client_journal::paths::DesktopPaths) {
    let home = temp.join("agent");
    write(
        &home.join("init.json"),
        r#"{"node_did":"did:key:forge","node_name":"Forge","tool_ceiling":"readonly"}"#,
    );
    write(&home.join("keys/Forge.key"), "identity");
    write(
        &home.join(gents_store::local::home::BOOTSTRAP_CONSUMED_FILE_NAME),
        "",
    );
    let desktop = gents_store::client_journal::paths::DesktopPaths::from_root(temp.join("desktop"));
    write(desktop.runtime_directory_path(), r#"{"connections": []}"#);
    (home, desktop)
}

#[tokio::test]
async fn a_home_from_an_unfinished_attempt_is_not_reported_as_another_node() {
    let temp = tempfile::tempdir().unwrap();
    let (home, _) = unfinished_home(temp.path());

    let refusal = refuse_renaming_home(&home, "Verify", true)
        .await
        .expect_err("a different name is never silently applied");
    assert_eq!(refusal.code, BridgeErrorCode::UnfinishedLocalSetup);
    assert!(
        refusal.message.contains("never finished")
            && refusal.message.contains("Continue with Forge")
            && refusal.message.contains("start over"),
        "{}",
        refusal.message
    );

    let settled = refuse_renaming_home(&home, "Verify", false)
        .await
        .expect_err("a finished home still refuses a rename");
    assert_eq!(settled.code, BridgeErrorCode::InvalidArgument);
    assert!(
        settled
            .message
            .contains("already has a local Node named Forge"),
        "{}",
        settled.message
    );

    refuse_renaming_home(&home, "Forge", true)
        .await
        .expect("the recorded name is always allowed to continue");
}

#[tokio::test]
async fn an_unfinished_home_is_recognized_only_before_it_served_or_paired() {
    let (temp, state) = orchestration_state();
    let home = state.policy.node_home.clone().unwrap();
    let desktop = state.policy.desktop_paths.clone();
    let (_, _) = unfinished_home(temp.path());
    assert!(unfinished_local_setup(&state, Some(&home)).await);

    write(
        &home.join(gents_store::local::home::RUNTIME_STATE_FILE_NAME),
        "{}",
    );
    assert!(
        !unfinished_local_setup(&state, Some(&home)).await,
        "a home that served is not an unfinished attempt"
    );
    std::fs::remove_file(home.join(gents_store::local::home::RUNTIME_STATE_FILE_NAME)).unwrap();

    write(
        desktop.runtime_directory_path(),
        r#"{"connections": [{"node_did": "did:key:forge", "label": "Forge", "graphql": "http://127.0.0.1:9191/api/v0/graphql", "local_node_home": null, "access_request": null, "access_state": null}]}"#,
    );
    assert!(
        !unfinished_local_setup(&state, Some(&home)).await,
        "a home this desktop paired with is not an unfinished attempt"
    );
}

#[test]
fn starting_over_on_an_unfinished_home_archives_it() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = unfinished_home(&temp);
    let home = std::fs::canonicalize(&home).unwrap();

    let plan = plan_home_reset(
        Some(&home),
        None,
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
        true,
    )
    .expect("an unfinished home is in scope for an explicit start over");
    let preview = plan.preview();
    assert_eq!(preview.reason, HomeResetReason::UnfinishedSetup);
    assert!(preview.stores.is_empty());

    let reset = plan
        .retire(
            HomeResetDisposition::Archive,
            Some(&plan.backup_path("20261112T000000.000Z").unwrap()),
        )
        .unwrap();
    let backup = PathBuf::from(reset.backup_path.unwrap());
    for moved in [
        "init.json",
        "keys/Forge.key",
        gents_store::local::home::BOOTSTRAP_CONSUMED_FILE_NAME,
    ] {
        assert!(backup.join("home").join(moved).exists(), "{moved} archived");
        assert!(!home.join(moved).exists(), "{moved} left the home");
    }
    assert!(
        home.is_dir(),
        "the home directory stays for the fresh start"
    );
}

#[test]
fn a_healthy_home_is_still_refused_without_an_unfinished_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = unfinished_home(&temp);

    let refusal = plan_home_reset(
        Some(&home),
        None,
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
        false,
    )
    .expect_err("nothing is retired without a reason");
    assert_eq!(refusal.code, BridgeErrorCode::InvalidArgument);
}

#[tokio::test]
async fn backing_up_drops_the_pairing_with_the_node_it_retired() {
    let (temp, state) = orchestration_state();
    let home = state.policy.node_home.clone().unwrap();
    let desktop = state.policy.desktop_paths.clone();
    let (_, _) = unfinished_home(temp.path());
    write(
        desktop.runtime_directory_path(),
        &format!(
            r#"{{"connections": [{{"node_did": "did:key:forge", "label": "Forge", "graphql": "http://127.0.0.1:9191/api/v0/graphql", "local_node_home": {home:?}, "access_request": null, "access_state": null}}, {{"node_did": "did:key:remote", "label": "Mandrake", "graphql": "http://127.0.0.1:9191/api/v0/graphql", "local_node_home": null, "access_request": null, "access_state": null}}]}}"#,
            home = home.to_string_lossy()
        ),
    );

    let dropped = retire_desktop_pairing(&state, &home).await;
    assert_eq!(dropped, 1);
    let left = gents_store::client_journal::runtime_directory::load_runtime_connections(
        desktop.runtime_directory_path(),
    )
    .await
    .unwrap();
    assert_eq!(
        left.iter()
            .map(|record| record.node_did.clone())
            .collect::<Vec<_>>(),
        vec!["did:key:remote".to_string()],
        "only the retired node's pairing is dropped"
    );
}
