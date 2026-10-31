use super::*;

#[cfg(unix)]
#[test]
fn stalled_reset_helper_is_killed_without_retiring_state() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("helper");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = temp.path().join("home");
    write(&home.join("init.json"), "original state");
    let before = std::time::Instant::now();
    let result = launch_reset_helper_with_timeout(
        &script,
        &home,
        temp.path(),
        "confirmation",
        HomeResetDisposition::Archive,
        Duration::from_millis(30),
    );
    assert!(result.is_err());
    assert!(before.elapsed() < Duration::from_secs(10));
    assert_eq!(
        std::fs::read_to_string(home.join("init.json")).unwrap(),
        "original state"
    );
}

#[test]
fn archive_moves_the_home_and_client_state_to_a_sibling_backup_and_spares_other_homes() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);

    let plan = plan_home_reset(
        Some(&home),
        Some(lineage_refusal(PathBuf::from("ignored"))),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    assert!(!preview.completed);
    assert_eq!(
        preview.managed_home.as_deref(),
        Some(home.to_str().unwrap())
    );
    assert_eq!(preview.stores.len(), 1);
    assert_eq!(preview.stores[0].scope, IncompatibleStoreScope::Runtime);
    assert_eq!(preview.stores[0].path, home.join("data").to_str().unwrap());
    assert_eq!(
        preview.retained_paths,
        vec![home.join("grok-port-home").to_string_lossy().into_owned()]
    );
    assert!(home.join("data").exists(), "a preview changes nothing");
    assert_eq!(
        plan.check_confirmation(
            preview.delete_confirmation.as_deref().unwrap(),
            HomeResetDisposition::Archive
        )
        .unwrap_err()
        .code,
        BridgeErrorCode::InvalidArgument,
        "each action has its own confirmation"
    );
    plan.check_confirmation(preview.confirmation.as_str(), HomeResetDisposition::Archive)
        .unwrap();

    let reset = plan
        .retire(
            HomeResetDisposition::Archive,
            Some(&plan.backup_path("20260924T000000.000Z").unwrap()),
        )
        .unwrap();

    assert!(reset.completed);
    assert_eq!(reset.disposition, Some(HomeResetDisposition::Archive));
    let backup = PathBuf::from(reset.backup_path.unwrap());
    assert_eq!(backup, temp.join(".gents-backup-20260924T000000.000Z"));
    assert_eq!(
        std::fs::read_to_string(backup.join("home/data/MANIFEST")).unwrap(),
        "REGOMAN old lineage"
    );
    for moved in [
        "init.json",
        "keys/local.key",
        "runtime.json",
        "p2p-secret-key",
    ] {
        assert!(backup.join("home").join(moved).exists(), "{moved} archived");
        assert!(!home.join(moved).exists(), "{moved} left the home");
    }
    for moved in [
        "node/MANIFEST",
        "peers.json",
        "principal.json",
        "principal.ed25519.key",
        "node.iroh.key",
        MANAGED_SERVER_CONFIG,
    ] {
        assert!(
            backup.join("desktop").join(moved).exists(),
            "{moved} archived"
        );
        assert!(
            !desktop.root().join(moved).exists(),
            "{moved} left the client"
        );
    }
    assert_eq!(
        std::fs::read_to_string(home.join("grok-port-home/data/MANIFEST")).unwrap(),
        "REGOMAN other",
        "another agent's home under the managed home is untouched"
    );
    assert_eq!(
        std::fs::read_to_string(desktop.root().join("runtime/gents")).unwrap(),
        "packaged runtime",
        "the packaged runtime is not client state"
    );
    assert!(
        home.is_dir(),
        "the home directory itself stays for the fresh start"
    );
}

#[test]
fn delete_removes_only_the_home_and_client_state_it_previewed() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let outside = temp.join("outside");
    write(&outside.join("keep.txt"), "keep");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, home.join("linked")).unwrap();

    let plan = plan_home_reset(
        Some(&home),
        Some(lineage_refusal(home.join("data"))),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    plan.check_confirmation(
        preview.delete_confirmation.as_deref().unwrap(),
        HomeResetDisposition::Delete,
    )
    .unwrap();
    let reset = plan.retire(HomeResetDisposition::Delete, None).unwrap();

    assert!(reset.completed);
    assert_eq!(reset.backup_path, None);
    assert!(!home.join("data").exists());
    assert!(!home.join("keys").exists());
    assert!(!desktop.node_data_dir().exists());
    assert!(!desktop.identity_key_path().exists());
    assert!(home.join("grok-port-home/init.json").is_file());
    assert!(desktop.root().join("runtime/gents").is_file());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep",
        "a symbolic link is removed, never followed"
    );
    assert!(std::fs::read_dir(&temp).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("backup")));
}

