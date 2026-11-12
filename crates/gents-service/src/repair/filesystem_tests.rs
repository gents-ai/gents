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
