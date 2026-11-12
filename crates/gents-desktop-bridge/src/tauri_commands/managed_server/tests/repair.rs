use super::super::repair::{archive_client_store, repair_with, service_executable};
use super::*;
use crate::config::ManagedServerPolicy;
use crate::types::ManagedServerRepairOutcome;
use gents::store::local::storage_backend::IncompatibleStoreKind;
use gents_server::repair::home::BrokenHome;
use gents_server::repair::{Blocker, Finding, HomeDecision, Prepared, Report};

fn report(repaired: Vec<Finding>, outcome: Prepared) -> anyhow::Result<Report<Prepared>> {
    Ok(Report {
        repaired,
        outcome,
        serving_home: None,
        older_data: None,
    })
}

fn ready() -> anyhow::Result<Report<Prepared>> {
    report(Vec::new(), Prepared::Ready)
}

#[test]
fn a_reinstalled_service_runs_the_desktop_runtime_not_the_app() {
    let (_temp, state) = orchestration_state();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock desktop app");
    let executable = service_executable(app.handle(), &state).unwrap();
    assert_ne!(executable, std::env::current_exe().unwrap());
    let runtime = crate::tauri_commands::service_executable::resolve_service_executable(
        app.handle(),
        state.policy.desktop_paths.root(),
    )
    .unwrap();
    let runtime = runtime.service_path();
    assert_eq!(
        executable,
        std::fs::canonicalize(runtime).unwrap_or_else(|_| runtime.to_path_buf())
    );
}

fn tree(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    found.sort();
    found
}

#[tokio::test]
async fn a_mac_without_a_local_node_is_left_alone() {
    let (temp, state) = orchestration_state();
    let result = repair_with(&state, false, |_, _| async { panic!("prepared") })
        .await
        .unwrap();
    assert_eq!(result.outcome, ManagedServerRepairOutcome::NoLocalNode);
    assert!(result.repaired.is_empty());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_remote_only_or_disabled_desktop_is_left_alone_even_with_a_preference() {
    for remote_only in [true, false] {
        let (temp, mut state) = orchestration_state();
        if remote_only {
            state.policy.node_home = None;
        } else {
            state.policy.managed_server = ManagedServerPolicy::Disabled;
        }
        write(
            &state
                .policy
                .desktop_paths
                .root()
                .join("managed-server.json"),
            "{}",
        );
        let before = tree(temp.path());
        let result = repair_with(&state, true, |_, _| async { panic!("prepared") })
            .await
            .unwrap();
        assert_eq!(result.outcome, ManagedServerRepairOutcome::NoLocalNode);
        assert_eq!(tree(temp.path()), before);
    }
}

#[tokio::test]
async fn unreadable_saved_connections_are_reported_as_repaired() {
    let (_temp, state) = orchestration_state();
    let paths = &state.policy.desktop_paths;
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.peer_directory_path(), "{ not json").unwrap();
    let result = repair_with(&state, true, |_, _| async { ready() })
        .await
        .unwrap();
    assert_eq!(result.outcome, ManagedServerRepairOutcome::Ready);
    assert_eq!(
        result.repaired,
        vec!["Saved connections couldn't be read and were set aside; add your remote nodes again."]
    );
    assert!(result.saved_peers.is_empty());
    assert!(!paths.peer_directory_path().exists());
    let set_aside = std::fs::read_dir(paths.root())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("peers.json.unreadable-")
        });
    assert!(set_aside, "the unreadable copy is kept");
}

#[tokio::test]
async fn the_outcome_mappings() {
    let capital = |text: String| {
        let mut chars = text.chars();
        chars
            .next()
            .unwrap()
            .to_uppercase()
            .chain(chars)
            .collect::<String>()
    };
    let decision = HomeDecision {
        home: PathBuf::from("/h"),
        why: BrokenHome::InitConfigMissing,
        default_home: PathBuf::from("/d"),
        serving_home: PathBuf::from("/s"),
    };
    let why = capital(decision.why.to_string());
    let keychain = Blocker::KeychainUnavailable;
    let keychain_text = capital(keychain.to_string());
    let cases = vec![
        (
            Prepared::Blocked(Blocker::BackgroundApprovalPending { detail: None }),
            ManagedServerRepairOutcome::Ready,
            None,
        ),
        (
            Prepared::Blocked(keychain),
            ManagedServerRepairOutcome::Blocked,
            Some(keychain_text),
        ),
        (
            Prepared::Decide(decision),
            ManagedServerRepairOutcome::Blocked,
            Some(why),
        ),
        (
            Prepared::NeedsSetup,
            ManagedServerRepairOutcome::NeedsSetup,
            None,
        ),
    ];
    for (prepared, outcome, blocker) in cases {
        let (_temp, state) = orchestration_state();
        let result = repair_with(&state, true, move |_, _| async move {
            report(vec![Finding::ServiceProgramMissing], prepared)
        })
        .await
        .unwrap();
        assert_eq!(result.outcome, outcome);
        assert_eq!(result.blocker, blocker);
        assert_eq!(
            result.repaired,
            vec![capital(Finding::ServiceProgramMissing.to_string())]
        );
    }
}