#[test]
fn keeping_the_home_is_a_preview_that_touches_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let listing = |dir: &Path| {
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let (home_before, desktop_before) = (listing(&home), listing(desktop.root()));

    let plan = plan_home_reset(
        Some(&home),
        Some(lineage_refusal(home.join("data"))),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let _ = plan.preview();
    assert!(plan
        .check_confirmation("RESET SOME OTHER HOME", HomeResetDisposition::Archive)
        .is_err());

    assert_eq!(listing(&home), home_before);
    assert_eq!(listing(desktop.root()), desktop_before);
}

#[test]
fn starting_fresh_from_a_refused_client_store_retires_its_paired_home_before_relaunch() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);

    let plan = plan_home_reset(
        Some(&home),
        None,
        Some(lineage_refusal(desktop.node_data_dir().to_path_buf())),
        &desktop,
        not_the_user_home(),
        ClientBinding::BoundToHome,
    )
    .unwrap();
    let preview = plan.preview();
    assert_eq!(
        preview.managed_home.as_deref(),
        Some(home.to_str().unwrap())
    );
    assert!(preview
        .planned_paths
        .contains(&home.join("data").to_string_lossy().into_owned()));
    assert!(
        preview.delete_confirmation.is_none(),
        "an unopened paired home may belong to a newer build: archive only"
    );
    plan.check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
        .unwrap();
    let backup = plan.backup_path("20260928T000000.000Z").unwrap();
    plan.retire(HomeResetDisposition::Archive, Some(&backup))
        .unwrap();

    let entries = gents::store::local::home::home_entries(&home, &[]).unwrap();
    assert!(entries.owned.is_empty(), "{:?}", entries.owned);
    assert!(!gents_server::server_host::initialized_home(&home));
    assert!(home.join("grok-port-home/data/MANIFEST").is_file());
    assert!(!desktop.node_data_dir().exists());
    assert!(!desktop.root().join(MANAGED_SERVER_CONFIG).exists());
}

#[test]
fn orphaned_client_state_is_archived_without_touching_any_home() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    std::fs::write(desktop.node_data_dir().join("MANIFEST"), "REGOMAN").unwrap();

    assert!(
        plan_home_reset(
            None,
            None,
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound
        )
        .is_err(),
        "healthy client state alone is not retired"
    );
    let plan = plan_home_reset(
        None,
        None,
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Orphaned,
    )
    .unwrap();
    let preview = plan.preview();
    assert_eq!(preview.managed_home, None);
    assert!(preview.stores.is_empty());
    assert!(preview.delete_confirmation.is_none(), "archive only");
    let reset = plan
        .retire(
            HomeResetDisposition::Archive,
            Some(&plan.backup_path("20260928T000000.000Z").unwrap()),
        )
        .unwrap();
    let backup = PathBuf::from(reset.backup_path.unwrap());
    assert!(backup.join("desktop/node/MANIFEST").is_file());
    assert!(backup.join("desktop").join(MANAGED_SERVER_CONFIG).is_file());
    assert!(!desktop.node_data_dir().exists());
    assert!(home.join("data/MANIFEST").is_file());
    assert!(home.join("init.json").is_file());
}

#[test]
fn a_reviewed_home_that_is_gone_or_reidentified_orphans_the_client_state() {
    let reviewed = ReviewedHome {
        node_home: "/homes/a".into(),
        node_did: "did:key:a".into(),
    };
    let identity = |home: &str, did: &str| HomeIdentity {
        reviewed: ReviewedHome {
            node_home: home.into(),
            node_did: did.into(),
        },
        tool_ceiling: None,
        tool_root: None,
    };
    let a = identity("/homes/a", "did:key:a");
    let b = identity("/homes/b", "did:key:b");
    assert_eq!(
        classify_binding(None, None, None, false),
        ClientBinding::Unbound
    );
    assert_eq!(
        classify_binding(Some(&reviewed), Some(&a), Some(&a), true),
        ClientBinding::BoundToHome
    );
    let reidentified = identity("/homes/a", "did:key:b");
    assert_eq!(
        classify_binding(
            Some(&reviewed),
            Some(&reidentified),
            Some(&reidentified),
            true
        ),
        ClientBinding::Orphaned
    );
    assert_eq!(
        classify_binding(Some(&reviewed), None, None, false),
        ClientBinding::Orphaned
    );
    assert_eq!(
        classify_binding(Some(&reviewed), None, None, true),
        ClientBinding::Unbound,
        "an unreadable init marker is not evidence the node is gone"
    );
    assert_eq!(
        classify_binding(Some(&reviewed), Some(&b), Some(&a), true),
        ClientBinding::Unbound,
        "switching to another home while the reviewed one still exists keeps the client"
    );
    assert_eq!(
        classify_binding(
            Some(&reviewed),
            Some(&identity("/homes/moved", "did:key:a")),
            None,
            false
        ),
        ClientBinding::Unbound,
        "the reviewed identity relocated to the configured home keeps the client"
    );
    assert_eq!(
        classify_binding(Some(&reviewed), Some(&b), None, false),
        ClientBinding::Orphaned,
        "the reviewed home is gone and the configured home is another node"
    );
}

