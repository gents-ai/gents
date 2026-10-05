//! The committed fixtures are exactly what the generator writes, and the
//! hostile ones are hostile in the way their names say.

#[allow(dead_code)]
#[path = "../tools/gen_fixtures.rs"]
mod generator;

use std::path::Path;

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn the_generator_reproduces_every_committed_fixture_byte_for_byte() {
    let dir = fixtures_dir();
    let files = generator::files();
    assert_eq!(files.len(), 19);
    for (name, bytes) in &files {
        let committed = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            &committed == bytes,
            "{name} differs from what gen_fixtures writes"
        );
    }
}

#[test]
fn no_committed_fixture_is_missing_from_the_generator() {
    let dir = fixtures_dir();
    let known: std::collections::BTreeSet<String> =
        generator::files().into_iter().map(|f| f.0).collect();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            let rel = path
                .strip_prefix(&dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let kind = e.file_type().unwrap();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_symlink() {
                assert_eq!(rel, "hostile/link.csv", "an unexpected symbolic link");
            } else {
                assert!(
                    known.contains(&rel),
                    "{rel} is committed but not generated: add it to tools/gen_fixtures.rs"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn the_symbolic_link_fixture_points_at_a_real_file_beside_the_hostile_folder() {
    let link = fixtures_dir().join("hostile/link.csv");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        Path::new("../data/sales.csv")
    );
    assert!(
        std::fs::metadata(&link).unwrap().is_file(),
        "it resolves to the sales file"
    );
}

#[test]
fn the_hostile_fixtures_fail_in_the_way_their_names_say() {
    let dir = fixtures_dir().join("hostile");
    let run = |file: &str| {
        let req = serde_json::json!({"chart": "bar", "path": dir.to_string_lossy(), "file": file})
            .to_string();
        crate::run(&req).unwrap_err().0
    };
    assert!(run("empty.csv").contains("is empty"));
    assert!(run("truncated.json").contains("truncated"));
    assert!(run("garbage.json").contains("JSON"));
    assert!(run("binary.csv").contains("binary"));
    assert!(run("header_only.csv").contains("no rows"));
    assert!(run("long_cell.csv").contains("longer than 65536 bytes"));
    assert!(run("text.csv").contains("no column of numbers"));
    #[cfg(unix)]
    assert!(run("link.csv").contains("symbolic link"));
}