#[tokio::test]
async fn older_data_set_aside_after_a_replaced_service_opens_setup_and_names_the_folder() {
    let (_temp, state) = orchestration_state();
    let kept_at = PathBuf::from("/Users/me/.gents.from-older-2026-11-12-120000");
    let set_aside = Finding::OlderDataSetAside {
        from: None,
        kept_at: kept_at.clone(),
    };
    let result = repair_with(&state, true, |_, _| async {
        report(vec![set_aside], Prepared::NeedsSetup)
    })
    .await
    .unwrap();
    assert_eq!(result.outcome, ManagedServerRepairOutcome::NeedsSetup);
    assert_eq!(result.blocker, None);
    assert_eq!(
        result.repaired,
        vec![format!(
            "This version couldn't open the data from an older Gents version, so it was set aside at {}",
            kept_at.display()
        )]
    );
}

fn refused_with(state: &DesktopAppState, kind: IncompatibleStoreKind) {
    state
        .managed_server
        .try_lock()
        .expect("managed server state is free")
        .incompatible_store = Some(gents::store::local::storage_backend::IncompatibleStore {
        kind,
        data_path: state.policy.node_home.clone().unwrap().join("data"),
    });
}

#[tokio::test]
async fn an_older_refusal_a_launch_saw_is_set_aside_then_setup_runs() {
    let (_temp, state) = orchestration_state();
    refused_with(&state, IncompatibleStoreKind::UnknownLineage);
    let kept_at = PathBuf::from("/Users/me/.gents.from-older-2026-11-12-120000");
    let set_aside = Finding::OlderDataSetAside {
        from: None,
        kept_at: kept_at.clone(),
    };
    let result = repair_with(&state, true, |choice, refused| async move {
        assert!(refused, "the observed refusal reaches the repair owner");
        assert!(!choice.explicit);
        report(vec![set_aside], Prepared::NeedsSetup)
    })
    .await
    .unwrap();
    assert_eq!(result.outcome, ManagedServerRepairOutcome::NeedsSetup);
    assert_eq!(
        result.repaired,
        vec![format!(
            "This version couldn't open the data from an older Gents version, so it was set aside at {}",
            kept_at.display()
        )]
    );
    assert!(state
        .managed_server
        .lock()
        .await
        .incompatible_store
        .is_none());
}

#[tokio::test]
async fn a_foreign_version_refusal_keeps_the_incompatible_home_screen() {
    let (_temp, state) = orchestration_state();
    refused_with(&state, IncompatibleStoreKind::ForeignVersion);
    let result = repair_with(&state, true, |_, refused| async move {
        assert!(!refused, "a newer build's data is never set aside");
        ready()
    })
    .await
    .unwrap();
    assert_eq!(result.outcome, ManagedServerRepairOutcome::Ready);
    assert_eq!(
        state
            .managed_server
            .lock()
            .await
            .incompatible_store
            .as_ref()
            .map(|store| store.kind),
        Some(IncompatibleStoreKind::ForeignVersion)
    );
}

#[tokio::test]
async fn a_late_failure_keeps_the_notes_already_gathered() {
    let (_temp, state) = orchestration_state();
    let paths = &state.policy.desktop_paths;
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.peer_directory_path(), "{ not json").unwrap();
    write(paths.store_encryption_path(), "not a record");
    let result = repair_with(&state, true, |_, _| async { ready() })
        .await
        .unwrap();
    assert_eq!(result.outcome, ManagedServerRepairOutcome::Blocked);
    assert!(result.blocker.is_some());
    assert_eq!(result.repaired.len(), 1);
}

