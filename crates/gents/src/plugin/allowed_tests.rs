use super::*;

#[test]
fn generated_operator_path_cases_protect_names_and_file_aliases() {
    #[derive(serde::Deserialize)]
    struct Case {
        kind: String,
        expected: bool,
        command: Option<CommandCase>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CommandCase {
        required: bool,
        supported: bool,
        inspected: bool,
        single_linked: bool,
    }

    let cases = &crate::lean_vocab_test::lean_contract_snapshot().operator_path_cases;
    assert_eq!(cases.len(), 29);
    for value in cases {
        let case: Case = serde_json::from_value(value.clone()).unwrap();
        if let Some(input) = case.command {
            assert_eq!(
                gents_loop::tool_policy::operator_paths::command_guard_allows(
                    input.required,
                    input.supported,
                    input.inspected,
                    input.single_linked,
                ),
                case.expected,
                "{value}",
            );
            continue;
        }
        let world = world();
        let guarded = file(&world.gents);
        let queue = world
            .gents
            .join(crate::store::local::home::PLUGIN_APPROVALS_DIR_NAME);
        std::fs::create_dir_all(queue.join("v3")).unwrap();
        std::fs::write(&guarded, "operator state").unwrap();
        let question = queue.join("v3/question.decision");
        std::fs::write(&question, "operator answer").unwrap();
        let mut paths = vec![guarded.clone(), queue];
        let staging = world
            .gents
            .join(crate::store::local::home::PERMISSION_STAGING_DIR_NAME);
        paths.push(staging.clone());
        let mut target = world.root.join("work/a.txt");
        match case.kind.as_str() {
            "ordinary" => {}
            "writeOnly" => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o200))
                        .unwrap();
                }
            }
            "ordinaryAlias" => {
                let alias = world.root.join("work/alias.txt");
                std::fs::hard_link(&target, &alias).unwrap();
                target = alias;
            }
            "missing" => target = world.root.join("work/missing.txt"),
            "staged" | "stagedAlias" => {
                std::fs::create_dir(&staging).unwrap();
                let staged = staging.join("pending");
                std::fs::write(&staged, "pending permissions").unwrap();
                target = if case.kind == "staged" {
                    staged
                } else {
                    let alias = world.root.join("work/staged-alias");
                    std::fs::hard_link(&staged, &alias).unwrap();
                    alias
                };
            }
            "uncreatedStaged" => target = staging.join("future"),
            "named" => target = guarded,
            "uncreatedNamed" => {
                std::fs::remove_file(&guarded).unwrap();
                target = guarded;
            }
            "alias" | "queueAlias" => {
                target = world.root.join("work/operator-alias.txt");
                let source = if case.kind == "alias" {
                    &guarded
                } else {
                    &question
                };
                std::fs::hard_link(source, &target).unwrap();
            }
            "unavailable" => target = world.root.join("work/invalid\0path"),
            "guardUnavailable" => paths.push(world.gents.join("invalid\0path")),
            other => panic!("unknown operator path case: {other}"),
        }
        assert_eq!(
            protected::inspect(&paths, &target).unwrap_or(true),
            case.expected,
            "{value}"
        );
    }
}

#[test]
fn a_mount_rejects_nested_operator_aliases_and_reserved_names() {
    let world = world();
    let guarded = file(&world.gents);
    std::fs::write(&guarded, "operator state").unwrap();
    let work = world.root.join("work");
    let paths = [guarded];
    std::fs::hard_link(work.join("a.txt"), work.join("sub/ordinary-alias")).unwrap();
    assert!(!protected::mount_contains_protected(&paths, &work).unwrap());
    std::fs::hard_link(&paths[0], work.join("sub/operator-alias")).unwrap();
    assert!(protected::mount_contains_protected(&paths, &work).unwrap());
    assert!(protected::mount_contains_protected(&[work.join("future-state")], &work).unwrap());
}

#[cfg(unix)]
#[test]
fn a_mount_does_not_follow_directory_links_or_cycles() {
    let world = world();
    let guarded = file(&world.gents);
    std::fs::write(&guarded, "operator state").unwrap();
    let work = world.root.join("work");
    std::os::unix::fs::symlink(&world.gents, work.join("outside-link")).unwrap();
    std::os::unix::fs::symlink(&work, work.join("sub/cycle")).unwrap();
    std::os::unix::fs::symlink(work.join("missing"), work.join("dangling-link")).unwrap();
    assert!(!protected::mount_contains_protected(&[guarded], &work).unwrap());
}