#[tokio::test]
async fn client_start_archives_state_paired_with_a_missing_home() {
    let (temp, state) = orchestration_state();
    let desktop = state.policy.desktop_paths.clone();
    write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN");
    write(desktop.peer_directory_path(), "{}");
    let stored = serde_json::json!({
        "nodeName": "Mandrake",
        "reviewedFor": {
            "nodeHome": temp.path().join("agent").to_string_lossy(),
            "nodeDid": "did:key:gone"
        }
    });
    write(
        &desktop.root().join(MANAGED_SERVER_CONFIG),
        &stored.to_string(),
    );

    let reset = retire_orphaned_client_state(&state)
        .await
        .unwrap()
        .expect("orphaned state is archived");
    assert!(reset.completed);
    assert!(PathBuf::from(reset.backup_path.unwrap())
        .join("desktop/node/MANIFEST")
        .is_file());
    assert!(!desktop.node_data_dir().exists());
    assert!(!desktop.peer_directory_path().exists());
    assert!(!desktop.root().join(MANAGED_SERVER_CONFIG).exists());
    assert!(
        retire_orphaned_client_state(&state)
            .await
            .unwrap()
            .is_none(),
        "nothing is left to retire"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn orphaned_state_that_cannot_be_archived_fails_without_moving_anything() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, state) = orchestration_state();
    let desktop = state.policy.desktop_paths.clone();
    write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN");
    let stored = serde_json::json!({
        "nodeName": "Mandrake",
        "reviewedFor": {
            "nodeHome": temp.path().join("agent").to_string_lossy(),
            "nodeDid": "did:key:gone"
        }
    });
    write(
        &desktop.root().join(MANAGED_SERVER_CONFIG),
        &stored.to_string(),
    );
    let parent = desktop.root().parent().unwrap().to_path_buf();
    let original = std::fs::metadata(&parent).unwrap().permissions();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = retire_orphaned_client_state(&state).await;
    std::fs::set_permissions(&parent, original).unwrap();

    assert!(
        result.is_err(),
        "startup must not reopen state it failed to retire"
    );
    assert!(desktop.node_data_dir().join("MANIFEST").is_file());
    assert!(desktop.root().join(MANAGED_SERVER_CONFIG).is_file());
}

#[tokio::test]
async fn client_start_keeps_state_paired_with_its_home() {
    let (temp, state) = orchestration_state();
    let desktop = state.policy.desktop_paths.clone();
    let home = temp.path().join("agent");
    write(
        &home.join("init.json"),
        r#"{"node_did":"did:key:here","node_name":"Mandrake"}"#,
    );
    write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN");
    let stored = serde_json::json!({
        "nodeName": "Mandrake",
        "reviewedFor": {
            "nodeHome": std::fs::canonicalize(&home).unwrap().to_string_lossy(),
            "nodeDid": "did:key:here"
        }
    });
    write(
        &desktop.root().join(MANAGED_SERVER_CONFIG),
        &stored.to_string(),
    );

    assert!(retire_orphaned_client_state(&state)
        .await
        .unwrap()
        .is_none());
    assert!(desktop.node_data_dir().join("MANIFEST").is_file());
    assert!(desktop.root().join(MANAGED_SERVER_CONFIG).is_file());
}

#[test]
fn a_refused_client_store_alone_retires_only_client_state() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);

    let plan = plan_home_reset(
        Some(&home),
        None,
        Some(lineage_refusal(desktop.node_data_dir().to_path_buf())),
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    assert_eq!(preview.managed_home, None);
    assert_eq!(preview.stores[0].scope, IncompatibleStoreScope::Client);
    let reset = plan
        .retire(
            HomeResetDisposition::Archive,
            Some(&plan.backup_path("20260924T000000.000Z").unwrap()),
        )
        .unwrap();

    let backup = PathBuf::from(reset.backup_path.unwrap());
    assert_eq!(backup, temp.join("desktop-backup-20260924T000000.000Z"));
    assert!(backup.join("desktop/node/MANIFEST").is_file());
    assert!(
        home.join("data/MANIFEST").is_file(),
        "the runtime home is not in scope"
    );
    assert!(home.join("init.json").is_file());
}