#[tokio::test]
async fn archiving_the_client_store_leaves_the_grant_and_identity_in_place() {
    let (temp, state) = orchestration_state();
    let paths = &state.policy.desktop_paths;
    write(&paths.node_data_dir().join("MANIFEST"), "store");
    write(&paths.root().join("managed-server.json"), "{}");
    write(paths.principal_metadata_path(), "meta");
    write(paths.identity_key_path(), "key");
    write(paths.peer_directory_path(), "{}");
    archive_client_store(
        &state,
        gents::store::local::storage_backend::IncompatibleStoreKind::MissingStoreKey,
    )
    .await
    .unwrap();
    let kept = tree(&temp.path().join("desktop"));
    assert!(kept.contains(&"managed-server.json".to_owned()), "{kept:?}");
    assert!(
        !kept.iter().any(|path| path.contains("MANIFEST")),
        "{kept:?}"
    );
    for name in [paths.principal_metadata_path(), paths.identity_key_path()] {
        assert!(name.exists(), "{}", name.display());
    }
    assert!(paths.peer_directory_path().exists());
    let backups = tree(temp.path())
        .into_iter()
        .filter(|path| path.contains("backup") && path.ends_with("MANIFEST"))
        .count();
    assert_eq!(backups, 1);
}

#[tokio::test]
async fn another_versions_client_store_is_set_aside_and_the_banner_names_the_backup() {
    use gents_desktop_core::client::{ClientCore, ClientCoreOptions};
    for encrypted in [false, true] {
        let (temp, state) = orchestration_state();
        let paths = &state.policy.desktop_paths;
        gents_desktop_core::test_fixtures::write_other_version_client_store(paths, true, encrypted)
            .await;
        write(
            &paths.root().join("managed-server.json"),
            r#"{"nodeName":"Forge"}"#,
        );
        let identity = std::fs::read(paths.identity_key_path()).unwrap();
        let result = repair_with(&state, true, |_, _| async { ready() })
            .await
            .unwrap();
        assert_eq!(result.outcome, ManagedServerRepairOutcome::Ready);
        assert_eq!(result.blocker, None);
        assert_eq!(result.repaired.len(), 1, "{:?}", result.repaired);
        let backup = tree(temp.path())
            .into_iter()
            .find(|path| path.contains("backup") && path.ends_with("MANIFEST"))
            .expect("the backup holds the old store");
        let backup_root = temp.path().join(backup.split('/').next().unwrap());
        assert!(
            result.repaired[0].contains(&backup_root.display().to_string()),
            "{:?} names {}",
            result.repaired,
            backup_root.display()
        );
        assert!(!paths.node_data_dir().exists());
        // An encrypted store's key record stays, so the archive stays readable.
        assert_eq!(paths.store_encryption_path().exists(), encrypted);
        assert_eq!(paths.store_key_path().exists(), encrypted);
        assert_eq!(std::fs::read(paths.identity_key_path()).unwrap(), identity);
        assert!(paths.root().join("managed-server.json").exists());

        let core = ClientCore::start_with_paths_and_options(
            paths.clone(),
            ClientCoreOptions::local_only(),
        )
        .await
        .expect("a fresh client store opens");
        assert_eq!(
            std::fs::read(paths.identity_key_path()).unwrap(),
            identity,
            "the fresh store keeps the identity"
        );
        Arc::new(core).shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn the_field_state_after_an_upgrade_of_another_versions_store_is_set_aside_whole() {
    use gents::store::local::store_key::upgrade;
    let (temp, state) = orchestration_state();
    let paths = &state.policy.desktop_paths;
    gents_desktop_core::test_fixtures::write_other_version_client_store(paths, true, false).await;
    let record =
        gents_desktop_core::test_fixtures::encrypt_client_store_without_its_record(paths).await;
    write(
        paths.store_encryption_path(),
        &serde_json::to_string(&record).unwrap(),
    );
    let source = paths.root().join("node.encryption-source");
    assert!(source.join("MANIFEST").is_file());
    assert!(upgrade::journal_path(paths.node_data_dir()).is_file());

    let result = repair_with(&state, true, |_, _| async { ready() })
        .await
        .unwrap();
    assert_eq!(result.blocker, None);
    assert_eq!(result.repaired.len(), 1, "{:?}", result.repaired);
    assert!(!paths.node_data_dir().exists());
    assert!(!source.exists());
    assert!(!upgrade::journal_path(paths.node_data_dir()).exists());
    assert!(paths.store_encryption_path().is_file());
    assert!(paths.store_key_path().is_file());
    let backed_up = tree(temp.path());
    for retired in [
        "node/MANIFEST",
        "node.encryption-source/MANIFEST",
        "node.encryption-upgrade.json",
    ] {
        assert!(
            backed_up
                .iter()
                .any(|path| path.contains("backup") && path.ends_with(retired)),
            "{retired} in {backed_up:?}"
        );
    }
}

#[tokio::test]
async fn a_held_store_or_an_unreadable_key_is_never_set_aside() {
    let (temp, state) = orchestration_state();
    let paths = &state.policy.desktop_paths;
    gents_desktop_core::test_fixtures::write_other_version_client_store(paths, true, true).await;
    let held = gents::store::local::home::lock_store(paths.root(), paths.node_data_dir()).unwrap();
    let before = tree(temp.path());
    let result = repair_with(&state, true, |_, _| async { ready() })
        .await
        .unwrap();
    assert!(result.repaired.is_empty(), "{:?}", result.repaired);
    assert_eq!(result.blocker, None);
    assert_eq!(tree(temp.path()), before);
    drop(held);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |mode| {
            std::fs::set_permissions(
                paths.store_key_path(),
                std::fs::Permissions::from_mode(mode),
            )
            .unwrap()
        };
        mode(0o000);
        let result = repair_with(&state, true, |_, _| async { ready() })
            .await
            .unwrap();
        mode(0o600);
        assert_eq!(result.outcome, ManagedServerRepairOutcome::Blocked);
        assert!(result.blocker.is_some());
        assert!(result.repaired.is_empty(), "{:?}", result.repaired);
        assert_eq!(tree(temp.path()), before);
    }
}

#[tokio::test]
async fn a_removed_home_reports_the_lost_local_node_and_where_its_data_went() {
    let (_temp, state) = orchestration_state();
    let paths = &state.policy.desktop_paths;
    let home = state.policy.node_home.clone().unwrap();
    write(&paths.node_data_dir().join("MANIFEST"), "store");
    let stored = serde_json::json!({
        "nodeName": "Forge",
        "reviewedFor": { "nodeHome": home.to_string_lossy(), "nodeDid": "did:key:gone" }
    });
    write(
        &paths.root().join("managed-server.json"),
        &stored.to_string(),
    );
    let peer = serde_json::json!({
        "peer_id": "old-local", "label": "Forge", "addr": "endpoint",
        "node_did": "did:key:gone", "source": "enrollment",
        "graphql": "http://127.0.0.1:21919/api/v0/graphql",
        "local_node_home": home.to_string_lossy(),
        "created_at": "2026-11-12T02:33:54Z", "updated_at": "2026-11-12T02:35:09Z"
    });
    write(
        paths.peer_directory_path(),
        &serde_json::json!({ "peers": [peer] }).to_string(),
    );

    let result = repair_with(&state, true, |_, _| async {
        report(Vec::new(), Prepared::NeedsSetup)
    })
    .await
    .unwrap();

    assert_eq!(result.outcome, ManagedServerRepairOutcome::NeedsSetup);
    assert!(result.local_node_lost);
    assert!(result.saved_peers.is_empty());
    assert!(
        result.repaired.is_empty(),
        "a deleted folder is not reported as a repair: {:?}",
        result.repaired
    );
}

#[tokio::test]
async fn a_node_serving_a_deleted_folder_is_a_lost_node_not_a_repair() {
    let (_temp, state) = orchestration_state();
    let result = repair_with(&state, true, |_, _| async {
        report(vec![Finding::StaleNode], Prepared::NeedsSetup)
    })
    .await
    .unwrap();
    assert!(result.local_node_lost);
    assert!(result.repaired.is_empty(), "{:?}", result.repaired);
}

#[tokio::test]
async fn a_preference_written_by_another_version_is_set_aside_so_setup_starts_fresh() {
    let (_temp, state) = orchestration_state();
    let root = state.policy.desktop_paths.root();
    let path = root.join(MANAGED_SERVER_CONFIG);
    write(
        &path,
        "{\n  \"agentName\": \"Forge\",\n  \"toolCeiling\": \"readwrite\",\n  \"toolRoot\": \"/Users/someone\"\n}",
    );
    assert!(load_preference(&state).await.unwrap().is_none());
    assert!(!path.exists());
    assert!(tree(root)
        .iter()
        .any(|name| name.starts_with("managed-server.json.unreadable-")));

    write(&path, "{\n  \"nodeName\": \"Fo");
    assert!(load_preference(&state).await.is_err());
    assert!(
        path.exists(),
        "a partly written preference is left in place"
    );
}
