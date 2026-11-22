use super::support::{
    all_rust_sources, package_name, repo_root, toml_file, Workspace, GENTS_TEST_FIXTURES,
    NATIVE_E2E, SERVICE_ISOLATION, TEST_FEATURES,
};
use std::collections::BTreeSet;

/// `cfg_test` treats `test-fixtures` items as test code, which holds only
/// while no production build can enable the feature.
fn test_fixtures_leaks(manifest: &toml::Table, inherited: &toml::Table) -> Vec<String> {
    let mut tables = vec![(
        "workspace.dependencies",
        manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies")),
    )];
    for kind in ["dependencies", "build-dependencies"] {
        tables.push((kind, manifest.get(kind)));
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            tables.extend(targets.values().map(|target| (kind, target.get(kind))));
        }
    }
    let mut leaks = Vec::new();
    for (kind, table) in tables {
        for (key, specification) in table.and_then(toml::Value::as_table).into_iter().flatten() {
            if package_name(key, specification, inherited) == "gents"
                && specification
                    .get("features")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|features| {
                        features
                            .iter()
                            .any(|feature| feature.as_str() == Some(GENTS_TEST_FIXTURES))
                    })
            {
                leaks.push(format!("[{kind}] {key}"));
            }
        }
    }
    let own = manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        == Some("gents");
    for (feature, enabled) in manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        if enabled
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .any(|entry| {
                matches!(entry, "gents/test-fixtures" | "gents?/test-fixtures")
                    || (own && entry == GENTS_TEST_FIXTURES)
            })
        {
            leaks.push(format!("[features] {feature}"));
        }
    }
    leaks
}

#[test]
fn gents_test_fixtures_stay_dev_only() {
    let root = repo_root();
    let workspace = toml_file(&root.join("Cargo.toml"));
    let inherited = workspace["workspace"]
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut leaks = test_fixtures_leaks(&workspace, &inherited)
        .into_iter()
        .map(|site| format!("Cargo.toml {site}"))
        .collect::<Vec<_>>();
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let member = member.as_str().expect("member path");
        let manifest = toml_file(&root.join(member).join("Cargo.toml"));
        leaks.extend(
            test_fixtures_leaks(&manifest, &inherited)
                .into_iter()
                .map(|site| format!("{member}/Cargo.toml {site}")),
        );
    }
    assert!(
        leaks.is_empty(),
        "the gents `test-fixtures` feature is enabled only from [dev-dependencies]:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn test_fixtures_fence_flags_production_and_default_enablement() {
    let leaks = |manifest: &str| {
        let manifest: toml::Table = manifest.parse().expect("parse manifest");
        let inherited = manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies"))
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        test_fixtures_leaks(&manifest, &inherited)
    };
    for (manifest, expected) in [
        (
            r#"[dependencies]
            gents = { path = "../gents", features = ["test-fixtures"] }"#,
            "[dependencies] gents",
        ),
        (
            r#"[build-dependencies]
            core = { package = "gents", path = "../gents", features = ["test-fixtures"] }"#,
            "[build-dependencies] core",
        ),
        (
            r#"[target.'cfg(unix)'.dependencies]
            gents = { path = "../gents", features = ["test-fixtures"] }"#,
            "[dependencies] gents",
        ),
        (
            r#"[workspace.dependencies]
            gents = { path = "crates/gents", features = ["test-fixtures"] }"#,
            "[workspace.dependencies] gents",
        ),
        (
            r#"[features]
            fixtures = ["gents/test-fixtures"]"#,
            "[features] fixtures",
        ),
        (
            r#"[package]
            name = "gents"
            [features]
            default = ["test-fixtures"]
            test-fixtures = []"#,
            "[features] default",
        ),
    ] {
        assert_eq!(leaks(manifest), [expected], "{manifest}");
    }
    for manifest in [
        r#"[dev-dependencies]
        gents = { path = "../gents", features = ["test-fixtures"] }"#,
        r#"[dependencies]
        gents = { path = "../gents", features = ["live-e2e"] }
        other = { path = "../other", features = ["test-fixtures"] }"#,
        r#"[package]
        name = "gents-desktop-core"
        [features]
        default = ["test-fixtures"]
        test-fixtures = []"#,
        r#"[package]
        name = "gents"
        [features]
        test-home-guard = []
        test-fixtures = ["test-home-guard"]"#,
    ] {
        assert!(leaks(manifest).is_empty(), "{manifest}");
    }
}

