use super::*;
use crate::pack::{write_home_install, HomePackInstall, InstalledPackPlugin, PackKind};

fn dev_plugin(home: &Path, id: &str, source: &str, css: Option<&str>) {
    let dir = dev_root(home).join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(DEV_ENTRY), source).unwrap();
    if let Some(css) = css {
        std::fs::write(dir.join(DEV_CSS), css).unwrap();
    }
}

fn ux(name: &str, entry: Option<&str>, plugin: Option<&str>) -> PackUxPlugin {
    PackUxPlugin {
        name: name.to_owned(),
        description: format!("{name} description"),
        entry: entry.map(str::to_owned),
        css: None,
        plugin: plugin.map(str::to_owned),
        contributes: UxContributions {
            areas: vec!["nav".to_owned()],
            directives: vec![],
        },
        default_enabled: None,
        data: vec![],
    }
}

fn install_record(
    home: &Path,
    coordinate: &str,
    ux: Vec<PackUxPlugin>,
    plugins: Vec<InstalledPackPlugin>,
) -> PathBuf {
    let assets = PathBuf::from("packs/.materialized")
        .join(coordinate)
        .join("deadbeef");
    let dir = home.join(&assets);
    std::fs::create_dir_all(&dir).unwrap();
    write_home_install(
        home,
        &HomePackInstall {
            coordinate: coordinate.to_owned(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
            kind: PackKind::Plugins,
            assets: assets.to_string_lossy().into_owned(),
            plugins,
            dependency_assets: Vec::new(),
            installed_via: None,
            source: None,
            source_history: Vec::new(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            ux,
            created_collections: Vec::new(),
        },
    )
    .unwrap();
    dir
}

#[test]
fn lists_dev_plugins_by_folder_and_skips_folders_without_an_entry() {
    let home = tempfile::tempdir().unwrap();
    dev_plugin(home.path(), "zeta", "export default {}", None);
    dev_plugin(home.path(), "alpha", "export default {}", Some("a{}"));
    std::fs::create_dir_all(dev_root(home.path()).join("empty")).unwrap();
    std::fs::create_dir_all(dev_root(home.path()).join("Not-Snake")).unwrap();
    std::fs::write(dev_root(home.path()).join("Not-Snake").join(DEV_ENTRY), "x").unwrap();

    let listed = list(home.path()).unwrap();
    assert_eq!(
        listed.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "zeta"]
    );
    assert!(listed
        .iter()
        .all(|l| l.door == UxDoor::Dev && l.producer == UxProducer::File));
    assert!(
        listed.iter().all(|l| l.contributes.is_none()),
        "a dev plugin declares nothing"
    );
}

#[test]
fn an_absent_dev_root_lists_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert!(list(home.path()).unwrap().is_empty());
}

#[test]
fn lists_pack_plugins_with_their_declaration_and_namespaced_id() {
    let home = tempfile::tempdir().unwrap();
    install_record(
        home.path(),
        "acme/board",
        vec![ux("board", Some("ux/board/plugin.js"), None)],
        vec![],
    );
    let listed = list(home.path()).unwrap();
    assert_eq!(listed.len(), 1);
    let l = &listed[0];
    assert_eq!(l.id, "acme/board/board");
    assert_eq!(l.door, UxDoor::Pack);
    assert_eq!(l.producer, UxProducer::File);
    assert_eq!(l.pack.as_deref(), Some("acme/board"));
    assert_eq!(l.contributes.as_ref().unwrap().areas, vec!["nav"]);
    assert!(l.file.ends_with("ux/board/plugin.js"));
    assert!(l.data.is_empty(), "nothing declared, nothing listed");
}

#[test]
fn a_pack_plugin_lists_its_declared_data_surfaces_and_a_dev_plugin_none() {
    let home = tempfile::tempdir().unwrap();
    let mut plugin = ux("board", Some("ux/board/plugin.js"), None);
    plugin.data = vec![UxDataSurface {
        surface: "board".into(),
        tools: vec!["list_tasks".into(), "add_task".into()],
    }];
    install_record(home.path(), "acme/board", vec![plugin.clone()], vec![]);
    dev_plugin(home.path(), "loose", "export default {}", None);

    let listed = list(home.path()).unwrap();
    let loose = listed.iter().find(|l| l.id == "loose").unwrap();
    assert!(loose.data.is_empty());
    let board = find(home.path(), "acme/board/board").unwrap();
    assert_eq!(board.data, plugin.data);
    let json = serde_json::to_value(&board).unwrap();
    assert_eq!(
        json["data"],
        serde_json::json!([{ "surface": "board", "tools": ["list_tasks", "add_task"] }])
    );
    let json = serde_json::to_value(loose).unwrap();
    assert_eq!(json["data"], serde_json::json!([]));
}

