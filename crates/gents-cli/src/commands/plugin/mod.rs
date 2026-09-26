//! `gents plugin`: build, publish, install and run plugins.
//!
//! A plugin is a compiled `.afb` that always travels inside a pack, under
//! `plugins/` (see `crates/gents/src/pack.rs`). Publishing one on its own
//! wraps it in a single-plugin `plugins` pack ([`publish`]); installing one
//! by name fetches its pack and installs the plugins it carries
//! ([`install`]). The rest works on what is installed locally: compiling
//! from source ([`build`]), listing and removing ([`list`], [`remove`]), and
//! running one to prove it works ([`run`]). Registry access is the client
//! `gents pack` uses ([`crate::commands::pack::registry::RegistryClient`]).

mod build;
mod install;
mod run;
pub(crate) use gents::plugin::store;

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::args::{PluginCommand, PluginListArgs, PluginPublishArgs, PluginRemoveArgs};

pub(crate) async fn dispatch(command: PluginCommand) -> Result<()> {
    match command {
        PluginCommand::Build(args) => build::dispatch(args),
        PluginCommand::Publish(args) => publish(args).await,
        PluginCommand::Install(args) => install::install(args).await,
        PluginCommand::List(args) => list(args),
        PluginCommand::Remove(args) => remove(args),
        PluginCommand::Run(args) => run::run(args).await,
    }
}

/// Installs one plugin a pack carries into the same content-addressed
/// store [`install::install`] uses, so a plugin that arrived bundled in a
/// pack is just as runnable by name (`gents plugin run <name>`) as one
/// installed on its own. Called from `gents pack install`, not this
/// module's own dispatch (a pack's plugins install as a side effect of
/// installing the pack, not through a separate `gents plugin` command).
///
/// A plugin declared inline in a pack manifest carries neither a
/// namespace nor a version of its own, so it takes its pack's: two packs
/// from different namespaces may each carry a `format_check`, and
/// recording both under one default namespace would have the second
/// silently replace the first.
///
/// `pack_coordinate` (`{namespace}/{name}` of the pack this plugin ships
/// in) is refused when the record already belongs to a different pack: see
/// [`store::check_plugin_ownership`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn install_from_pack(
    home: &std::path::Path,
    pack_namespace: &str,
    pack_coordinate: &str,
    pack_version: &str,
    pack_digest: &str,
    plugin: &gents::pack::PackPlugin,
    artifact_bytes: &[u8],
    instructions: Option<String>,
    consent: bool,
) -> Result<store::InstalledPlugin> {
    store::check_plugin_ownership(home, pack_namespace, &plugin.name, pack_coordinate)?;
    let granted = store::grant_on_install(home, pack_namespace, plugin, consent)?;
    use sha2::{Digest, Sha256};
    gents::plugin::PluginRunner::compile(artifact_bytes, plugin)
        .with_context(|| format!("admitting pack plugin {}", plugin.name))?;
    let digest_hex = format!("{:x}", Sha256::digest(artifact_bytes));
    store::store_bytes(home, &digest_hex, artifact_bytes)?;
    let record = store::InstalledPlugin {
        namespace: pack_namespace.to_owned(),
        name: plugin.name.clone(),
        version: pack_version.to_owned(),
        digest: format!("sha256:{digest_hex}"),
        language: plugin.language.clone(),
        declaration: plugin.clone(),
        granted,
        instructions,
        owner_pack_coordinate: Some(pack_coordinate.to_owned()),
        owner_pack_digest: Some(pack_digest.to_owned()),
    };
    store::write_record(home, &record)?;
    Ok(record)
}

async fn publish(args: PluginPublishArgs) -> Result<()> {
    let bytes =
        std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
    anyhow::ensure!(!bytes.is_empty(), "{} is empty", args.file.display());
    let afb = afterburner_cloud::Afb::from_bytes(&bytes)
        .with_context(|| format!("{} is not a readable plugin .afb", args.file.display()))?;
    let dir = tempfile::tempdir().context("staging the plugin's pack")?;
    let (pack, header) = plugin_pack(dir.path(), &afb, &bytes)?;

    let registry = crate::commands::pack::registry::resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let token_flag =
        crate::commands::pack::registry::resolve_token_flag(args.token, args.token_stdin)?;
    let token = crate::commands::pack::account::resolve_publish_token(
        token_flag.as_deref(),
        &registry,
        &home,
    )?;
    let client = crate::commands::pack::registry::RegistryClient::new(registry);
    let response = client.publish(&token, pack).await?;

    crate::print_json(&json!({
        "pack": header.coordinate,
        "version": header.version,
        "digest": header.digest,
        "registry_response": response,
    }))
}