#[test]
fn a_mount_fails_when_operator_objects_cannot_be_inspected() {
    let world = world();
    let work = world.root.join("work");
    assert!(
        protected::mount_contains_protected(&[world.gents.join("invalid\0path")], &work).is_err()
    );
    assert!(protected::mount_contains_protected(&[], &work.join("missing")).is_err());
}

#[test]
fn a_removed_operator_directory_is_absent_but_a_wrong_kind_is_an_error() {
    let world = world();
    assert!(
        protected::optional_directory(&world.root.join("removed-home"))
            .unwrap()
            .is_none()
    );
    assert!(protected::optional_directory(&world.root.join("work/a.txt")).is_err());
}

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

#[cfg(unix)]
#[test]
fn command_protection_rejects_existing_and_late_aliases() {
    let world = world();
    let state = file(&world.gents);
    std::fs::write(&state, "permissions").unwrap();
    let protection = CommandProtection::capture(&world.gents, true).unwrap();
    let alias = world.root.join("work/command-alias");
    std::fs::hard_link(&state, &alias).unwrap();
    assert!(protection.revalidate().is_err());
    assert!(CommandProtection::capture(&world.gents, true).is_err());
    std::fs::remove_file(alias).unwrap();
    assert!(protection.revalidate().is_ok());
}

#[test]
fn command_protection_has_no_unsupported_fallback() {
    let world = world();
    assert!(CommandProtection::capture(&world.gents, false).is_err());
}