#[test]
fn an_afb_produced_plugin_points_at_the_installed_artifact() {
    let home = tempfile::tempdir().unwrap();
    let digest = "c".repeat(64);
    install_record(
        home.path(),
        "acme/gen",
        vec![ux("report", None, Some("report_ui"))],
        vec![InstalledPackPlugin {
            name: "report_ui".into(),
            digest: format!("sha256:{digest}"),
        }],
    );
    let listed = list(home.path()).unwrap();
    assert_eq!(listed[0].producer, UxProducer::Afb);
    assert!(listed[0].file.ends_with(&format!("{digest}.afb")));
}

#[test]
fn a_pack_record_naming_an_uninstalled_producer_is_skipped_not_fatal() {
    let home = tempfile::tempdir().unwrap();
    install_record(
        home.path(),
        "acme/broken",
        vec![ux("x", None, Some("missing"))],
        vec![],
    );
    dev_plugin(home.path(), "still_here", "export default {}", None);
    let listed = list(home.path()).unwrap();
    assert_eq!(
        listed.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        vec!["still_here"]
    );
}

#[tokio::test]
async fn resolves_a_dev_module_with_the_css_beside_it() {
    let home = tempfile::tempdir().unwrap();
    dev_plugin(
        home.path(),
        "styled",
        "export default { id: 'styled' }",
        Some(".a{color:red}"),
    );
    let listing = find(home.path(), "styled").unwrap();
    let module = resolve_module(home.path(), &listing, |_, _| async { unreachable!() })
        .await
        .unwrap();
    assert_eq!(module.source, "export default { id: 'styled' }");
    assert_eq!(module.css.as_deref(), Some(".a{color:red}"));
    assert!(module.digest.starts_with("sha256:"));
}

#[tokio::test]
async fn resolves_a_pack_file_module_and_its_declared_css() {
    let home = tempfile::tempdir().unwrap();
    let mut plugin = ux("board", Some("ux/board/plugin.js"), None);
    plugin.css = Some("ux/board/plugin.css".into());
    let dir = install_record(home.path(), "acme/board", vec![plugin], vec![]);
    std::fs::create_dir_all(dir.join("ux/board")).unwrap();
    std::fs::write(
        dir.join("ux/board/plugin.js"),
        "export default { id: 'board' }",
    )
    .unwrap();
    std::fs::write(dir.join("ux/board/plugin.css"), ".b{}").unwrap();
    let listing = find(home.path(), "acme/board/board").unwrap();
    let module = resolve_module(home.path(), &listing, |_, _| async { unreachable!() })
        .await
        .unwrap();
    assert_eq!(module.source, "export default { id: 'board' }");
    assert_eq!(module.css.as_deref(), Some(".b{}"));
}

#[tokio::test]
async fn resolves_an_afb_produced_module_through_the_supplied_runner() {
    let home = tempfile::tempdir().unwrap();
    install_record(
        home.path(),
        "acme/gen",
        vec![ux("report", None, Some("report_ui"))],
        vec![InstalledPackPlugin {
            name: "report_ui".into(),
            digest: format!("sha256:{}", "c".repeat(64)),
        }],
    );
    let listing = find(home.path(), "acme/gen/report").unwrap();
    let module = resolve_module(home.path(), &listing, |namespace, plugin| async move {
        assert_eq!(namespace, "acme");
        assert_eq!(plugin, "report_ui");
        Ok(serde_json::json!({ "module": "export default { id: 'report' }", "css": ".r{}" }))
    })
    .await
    .unwrap();
    assert_eq!(module.source, "export default { id: 'report' }");
    assert_eq!(module.css.as_deref(), Some(".r{}"));
}

#[test]
fn a_producer_that_prints_the_wrong_shape_is_refused_by_name() {
    let error = module_from_producer_output(&serde_json::json!({ "code": "x" })).unwrap_err();
    assert!(format!("{error:#}").contains("{\"module\""), "{error:#}");
    let error = module_from_producer_output(&serde_json::json!("just a string")).unwrap_err();
    assert!(
        format!("{error:#}").contains("one JSON object"),
        "{error:#}"
    );
}

#[test]
fn an_oversize_module_is_refused() {
    let big = "x".repeat(MAX_MODULE_BYTES + 1);
    let error = module_from_producer_output(&serde_json::json!({ "module": big })).unwrap_err();
    assert!(format!("{error:#}").contains("limit"), "{error:#}");
}

