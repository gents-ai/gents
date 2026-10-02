use super::*;

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
        format!("{error:#}").contains("changed after it was validated"),
        "{error:#}"
    );
    let error = BoundDir::folder(&base.join("missing"), BindAccess::Read).unwrap_err();
    assert!(format!("{error:#}").contains("cannot be read"), "{error:#}");
    std::fs::create_dir(base.join("d")).unwrap();
    std::os::unix::fs::symlink(base.join("d"), base.join("dlink")).unwrap();
    assert!(BoundDir::folder(&base.join("dlink"), BindAccess::Read).is_err());
    assert!(BoundDir::folder(&base.join("d"), BindAccess::Read).is_ok());
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
