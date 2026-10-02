//! `BoundDir` and `PluginRunner::call_bound`.

use super::*;

#[test]
fn bound_dir_requires_a_directory() {
    let file = tempfile::NamedTempFile::new().expect("tempfile");
    BoundDir::new(file.path(), None).expect_err("a file is not a directory");
}

#[test]
fn bound_dir_refuses_a_path_outside_within() {
    let root = tempfile::tempdir().expect("tempdir");
    let within = root.path().join("within");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&within).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let error = BoundDir::new(&outside, Some(&within)).expect_err("outside must be refused within");
    let message = format!("{error:#}");
    assert!(message.contains("outside"), "{message}");
}

#[test]
fn bound_dir_allows_a_path_inside_within() {
    let root = tempfile::tempdir().expect("tempdir");
    let within = root.path().join("within");
    let nested = within.join("nested");
    std::fs::create_dir_all(&nested).unwrap();

    let bound = BoundDir::new(&nested, Some(&within)).expect("nested is inside within");
    assert_eq!(bound.path(), nested.canonicalize().unwrap().as_path());
}

/// A lexical `starts_with` on the uncanonicalized path would accept this: as
/// text, `within/../outside` starts with `within`. Canonicalizing first
/// (what `BoundDir::new` actually does) resolves the `..` away, so the
/// comparison sees `outside` sitting beside `within`, not inside it.
#[test]
fn bound_dir_refuses_dot_dot_traversal_out_of_within() {
    let root = tempfile::tempdir().expect("tempdir");
    let within = root.path().join("within");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&within).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let traversal = within.join("..").join("outside");
    let error = BoundDir::new(&traversal, Some(&within))
        .expect_err("../outside must resolve outside within and be refused");
    assert!(format!("{error:#}").contains("outside"));
}

/// A symlink physically inside `within` whose target lives outside it must
/// still be refused: `canonicalize` resolves the symlink before the prefix
/// check runs, so the comparison sees where it actually points, not where
/// it sits in the directory tree.
#[test]
#[cfg(unix)]
fn bound_dir_refuses_a_symlink_inside_within_pointing_outside() {
    let root = tempfile::tempdir().expect("tempdir");
    let within = root.path().join("within");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&within).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let link = within.join("link");
    std::os::unix::fs::symlink(&outside, &link).expect("creating the symlink");

    let error = BoundDir::new(&link, Some(&within))
        .expect_err("a symlink resolving outside within must be refused");
    assert!(format!("{error:#}").contains("outside"));
}

#[test]
fn call_bound_refuses_a_plugin_that_declares_no_bind_dir() {
    let (plugin, afb) = build_plugin_pack("no_bind_pack", ECHO_WAT, None);
    let runner = PluginRunner::compile(&afb, &plugin).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bound = BoundDir::new(dir.path(), None).unwrap();

    let error = runner
        .call_bound(&serde_json::json!({}), &PluginBudget::default(), &bound)
        .expect_err("a plugin with no bind_dir cannot be bound");
    assert!(format!("{error:#}").contains("bind_dir"));
}

#[test]
fn call_bound_overwrites_the_declared_input_field_with_the_canonical_bound_path() {
    let (mut plugin, afb) = build_plugin_pack("bind_pack", ECHO_WAT, None);
    plugin.bind_dir = Some(crate::pack::PluginDirBinding {
        input_field: "root".to_owned(),
        original_field: None,
        description: "a directory".to_owned(),
        access: Default::default(),
    });
    let runner = PluginRunner::compile(&afb, &plugin).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bound = BoundDir::new(dir.path(), None).unwrap();

    let outcome = runner
        .call_bound(
            &serde_json::json!({"root": "whatever-the-caller-wrote", "other": 1}),
            &PluginBudget::default(),
            &bound,
        )
        .expect("a bind_dir plugin can be called bound");
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(
        outcome.output,
        serde_json::json!({
            "root": bound.path().to_str().unwrap(),
            "other": 1,
        })
    );
}

/// A missing operator input (`Value::Null`, what a CLI turns an omitted
/// `--input` into) is not a refusal when the binding itself supplies the
/// only field the call needs: it is treated as an empty object before
/// `input_field` is inserted. A non-object, non-null value is still refused.
#[test]
fn call_bound_treats_null_arguments_as_an_empty_object() {
    let (mut plugin, afb) = build_plugin_pack("bind_null_pack", ECHO_WAT, None);
    plugin.bind_dir = Some(crate::pack::PluginDirBinding {
        input_field: "root".to_owned(),
        original_field: None,
        description: "a directory".to_owned(),
        access: Default::default(),
    });
    let runner = PluginRunner::compile(&afb, &plugin).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bound = BoundDir::new(dir.path(), None).unwrap();

    let outcome = runner
        .call_bound(&serde_json::Value::Null, &PluginBudget::default(), &bound)
        .expect("null arguments must be treated as an empty object, not refused");
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(
        outcome.output,
        serde_json::json!({"root": bound.path().to_str().unwrap()})
    );

    let error = runner
        .call_bound(&serde_json::json!([1, 2]), &PluginBudget::default(), &bound)
        .expect_err("a non-object, non-null value must still be refused");
    assert!(format!("{error:#}").contains("must be a JSON object"));
}

