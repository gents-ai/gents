//! `gents plugin build`: compile one Afterburner package directory into a
//! standalone `.afb`, independent of any pack. Reuses
//! [`crate::commands::afterburner_build`], the exact compiler and language
//! gate `gents pack build` uses for a pack's own plugins, so a plugin
//! built standalone and one carried inside a pack can never drift apart.

use std::path::{Path, PathBuf};

use afterburner_cloud::Afb;
use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::args::PluginBuildArgs;
use crate::commands::afterburner_build;

pub(crate) fn dispatch(args: PluginBuildArgs) -> Result<()> {
    let dir = &args.dir;
    let owner = format!("the plugin at {}", dir.display());
    let local = afterburner_build::load_and_validate(&owner, dir)?;
    let out_path = args
        .out
        .clone()
        .unwrap_or_else(|| default_out_path(dir, &local));
    afterburner_build::compile(&owner, dir, local, &out_path)?;

    let bytes =
        std::fs::read(&out_path).with_context(|| format!("reading back {}", out_path.display()))?;
    let afb = Afb::from_bytes(&bytes).context("reading back the plugin that was just built")?;
    crate::print_json(&json!({
        "namespace": afb.manifest.package.namespace,
        "name": afb.manifest.package.name,
        "version": afb.manifest.package.version,
        "language": afb.manifest.package.language,
        "digest": afterburner_cloud::afterburner_afb::digest::hex(&afb.digest),
        "size_bytes": bytes.len(),
        "out": out_path,
    }))
}

fn default_out_path(dir: &Path, local: &afterburner_cloud::pkg::LocalPackage) -> PathBuf {
    let parent = dir.parent().unwrap_or_else(|| Path::new("."));
    parent.join(local.output_filename())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn write_rust_plugin(root: &Path) {
        write(
            &root.join("afb.toml"),
            b"[format]\nversion = \"1.0\"\n\n\
              [package]\nname = \"echo\"\nnamespace = \"gents\"\nversion = \"0.1.0\"\n\
              language = \"rust\"\nentry = \"source/main.rs\"\n\n\
              [runtime]\nmin = \"0.1.0\"\n",
        );
        write(
            &root.join("manifold.json"),
            br#"{"fs":"None","net":"None","env":"None","crypto":false,"child_process":false}"#,
        );
        write(
            &root.join("source/main.rs"),
            b"fn main() { println!(\"ok\"); }",
        );
        // Rust is the one language whose compile path shells out to
        // `cargo build`, so its source directory needs its own manifest as
        // well as the afb.toml (`crate::commands::afterburner_build`'s own
        // Rust test says the same). `[workspace]` keeps it standalone: the
        // temp directory is under /tmp, but a stray parent manifest would
        // otherwise pull it into someone else's workspace.
        write(
            &root.join("Cargo.toml"),
            b"[workspace]\n\n              [package]\nname = \"echo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n              [[bin]]\nname = \"echo\"\npath = \"source/main.rs\"\n",
        );
    }

    #[test]
    fn a_directory_with_no_afb_toml_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("not_a_plugin");
        std::fs::create_dir_all(&root).unwrap();

        let error = dispatch(PluginBuildArgs {
            dir: root.clone(),
            out: None,
        })
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(&root.display().to_string()), "{message}");
        assert!(message.contains("own Afterburner package"), "{message}");
    }

    #[test]
    fn a_rust_plugin_builds_to_a_genuine_afb_at_the_default_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("echo_plugin");
        write_rust_plugin(&root);

        dispatch(PluginBuildArgs {
            dir: root.clone(),
            out: None,
        })
        .unwrap();

        let out = dir.path().join("gents-echo-0.1.0.afb");
        assert!(out.is_file(), "expected {}", out.display());
        let afb = Afb::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(afb.manifest.package.name, "echo");
        assert_eq!(afb.manifest.package.language, "rust");
        assert_eq!(
            afb.manifest.runtime.target.as_deref(),
            Some("wasm32-wasip1")
        );
    }

    #[test]
    fn an_explicit_out_path_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("echo_plugin");
        write_rust_plugin(&root);
        let out = dir.path().join("custom.afb");

        dispatch(PluginBuildArgs {
            dir: root.clone(),
            out: Some(out.clone()),
        })
        .unwrap();

        assert!(out.is_file());
    }
}