#[test]
fn lint_pack_dir_reports_per_plugin_and_skips_afb_produced_ones() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("ux/good")).unwrap();
    std::fs::create_dir_all(dir.path().join("ux/bad")).unwrap();
    std::fs::write(dir.path().join("ux/good/plugin.js"), "export default {}").unwrap();
    std::fs::write(dir.path().join("ux/bad/plugin.js"), "eval('1')").unwrap();
    let findings = lint_pack_dir(
        dir.path(),
        &[
            ux("good", Some("ux/good/plugin.js"), None),
            ux("bad", Some("ux/bad/plugin.js"), None),
            ux("gen", None, Some("gen")),
        ],
    )
    .unwrap();
    assert_eq!(findings.keys().collect::<Vec<_>>(), vec!["bad", "good"]);
    assert!(findings["good"].is_empty());
    assert_eq!(findings["bad"][0].rule, "dynamic code");
}

#[test]
fn lint_pack_dir_reads_the_declared_stylesheet() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("ux/styled")).unwrap();
    std::fs::write(dir.path().join("ux/styled/plugin.js"), "export default {}").unwrap();
    std::fs::write(
        dir.path().join("ux/styled/plugin.css"),
        ".a { color: var(--background) }",
    )
    .unwrap();
    let mut styled = ux("styled", Some("ux/styled/plugin.js"), None);
    styled.css = Some("ux/styled/plugin.css".into());
    let findings = lint_pack_dir(dir.path(), &[styled]).unwrap();
    assert_eq!(findings["styled"][0].rule, "undefined css token");
    assert!(findings["styled"][0].stylesheet);
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn plugin_files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            plugin_files_under(&path, found);
        } else if path.file_name().is_some_and(|name| name == DEV_ENTRY) {
            found.push(path);
        }
    }
}

/// Every plugin this repository ships, scaffold output aside (the scaffold
/// tests run it through `pack check`), has to pass the lint it teaches.
#[test]
fn every_shipped_plugin_passes_the_lint() {
    let root = repo_root();
    let mut plugins = vec![
        root.join("crates/gents/tests/fixtures/packs/ux_fixture/ux/badge/plugin.js"),
        root.join("apps/gents-desktop/src/ui/ux-plugins/hello/plugin.js"),
    ];
    plugin_files_under(&root.join("examples/ux-plugins"), &mut plugins);
    assert!(
        plugins
            .iter()
            .any(|p| p.ends_with("ux/dashboard/plugin.js")),
        "the walk found the examples: {plugins:?}"
    );
    for plugin in plugins {
        let source = std::fs::read_to_string(&plugin).unwrap();
        let css = std::fs::read_to_string(plugin.with_file_name(DEV_CSS)).ok();
        let findings = lint::lint_plugin(&source, css.as_deref());
        assert!(
            findings.is_empty(),
            "{}:\n{}",
            plugin.display(),
            lint::describe(&findings)
        );
    }
}

#[tokio::test]
async fn the_dashboard_example_is_a_checked_pack() {
    let dir = repo_root().join("examples/ux-plugins/board_dashboard");
    let check = crate::pack::authoring::check_dir(&dir).await;
    assert!(check.problems.is_empty(), "{:#?}", check.problems);
}

#[test]
fn the_sdk_reference_carries_the_skeleton_and_every_token() {
    assert!(reference::SDK_REFERENCE.contains("export default {"));
    assert!(reference::SDK_REFERENCE.contains("ctx.registerMany"));
    assert!(reference::SDK_REFERENCE.contains("useQuery(ctx, surface, tool, options?)"));
    let tokens: Vec<&str> = reference::frame_tokens().collect();
    assert!(tokens.contains(&"--bg"));
    for token in tokens {
        assert!(reference::SDK_REFERENCE.contains(token), "{token}");
    }
}

/// The index is what every page author reads; a section is what one block
/// costs. Ceilings sit a little above the generated sizes.
#[test]
fn the_sdk_reference_is_an_index_and_one_section_per_export() {
    let index = reference::sdk_index();
    assert!(index.contains("export default {") && index.contains("ctx.registerMany"));
    assert!(
        index.contains("`gents config pack ux lint` refuses raw hex"),
        "{index}"
    );
    assert!(
        !index.contains("<!--") && !index.contains("\n### "),
        "{index}"
    );
    assert!(index.len() <= 4_800, "the index is {} chars", index.len());
    let sections: Vec<_> = reference::sdk_sections().collect();
    assert!(sections.len() > 50, "{}", sections.len());
    for (name, section) in &sections {
        assert_eq!(reference::sdk_section(name), Some(*section));
        assert!(
            !section.is_empty() && !section.contains("### "),
            "{section}"
        );
        if *name == "tokens" {
            assert!(section.len() <= 2_800, "tokens is {} chars", section.len());
            continue;
        }
        assert!(
            index.contains(&format!("`{name}`")) || index.contains(&format!("`{name}(")),
            "{name} is missing from the index"
        );
        assert!(section.len() <= 600, "{name} is {} chars", section.len());
    }
    assert!(
        reference::sdk_section("table").is_some(),
        "names ignore case"
    );
    assert!(reference::sdk_section("Chart").is_none());
}
