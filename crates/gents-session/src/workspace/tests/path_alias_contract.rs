use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

fn prefixes(paths: &[String]) -> Vec<(String, String)> {
    paths
        .iter()
        .flat_map(|path| {
            let mut prefix = String::new();
            path.split('/').map(move |component| {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                (
                    prefix.clone(),
                    super::super::path_capability::alias_key(&prefix),
                )
            })
        })
        .collect()
}

fn index_tree(repo: &Path, paths: &[String], changed: &[String], old: &str, new: &str) -> String {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index");
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .args(["-c", "core.ignorecase=false"])
            .current_dir(repo)
            .env("GIT_INDEX_FILE", &index)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    run(&["read-tree", "--empty"]);
    for path in paths {
        let blob = if changed.contains(path) { new } else { old };
        run(&["update-index", "--add", "--cacheinfo", "100644", blob, path]);
    }
    run(&["write-tree"])
}

#[test]
fn generated_workspace_path_alias_cases_drive_real_git_delta() {
    gents_lean_vocab::families::workspace_path_alias_admission::replay(|case| {
        let changed = case.changed_paths.clone();
        for (leaf_paths, modeled_prefixes) in [
            (&case.changed_paths, &case.changed_prefixes),
            (&case.base_paths, &case.base_prefixes),
            (&case.tree_paths, &case.tree_prefixes),
        ] {
            let actual: Vec<Vec<String>> = prefixes(leaf_paths)
                .into_iter()
                .map(|(path, key)| vec![path, key])
                .collect();
            assert_eq!(&actual, modeled_prefixes, "{}", case.name);
        }
        let fx = Fixture::new();
        let old = git(&fx.repo, &["rev-parse", "HEAD:README.md"]);
        let mut command = Command::new("git")
            .current_dir(&fx.repo)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        command
            .stdin
            .take()
            .unwrap()
            .write_all(b"owned delta\n")
            .unwrap();
        let output = command.wait_with_output().unwrap();
        assert!(output.status.success());
        let new = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        let base = index_tree(&fx.repo, &case.base_paths, &[], &old, &new);
        let tree = index_tree(&fx.repo, &case.tree_paths, &changed, &old, &new);
        let cap = WorkspacePathCapability::exact_paths(changed).unwrap();
        super::super::adapter::validate_tree_delta(&fx.repo, &base, &tree, &cap).is_ok()
    });
}

#[test]
fn unchanged_non_utf8_git_entry_does_not_widen_changed_path_admission() {
    let mut fx = Fixture::new();
    fx.commit("other.txt", "different bytes\n");
    let old = git(&fx.repo, &["rev-parse", "HEAD:README.md"]);
    let other = git(&fx.repo, &["rev-parse", "HEAD:other.txt"]);
    let tree = |owned: &str, opaque: &str| {
        let mut bytes = format!("100644 blob {owned}\towned.rs\0").into_bytes();
        bytes.extend_from_slice(format!("100644 blob {opaque}\t").as_bytes());
        bytes.extend_from_slice(&[0xff, 0]);
        let mut child = Command::new("git")
            .current_dir(&fx.repo)
            .args(["mktree", "-z"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let base = tree(&old, &old);
    let cap = WorkspacePathCapability::exact_paths(vec!["owned.rs".into()]).unwrap();
    assert!(
        super::super::adapter::validate_tree_delta(&fx.repo, &base, &tree(&other, &old), &cap)
            .is_ok()
    );
    let error =
        super::super::adapter::validate_tree_delta(&fx.repo, &base, &tree(&other, &other), &cap)
            .unwrap_err();
    assert!(
        error.to_string().contains("non-UTF-8 Git path"),
        "{error:#}"
    );
}

#[test]
fn opaque_git_basename_keeps_parent_alias_fence() {
    let fx = Fixture::new();
    let blob = git(&fx.repo, &["rev-parse", "HEAD:README.md"]);
    let make_tree = |bytes: &[u8]| {
        let mut child = Command::new("git")
            .current_dir(&fx.repo)
            .args(["mktree", "-z"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let mut opaque = format!("100644 blob {blob}\t").into_bytes();
    opaque.extend_from_slice(&[0xff, 0]);
    let opaque_subtree = make_tree(&opaque);
    let owned_subtree = make_tree(format!("100644 blob {blob}\towned.rs\0").as_bytes());
    let base = make_tree(format!("040000 tree {opaque_subtree}\tSrc\0").as_bytes());
    let tree = make_tree(
        format!(
            "040000 tree {opaque_subtree}\tSrc\0\
        040000 tree {owned_subtree}\tsrc\0"
        )
        .as_bytes(),
    );
    let cap = WorkspacePathCapability::exact_paths(vec!["src/owned.rs".into()]).unwrap();
    let error =
        super::super::adapter::validate_tree_delta(&fx.repo, &base, &tree, &cap).unwrap_err();
    assert!(
        error.to_string().contains("component aliases conflict"),
        "{error:#}"
    );
}