/// Packs one compiled plugin on its own: a `plugins` pack named after it,
/// carrying the artifact under `plugins/` and a README from its description.
fn plugin_pack(
    dir: &std::path::Path,
    afb: &afterburner_cloud::Afb,
    bytes: &[u8],
) -> Result<(Vec<u8>, gents::pack_archive::PackHeader)> {
    let package = &afb.manifest.package;
    anyhow::ensure!(
        gents::pack::is_valid_pack_name(&package.name)
            && gents::pack::is_valid_pack_name(&package.namespace),
        "{}/{} cannot be a pack: pack names are snake_case; rename the plugin",
        package.namespace,
        package.name
    );
    let declaration = declaration_from_artifact(afb)?;
    let artifact = declaration.artifact.clone();
    let description = declaration.description.clone();
    std::fs::create_dir_all(dir.join("plugins"))?;
    std::fs::write(dir.join(&artifact), bytes)?;
    std::fs::write(
        dir.join("README.md"),
        format!(
            "# {}\n\n{description}\n\nInstall with `gents plugin install {}/{}`.\n",
            package.name, package.namespace, package.name
        ),
    )?;
    let manifest = json!({
        "manifest_version": 1,
        "name": package.name,
        "namespace": package.namespace,
        "version": package.version,
        "description": description,
        "authors": [package.namespace],
        "tags": package.keywords,
        "kind": "plugins",
        "assets": ["README.md", artifact],
        "plugins": [declaration],
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).context("encoding the plugin pack manifest")?,
    )?;
    gents::pack_archive::pack_dir(dir)
}

fn list(args: PluginListArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let records = store::list_records(&home)?;
    crate::print_json(&json!({ "plugins": records }))
}

fn remove(args: PluginRemoveArgs) -> Result<()> {
    let (namespace, name) = crate::commands::pack::split_namespace(&args.name);
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let removed = store::remove_record(&home, namespace, name).with_context(|| {
        format!(
            "{namespace}/{name} is not installed under {}",
            home.display()
        )
    })?;
    crate::print_json(&json!({ "removed": removed }))
}

/// Test-only fixtures shared by this module's tests and its submodules'.
///
/// One builder rather than a copy per file: the three that preceded it
/// differed only in a package name and a `main.rs` body, and all three
/// carried the same defect (no `Cargo.toml`, which the Rust compile path
/// shells out to `cargo build` and therefore requires), so all three were
/// failing in the same way.
/// Standalone artifacts have no enclosing pack declaration. Capture their own
/// manifest and authority once at installation; pack installs retain theirs.
pub(crate) fn declaration_from_artifact(
    afb: &afterburner_cloud::Afb,
) -> Result<gents::pack::PackPlugin> {
    let package = &afb.manifest.package;
    Ok(gents::pack::PackPlugin {
        name: package.name.clone(),
        description: package
            .description
            .clone()
            .unwrap_or_else(|| format!("installed plugin {}/{}", package.namespace, package.name)),
        artifact: format!("plugins/{}.afb", package.name),
        source: None,
        language: package.language.clone(),
        // The AFB manifest has no model-facing input schema.
        input_schema: serde_json::json!({"type": "object"}),
        manifold: Some(serde_json::to_value(&afb.manifold).context("encoding plugin manifold")?),
        instructions: None,
    })
}

