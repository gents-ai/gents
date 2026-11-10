use super::*;
use crate::commands::grok_shim::server::socket::{ensure_socket_parent, StagingDir};

#[cfg(unix)]
#[test]
fn the_staging_dir_is_0700_from_creation_and_rejects_unsafe_parents() {
    let root = unique_test_root("staging-create-mode");
    let staging = StagingDir::create(&root).expect("creating the staging ancestor");
    assert_eq!(
        std::fs::metadata(&staging.path)
            .expect("staging metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the staging ancestor must be 0700 from creation"
    );
    staging.cleanup();
    let _ = std::fs::remove_dir_all(&root);

    let root = unique_test_root("staging-symlink-parent");
    let target = root.join("real");
    let link = root.join("link");
    std::fs::create_dir(&target).expect("creating the symlink target");
    std::os::unix::fs::symlink(&target, &link).expect("creating the parent symlink");
    let socket = link.join("leader.sock");
    assert!(
        publish_listener(&socket).is_err(),
        "publication through a symlinked parent must be refused"
    );
    assert!(
        !target.join("leader.sock").exists(),
        "nothing may be published through the symlink"
    );
    let leaked: Vec<_> = std::fs::read_dir(&target)
        .expect("target readable")
        .filter_map(std::result::Result::ok)
        .collect();
    assert!(
        leaked.is_empty(),
        "no staging directory may be created through the symlinked parent"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nested_missing_socket_parents_are_created_0700() {
    let root = unique_test_root("parent-nested");
    let parent = root.join("a").join("b").join("c");
    ensure_socket_parent(&parent).expect("the nested parent chain must be created");
    for component in [root.join("a"), root.join("a").join("b"), parent.clone()] {
        let mode = std::fs::metadata(&component)
            .unwrap_or_else(|error| panic!("component {} must exist: {error}", component.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode,
            0o700,
            "component {} must be created 0700 from creation",
            component.display()
        );
    }
    ensure_socket_parent(&parent).expect("existing components must be accepted");
    let mode = std::fs::metadata(&parent)
        .expect("parent metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_pre_existing_socket_parent_keeps_its_own_mode() {
    let root = unique_test_root("parent-mode");
    let parent = root.join("owned");
    std::fs::create_dir(&parent).expect("creating the operator-owned parent");
    std::fs::set_permissions(&parent, Permissions::from_mode(0o755))
        .expect("loosening the parent mode");
    ensure_socket_parent(&parent).expect("an existing parent must be accepted");
    let mode = std::fs::metadata(&parent)
        .expect("parent metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o755,
        "the pre-existing parent mode must be left untouched"
    );
    let deep = parent.join("created");
    ensure_socket_parent(&deep).expect("the deeper chain must be created");
    assert_eq!(
        std::fs::metadata(&parent)
            .expect("parent metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the existing component must not be chmodmed even mid-walk"
    );
    assert_eq!(
        std::fs::metadata(&deep)
            .expect("new component metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the new component must be created 0700"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_symlink_component_is_rejected_and_its_target_untouched() {
    let root = unique_test_root("parent-symlink");
    let target = root.join("target-dir");
    std::fs::create_dir(&target).expect("creating the symlink target");
    std::fs::set_permissions(&target, Permissions::from_mode(0o755))
        .expect("setting the target mode");
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("creating the symlink");
    let deep = link.join("nested");
    let error = ensure_socket_parent(&deep).expect_err("a symlink component must be rejected");
    assert!(
        error.to_string().contains("is a symlink"),
        "the error should name the symlink component: {error:#}"
    );
    assert!(
        !target.join("nested").exists(),
        "the symlink target must be untouched"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("link metadata")
            .file_type()
            .is_symlink(),
        "the symlink itself must be left in place"
    );
    assert_eq!(
        std::fs::metadata(&target)
            .expect("target metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "the target's mode must be untouched"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_regular_file_component_is_rejected() {
    let root = unique_test_root("parent-file");
    let blocker = root.join("blocker");
    std::fs::write(&blocker, "not a directory\n").expect("writing the blocking file");
    let deep = blocker.join("nested");
    let error =
        ensure_socket_parent(&deep).expect_err("a non-directory component must be rejected");
    assert!(
        error.to_string().contains("is not a directory"),
        "the error should name the non-directory component: {error:#}"
    );
    assert_eq!(
        std::fs::read_to_string(&blocker).expect("the blocking file should be readable"),
        "not a directory\n",
        "the blocking file must be untouched"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn spawn_refuses_a_symlinked_socket_parent() {
    let root = unique_test_root("parent-spawn-symlink");
    let target = root.join("target-dir");
    std::fs::create_dir(&target).expect("creating the symlink target");
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("creating the symlink");
    let socket = link.join("leader.sock");
    let spawn = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
    assert!(
        spawn.is_err(),
        "a spawn through a symlinked parent must be refused"
    );
    assert!(
        !target.join("leader.sock").exists(),
        "nothing may be published through the symlink"
    );
    assert!(
        !target.join("leader.lock").exists(),
        "no lock file may be created through the symlink"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn spawn_refuses_a_regular_file_socket_parent_component() {
    let root = unique_test_root("parent-spawn-file");
    let blocker = root.join("blocker");
    std::fs::write(&blocker, "not a directory\n").expect("writing the blocking file");
    let socket = blocker.join("leader.sock");
    let spawn = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
    assert!(
        spawn.is_err(),
        "a spawn through a regular-file parent component must be refused"
    );
    assert_eq!(
        std::fs::read_to_string(&blocker).expect("the blocking file should be readable"),
        "not a directory\n",
        "the blocking file must be untouched"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn near_limit_long_filename_binds_and_connects() {
    let root = unique_test_root("longfile");
    let socket = long_filename_socket_path(&root, NEAR_LIMIT_PATH_BYTES);
    assert!(socket.as_os_str().len() <= NEAR_LIMIT_PATH_BYTES);
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn near the path-length limit");
    assert_socket_mode_0600(&socket);
    exercise_leader(&socket, &factory).await;
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn near_limit_long_parent_binds_and_connects() {
    let root = unique_test_root("longparent");
    let socket = long_parent_socket_path(&root, NEAR_LIMIT_PATH_BYTES);
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the leader should spawn near the path-length limit");
    assert_socket_mode_0600(&socket);
    exercise_leader(&socket, &factory).await;
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_production_spawn_lifetime_publishes_serves_and_cleans_up() {
    let root = unique_test_root("lifetime");
    let socket = root.join("leader.sock");
    let lock_path = socket.with_extension("lock");
    let factory = recording_factory();
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
        .expect("the production leader should spawn");
    assert_eq!(handle.socket_path(), socket.as_path());
    assert_eq!(handle.lock_path(), lock_path.as_path());
    assert_socket_mode_0600(&socket);
    assert_eq!(
        std::fs::read_to_string(&lock_path)
            .expect("the lock file should be readable")
            .trim()
            .parse::<u32>()
            .expect("the lock file should hold the holder pid"),
        process::id(),
        "the lock file must record the holder pid"
    );
    assert_eq!(
        std::fs::metadata(&lock_path)
            .expect("lock metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the lock file must be forced to 0600"
    );
    exercise_leader(&socket, &factory).await;
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_second_leader_fails_while_the_first_holds_the_lock() {
    let root = unique_test_root("exclusive");
    let socket = root.join("leader.sock");
    let lock_path = socket.with_extension("lock");
    let first_factory = recording_factory();
    let mut first = spawn_leader(
        LeaderServerConfig::new(socket.clone()),
        first_factory.clone(),
    )
    .expect("the first leader should spawn");

    let second = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
    assert!(
        second.is_err(),
        "a second leader must fail while the first holds the lock"
    );
    assert!(
        socket.exists(),
        "the failed second leader must not remove the winner's socket"
    );
    assert!(
        lock_path.exists(),
        "the failed second leader must not remove the winner's lock file"
    );
    assert_eq!(
        std::fs::read_to_string(&lock_path)
            .expect("the winner's lock file should be readable")
            .trim()
            .parse::<u32>()
            .expect("the winner's lock file should hold a pid"),
        process::id(),
        "the failed leader must not overwrite the holder pid"
    );

    exercise_leader(&socket, &first_factory).await;
    first.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&first);

    let mut second = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("a leader should spawn after the previous one stopped");
    second.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&second);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_stale_lock_file_is_reclaimed_and_forced_to_0600() {
    let root = unique_test_root("stale");
    let socket = root.join("leader.sock");
    let lock_path = socket.with_extension("lock");
    std::fs::write(&lock_path, "999999\n").expect("writing a stale lock file");
    std::fs::set_permissions(&lock_path, Permissions::from_mode(0o644))
        .expect("loosening the stale lock mode");
    let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("a stale lock file must be reclaimable");
    let metadata = std::fs::metadata(&lock_path).expect("the lock file should exist");
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o600,
        "the reclaimed lock must be forced to 0600"
    );
    assert_eq!(
        std::fs::read_to_string(&lock_path)
            .expect("the lock file should be readable")
            .trim()
            .parse::<u32>()
            .expect("the lock file should hold a pid"),
        process::id()
    );
    handle.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&handle);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn spawn_refuses_to_replace_a_non_socket_path() {
    let root = unique_test_root("occupied");
    let socket = root.join("leader.sock");
    std::fs::write(&socket, "not a socket\n").expect("writing a blocking file");
    let spawn = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
    assert!(spawn.is_err(), "a non-socket path must be refused");
    assert_eq!(
        std::fs::read_to_string(&socket).expect("the blocking file should be untouched"),
        "not a socket\n"
    );
    let lock = socket.with_extension("lock");
    assert!(
        lock.exists(),
        "a failed spawn retains the stable lock inode"
    );
    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .expect("opening the released lock");
    probe
        .try_lock()
        .expect("a failed spawn must release its lock");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn dropping_the_handle_unlinks_the_socket_and_leaves_a_reclaimable_lock() {
    let root = unique_test_root("drop");
    let socket = root.join("leader.sock");
    let handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
        .expect("the leader should spawn");
    assert!(socket.exists());
    drop(handle);
    assert!(
        !socket.exists(),
        "dropping the handle must unlink the published socket"
    );
    assert!(
        socket.with_extension("lock").exists(),
        "drop leaves the lock file for the next leader to reclaim"
    );
    let factory = recording_factory();
    let mut next = spawn_with_retry(&socket, factory.clone()).await;
    exercise_leader(&socket, &factory).await;
    next.shutdown().await.expect("clean shutdown");
    assert_clean_stop(&next);
    let _ = std::fs::remove_dir_all(&root);
}
