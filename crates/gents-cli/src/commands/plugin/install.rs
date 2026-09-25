//! `gents plugin install`: fetch a plugin's pack from the registry and
//! install the plugins it carries. The pack is downloaded, verified and
//! stored exactly as `gents pack install` does; only its plugins are
//! installed, each recorded so `gents plugin run` and `gents plugin list`
//! find it by name.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::args::PluginInstallArgs;
use crate::commands::pack::registry::{fetch_pack, resolve_registry_url, RegistryClient};

pub(crate) async fn install(args: PluginInstallArgs) -> Result<()> {
    let (namespace, name) = crate::commands::pack::split_namespace(&args.name);
    let base_url = resolve_registry_url(args.registry.as_deref());
    let client = RegistryClient::new(base_url.clone());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());

    let pack = fetch_pack(
        &client,
        Some(&home),
        namespace,
        name,
        args.version.as_deref(),
    )
    .await
    .with_context(|| format!("fetching {namespace}/{name} from {base_url}"))?;
    let manifest = pack.archive.manifest();
    anyhow::ensure!(
        !manifest.metadata.plugins.is_empty(),
        "{namespace}/{name} carries no plugins; install the whole pack with gents pack install"
    );
    let installed = crate::commands::pack::install_pack_plugins(
        &home,
        manifest,
        |path| pack.archive.asset(path),
        args.grant_authority,
    )?;

    crate::print_json(&json!({
        "pack": format!("{namespace}/{name}"),
        "version": manifest.version,
        "plugins": installed,
        "home": home,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pack::registry::tests::serve_fake_pack;
    use std::sync::atomic::Ordering;

    use super::super::store;

    /// A real compiled plugin, wrapped by the same code `gents plugin
    /// publish` uses, with the digest of the pack's bytes a registry
    /// advertises.
    fn echo_pack() -> (Vec<u8>, String) {
        let afb_bytes = crate::commands::plugin::testing::build_plugin_afb(
            "echo",
            b"fn main() { println!(\"ok\"); }",
        );
        let afb = afterburner_cloud::Afb::from_bytes(&afb_bytes).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (bytes, _) = super::super::plugin_pack(dir.path(), &afb, &afb_bytes).unwrap();
        let digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&bytes))
        };
        (bytes, digest)
    }

    fn args(name: &str, registry: String, home: &std::path::Path) -> PluginInstallArgs {
        PluginInstallArgs {
            grant_authority: false,
            name: name.to_owned(),
            version: None,
            registry: Some(registry),
            home: Some(home.to_owned()),
        }
    }

    #[tokio::test]
    async fn a_published_plugin_installs_from_its_pack_and_runs_by_name() {
        let (bytes, digest) = echo_pack();
        let archive = gents::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        let (namespace, version) = (
            archive.manifest().metadata.namespace.clone(),
            archive.manifest().version.clone(),
        );
        let (base_url, downloads) = serve_fake_pack("echo", &version, bytes, digest).await;
        let home = tempfile::tempdir().unwrap();

        install(args(&format!("{namespace}/echo"), base_url, home.path()))
            .await
            .expect("install must succeed");

        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        let record = store::read_record(home.path(), &namespace, "echo").expect("recorded");
        assert_eq!(record.version, version);
        assert_eq!(record.language, "rust");
        let hex = record.digest.strip_prefix("sha256:").unwrap();
        assert!(store::read_bytes(home.path(), hex).is_ok());
        assert_eq!(store::list_records(home.path()).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_digest_mismatch_is_refused_and_nothing_is_installed() {
        let (bytes, _) = echo_pack();
        let archive = gents::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        let namespace = archive.manifest().metadata.namespace.clone();
        let version = archive.manifest().version.clone();
        let (base_url, _) = serve_fake_pack("echo", &version, bytes, "0".repeat(64)).await;
        let home = tempfile::tempdir().unwrap();

        let error = install(args(&format!("{namespace}/echo"), base_url, home.path()))
            .await
            .expect_err("a digest mismatch must be refused");
        assert!(format!("{error:#}").contains(&"0".repeat(64)), "{error:#}");
        assert!(store::list_records(home.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_pack_from_another_namespace_is_refused() {
        let (bytes, digest) = echo_pack();
        let version = gents::pack_archive::PackArchive::from_bytes(&bytes)
            .unwrap()
            .manifest()
            .version
            .clone();
        let (base_url, _) = serve_fake_pack("echo", &version, bytes, digest).await;
        let home = tempfile::tempdir().unwrap();

        let error = install(args("someone_else/echo", base_url, home.path()))
            .await
            .expect_err("an artifact from another namespace must be refused");
        assert!(
            format!("{error:#}").contains("different identity"),
            "{error:#}"
        );
        assert!(store::list_records(home.path()).unwrap().is_empty());
    }

    #[test]
    fn a_plugin_is_published_as_a_single_plugin_pack() {
        let (bytes, _) = echo_pack();
        let archive = gents::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        let manifest = archive.manifest();
        assert_eq!(manifest.name, "echo");
        assert_eq!(manifest.metadata.kind, gents::pack::PackKind::Plugins);
        assert_eq!(manifest.metadata.plugins.len(), 1);
        assert_eq!(manifest.metadata.plugins[0].artifact, "plugins/echo.afb");
        assert!(archive.plugin_artifact("echo").is_ok());
        assert!(archive.asset("README.md").is_ok());
    }
}

#[cfg(test)]
mod instruction_tests {
    use crate::commands::plugin::store;

    /// A pack whose plugin ships a TOOL.md installs it with the plugin, and the
    /// model-facing tool shows that markdown rather than the one-line description.
    #[tokio::test]
    async fn a_plugins_tool_markdown_is_what_the_model_sees() {
        let afb = crate::commands::plugin::testing::build_plugin_afb(
            "echo",
            b"fn main() { println!(\"{{}}\"); }",
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tools");
        std::fs::create_dir_all(root.join("plugins/echo")).unwrap();
        std::fs::write(root.join("plugins/echo.afb"), &afb).unwrap();
        std::fs::write(root.join("README.md"), "# tools").unwrap();
        let markdown = "# echo\n\nReturns its input unchanged.";
        std::fs::write(root.join("plugins/echo/TOOL.md"), markdown).unwrap();
        let language = afterburner_cloud::Afb::from_bytes(&afb)
            .unwrap()
            .manifest
            .package
            .language;
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "manifest_version": 1, "name": "tools", "namespace": "team", "version": "1.0.0",
                "description": "Tools", "authors": ["team"], "tags": [], "kind": "plugins",
                "assets": ["README.md", "plugins/echo.afb", "plugins/echo/TOOL.md"],
                "plugins": [{"name": "echo", "description": "Echo tool.",
                             "artifact": "plugins/echo.afb", "language": language,
                             "input_schema": {"type": "object"},
                             "instructions": "plugins/echo/TOOL.md"}],
            }))
            .unwrap(),
        )
        .unwrap();
        let (bytes, _) = gents::pack_archive::pack_dir(&root).unwrap();
        let archive = gents::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        let home = tempfile::tempdir().unwrap();

        crate::commands::pack::install_pack_plugins(
            home.path(),
            archive.manifest(),
            |path| archive.asset(path),
            false,
        )
        .unwrap();

        let record = store::read_record(home.path(), "team", "echo").unwrap();
        assert_eq!(record.instructions.as_deref(), Some(markdown));
        let executor = std::sync::Arc::new(gents::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        )));
        let tool = gents::plugin::tool::PluginTool::resolve(
            executor,
            &gents::document_config::PluginToolRef {
                plugin: "team/echo".into(),
                digest: None,
            },
        )
        .unwrap();
        let definition = gents::llm::tool::ToolDyn::definition(&tool, String::new()).await;
        assert_eq!(definition.description, markdown);
    }
}
