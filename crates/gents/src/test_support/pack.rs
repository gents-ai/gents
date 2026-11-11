#[cfg(test)]
pub(crate) fn fixture_pack_copy(
    name: &str,
    plugin_output: &serde_json::Value,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs")
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
