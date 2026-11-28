use super::support::{repo_root, Workspace};
use std::collections::BTreeSet;
use std::path::Path;

/// Integration-test targets that link on their own beside their crate's one
/// integration binary, each with the process-wide state that keeps it apart.
/// Every other integration test is a module of that binary: each extra target
/// relinks the crate's whole dependency graph.
#[rustfmt::skip]
const SEPARATE_TEST_TARGETS: &[(&str, &str, &str)] = &[
    ("gents", "e2e_triggers", "its scenarios serialise on the process-global embedded P2P transport"),
    ("gents-cli", "heap_profile", "installs a global allocator"),
    ("gents-cli", "port_allocation", "the Windows editor job builds and runs it without the CLI suite"),
    ("gents-cli", "plugin_build_sandbox", "sets GENTS_PLUGIN_BUILDER for the process"),
    ("gents-desktop-bridge", "connect_this_desktop", "sets HOME and the XDG directories for the process"),
    ("gents-session", "log_rate_filter", "installs the global tracing subscriber and drives its clock"),
];

/// Integration-test roots on disk: `tests/*.rs` and `tests/*/main.rs`.
fn integration_test_roots(tests: &Path) -> Vec<(String, String)> {
    let mut roots = Vec::new();
    for entry in std::fs::read_dir(tests).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            roots.push((name.trim_end_matches(".rs").to_owned(), name));
        } else if path.join("main.rs").is_file() {
            roots.push((name.clone(), format!("{name}/main.rs")));
        }
    }
    roots.sort();
    roots
}

/// Test targets a member links and the `tests/` files no target compiles.
fn integration_test_layout(
    root: &Path,
    member: &str,
    manifest: &toml::Table,
) -> (Vec<String>, Vec<String>) {
    let tests = root.join(member).join("tests");
    let declared: Vec<(String, String)> = manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .map(|target| {
            let name = target["name"].as_str().expect("[[test]] name").to_owned();
            let path = target
                .get("path")
                .and_then(toml::Value::as_str)
                .map(|path| path.trim_start_matches("tests/").to_owned())
                .unwrap_or_else(|| format!("{name}.rs"));
            (name, path)
        })
        .collect();
    let discovered = manifest
        .get("package")
        .and_then(|package| package.get("autotests"))
        .and_then(toml::Value::as_bool)
        != Some(false);
    let mut targets: BTreeSet<String> = declared.iter().map(|(name, _)| name.clone()).collect();
    let mut compiled: BTreeSet<String> = declared.iter().map(|(_, path)| path.clone()).collect();
    let module_path = regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).unwrap();
    for (_, path) in &declared {
        if let Ok(source) = std::fs::read_to_string(tests.join(path)) {
            compiled.extend(
                module_path
                    .captures_iter(&source)
                    .map(|capture| capture[1].to_owned()),
            );
        }
    }
    let mut uncompiled = Vec::new();
    for (name, path) in integration_test_roots(&tests) {
        if discovered {
            targets.insert(name);
        } else if !compiled.contains(&path) {
            uncompiled.push(format!("{member}/tests/{path}"));
        }
    }
    (targets.into_iter().collect(), uncompiled)
}

#[test]
fn integration_tests_share_one_binary_per_crate() {
    let root = repo_root();
    let workspace = Workspace::load(&root);
    let mut findings = Vec::new();
    let mut separate_seen = BTreeSet::new();
    for (name, (member, manifest)) in &workspace.members {
        let (targets, uncompiled) = integration_test_layout(&root, member, manifest);
        for file in uncompiled {
            findings.push(format!(
                "{file} is not compiled: with `autotests = false` add it as a `#[path]` module of tests/integration.rs"
            ));
        }
        let mut shared = Vec::new();
        for target in targets {
            match SEPARATE_TEST_TARGETS
                .iter()
                .find(|(package, separate, _)| package == name && *separate == target)
            {
                Some(entry) => {
                    separate_seen.insert(*entry);
                }
                None => shared.push(target),
            }
        }
        if shared.len() > 1 {
            findings.push(format!(
                "{member}: integration-test targets {shared:?} each relink the crate's dependency graph; make them modules of one tests/integration.rs"
            ));
        }
    }
    for entry in SEPARATE_TEST_TARGETS {
        let (package, target, reason) = entry;
        if !separate_seen.contains(entry) {
            findings.push(format!(
                "({package}, {target}) no longer exists; remove it from SEPARATE_TEST_TARGETS"
            ));
        }
        if reason.trim().is_empty() {
            findings.push(format!(
                "({package}, {target}) names no reason to link on its own"
            ));
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

#[test]
fn integration_test_layout_flags_extra_and_uncompiled_targets() {
    let root = tempfile::tempdir().unwrap();
    let tests = root.path().join("member/tests");
    std::fs::create_dir_all(tests.join("suite")).unwrap();
    for file in ["integration.rs", "folded.rs", "stray.rs", "suite/main.rs"] {
        std::fs::write(tests.join(file), "").unwrap();
    }
    std::fs::write(
        tests.join("integration.rs"),
        "#[path = \"folded.rs\"]\nmod folded;\n",
    )
    .unwrap();
    let manifest = |text: &str| text.parse::<toml::Table>().unwrap();
    let discovered = integration_test_layout(
        root.path(),
        "member",
        &manifest("[package]\nname = \"m\"\n"),
    );
    assert_eq!(discovered.0, ["folded", "integration", "stray", "suite"]);
    assert!(discovered.1.is_empty());
    let folded = integration_test_layout(
        root.path(),
        "member",
        &manifest(
            "[package]\nname = \"m\"\nautotests = false\n\n[[test]]\nname = \"integration\"\npath = \"tests/integration.rs\"\n",
        ),
    );
    assert_eq!(folded.0, ["integration"]);
    assert_eq!(
        folded.1,
        ["member/tests/stray.rs", "member/tests/suite/main.rs"]
    );
}