/// `cfg_test` treats `native-e2e` items as test code, which holds only while
/// no manifest enables the feature outside `[dev-dependencies]` except a
/// crate's own `native-e2e` feature forwarding it to a dependency.
fn native_e2e_leaks(manifest: &toml::Table) -> Vec<String> {
    let mut tables = Vec::new();
    for kind in ["dependencies", "build-dependencies"] {
        tables.push((kind, manifest.get(kind)));
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            tables.extend(targets.values().map(|target| (kind, target.get(kind))));
        }
    }
    let mut leaks = Vec::new();
    for (kind, table) in tables {
        for (key, specification) in table.and_then(toml::Value::as_table).into_iter().flatten() {
            if specification
                .get("features")
                .and_then(toml::Value::as_array)
                .is_some_and(|features| {
                    features
                        .iter()
                        .any(|feature| feature.as_str() == Some(NATIVE_E2E))
                })
            {
                leaks.push(format!("[{kind}] {key}"));
            }
        }
    }
    for (feature, enabled) in manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        if feature != NATIVE_E2E
            && enabled
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .any(|entry| entry.rsplit('/').next() == Some(NATIVE_E2E))
        {
            leaks.push(format!("[features] {feature}"));
        }
    }
    leaks
}

#[test]
fn native_e2e_stays_test_only() {
    let root = repo_root();
    let workspace = toml_file(&root.join("Cargo.toml"));
    let mut leaks = Vec::new();
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let member = member.as_str().expect("member path");
        let manifest = toml_file(&root.join(member).join("Cargo.toml"));
        leaks.extend(
            native_e2e_leaks(&manifest)
                .into_iter()
                .map(|site| format!("{member}/Cargo.toml {site}")),
        );
    }
    assert!(
        leaks.is_empty(),
        "the `native-e2e` feature is enabled only from the command line, [dev-dependencies] or a crate's own `native-e2e` feature:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn native_e2e_fence_flags_production_and_default_enablement() {
    let leaks = |manifest: &str| native_e2e_leaks(&manifest.parse().expect("parse manifest"));
    for (manifest, expected) in [
        (
            r#"[dependencies]
            bridge = { path = "../bridge", features = ["native-e2e"] }"#,
            "[dependencies] bridge",
        ),
        (
            r#"[target.'cfg(unix)'.dependencies]
            bridge = { path = "../bridge", features = ["native-e2e"] }"#,
            "[dependencies] bridge",
        ),
        (
            r#"[features]
            default = ["native-e2e"]
            native-e2e = []"#,
            "[features] default",
        ),
        (
            r#"[features]
            desktop = ["bridge?/native-e2e"]"#,
            "[features] desktop",
        ),
    ] {
        assert_eq!(leaks(manifest), [expected], "{manifest}");
    }
    for manifest in [
        r#"[dev-dependencies]
        bridge = { path = "../bridge", features = ["native-e2e"] }"#,
        r#"[features]
        native-e2e = ["tauri-host", "bridge/native-e2e"]"#,
        r#"[dependencies]
        bridge = { path = "../bridge", features = ["native-e2e-ui"] }"#,
    ] {
        assert!(leaks(manifest).is_empty(), "{manifest}");
    }
}

/// Sites that would enable the service isolation in a production build: a
/// normal, build or workspace dependency enabling it, or a feature other than
/// a test feature (`test-home-guard`, `test-fixtures`) that does.
fn service_isolation_leaks(workspace: &Workspace) -> Vec<String> {
    let mut leaks = Vec::new();
    for (name, (member, manifest)) in &workspace.members {
        for kind in [
            "dependencies",
            "build-dependencies",
            "workspace.dependencies",
        ] {
            for (key, package, features, _) in workspace.entries(manifest, kind) {
                if workspace.isolates(&package, &features) {
                    leaks.push(format!("{member}/Cargo.toml [{kind}] {key}"));
                }
            }
        }
        for feature in manifest
            .get("features")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flat_map(|table| table.keys())
        {
            let test_feature = TEST_FEATURES.contains(&feature.as_str());
            if !test_feature && workspace.isolates(name, std::slice::from_ref(feature)) {
                leaks.push(format!("{member}/Cargo.toml [features] {feature}"));
            }
        }
    }
    leaks
}

