//! `gents plugin`: manage Afterburner plugins independently of any pack.
//!
//! A plugin is a complete Afterburner `.afb` - first class, publishable
//! and installable on its own, and also carried inside a pack under
//! `plugins/` (see `crates/gents/src/pack.rs`'s own doc for that side).
//! This module owns the standalone surface: compiling one from source
//! ([`build`]), publishing and installing it on its own ([`publish`],
//! [`install`]), finding what is installed ([`list`], [`remove`]), and
//! running one to prove it actually works ([`run`]). Registry access is
//! the exact client `gents pack` already uses
//! ([`crate::commands::pack::registry::RegistryClient`]) - a plugin and a
//! pack are both just bytes to the registry, so there is no second HTTP
//! client here.

mod build;
mod install;
mod run;
pub(crate) mod store;

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
pub(crate) fn install_from_pack(
    home: &std::path::Path,
    pack_namespace: &str,
    pack_version: &str,
    plugin: &gents::pack::PackPlugin,
    artifact_bytes: &[u8],
) -> Result<store::InstalledPlugin> {
    use sha2::{Digest, Sha256};
    let digest_hex = format!("{:x}", Sha256::digest(artifact_bytes));
    store::store_bytes(home, &digest_hex, artifact_bytes)?;
    let record = store::InstalledPlugin {
        namespace: pack_namespace.to_owned(),
        name: plugin.name.clone(),
        version: pack_version.to_owned(),
        digest: format!("sha256:{digest_hex}"),
        language: plugin.language.clone(),
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

    let token = crate::commands::pack::registry::resolve_registry_token(args.token.as_deref())
        .context("a registry token is required; pass --token or set GENTS_REGISTRY_TOKEN")?;
    let client = crate::commands::pack::registry::RegistryClient::for_plugins(
        crate::commands::pack::registry::resolve_registry_url(args.registry.as_deref()),
    );
    let response = client.publish(&token, bytes).await?;

    crate::print_json(&json!({
        "namespace": afb.manifest.package.namespace,
        "name": afb.manifest.package.name,
        "version": afb.manifest.package.version,
        "language": afb.manifest.package.language,
        "registry_response": response,
    }))
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

    /// A bare-hex digest, the exact form the registry actually advertises.
    fn digest_of(bytes: &[u8]) -> String {
        testing::digest_of(bytes)
    }

    /// End-to-end round trip through this module's public surface, backed
    /// by a local fake registry: install, list reflects it, run returns
    /// the plugin's own output, remove takes it away again.
    #[tokio::test]
    async fn install_list_run_remove_round_trip() {
        let bytes = sample_plugin_afb();
        let digest = digest_of(&bytes);

        let downloads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let base_url = start_fake_registry(bytes, digest.clone(), downloads.clone()).await;
        let home = tempfile::tempdir().unwrap();

        install::install(PluginInstallArgs {
            name: "gents/noop".to_owned(),
            version: None,
            registry: Some(base_url),
            home: Some(home.path().to_owned()),
        })
        .await
        .expect("install must succeed");

        let records = store::list_records(home.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "noop");
        assert_eq!(records[0].digest, format!("sha256:{digest}"));

        run::run(PluginRunArgs {
            name: "gents/noop".to_owned(),
            input: None,
            home: Some(home.path().to_owned()),
        })
        .await
        .expect("running the just-installed plugin must succeed");

        let removed = store::remove_record(home.path(), "gents", "noop").expect("remove");
        assert_eq!(removed.digest, format!("sha256:{digest}"));
        assert!(store::list_records(home.path()).unwrap().is_empty());
    }

    async fn start_fake_registry(
        bytes: Vec<u8>,
        digest: String,
        downloads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> String {
        use axum::extract::{Path as AxumPath, State};
        use axum::http::StatusCode;
        use axum::response::{IntoResponse, Response};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        struct FakeState {
            bytes: Vec<u8>,
            digest: String,
            downloads: Arc<std::sync::atomic::AtomicUsize>,
        }

        async fn package(AxumPath((_ns, name)): AxumPath<(String, String)>) -> Response {
            if name == "noop" {
                Json(json!({ "latest": "0.1.0" })).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        async fn version(State(state): State<Arc<FakeState>>) -> Json<serde_json::Value> {
            Json(json!({ "digest": state.digest }))
        }
        async fn download(
            State(state): State<Arc<FakeState>>,
            AxumPath(_): AxumPath<(String, String, String)>,
        ) -> impl IntoResponse {
            state.downloads.fetch_add(1, Ordering::SeqCst);
            state.bytes.clone()
        }

        let state = Arc::new(FakeState {
            bytes,
            digest,
            downloads,
        });
        let app = Router::new()
            .route("/api/v1/packages/{ns}/{name}", get(package))
            .route("/api/v1/packages/{ns}/{name}/{version}", get(version))
            .route(
                "/api/v1/packages/{ns}/{name}/{version}/download",
                get(download),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }
}
