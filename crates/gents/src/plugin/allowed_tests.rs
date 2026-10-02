use super::*;

struct World {
    _root: tempfile::TempDir,
    root: PathBuf,
    gents: PathBuf,
}

fn world() -> World {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let gents = base.join("gents-home");
    for dir in ["work/sub", "docs", "outside", "gents-home/plugins"] {
        std::fs::create_dir_all(base.join(dir)).unwrap();
    }
    std::fs::write(base.join("work/a.txt"), "a").unwrap();
    std::fs::write(base.join("work/b.txt"), "b").unwrap();
    std::fs::write(base.join("docs/x.pdf"), "pdf").unwrap();
    std::fs::write(base.join("outside/o.txt"), "o").unwrap();
    World {
        _root: root,
        root: base,
        gents,
    }
}

fn resolved(world: &World, path: &str) -> Result<Resolved, String> {
    resolve(
        path,
        Some(&world.root.join("work")),
        Some(&world.root),
        &world.gents,
    )
}

#[test]
fn resolve_expands_tilde_and_takes_relative_paths_from_the_workdir() {
    let world = world();
    assert_eq!(
        resolved(&world, "~/docs/x.pdf").unwrap().target,
        world.root.join("docs/x.pdf")
    );
    assert_eq!(
        resolved(&world, "a.txt").unwrap().target,
        world.root.join("work/a.txt")
    );
    let folder = resolved(&world, "sub").unwrap();
    assert!(folder.is_dir);
    assert_eq!(folder.folder(), world.root.join("work/sub"));
    let file = resolved(&world, "a.txt").unwrap();
    assert_eq!(file.folder(), world.root.join("work"));
    let error = resolve("a.txt", None, None, &world.gents).unwrap_err();
    assert!(error.contains("absolute"), "{error}");
    let error = resolved(&world, "missing.txt").unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
}

#[test]
fn resolve_never_reaches_the_gents_home_or_a_folder_holding_it() {
    let world = world();
    for path in ["~/gents-home", "~/gents-home/plugins", "~"] {
        let error = resolved(&world, path).unwrap_err();
        assert!(error.contains("gents home"), "{path}: {error}");
    }
}

#[test]
fn the_workdir_is_read_only_and_the_list_adds_folders() {
    let world = world();
    let work = world.root.join("work");
    let scope = Scope::load(&world.gents, Some(&work), Some(&world.root)).unwrap();
    assert_eq!(scope.granted(&work.join("a.txt")), Some(BindAccess::Read));
    assert_eq!(scope.granted(&world.root.join("docs/x.pdf")), None);

    add(&world.gents, &world.root.join("docs"), BindAccess::Read).unwrap();
    add(&world.gents, &work, BindAccess::ReadWrite).unwrap();
    let scope = Scope::load(&world.gents, Some(&work), Some(&world.root)).unwrap();
    assert_eq!(
        scope.granted(&world.root.join("docs/x.pdf")),
        Some(BindAccess::Read)
    );
    assert_eq!(
        scope.granted(&work.join("sub")),
        Some(BindAccess::ReadWrite)
    );
    assert_eq!(scope.granted(&world.root.join("outside/o.txt")), None);
}

#[test]
fn dot_dot_and_symlinks_resolve_before_the_scope_check() {
    let world = world();
    let work = world.root.join("work");
    let scope = Scope::load(&world.gents, Some(&work), Some(&world.root)).unwrap();
    let escaped = resolved(&world, "../outside/o.txt").unwrap();
    assert_eq!(escaped.target, world.root.join("outside/o.txt"));
    assert_eq!(scope.granted(&escaped.target), None);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(world.root.join("outside"), work.join("link")).unwrap();
        let through = resolved(&world, "link/o.txt").unwrap();
        assert_eq!(through.target, world.root.join("outside/o.txt"));
        assert_eq!(scope.granted(&through.target), None);
    }
}

#[test]
fn the_list_adds_replaces_and_removes_folders() {
    let world = world();
    let docs = world.root.join("docs");
    assert!(list(&world.gents).unwrap().is_empty());
    add(&world.gents, &docs, BindAccess::Read).unwrap();
    add(&world.gents, &docs, BindAccess::ReadWrite).unwrap();
    assert_eq!(
        list(&world.gents).unwrap(),
        vec![AllowedDir {
            path: docs.clone(),
            access: BindAccess::ReadWrite
        }]
    );
    assert!(add(
        &world.gents,
        &world.root.join("docs/missing.pdf"),
        BindAccess::Read
    )
    .is_err());
    let file = add(
        &world.gents,
        &world.root.join("docs/x.pdf"),
        BindAccess::Read,
    )
    .unwrap();
    assert_eq!(file.path, world.root.join("docs/x.pdf"));
    assert!(remove(&world.gents, &file.path).unwrap());
    assert!(remove(&world.gents, &docs).unwrap());
    assert!(!remove(&world.gents, &docs).unwrap());
}

