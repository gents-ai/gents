#[cfg(any(test, feature = "test-fixtures"))]
pub fn fixture_pack_copy(
    name: &str,
    plugin_output: &serde_json::Value,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/packs")
        .join(name);
    let manifest: crate::pack::PackManifest = serde_json::from_slice(
        &std::fs::read(source.join("manifest.json"))
            .unwrap_or_else(|error| panic!("fixture {name:?} manifest: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture {name:?} manifest: {error}"));
    let artifacts: std::collections::BTreeSet<&str> = manifest
        .metadata
        .plugins
        .iter()
        .map(|plugin| plugin.artifact.as_str())
        .collect();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(name);
    for path in crate::pack::declared_paths(&manifest) {
        let target = root.join(&path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        if artifacts.contains(path.as_str()) {
            let wat = crate::plugin::test_fixtures::constant_output_wat(
                &serde_json::to_vec(plugin_output).expect("encode plugin output"),
            );
            std::fs::write(
                &target,
                crate::plugin::test_fixtures::build_plugin_afb(&wat),
            )
            .expect("write artifact");
        } else {
            std::fs::copy(source.join(&path), &target)
                .unwrap_or_else(|error| panic!("fixture {name:?} file {path:?}: {error}"));
        }
    }
    (dir, root)
}

#[cfg(test)]
pub fn fixture_pack_names() -> Vec<String> {
    const KNOWN: [&str; 8] = [
        "assets_fixture",
        "documents_fixture",
        "slot_fixture",
        "dependent_fixture",
        "dependee_fixture",
        "bind_plugin_fixture",
        "job_plugin_fixture",
        "plugin_documents_fixture",
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/tests/fixtures/packs");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("reading {}: {error}", root.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("reading an entry of {}: {error}", root.display()))
            .path();
        if !path.is_dir() {
            continue;
        }
        let manifest = path.join("manifest.json");
        let bytes = std::fs::read(&manifest)
            .unwrap_or_else(|error| panic!("reading {}: {error}", manifest.display()));
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", manifest.display()));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} is not a UTF-8 name", path.display()));
        names.push(name.to_owned());
    }
    names.sort();
    for known in KNOWN {
        assert!(
            names.iter().any(|name| name == known),
            "fixture {known} is missing from {}",
            root.display()
        );
    }
    names
}