#[test]
fn staged_permission_objects_are_rejected_by_native_paths_and_plugin_mounts() {
    use std::io::Write;

    let world = world();
    let directory = world
        .gents
        .join(crate::store::local::home::PERMISSION_STAGING_DIR_NAME);
    let context = crate::tools::native::ToolContext::new(world.root.clone(), false).unwrap();
    write_permission_file(&world.gents, &file(&world.gents), |output| {
        output.write_all(b"new permissions")?;
        let staged = std::fs::read_dir(&directory)?.next().unwrap()?.path();
        let alias = world.root.join("work/permission-alias");
        std::fs::hard_link(&staged, &alias)?;
        assert!(context.resolve_path(staged.to_str().unwrap()).is_err());
        assert!(context.resolve_path(alias.to_str().unwrap()).is_err());
        assert!(context
            .resolve_path_allow_create(staged.to_str().unwrap())
            .is_err());
        assert!(context
            .resolve_path_allow_create(alias.to_str().unwrap())
            .is_err());
        assert!(ensure_mount_safe(&directory).is_err());
        assert!(ensure_mount_safe(&world.root.join("work")).is_err());
        std::fs::remove_file(alias)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        std::fs::read(file(&world.gents)).unwrap(),
        b"new permissions"
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    let ordinary = world.gents.join("plugins/source.txt");
    std::fs::write(&ordinary, "plugin source").unwrap();
    assert_eq!(
        context.resolve_path(ordinary.to_str().unwrap()).unwrap(),
        ordinary
    );
}

#[test]
fn failed_permission_encoding_keeps_the_prior_file_and_removes_staging() {
    use std::io::Write;

    let world = world();
    let target = file(&world.gents);
    std::fs::write(&target, "prior permissions").unwrap();
    let result = write_permission_file(&world.gents, &target, |output| {
        output.write_all(b"incomplete")?;
        anyhow::bail!("fixture encoding failure")
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"prior permissions");
    assert_eq!(
        std::fs::read_dir(
            world
                .gents
                .join(crate::store::local::home::PERMISSION_STAGING_DIR_NAME)
        )
        .unwrap()
        .count(),
        0
    );
}

#[test]
fn both_permission_writers_publish_without_leaving_staged_files() {
    let world = world();
    let entry = add(
        &world.gents,
        &world.root.join("work/a.txt"),
        BindAccess::Read,
    )
    .unwrap();
    assert_eq!(list(&world.gents).unwrap(), vec![entry]);
    let key = crate::plugin::approval::workspace::WorkspaceKey::canonical(
        "node",
        "requester",
        "agent",
        &world.root.join("work"),
    )
    .unwrap();
    let saved = crate::plugin::approval::workspace::save_mode(
        &world.gents,
        &key,
        0,
        crate::plugin::approval::workspace::Mode::FullAccess,
        None,
    )
    .unwrap();
    let loaded = crate::plugin::approval::workspace::get(&world.gents, &key)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.scope(), saved.scope());
    assert_eq!(loaded.selection.mode, saved.selection.mode);
    assert_eq!(
        std::fs::read_dir(
            world
                .gents
                .join(crate::store::local::home::PERMISSION_STAGING_DIR_NAME)
        )
        .unwrap()
        .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn a_retargeted_staging_directory_cannot_publish_permissions() {
    use std::io::Write;

    let world = world();
    let target = file(&world.gents);
    std::fs::write(&target, "prior permissions").unwrap();
    let directory = world
        .gents
        .join(crate::store::local::home::PERMISSION_STAGING_DIR_NAME);
    let outside = world.root.join("outside");
    std::os::unix::fs::symlink(&outside, &directory).unwrap();
    assert!(write_permission_file(&world.gents, &target, |_| {
        panic!("an aliased staging directory reached the writer")
    })
    .is_err());
    std::fs::remove_file(&directory).unwrap();
    assert!(write_permission_file(&world.gents, &target, |output| {
        output.write_all(b"unpublished permissions")?;
        std::fs::rename(&directory, world.root.join("moved-staging"))?;
        std::fs::create_dir(&directory)?;
        Ok(())
    })
    .is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"prior permissions");
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 1);
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

#[cfg(unix)]
#[test]
fn a_remembered_path_cannot_follow_a_replacement_symlink() {
    let world = world();
    let docs = world.root.join("docs");
    let outside = world.root.join("outside");
    add(&world.gents, &docs, BindAccess::Read).unwrap();
    let scope = Scope::load(&world.gents, None, Some(&world.root)).unwrap();
    std::fs::rename(&docs, world.root.join("original-docs")).unwrap();
    std::os::unix::fs::symlink(&outside, &docs).unwrap();
    assert_eq!(scope.granted(&outside.join("o.txt")), None);
    assert!(Scope::load(&world.gents, None, Some(&world.root)).is_err());
}

#[cfg(unix)]
#[test]
fn a_queued_answer_cannot_remember_a_different_canonical_target() {
    let world = world();
    let docs = world.root.join("docs");
    let outside = world.root.join("outside");
    std::fs::rename(&docs, world.root.join("original-docs")).unwrap();
    std::os::unix::fs::symlink(&outside, &docs).unwrap();
    assert!(grant_at_least(&world.gents, &docs, BindAccess::ReadWrite).is_err());
    assert!(list(&world.gents).unwrap().is_empty());
    assert_eq!(
        add(&world.gents, &docs, BindAccess::Read).unwrap().path,
        outside
    );
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
        &world
            .gents
            .join(crate::store::local::home::ALLOWED_DIRS_FILE_NAME)
    ));
    for name in [
        crate::plugin::approval::workspace::FILE,
        crate::plugin::approval::workspace::LOCK,
    ] {
        assert!(is_protected(&world.gents.join(name)));
    }
    assert!(is_protected(
        &world
            .gents
            .join(crate::store::local::home::PLUGIN_APPROVALS_DIR_NAME)
            .join("q.decision")
    ));
    assert!(is_protected(
        &world
            .gents
            .join(crate::store::local::home::PERMISSION_STAGING_DIR_NAME)
            .join("pending")
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
#[test]
fn concurrent_grants_keep_other_paths_and_the_widest_approval() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..12)
        .map(|index| {
            let path = outside.path().join(format!("document-{index}"));
            std::fs::write(&path, "document").unwrap();
            path.canonicalize().unwrap()
        })
        .collect();
    std::thread::scope(|scope| {
        for path in &paths {
            for access in [BindAccess::Read, BindAccess::ReadWrite] {
                let home = home.path();
                scope.spawn(move || {
                    grant_at_least(home, path, access).unwrap();
                });
            }
        }
    });
    let grants = list(home.path()).unwrap();
    assert_eq!(grants.len(), paths.len());
    assert!(grants
        .iter()
        .all(|grant| grant.access == BindAccess::ReadWrite));
}