#[test]
fn a_folder_binds_as_itself() {
    let world = world();
    let bound = bind(&resolved(&world, "sub").unwrap(), BindAccess::Read, true).unwrap();
    assert_eq!(bound.path(), world.root.join("work/sub"));
    assert_eq!(bound.target(), world.root.join("work/sub"));
    assert_eq!(bound.access(), BindAccess::Read);
}

#[cfg(unix)]
#[test]
fn a_file_binds_alone_through_a_hard_link_not_a_copy() {
    use std::os::unix::fs::MetadataExt;
    let world = world();
    let file = world.root.join("work/a.txt");
    let bound = bind(&resolved(&world, "a.txt").unwrap(), BindAccess::Read, true).unwrap();
    let names: Vec<_> = std::fs::read_dir(bound.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("a.txt")],
        "no sibling is visible"
    );
    assert_eq!(bound.target(), bound.path().join("a.txt"));
    assert_eq!(
        std::fs::metadata(bound.target()).unwrap().ino(),
        std::fs::metadata(&file).unwrap().ino(),
        "the same inode, nothing copied"
    );
    let private = bound.path().to_path_buf();
    drop(bound);
    assert!(
        !private.exists(),
        "the private folder goes with the binding"
    );
    assert!(file.exists());
}

#[test]
fn the_recheck_catches_a_binding_whose_target_was_swapped() {
    let world = world();
    let bound = bind(&resolved(&world, "sub").unwrap(), BindAccess::Read, true).unwrap();
    bound.recheck().unwrap();
    let file = bind(&resolved(&world, "a.txt").unwrap(), BindAccess::Read, true).unwrap();
    file.recheck().unwrap();
    std::fs::remove_file(file.target()).unwrap();
    assert!(file.recheck().is_err());
}

#[test]
fn a_working_folder_that_is_the_root_the_home_or_holds_either_is_not_allowed() {
    let world = world();
    let user_home = world.root.join("docs");
    let gents_home = world.gents.clone();
    for broad in [
        PathBuf::from("/"),
        user_home.clone(),
        world.root.clone(),
        user_home.parent().unwrap().to_path_buf(),
    ] {
        let scope = Scope::load(&gents_home, Some(&broad), Some(&user_home)).unwrap();
        assert_eq!(
            scope.granted(&user_home.join("x.pdf")),
            None,
            "{} must not be auto-allowed",
            broad.display()
        );
        assert_eq!(scope.granted(&world.root.join("outside/o.txt")), None);
    }
    let work = world.root.join("work");
    let scope = Scope::load(&gents_home, Some(&work), Some(&user_home)).unwrap();
    assert_eq!(scope.granted(&work.join("a.txt")), Some(BindAccess::Read));
    let own_folder = Scope::load(&gents_home, Some(&work.join("sub")), None).unwrap();
    assert_eq!(own_folder.granted(&work.join("a.txt")), None);
}

#[test]
fn the_file_tools_refuse_the_allowed_folders_file_and_the_approval_queue() {
    let world = world();
    protect(&world.gents);
    assert!(is_protected(
        &world.gents.join(crate::home::ALLOWED_DIRS_FILE_NAME)
    ));
    assert!(is_protected(
        &world
            .gents
            .join(crate::home::PLUGIN_APPROVALS_DIR_NAME)
            .join("q.decision")
    ));
    assert!(!is_protected(&world.gents.join("plugins/x")));
    assert!(!is_protected(&world.root.join("work/a.txt")));
}

#[test]
fn an_allowed_file_does_not_authorize_its_parent_or_sibling() {
    let world = world();
    let file = world.root.join("work/a.txt");
    add(&world.gents, &file, BindAccess::Read).unwrap();
    let scope = Scope::load(&world.gents, None, Some(&world.root)).unwrap();
    assert_eq!(scope.granted(&file), Some(BindAccess::Read));
    assert_eq!(scope.granted(file.parent().unwrap()), None);
    assert_eq!(scope.granted(&world.root.join("work/b.txt")), None);
}

#[test]
fn persisted_allowances_apply_the_same_breadth_rules_as_working_folders() {
    let world = world();
    for path in [Path::new("/"), world.root.as_path(), world.gents.as_path()] {
        assert!(
            add(&world.gents, path, BindAccess::Read).is_err(),
            "{}",
            path.display()
        );
    }
    save(
        &world.gents,
        vec![AllowedDir {
            path: world.root.clone(),
            access: BindAccess::Read,
        }],
    )
    .unwrap();
    assert!(Scope::load(&world.gents, None, Some(&world.root)).is_err());
}
