use super::*;
use gents_protocol::error_chain::ErrorChain;

fn dir_names(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn sharing_a_file_leaves_its_folder_untouched() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().canonicalize().unwrap().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.txt"), "a").unwrap();
    std::fs::write(work.join("b.txt"), "b").unwrap();
    let before = dir_names(&work);
    let bound = BoundDir::for_file(&work.join("a.txt"), BindAccess::Read).unwrap();
    assert_eq!(
        dir_names(bound.path()),
        vec![std::ffi::OsString::from("a.txt")]
    );
    assert!(!bound.path().starts_with(&work));
    assert_eq!(
        dir_names(&work),
        before,
        "the folder gains no entry during the call"
    );
    drop(bound);
    assert_eq!(dir_names(&work), before, "nor after it");
}

#[test]
fn a_stale_private_folder_is_swept_and_a_fresh_one_kept() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().canonicalize().unwrap();
    std::fs::write(work.join("a.txt"), "a").unwrap();
    let first = BoundDir::for_file(&work.join("a.txt"), BindAccess::Read).unwrap();
    let shared = first.path().parent().unwrap().to_path_buf();
    let stale = shared.join("b-stale");
    let fresh = shared.join("b-fresh");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::create_dir_all(&fresh).unwrap();
    File::open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - 2 * STALE)
        .unwrap();
    let second = BoundDir::for_file(&work.join("a.txt"), BindAccess::Read).unwrap();
    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(first.path().exists() && second.path().exists());
    std::fs::remove_dir_all(&fresh).unwrap();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_path_that_is_not_the_one_the_scope_checked_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::write(base.join("real.txt"), "x").unwrap();
    std::os::unix::fs::symlink(base.join("real.txt"), base.join("link.txt")).unwrap();
    let error = BoundDir::for_file(&base.join("link.txt"), BindAccess::Read).unwrap_err();
    assert!(
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .contains("changed after it was validated"),
        "{}",
        ErrorChain(&error)
    );
    let error = BoundDir::folder(&base.join("missing"), BindAccess::Read).unwrap_err();
    assert!(
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .contains("cannot be read"),
        "{}",
        ErrorChain(&error)
    );
    std::fs::create_dir(base.join("d")).unwrap();
    std::os::unix::fs::symlink(base.join("d"), base.join("dlink")).unwrap();
    assert!(BoundDir::folder(&base.join("dlink"), BindAccess::Read).is_err());
    assert!(BoundDir::folder(&base.join("d"), BindAccess::Read).is_ok());
}

#[cfg(unix)]
#[test]
fn a_hard_linked_file_is_named_by_its_own_folder_and_a_symlink_is_not() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::write(base.join("a.txt"), "a").unwrap();
    std::fs::create_dir(base.join("other")).unwrap();
    std::fs::hard_link(base.join("a.txt"), base.join("other/b.txt")).unwrap();
    std::os::unix::fs::symlink(base.join("a.txt"), base.join("link.txt")).unwrap();
    let identity = identity_of(&base.join("a.txt"));
    assert!(named_in_pinned_folder(&base.join("a.txt"), identity));
    assert!(named_in_pinned_folder(&base.join("other/b.txt"), identity));
    assert!(!named_in_pinned_folder(&base.join("link.txt"), identity));
    std::fs::write(base.join("c.txt"), "c").unwrap();
    assert!(!named_in_pinned_folder(&base.join("c.txt"), identity));
}

#[test]
fn a_file_bound_beside_exposes_its_folder_and_names_the_file() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::write(base.join("a.txt"), "a").unwrap();
    let bound = BoundDir::beside(&base.join("a.txt"), BindAccess::Read).unwrap();
    assert_eq!(bound.path(), base);
    assert_eq!(bound.target(), base.join("a.txt"));
}

#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
#[test]
fn a_bound_dir_swapped_to_a_fifo_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    make_fifo(&base.join("d"));
    let error = BoundDir::folder(&base.join("d"), BindAccess::Read)
        .expect_err("a FIFO read-open blocks until a writer appears");
    assert!(
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .contains("is not a folder"),
        "{}",
        ErrorChain(&error)
    );
}

#[cfg(unix)]
#[test]
fn a_bound_file_swapped_to_a_fifo_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    make_fifo(&base.join("f.txt"));
    let error = BoundDir::for_file(&base.join("f.txt"), BindAccess::Read)
        .expect_err("a FIFO read-open blocks until a writer appears");
    assert!(
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .contains("is not a file"),
        "{}",
        ErrorChain(&error)
    );
}

#[test]
fn a_bound_dir_swapped_to_a_file_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::write(base.join("d"), "x").unwrap();
    let error = BoundDir::folder(&base.join("d"), BindAccess::Read)
        .expect_err("a regular file cannot be bound as a folder");
    assert!(
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .contains("is not a folder"),
        "{}",
        ErrorChain(&error)
    );
}

#[test]
fn a_bound_file_swapped_to_a_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::create_dir(base.join("f.txt")).unwrap();
    let error = BoundDir::for_file(&base.join("f.txt"), BindAccess::Read)
        .expect_err("a folder cannot be shared as a file");
    assert!(
        gents_protocol::error_chain::ErrorChain(&error)
            .to_string()
            .contains("is not a file"),
        "{}",
        ErrorChain(&error)
    );
}

#[cfg(unix)]
#[test]
fn a_fifo_in_a_pinned_folder_is_not_the_pinned_name() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::write(base.join("a.txt"), "a").unwrap();
    make_fifo(&base.join("pipe"));
    let identity = identity_of(&base.join("a.txt"));
    assert!(
        !named_in_pinned_folder(&base.join("pipe"), identity),
        "a FIFO is not a regular file, and opening it must not block"
    );
}

#[cfg(unix)]
#[test]
fn a_pinned_folder_swapped_to_a_fifo_is_not_the_pinned_folder() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::write(base.join("a.txt"), "a").unwrap();
    make_fifo(&base.join("folderpipe"));
    let identity = identity_of(&base.join("a.txt"));
    assert!(
        !named_in_pinned_folder(&base.join("folderpipe/x.txt"), identity),
        "a FIFO swapped in as the pinned folder does not name the pinned file"
    );
}
