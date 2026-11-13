use std::io::Write;
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

fn staged_object(directory: &Path) -> PathBuf {
    std::fs::read_dir(directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

#[test]
fn staged_objects_refusal_matches_the_lean_model() {
    gents_lean_vocab::families::staged_permission_objects_refusal::replay::<String>(|i| {
        let world = world();
        let directory = world
            .gents
            .join(gents_store::local::home::PERMISSION_STAGING_DIR_NAME);
        let context = crate::native::ToolContext::new(world.root.clone(), false).unwrap();
        let mut observed = None;
        gents_pack::plugin::allowed::write_permission_file(
            &world.gents,
            &allowed_file(&world.gents),
            |output| {
                output.write_all(b"new permissions")?;
                let (target, mount) = match i.object.as_str() {
                    "staged" => (staged_object(&directory), directory.clone()),
                    "staged_alias" => {
                        let alias = world.root.join("work/permission-alias");
                        std::fs::hard_link(staged_object(&directory), &alias)?;
                        (alias, world.root.join("work"))
                    }
                    "ordinary" => {
                        let ordinary = world.gents.join("plugins/source.txt");
                        std::fs::write(&ordinary, "plugin source")?;
                        (ordinary, world.gents.join("plugins"))
                    }
                    other => panic!("unknown staged object {other}"),
                };
                let text = target.to_str().unwrap();
                let resolved = context.resolve_path(text);
                observed = Some(
                    gents_lean_vocab::families::staged_permission_objects_refusal::RefusalOutput {
                        resolve_refused: resolved.is_err(),
                        resolved_verbatim: resolved
                            .as_ref()
                            .is_ok_and(|path| path == &target),
                        allow_create_refused: context.resolve_path_allow_create(text).is_err(),
                        mount_refused: gents_pack::plugin::allowed::ensure_mount_safe(&mount)
                            .is_err(),
                    },
                );
                Ok(())
            },
        )
        .unwrap();
        observed.unwrap()
    });

    let world = world();
    let directory = world
        .gents
        .join(gents_store::local::home::PERMISSION_STAGING_DIR_NAME);
    gents_pack::plugin::allowed::write_permission_file(
        &world.gents,
        &allowed_file(&world.gents),
        |output| {
            output.write_all(b"new permissions")?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read(allowed_file(&world.gents)).unwrap(),
        b"new permissions"
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
}
