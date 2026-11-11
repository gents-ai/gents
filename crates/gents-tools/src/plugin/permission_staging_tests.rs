use std::path::{Path, PathBuf};

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

fn allowed_file(home: &Path) -> PathBuf {
    home.join(gents_store::local::home::ALLOWED_DIRS_FILE_NAME)
}

#[test]
fn staged_permission_objects_are_rejected_by_native_paths_and_plugin_mounts() {
    use std::io::Write;

    let world = world();
    let directory = world
        .gents
        .join(gents_store::local::home::PERMISSION_STAGING_DIR_NAME);
    let context = crate::native::ToolContext::new(world.root.clone(), false).unwrap();
    gents_pack::plugin::allowed::write_permission_file(
        &world.gents,
        &allowed_file(&world.gents),
        |output| {
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
            assert!(gents_pack::plugin::allowed::ensure_mount_safe(&directory).is_err());
            assert!(
                gents_pack::plugin::allowed::ensure_mount_safe(&world.root.join("work")).is_err()
            );
            std::fs::remove_file(alias)?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read(allowed_file(&world.gents)).unwrap(),
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