#[test]
fn service_isolation_stays_test_only() {
    let workspace = Workspace::load(&repo_root());
    assert!(
        workspace.isolates("gents-service", &["test-home-guard".to_owned()]),
        "the service isolation feature is gents-service's test-home-guard"
    );
    let leaks = service_isolation_leaks(&workspace);
    assert!(
        leaks.is_empty(),
        "a production build would never reach the service manager; enable gents-service's `test-home-guard` only from [dev-dependencies] or a test feature:\n{}",
        leaks.join("\n")
    );
}

/// Every package whose tests can reach the service manager, by building
/// against gents-service or by spawning the `gents` binary, builds it with
/// the isolation through its dev-dependencies, so that the `gents` its tests
/// spawn is cut off from the machine's service manager too.
#[test]
fn tests_reaching_the_service_manager_are_isolated() {
    let root = repo_root();
    let workspace = Workspace::load(&root);
    let mut unisolated = Vec::new();
    for (name, (member, manifest)) in &workspace.members {
        let mut sources = Vec::new();
        for directory in ["src", "tests", "benches", "examples"] {
            let directory = root.join(member).join(directory);
            if directory.is_dir() {
                all_rust_sources(&directory, &mut sources);
            }
        }
        let spawns_gents = sources.iter().any(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| text.contains("CARGO_BIN_EXE_gents\""))
        });
        if !spawns_gents && !workspace.reaches(name, SERVICE_ISOLATION.0) {
            continue;
        }
        let isolated = workspace
            .entries(manifest, "dev-dependencies")
            .iter()
            .any(|(_, package, features, _)| workspace.isolates(package, features));
        if !isolated {
            unisolated.push(format!(
                "{member}: its tests reach the service manager; add `{} = {{ workspace = true, features = [\"{}\"] }}` (or a test feature enabling it) to its [dev-dependencies]",
                SERVICE_ISOLATION.0, SERVICE_ISOLATION.1
            ));
        }
    }
    assert!(unisolated.is_empty(), "{}", unisolated.join("\n"));
}

fn snippet_workspace(manifests: &[(&str, &str)]) -> Workspace {
    Workspace {
        inherited: toml::Table::new(),
        members: manifests
            .iter()
            .map(|(name, manifest)| {
                let manifest: toml::Table = format!("[package]\nname = \"{name}\"\n{manifest}")
                    .parse()
                    .expect("parse manifest");
                ((*name).to_owned(), ((*name).to_owned(), manifest))
            })
            .collect(),
    }
}

#[test]
fn service_isolation_fence_flags_production_enablement() {
    let service = ("gents-service", "[features]\ntest-home-guard = []");
    let leaking = snippet_workspace(&[
        service,
        (
            "host",
            "[dependencies]\ngents-service = { path = \"x\" }\n[features]\nfixtures = [\"gents-service/test-home-guard\"]\ntest-fixtures = [\"gents-service/test-home-guard\"]",
        ),
        (
            "cli",
            "[dependencies]\nhost = { path = \"h\", features = [\"test-fixtures\"] }",
        ),
    ]);
    assert_eq!(
        service_isolation_leaks(&leaking),
        [
            "cli/Cargo.toml [dependencies] host",
            "host/Cargo.toml [features] fixtures"
        ]
    );
    let fenced = snippet_workspace(&[
        service,
        (
            "host",
            "[dependencies]\ngents-service = { path = \"x\" }\n[features]\ntest-fixtures = [\"gents-service/test-home-guard\"]",
        ),
        (
            "cli",
            "[dependencies]\nhost = { path = \"h\" }\n[dev-dependencies]\nhost = { path = \"h\", features = [\"test-fixtures\"] }",
        ),
    ]);
    assert!(service_isolation_leaks(&fenced).is_empty());
    assert!(fenced.isolates("host", &["test-fixtures".to_owned()]));
    assert!(fenced.reaches("cli", "gents-service"));
}

