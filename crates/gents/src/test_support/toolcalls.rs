#[cfg(test)]
use super::pack::fixture_pack_copy;

#[cfg(test)]
pub(crate) fn home_with_fixture_pack(
    name: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::plugin::executor::PluginExecutor>,
) {
    let (_guard, dir) = fixture_pack_copy(name, &serde_json::json!({}));
    home_with_pack_dir(&dir)
}

pub(crate) fn home_with_pack_dir(
    dir: &std::path::Path,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::plugin::executor::PluginExecutor>,
) {
    let (bytes, _) = crate::pack::archive::pack_dir(dir)
        .unwrap_or_else(|error| panic!("packing {}: {error:#}", dir.display()));
    let home = tempfile::tempdir().expect("home");
    crate::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap_or_else(|error| panic!("storing {}: {error:#}", dir.display()));
    let plugins = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_path_buf(),
    )));
    (home, plugins)
}

pub(crate) fn default_plugin_host() -> std::sync::Arc<dyn crate::tools::plugin_host::PluginHost> {
    std::sync::Arc::new(crate::plugin::executor::PluginExecutor::default())
}