#[cfg(test)]
pub(crate) mod testing {
    /// Compiles a real Rust plugin through the same path `gents plugin
    /// build` uses, and returns its `.afb` bytes. Not a stub: these tests
    /// are about what a compiled plugin does, so a stand-in would prove
    /// nothing.
    pub(crate) fn build_plugin_afb(name: &str, main_rs: &[u8]) -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join("source")).expect("creating the source directory");
        std::fs::write(
            root.join("afb.toml"),
            format!(
                "[format]\nversion = \"1.0\"\n\n\
                 [package]\nname = \"{name}\"\nnamespace = \"gents\"\nversion = \"0.1.0\"\n\
                 language = \"rust\"\nentry = \"source/main.rs\"\n\n\
                 [runtime]\nmin = \"0.1.0\"\n"
            )
            .as_bytes(),
        )
        .expect("writing afb.toml");
        std::fs::write(
            root.join("manifold.json"),
            br#"{"fs":"None","net":"None","env":"None","crypto":false,"child_process":false}"#,
        )
        .expect("writing manifold.json");
        // Rust compiles by shelling out to `cargo build`, so the directory
        // needs its own manifest as well as the afb.toml. `[workspace]`
        // keeps it standalone: without it a parent manifest above the temp
        // directory would claim it.
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\n\n\
                 [package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [[bin]]\nname = \"{name}\"\npath = \"source/main.rs\"\n"
            )
            .as_bytes(),
        )
        .expect("writing Cargo.toml");
        std::fs::write(root.join("source/main.rs"), main_rs).expect("writing source/main.rs");

        let owner = format!("test plugin ({name})");
        let local = crate::commands::afterburner_build::load_and_validate(&owner, &root)
            .unwrap_or_else(|error| panic!("loading the {name} package: {error:#}"));
        let out = dir.path().join(format!("{name}.afb"));
        // The one lock every test that shells out to a toolchain takes.
        let _guard = crate::commands::afterburner_build::compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::commands::afterburner_build::compile(&owner, &root, local, &out)
            .unwrap_or_else(|error| panic!("compiling {name}: {error:#}"));
        std::fs::read(&out).expect("reading the compiled .afb")
    }

    /// The identity plugin: reads all of stdin, writes it back unchanged.
    /// The vehicle for every "does the ABI carry arguments through" test.
    pub(crate) fn build_echo_plugin() -> Vec<u8> {
        build_plugin_afb(
            "echo",
            b"use std::io::{Read, Write};\n\
              fn main() {\n\
              \x20\x20\x20\x20let mut buf = Vec::new();\n\
              \x20\x20\x20\x20if std::io::stdin().read_to_end(&mut buf).is_ok() {\n\
              \x20\x20\x20\x20\x20\x20\x20\x20let _ = std::io::stdout().write_all(&buf);\n\
              \x20\x20\x20\x20}\n\
              }\n",
        )
    }

    /// Bare-hex SHA-256: the exact form the registry advertises
    /// (`verify_digest` compares against `sha256_hex`'s own bare output,
    /// never a `sha256:`-prefixed one).
    pub(crate) fn digest_of(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{PluginInstallArgs, PluginRunArgs};

    /// A minimal real plugin `.afb`: the shared fixture, built once here
    /// rather than described again.
    fn sample_plugin_afb() -> Vec<u8> {
        // Writes one JSON value, because that is the plugin ABI: a plugin
        // that prints nothing is `BadOutput` by design, so `fn main() {}`
        // would make this fixture fail for a reason that has nothing to do
        // with installing and running it.
        testing::build_plugin_afb("noop", b"fn main() { println!(\"{{}}\"); }")
    }

    /// End-to-end round trip through this module's public surface, backed
    /// by a local fake registry serving the plugin's pack: install, list
    /// reflects it, run returns the plugin's own output, remove takes it
    /// away again.
    #[tokio::test]
    async fn install_list_run_remove_round_trip() {
        let afb_bytes = sample_plugin_afb();
        let afb = afterburner_cloud::Afb::from_bytes(&afb_bytes).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (pack, header) = plugin_pack(dir.path(), &afb, &afb_bytes).unwrap();
        let (base_url, _) = crate::commands::pack::registry::tests::serve_fake_pack(
            "noop",
            &header.version,
            pack.clone(),
            testing::digest_of(&pack),
        )
        .await;
        let home = tempfile::tempdir().unwrap();
        let namespace = header.coordinate.split('/').next().unwrap().to_owned();

        install::install(PluginInstallArgs {
            grant_authority: false,
            name: format!("{namespace}/noop"),
            version: None,
            registry: Some(base_url),
            home: Some(home.path().to_owned()),
        })
        .await
        .expect("install must succeed");

        let records = store::list_records(home.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "noop");
        assert_eq!(
            records[0].digest,
            format!("sha256:{}", testing::digest_of(&afb_bytes))
        );

        run::run(PluginRunArgs {
            name: format!("{namespace}/noop"),
            input: None,
            home: Some(home.path().to_owned()),
        })
        .await
        .expect("running the just-installed plugin must succeed");

        store::remove_record(home.path(), &namespace, "noop").expect("remove");
        assert!(store::list_records(home.path()).unwrap().is_empty());
    }
}
