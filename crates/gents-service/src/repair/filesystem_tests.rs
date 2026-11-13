use super::home::*;
use gents_protocol::operations::install_repair::Blocker;
use std::fs;
use std::path::Path;

#[tokio::test]
async fn set_aside_moves_everything_and_keeps_nothing_behind() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    complete(&home);
    fs::create_dir(home.join("keys")).unwrap();
    fs::write(home.join("keys/user.key"), b"user key").unwrap();
    fs::write(home.join("keys/node.key"), b"node key").unwrap();
    fs::write(home.join("data/documents"), b"stored documents").unwrap();
    let to = set_aside_home(&home).await.unwrap();
    assert!(!home.exists());
    assert!(to.join("init.json").exists());
    assert!(to.join("data").is_dir());
    assert_eq!(fs::read(to.join("keys/user.key")).unwrap(), b"user key");
    assert_eq!(fs::read(to.join("keys/node.key")).unwrap(), b"node key");
    assert_eq!(
        fs::read(to.join("data/documents")).unwrap(),
        b"stored documents"
    );
}

#[tokio::test]
async fn a_held_store_prevents_archiving_its_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    complete(&home);
    let _held = gents_store::local::home::lock_home_store(&home).unwrap();
    assert!(matches!(
        set_aside_home(&home).await,
        Err(Blocker::HomeBusy { .. })
    ));
    assert!(home.join("init.json").is_file());
    assert!(home.join("data").is_dir());
}

#[tokio::test]
async fn a_folder_that_cannot_be_renamed_is_left_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("parent");
    let home = parent.join(".gents");
    complete(&home);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let result = set_aside_home(&home).await;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    match result {
        Err(Blocker::CannotSetAside { home: blocked, .. }) => assert_eq!(blocked, home),
        other => panic!("expected CannotSetAside, got {other:?}"),
    }
    assert!(home.join("init.json").exists());
    assert!(home.join("data").is_dir());
}

#[tokio::test]
async fn a_symlinked_home_moves_the_link_not_the_data() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("drive");
    complete(&target);
    let home = temp.path().join(".gents");
    std::os::unix::fs::symlink(&target, &home).unwrap();
    let to = set_aside_home(&home).await.unwrap();
    assert!(fs::symlink_metadata(&home).is_err());
    assert!(fs::symlink_metadata(&to).unwrap().file_type().is_symlink());
    assert!(target.join("init.json").exists());
    assert!(target.join("data").is_dir());
}

fn complete(home: &Path) {
    fs::create_dir_all(home.join("data")).unwrap();
    fs::write(
        home.join("init.json"),
        serde_json::json!({
            "home": home,
            "node_name": "local",
            "node_did": "did:key:zlocal",
            "key_path": null,
            "tool_ceiling": "Readwrite",
            "tool_root": null,
        })
        .to_string(),
    )
    .unwrap();
}

/// Every entry under `root`: its path, kind, mode, length, modification time
/// and content digest.
fn checksum(root: &Path) -> Vec<String> {
    use sha2::Digest as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let digest = if metadata.is_file() {
                format!("{:x}", sha2::Sha256::digest(fs::read(&path).unwrap()))
            } else {
                String::new()
            };
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            entries.push(format!(
                "{} {:?} {:o} {} {:?} {digest}",
                path.strip_prefix(root).unwrap().display(),
                metadata.file_type(),
                metadata.permissions().mode(),
                metadata.len(),
                metadata.modified().unwrap(),
            ));
        }
    }
    entries.sort();
    entries
}

struct Silent;

impl super::Progress for Silent {
    fn finding(&mut self, _: &gents_protocol::operations::install_repair::Finding) {}
    fn step(
        &mut self,
        _: usize,
        _: usize,
        _: gents_protocol::operations::install_repair::RepairStep,
    ) {
    }
}

async fn prepare_staged(home: &Path, lock: &Path) -> super::RepairReceipt {
    use gents_protocol::operations::install_repair::{
        HomeChoice, RepairAnswer, RepairOperation, ServiceMove,
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    super::prepare_with_lock(
        lock,
        None,
        &crate::server_host::ServerConfig {
            home: home.to_path_buf(),
            http_addr: std::net::Ipv4Addr::LOCALHOST.into(),
            http_port: port,
        },
        &HomeChoice {
            home: home.to_path_buf(),
            explicit: false,
        },
        false,
        &super::ServiceProgram::new(super::current_program),
        ServiceMove::AfterSetup,
        RepairOperation::Run,
        super::Asked {
            interactive: true,
            answer: RepairAnswer::SetAside,
        },
        &mut Silent,
    )
    .await
    .unwrap()
}

/// The onboarding test of #3670 ran this build on an rc2 home: the repair
/// installed a service, the node opened the store and rewrote its manifest,
/// and nothing said the home was older. A home an older Gents made is now
/// refused before anything touches it, even when the user answered to set
/// it aside.
#[tokio::test]
async fn an_older_home_is_reported_and_left_exactly_as_it_was() {
    use gents_protocol::operations::install_repair::{IncompatibleStoreKind, RepairOutcome};
    for stage in [
        |data: &Path| fs::write(data.join("MANIFEST"), b"REGOMAN older store").unwrap(),
        |data: &Path| fs::write(data.join("CURRENT"), b"MANIFEST-000001\n").unwrap(),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        complete(&home);
        fs::create_dir_all(home.join("keys")).unwrap();
        fs::write(home.join("keys/local.key"), b"node key").unwrap();
        fs::set_permissions(
            home.join("keys/local.key"),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        fs::write(home.join("data/wal_000019.log"), b"wal").unwrap();
        stage(&home.join("data"));
        let before = checksum(temp.path());
        let receipt = prepare_staged(&home, &temp.path().join("repair.lock")).await;
        fs::remove_file(temp.path().join("repair.lock")).unwrap();
        assert_eq!(checksum(temp.path()), before);
        match receipt.outcome {
            RepairOutcome::Blocked(Blocker::StoreRefused { kind, message }) => {
                assert!(kind.made_by_older_version(), "{kind:?}");
                assert_ne!(kind, IncompatibleStoreKind::ForeignVersion);
                assert!(message.contains("older version of Gents"), "{message}");
                assert!(message.contains("Nothing in it was changed"), "{message}");
            }
            other => panic!("expected an older-store refusal, got {other:?}"),
        }
        assert!(receipt.repaired.is_empty());
    }
}

/// A key file others can read stops the repair with the `chmod` that fixes
/// it; the home is not moved.
#[tokio::test]
async fn a_readable_key_names_the_chmod_and_moves_nothing() {
    use gents_protocol::operations::install_repair::{IncompatibleStoreKind, RepairOutcome};
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    complete(&home);
    fs::create_dir_all(home.join("keys")).unwrap();
    fs::write(home.join("keys/local.key"), b"node key").unwrap();
    fs::set_permissions(
        home.join("keys/local.key"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let before = checksum(temp.path());
    let receipt = prepare_staged(&home, &temp.path().join("repair.lock")).await;
    fs::remove_file(temp.path().join("repair.lock")).unwrap();
    assert_eq!(checksum(temp.path()), before);
    match receipt.outcome {
        RepairOutcome::Blocked(Blocker::StoreRefused { kind, message }) => {
            assert_eq!(kind, IncompatibleStoreKind::InsecureKey);
            assert!(message.contains("chmod 600"), "{message}");
            assert!(message.contains("local.key"), "{message}");
        }
        other => panic!("expected an insecure-key refusal, got {other:?}"),
    }
}
