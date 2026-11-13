use super::*;

#[test]
fn operator_path_protection_follows_the_lean_model() {
    gents_lean_vocab::families::operator_paths_protection::replay::<String>(|i| {
        let world = world();
        let guarded = file(&world.gents);
        let queue = world
            .gents
            .join(gents_store::local::home::PLUGIN_APPROVALS_DIR_NAME);
        std::fs::create_dir_all(queue.join("v3")).unwrap();
        std::fs::write(&guarded, "operator state").unwrap();
        let question = queue.join("v3/question.decision");
        std::fs::write(&question, "operator answer").unwrap();
        let mut paths = vec![guarded.clone(), queue];
        let staging = world
            .gents
            .join(gents_store::local::home::PERMISSION_STAGING_DIR_NAME);
        paths.push(staging.clone());
        let mut target = world.root.join("work/a.txt");
        match i.kind.as_str() {
            "ordinary" => {}
            "write_only" => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o200))
                        .unwrap();
                }
            }
            "ordinary_alias" => {
                let alias = world.root.join("work/alias.txt");
                std::fs::hard_link(&target, &alias).unwrap();
                target = alias;
            }
            "missing" => target = world.root.join("work/missing.txt"),
            "staged" | "staged_alias" => {
                std::fs::create_dir(&staging).unwrap();
                let staged = staging.join("pending");
                std::fs::write(&staged, "pending permissions").unwrap();
                target = if i.kind == "staged" {
                    staged
                } else {
                    let alias = world.root.join("work/staged-alias");
                    std::fs::hard_link(&staged, &alias).unwrap();
                    alias
                };
            }
            "uncreated_staged" => target = staging.join("future"),
            "named" => target = guarded,
            "uncreated_named" => {
                std::fs::remove_file(&guarded).unwrap();
                target = guarded;
            }
            "alias" | "queue_alias" => {
                target = world.root.join("work/operator-alias.txt");
                let source = if i.kind == "alias" {
                    &guarded
                } else {
                    &question
                };
                std::fs::hard_link(source, &target).unwrap();
            }
            "unavailable" => target = world.root.join("work/invalid\0path"),
            "guard_unavailable" => paths.push(world.gents.join("invalid\0path")),
            other => panic!("unknown operator path case: {other}"),
        }
        protected::inspect(&paths, &target).unwrap_or(true)
    });
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
fn operator_mounts_conform_to_the_model() {
    gents_lean_vocab::families::operator_protection_mount::replay::<String, String, String>(|input| {
        let world = world();
        let guarded = file(&world.gents);
        std::fs::write(&guarded, "operator state").unwrap();
        let work = world.root.join("work");
        let root = if input.root_exists {
            work.clone()
        } else {
            work.join("missing")
        };
        let paths = match input.guard.as_str() {
            "unguarded" => Vec::new(),
            "outside" => vec![guarded.clone()],
            "named_within" => vec![work.join("future-state")],
            "uninspectable" => vec![world.gents.join("invalid\0path")],
            other => panic!("unknown mount guard: {other}"),
        };
        for (index, entry) in input.entries.iter().enumerate() {
            let link = work.join(format!("sub/mount-entry-{index}"));
            match entry.as_str() {
                "ordinary" => std::fs::hard_link(work.join("a.txt"), link).unwrap(),
                "shares_operator_object" => std::fs::hard_link(&guarded, link).unwrap(),
                other => panic!("unknown mount entry: {other}"),
            }
        }
        let verdict = match protected::mount_contains_protected(&paths, &root) {
            Ok(false) => "clear",
            Ok(true) => "reserved",
            Err(_) => "unreadable",
        };
        verdict.to_owned()
    });
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
fn command_protection_conforms_to_the_model() {
    gents_lean_vocab::families::operator_protection_command::replay::<String, String>(|trace| {
        let world = world();
        let state = file(&world.gents);
        std::fs::write(&state, "permissions").unwrap();
        let alias = world.root.join("work/command-alias");
        let mut held: Option<CommandProtection> = None;
        let mut outcomes = Vec::new();
        for step in &trace.steps {
            let outcome = match step.as_str() {
                "capture" => match CommandProtection::capture(&world.gents, true) {
                    Ok(protection) => {
                        held = Some(protection);
                        "allowed"
                    }
                    Err(_) => "refused",
                },
                "revalidate" => {
                    match held
                        .as_ref()
                        .expect("a revalidation follows a granted capture")
                        .revalidate()
                    {
                        Ok(()) => "allowed",
                        Err(_) => "refused",
                    }
                }
                "add_alias" => {
                    std::fs::hard_link(&state, &alias).unwrap();
                    "applied"
                }
                "remove_alias" => {
                    std::fs::remove_file(&alias).unwrap();
                    "applied"
                }
                other => panic!("unknown protection step: {other}"),
            };
            outcomes.push(outcome.to_owned());
        }
        outcomes
    });
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
                .join(gents_store::local::home::PERMISSION_STAGING_DIR_NAME)
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
        .join(gents_store::local::home::PERMISSION_STAGING_DIR_NAME);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ListAction {
    Add,
    GrantAtLeast,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ListOutcome {
    Granted,
    Refused,
    Removed,
    Absent,
}

fn modelled_path(world: &World, components: &[String]) -> PathBuf {
    match components.split_first() {
        None => PathBuf::from("/"),
        Some((first, rest)) => {
            assert_eq!(
                first, "tmp",
                "a modelled path is the root or inside the world"
            );
            rest.iter()
                .fold(world.root.clone(), |path, part| path.join(part))
        }
    }
}

fn modelled_components(world: &World, path: &Path) -> Vec<String> {
    let inside = path.strip_prefix(&world.root).expect("a path of the world");
    std::iter::once("tmp".to_owned())
        .chain(
            inside
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned()),
        )
        .collect()
}

fn modelled_world(
    world: &World,
    gents_home: &[String],
    present: &[Vec<String>],
    named: &[&Vec<String>],
) {
    assert_eq!(modelled_path(world, gents_home), world.gents);
    for path in present {
        assert!(modelled_path(world, path).exists(), "{path:?} exists");
    }
    for path in named {
        assert_eq!(
            modelled_path(world, path).exists(),
            present.contains(path),
            "the model knows whether {path:?} exists"
        );
    }
}

#[test]
fn the_allowed_list_conforms_to_the_model() {
    use gents_lean_vocab::families::pack_allowed_list as family;

    family::replay::<BindAccess, ListAction, ListOutcome>(|input| {
        let world = world();
        let named: Vec<&Vec<String>> = input.steps.iter().map(|step| &step.path).collect();
        modelled_world(
            &world,
            &input.world.gents_home,
            &input.world.present,
            &named,
        );
        let outcomes = input
            .steps
            .iter()
            .map(|step| {
                let path = modelled_path(&world, &step.path);
                let granted = |result: Result<AllowedDir>| match result {
                    Ok(_) => ListOutcome::Granted,
                    Err(_) => ListOutcome::Refused,
                };
                match step.action {
                    ListAction::Add => granted(add(&world.gents, &path, step.access)),
                    ListAction::GrantAtLeast => {
                        granted(grant_at_least(&world.gents, &path, step.access))
                    }
                    ListAction::Remove => {
                        if remove(&world.gents, &path).unwrap() {
                            ListOutcome::Removed
                        } else {
                            ListOutcome::Absent
                        }
                    }
                }
            })
            .collect();
        family::AllowedListResult {
            outcomes,
            entries: list(&world.gents)
                .unwrap()
                .into_iter()
                .map(|entry| family::Entry {
                    path: modelled_components(&world, &entry.path),
                    access: entry.access,
                })
                .collect(),
        }
    });
}

#[test]
fn the_allowed_scope_conforms_to_the_model() {
    use gents_lean_vocab::families::pack_allowed_scope as family;

    family::replay::<BindAccess>(|input| {
        let world = world();
        let named: Vec<&Vec<String>> = input
            .entries
            .iter()
            .map(|entry| &entry.path)
            .chain(&input.workdir)
            .chain(&input.targets)
            .collect();
        modelled_world(
            &world,
            &input.world.gents_home,
            &input.world.present,
            &named,
        );
        let user_home = input
            .world
            .user_home
            .as_ref()
            .map(|home| modelled_path(&world, home));
        let workdir = input.workdir.as_ref().map(|dir| modelled_path(&world, dir));
        save(
            &world.gents,
            input
                .entries
                .iter()
                .map(|entry| AllowedDir {
                    path: modelled_path(&world, &entry.path),
                    access: entry.access,
                })
                .collect(),
        )
        .unwrap();
        let scope = Scope::load(&world.gents, workdir.as_deref(), user_home.as_deref());
        let targets: Vec<PathBuf> = input
            .targets
            .iter()
            .map(|target| modelled_path(&world, target))
            .collect();
        family::AllowedScopeResult {
            loads: scope.is_ok(),
            granted: scope.map_or_else(
                |_| Vec::new(),
                |scope| targets.iter().map(|target| scope.granted(target)).collect(),
            ),
            reachable: targets
                .iter()
                .map(|target| {
                    resolve(
                        &target.display().to_string(),
                        None,
                        user_home.as_deref(),
                        &world.gents,
                    )
                    .is_ok()
                })
                .collect(),
        }
    });
}
