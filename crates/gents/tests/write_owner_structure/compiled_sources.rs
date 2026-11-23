use super::support::repo_root;
use gents_lean_vocab::module_tree::{module_tree, workspace_members};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Rust files under a member's `src/` or `tests/` that no target compiles and
/// no compiled file reads as data; a directory with its own `Cargo.toml` is
/// another package, such as a fixture a test builds. The list only shrinks: wire each file into
/// its module tree or delete it.
#[rustfmt::skip]
const UNCOMPILED: &[&str] = &[
    "crates/gents-cli/src/desired_state/validate/storage.rs",
    "crates/gents-cli/tests/suites/cli_config_apply_order.rs",
    "crates/gents-documents/src/event_group_validation_tests.rs",
    "crates/gents-documents/src/projection_acp/tests.rs",
    "crates/gents-documents/src/schedule_cadence_tests.rs",
    "crates/gents-documents/src/task_validation/tests.rs",
    "crates/gents-tools/src/mcp/registry/tests.rs",
    "crates/gents/src/runtime/agent/retry/tests.rs",
];

fn rust_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            let package = path.join("Cargo.toml").is_file();
            if !package
                && path
                    .file_name()
                    .is_none_or(|name| name != "target" && name != "node_modules")
            {
                rust_files(&path, output);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

#[test]
fn every_rust_source_is_compiled() {
    let root = repo_root();
    let tree = module_tree(&root);
    let mut sources = Vec::new();
    for member in workspace_members(&root) {
        for directory in ["src", "tests"] {
            rust_files(&member.join(directory), &mut sources);
        }
    }
    let uncompiled: BTreeSet<String> = sources
        .into_iter()
        .filter(|path| !tree.compiled.contains(path) && !tree.data.contains(path))
        .map(|path| {
            path.strip_prefix(&root)
                .expect("source under the repository")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    let listed: BTreeSet<String> = UNCOMPILED.iter().map(|path| path.to_string()).collect();
    let added: Vec<_> = uncompiled.difference(&listed).collect();
    assert!(
        added.is_empty(),
        "no target compiles these Rust files, so their tests never run; declare each in its \
         parent's module tree or delete it: {added:?}"
    );
    let compiled: Vec<_> = listed.difference(&uncompiled).collect();
    assert!(
        compiled.is_empty(),
        "these files are compiled now or gone; delete them from UNCOMPILED: {compiled:?}"
    );
}
