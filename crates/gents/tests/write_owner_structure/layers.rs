use super::support::{package_name, repo_root, toml_file};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Workspace crates by layer, bottom to top. A crate names only crates in layers
/// below its own in `[dependencies]`, `[build-dependencies]` and
/// `[dev-dependencies]`, so Cargo itself rejects an upward or peer reference in
/// source; this test fixes the direction the manifests may declare. Two crates
/// in one layer are peers and name neither. The Lean decoders are test-only
/// leaves that a lower crate may dev-depend on although they name `gents-protocol`.
const CRATE_LAYERS: &[&[&str]] = &[
    &["gents-schemas"],
    &["gents-protocol"],
    &["gents-lean-vocab"],
    &["gents-loop"],
    &["gents-store"],
    &["gents-documents"],
    &["gents-inference", "gents-p2p"],
    &["gents-pack"],
    &["gents-session"],
    &["gents-tools"],
    &["gents-toolcalls"],
    &["gents-automation"],
    &["gents-observe"],
    &["gents"],
];

const TEST_ONLY_DECODERS: &[&str] = &["gents-lean-vocab", "gents-lean-contract"];

/// Crates that operate or expose the runtime; no layered crate may name them.
const CLIENT_CRATES: &[&str] = &[
    "gents-cli",
    "gents-eval",
    "gents-desktop-core",
    "gents-desktop",
    "gents-desktop-bridge",
    "gents-client-host",
    "gents-media-host",
];

fn crate_layer(name: &str) -> Option<usize> {
    CRATE_LAYERS.iter().position(|layer| layer.contains(&name))
}

/// Package names a manifest declares in the given dependency table kinds,
/// including target-specific tables.
fn declared_dependencies(
    manifest: &toml::Table,
    inherited: &toml::Table,
    kind: &str,
) -> BTreeSet<String> {
    let mut tables = vec![manifest.get(kind)];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().map(|target| target.get(kind)));
    }
    tables
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flat_map(|table| table.iter())
        .map(|(key, specification)| package_name(key, specification, inherited))
        .collect()
}

fn crate_dependency_violations(
    manifests: &BTreeMap<String, toml::Table>,
    inherited: &toml::Table,
) -> Vec<String> {
    let mut violations = Vec::new();
    for (name, manifest) in manifests {
        let Some(own) = crate_layer(name) else {
            continue;
        };
        for kind in ["dependencies", "build-dependencies", "dev-dependencies"] {
            for dependency in declared_dependencies(manifest, inherited, kind) {
                if dependency == *name {
                    continue;
                }
                if CLIENT_CRATES.contains(&dependency.as_str()) {
                    violations.push(format!("{name} [{kind}] names client crate {dependency}"));
                    continue;
                }
                if kind == "dev-dependencies" && TEST_ONLY_DECODERS.contains(&dependency.as_str()) {
                    continue;
                }
                if let Some(theirs) = crate_layer(&dependency) {
                    if theirs >= own {
                        violations.push(format!(
                            "{name} [{kind}] names {dependency}, which is not in a lower layer"
                        ));
                    }
                }
            }
        }
    }
    violations
}

#[test]
fn crate_dependencies_follow_the_layers() {
    let root = repo_root();
    let inherited = toml_file(&root.join("Cargo.toml"))["workspace"]
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut manifests = BTreeMap::new();
    for layer in CRATE_LAYERS {
        for name in *layer {
            let directory = if *name == "gents" { "gents" } else { name };
            manifests.insert(
                (*name).to_string(),
                toml_file(&root.join("crates").join(directory).join("Cargo.toml")),
            );
        }
    }
    let violations = crate_dependency_violations(&manifests, &inherited);
    assert!(
        violations.is_empty(),
        "crate layering rule failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn crate_layer_fence_flags_upward_peer_and_client_references() {
    let manifest = |dependencies: &str| -> toml::Table {
        format!("[package]\nname = \"x\"\n{dependencies}")
            .parse()
            .expect("parse snippet")
    };
    let inherited = toml::Table::new();
    let check = |name: &str, dependencies: &str| {
        crate_dependency_violations(
            &BTreeMap::from([(name.to_string(), manifest(dependencies))]),
            &inherited,
        )
        .len()
    };
    assert_eq!(
        check("gents-store", "[dependencies]\ngents-protocol = \"1\"\n"),
        0
    );
    assert_eq!(
        check("gents-store", "[dependencies]\ngents-documents = \"1\"\n"),
        1
    );
    assert_eq!(
        check("gents-inference", "[dependencies]\ngents-p2p = \"1\"\n"),
        1
    );
    assert_eq!(
        check(
            "gents-tools",
            "[dev-dependencies]\ngents-toolcalls = \"1\"\n"
        ),
        1
    );
    assert_eq!(
        check(
            "gents-store",
            "[dev-dependencies]\ngents-lean-vocab = \"1\"\n"
        ),
        0
    );
    assert_eq!(check("gents", "[dev-dependencies]\ngents = \"1\"\n"), 0);
    assert_eq!(
        check("gents-observe", "[dependencies]\ngents-cli = \"1\"\n"),
        1
    );
    assert_eq!(
        check(
            "gents-session",
            "[target.'cfg(unix)'.dependencies]\ngents-tools = \"1\"\n"
        ),
        1
    );
}