/// The members no production build compiles (`Workspace::test_only`), whose
/// sources `runtime_environment_reads_are_named_exceptions` skips. Pinned so that one
/// becoming reachable from a production build, or a new one becoming
/// test-only, is a visible change.
const TEST_ONLY_CRATES: &[&str] = &[
    "gents-callback-fixture-create-workspace",
    "gents-lean-contract",
    "gents-lean-vocab",
    "gents-workspace-hack",
];

#[test]
fn test_only_crates_stay_off_production_builds() {
    let workspace = Workspace::load(&repo_root());
    let production = workspace.production();
    let mut violations = TEST_ONLY_CRATES
        .iter()
        .filter_map(|name| match production.get(*name) {
            Some(path) => Some(format!(
                "{name} is compiled into a production build ({path}); name it only from [dev-dependencies] or a test feature"
            )),
            None if !workspace.members.contains_key(*name) => Some(format!(
                "{name} is not a workspace member; remove it from TEST_ONLY_CRATES"
            )),
            None => None,
        })
        .collect::<Vec<_>>();
    violations.extend(
        workspace
            .test_only()
            .into_iter()
            .filter(|name| !TEST_ONLY_CRATES.contains(&name.as_str()))
            .map(|name| {
                format!("{name}: no production build compiles it; name it in TEST_ONLY_CRATES")
            }),
    );
    assert!(
        violations.is_empty(),
        "test-only crates rule failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_only_fence_follows_production_dependencies_and_features() {
    const APP: &str = "[dependencies]\ncore = { path = \"c\" }\n[dev-dependencies]\ncore = { path = \"c\", features = [\"test-fixtures\"] }";
    const CORE: &str = "[dependencies]\ndecoder = { path = \"d\", optional = true }\n[features]\ndefault = []\ntest-fixtures = [\"dep:decoder\"]";
    let test_only = |app: &str, core: &str| {
        snippet_workspace(&[
            ("app", app),
            ("core", core),
            (
                "decoder",
                "[dependencies]\nhelper = { path = \"h\" }\n[dev-dependencies]\nfixture = { path = \"f\" }",
            ),
            ("helper", ""),
            ("fixture", ""),
        ])
        .test_only()
    };
    for app in [APP.to_owned(), format!("{APP}\napp = {{ path = \".\" }}")] {
        assert_eq!(
            test_only(&app, CORE),
            BTreeSet::from(["decoder", "fixture", "helper"].map(str::to_owned)),
            "{app}"
        );
    }
    let implicit = CORE.replace("dep:decoder", "decoder");
    for (app, core) in [
        (
            APP.to_owned(),
            format!("{CORE}\nlive = [\"test-fixtures\"]"),
        ),
        (APP.to_owned(), format!("{CORE}\nlive = [\"dep:decoder\"]")),
        (APP.to_owned(), implicit.clone()),
        (
            APP.replacen(
                "{ path = \"c\" }",
                "{ path = \"c\", features = [\"decoder\"] }",
                1,
            ),
            implicit,
        ),
        (APP.to_owned(), format!("{CORE}\nlive = [\"decoder/x\"]")),
        (
            APP.to_owned(),
            format!(
                "{CORE}\n[target.'cfg(unix)'.build-dependencies]\ndecoder = {{ path = \"d\" }}"
            ),
        ),
        (APP.to_owned(), CORE.replace(", optional = true", "")),
        (
            APP.replacen(
                "{ path = \"c\" }",
                "{ path = \"c\", features = [\"test-fixtures\"] }",
                1,
            ),
            CORE.to_owned(),
        ),
        (
            format!("{APP}\n[features]\nx = [\"core/test-fixtures\"]"),
            CORE.to_owned(),
        ),
    ] {
        assert_eq!(
            test_only(&app, &core),
            BTreeSet::from(["fixture".to_owned()]),
            "{app}\n{core}"
        );
    }
}
