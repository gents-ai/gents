//! Looking at a pack without installing it: the shared-library owner behind
//! `gents pack verify` (a `.pack` file) and `gents pack show --config`'s
//! loaded-configuration preview.
//!
//! Canonical with `crates/gents-cli/src/commands/pack/inspect.rs`. The
//! registry, store and diff halves of `inspect.rs` stay with the CLI:
//! they need a home's pack store, registry credentials and a resolved
//! [`PackSource`], all of which are CLI state. What a caller can ask of a
//! plain `.pack` file or of a directory archive is owned here.

use anyhow::{Context, Result};
use serde_json::json;

use crate::pack::{load_pack_config, PackInstallOptions, PackManifest};
use crate::pack_archive::read_pack;

/// The owner a previewed configuration is bound to. Never written anywhere.
pub const PREVIEW_OWNER: &str = "did:key:zPackShowPlaceholder";

/// Checks a `.pack` file, streaming, without installing or storing
/// anything. Returns the verified header, the way `gents pack verify`
/// prints it.
pub fn verify_pack_file(path: &std::path::Path) -> Result<crate::pack_archive::PackHeader> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    read_pack(
        std::io::BufReader::new(file),
        crate::pack_archive::Bounds::default(),
        |_, _| Ok(()),
    )
    .with_context(|| format!("{} is not a valid pack", path.display()))
    .map(|verified| verified.header)
}

/// The pack's canonical configuration, loaded exactly like an install would
/// (for a placeholder owner, since nothing is written), and its
/// `experiment.json`, interpolated at declared defaults, or `null` when the
/// pack declares no scenario. The environment is never read: a missing
/// default is refused so the preview never differs from the pack's own
/// declared defaults.
pub fn preview_config(
    manifest: &PackManifest,
    asset: &dyn Fn(&str) -> Result<Vec<u8>>,
) -> Result<serde_json::Value> {
    let config = load_pack_config(
        manifest,
        &PackInstallOptions {
            node_did: PREVIEW_OWNER.into(),
        },
        &|path| asset(path).map(Vec::from),
        &|_| None,
    )?;
    let scenario = crate::pack::declared_paths(manifest)
        .iter()
        .any(|path| path == "experiment.json")
        .then(|| -> Result<serde_json::Value> {
            let bytes = asset("experiment.json")?;
            let raw = std::str::from_utf8(&bytes).context("experiment.json is not UTF-8")?;
            let expanded =
                crate::pack::interpolate::interpolate_with(raw, &|_| None).map_err(|missing| {
                    anyhow::anyhow!(
                        "experiment.json references environment variable(s) with no default: {}",
                        missing.join(", ")
                    )
                })?;
            serde_json::from_str(&expanded).context("parsing experiment.json")
        })
        .transpose()?;
    Ok(json!({ "config": config, "scenario": scenario }))
}

#[cfg(test)]
mod tests {
    use super::super::build::PluginCompiler;
    use super::*;
    use std::path::Path;

    /// A compiler seam that records it was asked and refuses: no real
    /// toolchain in this test.
    struct RefusingCompiler {
        called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl RefusingCompiler {
        fn new() -> (Self, std::sync::Arc<std::sync::atomic::AtomicBool>) {
            let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            (
                Self {
                    called: called.clone(),
                },
                called,
            )
        }
    }

    impl PluginCompiler for RefusingCompiler {
        fn load_and_validate(&self, _owner: &str, _dir: &Path) -> Result<()> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("no compiler in this test")
        }
        fn compile(&self, _owner: &str, _dir: &Path, _out_path: &Path) -> Result<()> {
            unreachable!("load_and_validate refuses first")
        }
    }

    /// A `.pack` produced by the authoring build verifies, and a tampered
    /// one does not.
    #[test]
    fn a_built_pack_verifies_and_a_tampered_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plain pack").unwrap();
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "manifest_version": 1,
                "name": "plain_pack",
                "version": "1.0.0",
                "description": "A plain pack with no plugins",
                "authors": ["gents-ai contributors"],
                "namespace": "acme",
                "kind": "assets",
                "assets": ["README.md"],
                "dependencies": [],
            }))
            .unwrap(),
        )
        .unwrap();
        let (compiler, _) = RefusingCompiler::new();
        let out = dir.path().join("plain.pack");
        super::super::build::build_pack(&root, &compiler, Some(&out)).unwrap();

        let header = verify_pack_file(&out).unwrap();
        assert_eq!(header.coordinate, "acme/plain_pack");

        let tampered = dir.path().join("tampered.pack");
        let mut bytes = std::fs::read(&out).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&tampered, bytes).unwrap();
        let error = verify_pack_file(&tampered).unwrap_err();
        assert!(
            format!("{error:#}").contains("is not a valid pack"),
            "{error:#}"
        );
    }

    #[test]
    fn preview_loads_the_configuration_and_refuses_an_un_defaulted_scenario() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("review_toolkit");
        super::super::scaffold::scaffold(
            &root,
            "review_toolkit",
            &super::super::scaffold::ScaffoldInput {
                kind: None,
                namespace: "acme".into(),
                template: Some(super::super::template::Template::Minimal),
                language: None,
            },
        )
        .unwrap();
        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let asset = |path: &str| -> Result<Vec<u8>> { Ok(std::fs::read(root.join(path))?) };
        let report = preview_config(&manifest, &asset).unwrap();
        assert_eq!(
            report["config"]["agents"][0]["agent_id"],
            "review-toolkit-worker"
        );
        assert_eq!(report["scenario"], serde_json::Value::Null);

        // A scenario with an un-defaulted variable is refused: the preview
        // never reads the environment, so it can never disagree with the
        // pack's declared defaults.
        std::fs::write(
            root.join("experiment.json"),
            br#"{"endpoint": "${GENTS_PREVIEW_ENDPOINT}"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("manifest.json"),
            std::fs::read_to_string(root.join("manifest.json"))
                .unwrap()
                .replace("\"assets\": [", "\"assets\": [\n    \"experiment.json\","),
        )
        .unwrap();
        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let error = preview_config(&manifest, &asset).unwrap_err();
        assert!(
            format!("{error:#}").contains("GENTS_PREVIEW_ENDPOINT"),
            "{error:#}"
        );

        // A defaulted variable resolves at its default.
        std::fs::write(
            root.join("experiment.json"),
            br#"{"endpoint": "${GENTS_PREVIEW_ENDPOINT:-http://127.0.0.1:1/v1}"}"#,
        )
        .unwrap();
        let report = preview_config(&manifest, &asset).unwrap();
        assert_eq!(report["scenario"]["endpoint"], "http://127.0.0.1:1/v1");
    }
}