#[test]
fn admission_refuses_a_bind_dir_plugin_whose_dispatch_path_cannot_enforce_read_only_fs() {
    let afb_bytes = source_only_afb("py_plugin", "python", "source/main.py", b"print(1)");
    let mut plugin = plugin_named("py_plugin", "python");
    plugin.bind_dir = Some(crate::pack::PluginDirBinding {
        input_field: "root".to_owned(),
        original_field: None,
        description: "a directory".to_owned(),
        access: Default::default(),
    });

    let error = PluginRunner::compile(&afb_bytes, &plugin)
        .expect_err("python cannot enforce a read-only filesystem grant");
    let message = format!("{error:#}");
    assert!(message.contains("py_plugin"), "{message}");
    assert!(message.contains("read-only"), "{message}");
}

/// A guest that creates `path` in its one preopen (fd 3) holding `{}`, then
/// writes `{}` to stdout; stdout stays empty when the create fails, so a
/// refusal shows as `BadOutput`.
pub(crate) fn create_file_wat(path: &str) -> String {
    format!(
        r#"(module
          (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "{path}")
          (data (i32.const 600) "{{}}")
          (func (export "_start")
            (if (call $path_open (i32.const 3) (i32.const 0) (i32.const 512) (i32.const {len})
                  (i32.const 1) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 272))
              (then (return)))
            i32.const 256  i32.const 600  i32.store
            i32.const 260  i32.const 2    i32.store
            (drop (call $fd_write (i32.load (i32.const 272)) (i32.const 256) (i32.const 1) (i32.const 268)))
            (drop (call $fd_write (i32.const 1) (i32.const 256) (i32.const 1) (i32.const 268)))))"#,
        len = path.len()
    )
}

#[test]
fn call_bound_read_write_lets_the_guest_create_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let bound = BoundDir::new(dir.path(), None).unwrap();
    let (mut plugin, afb) = build_plugin_pack("writer", &create_file_wat("new.json"), None);
    plugin.bind_dir = Some(crate::pack::PluginDirBinding {
        input_field: "root".to_owned(),
        original_field: None,
        description: "a directory".to_owned(),
        access: crate::pack::BindAccess::ReadWrite,
    });
    let runner = PluginRunner::compile(&afb, &plugin).unwrap();
    let outcome = call_bound_verdict(&runner, &bound);
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.json")).unwrap(),
        "{}"
    );
}

/// A guest that opens `path` (relative to its one preopen, fd 3) with
/// `oflags`, then copies the file to stdout; stdout stays empty when the
/// open fails, so a refusal shows as `BadOutput`.
pub(crate) fn open_and_copy_wat(path: &str, oflags: u32) -> String {
    format!(
        r#"(module
          (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_read"
            (func $fd_read (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "{path}")
          (func (export "_start")
            (if (call $path_open (i32.const 3) (i32.const 0) (i32.const 512) (i32.const {len})
                  (i32.const {oflags}) (i64.const {rights}) (i64.const 0) (i32.const 0) (i32.const 272))
              (then (return)))
            i32.const 256  i32.const 0    i32.store
            i32.const 260  i32.const 128  i32.store
            (drop (call $fd_read (i32.load (i32.const 272)) (i32.const 256) (i32.const 1) (i32.const 264)))
            i32.const 260  i32.const 264 i32.load  i32.store
            (drop (call $fd_write (i32.const 1) (i32.const 256) (i32.const 1) (i32.const 268)))))"#,
        len = path.len(),
        rights = if oflags == 0 { 2 } else { 66 }
    )
}

fn call_bound_verdict(runner: &PluginRunner, bound: &BoundDir) -> PluginOutcome {
    runner
        .call_bound(&serde_json::json!({}), &PluginBudget::default(), bound)
        .expect("the call runs at the VM level")
}

#[test]
fn call_bound_reads_inside_and_never_writes_for_a_read_plugin() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.json"), r#"{"read":true}"#).unwrap();
    let bound = BoundDir::new(dir.path(), None).unwrap();
    let bind = |access| crate::pack::PluginDirBinding {
        input_field: "root".to_owned(),
        original_field: None,
        description: "a directory".to_owned(),
        access,
    };
    let runner = |wat: &str| {
        let (mut plugin, afb) = build_plugin_pack("bound", wat, None);
        plugin.bind_dir = Some(bind(crate::pack::BindAccess::Read));
        PluginRunner::compile(&afb, &plugin).unwrap()
    };
    let read = call_bound_verdict(&runner(&open_and_copy_wat("a.json", 0)), &bound);
    assert_eq!(read.output, serde_json::json!({"read": true}));
    let create = call_bound_verdict(&runner(&open_and_copy_wat("new.json", 1)), &bound);
    assert_eq!(create.verdict, PluginVerdict::BadOutput);
    assert!(!dir.path().join("new.json").exists());
    let outside = runner(&open_and_copy_wat("../a.json", 0));
    assert_eq!(
        call_bound_verdict(&outside, &bound).verdict,
        PluginVerdict::BadOutput
    );
}