#[test]
fn backups_user_files_and_unknown_homes_are_retained_and_shown() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    write(
        &home.join("backups/legacy-store-1/data/MANIFEST"),
        "older backup",
    );
    write(&home.join("notes.md"), "mine");
    write(
        &home.join("half-onboarded/data/MANIFEST"),
        "no init.json yet",
    );
    write(&home.join("a/b/c/d/e/init.json"), "{}");
    write(&home.join("data.lock"), "4242");

    let plan = plan_home_reset(
        Some(&home),
        Some(lineage_refusal(home.join("data"))),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    for retained in ["a", "backups", "data.lock", "half-onboarded", "notes.md"] {
        let path = home.join(retained).to_string_lossy().into_owned();
        assert!(
            preview.retained_paths.contains(&path),
            "{retained} retained"
        );
        assert!(
            !preview.planned_paths.contains(&path),
            "{retained} not planned"
        );
    }
    assert!(preview
        .planned_paths
        .contains(&home.join("data").to_string_lossy().into_owned()));
    assert!(preview
        .planned_paths
        .contains(&desktop.identity_key_path().to_string_lossy().into_owned()));

    plan.retire(HomeResetDisposition::Delete, None).unwrap();
    assert!(home.join("backups/legacy-store-1/data/MANIFEST").is_file());
    assert!(home.join("notes.md").is_file());
    assert!(home.join("half-onboarded/data/MANIFEST").is_file());
    assert!(home.join("a/b/c/d/e/init.json").is_file());
    assert!(home.join("data.lock").is_file());
}

#[test]
fn a_confirmation_authorizes_only_the_previewed_set() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    let refused = || Some(lineage_refusal(home.join("data")));
    let preview = plan_home_reset(
        Some(&home),
        refused(),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap()
    .preview();

    write(&home.join("plugins/new/manifest.json"), "{}");
    let changed = plan_home_reset(
        Some(&home),
        refused(),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    for (supplied, disposition) in [
        (preview.confirmation.as_str(), HomeResetDisposition::Archive),
        (
            preview.delete_confirmation.as_deref().unwrap(),
            HomeResetDisposition::Delete,
        ),
    ] {
        assert_eq!(
            changed
                .check_confirmation(supplied, disposition)
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
    }
    let reviewed = changed.preview();
    changed
        .check_confirmation(
            reviewed.delete_confirmation.as_deref().unwrap(),
            HomeResetDisposition::Delete,
        )
        .unwrap();
    assert!(reviewed
        .delete_confirmation
        .as_deref()
        .unwrap()
        .starts_with(&format!("DELETE {} PERMANENTLY [", home.display())));
}

#[test]
fn delete_removes_only_this_homes_key_and_keeps_other_keys() {
    let temp = tempfile::tempdir().unwrap();
    let temp = std::fs::canonicalize(temp.path()).unwrap();
    let (home, desktop) = dirty_home(&temp);
    write(
        &home.join("keys/other-agent.key"),
        "another home's identity",
    );
    write(&home.join("packs/p/manifest.json"), "{}");

    let plan = plan_home_reset(
        Some(&home),
        Some(lineage_refusal(home.join("data"))),
        None,
        &desktop,
        not_the_user_home(),
        ClientBinding::Unbound,
    )
    .unwrap();
    let preview = plan.preview();
    let delete_paths = preview.delete_paths.clone();
    assert!(delete_paths.contains(&home.join("keys/local.key").to_string_lossy().into_owned()));
    assert!(!delete_paths.contains(&home.join("keys").to_string_lossy().into_owned()));
    assert!(delete_paths.contains(&home.join("packs").to_string_lossy().into_owned()));
    assert!(
        preview
            .planned_paths
            .contains(&home.join("keys").to_string_lossy().into_owned()),
        "an archive still moves the whole key directory"
    );

    plan.check_confirmation(
        preview.delete_confirmation.as_deref().unwrap(),
        HomeResetDisposition::Delete,
    )
    .unwrap();
    plan.retire(HomeResetDisposition::Delete, None).unwrap();
    assert!(!home.join("keys/local.key").exists());
    assert_eq!(
        std::fs::read_to_string(home.join("keys/other-agent.key")).unwrap(),
        "another home's identity"
    );
}
