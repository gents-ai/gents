//! Run a pack's plugin test cases against their built artifacts: the
//! shared-library owner behind `gents pack test`'s plugin half.
//!
//! A plugin case is a JSON file in the plugin source's `tests/` directory:
//! `{"input": <arguments>, "expect": <result>}`. Each case runs through the
//! same runner, bounds and host-served network a real call uses, against
//! the artifact the build just produced. `expect` absent means the call
//! only has to succeed.
//!
//! The CLI delegates plugin cases here; its `--scenario` half stays with the CLI's
//! scenario owner (`commands::pack::scenario::run`), which runs a live
//! model endpoint and is not a library call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pack::PackManifest;
use crate::plugin::executor::call_runner;
use crate::plugin::{BoundDir, PluginBudget, PluginRunner, PluginVerdict};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCase {
    pub input: Value,
    #[serde(default)]
    pub expect: Option<Value>,
    /// A directory to bind for this call, relative to the case file's own
    /// directory; only a plugin declaring `bind_dir` may use it. `within`
    /// for [`BoundDir::new`] is that same directory, so a case cannot bind
    /// anything outside its own tree.
    #[serde(default)]
    pub bind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginCases {
    pub plugin: String,
    pub passed: usize,
    pub failures: Vec<String>,
}

/// Runs every case of every plugin that has a source, against its built
/// artifact.
pub async fn run_plugin_cases(dir: &Path) -> Result<Vec<PluginCases>> {
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).context("reading manifest.json")?,
    )
    .context("parsing manifest.json")?;
    let mut results = Vec::new();
    for plugin in &manifest.metadata.plugins {
        let Some(source) = &plugin.source else {
            continue;
        };
        let cases_dir = dir.join(source).join("tests");
        let mut cases: Vec<PathBuf> = match std::fs::read_dir(&cases_dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect(),
            Err(_) => Vec::new(),
        };
        cases.sort();
        let artifact = std::fs::read(dir.join(&plugin.artifact))
            .with_context(|| format!("reading the built {}", plugin.artifact))?;
        // The author's own plugin runs with the authority it declares.
        let runner = Arc::new(PluginRunner::compile_within(
            &artifact,
            plugin,
            &crate::plugin::authority::declared_manifold(plugin)?,
        )?);
        let coordinate = format!("{}/{}", manifest.metadata.namespace, plugin.name);
        // The budget a real call of this artifact gets, raised to what the
        // plugin itself declares in `limits`: an interpreted plugin needs
        // room to boot its runtime, and a plugin like `review_evidence`
        // needs more than the bare default to process real input.
        let budget = PluginBudget::for_plugin(
            &afterburner_afb::Afb::from_bytes(&artifact)
                .with_context(|| format!("{} is not a readable plugin", plugin.artifact))?,
            plugin,
        );
        let mut report = PluginCases {
            plugin: plugin.name.clone(),
            passed: 0,
            failures: Vec::new(),
        };
        for path in cases {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match run_case(&coordinate, &runner, budget, &path).await {
                Ok(()) => report.passed += 1,
                Err(error) => report.failures.push(format!("{name}: {error:#}")),
            }
        }
        results.push(report);
    }
    Ok(results)
}

async fn run_case(
    coordinate: &str,
    runner: &Arc<PluginRunner>,
    budget: PluginBudget,
    path: &Path,
) -> Result<()> {
    let case: PluginCase = serde_json::from_slice(&std::fs::read(path)?)
        .context("a case is {\"input\": ..., \"expect\": ..., \"bind\": ...}")?;
    let bound = match &case.bind {
        Some(relative) => {
            let case_dir = path
                .parent()
                .context("the case file has no parent directory")?;
            Some(
                BoundDir::new(&case_dir.join(relative), Some(case_dir)).with_context(|| {
                    format!("binding {relative:?} for the case at {}", path.display())
                })?,
            )
        }
        None => None,
    };
    let outcome = call_runner(coordinate, runner.clone(), case.input, budget, bound).await?;
    anyhow::ensure!(
        outcome.verdict == PluginVerdict::Success,
        "{:?}: {}",
        outcome.verdict,
        outcome.diagnostics
    );
    if let Some(expect) = case.expect {
        anyhow::ensure!(
            outcome.output == expect,
            "returned {} instead of {expect}",
            outcome.output
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A case file that is not the documented shape is refused with the
    /// shape named, before anything runs.
    #[test]
    fn a_malformed_case_names_the_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, br#"{"arguments": {}}"#).unwrap();
        let error = serde_json::from_slice::<PluginCase>(&std::fs::read(&path).unwrap())
            .context("a case is {\"input\": ..., \"expect\": ..., \"bind\": ...}")
            .unwrap_err();
        assert!(format!("{error:#}").contains("a case is"), "{error:#}");
    }

    /// A missing artifact is refused with its path, and a plugin with no
    /// source is skipped — the two cases a half-built pack presents.
    #[tokio::test]
    async fn a_missing_artifact_is_refused_and_a_sourceless_plugin_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plugins pack").unwrap();
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "manifest_version": 1,
                "name": "shipping_plugins",
                "version": "0.1.0",
                "description": "A pack that is nothing but capabilities",
                "authors": ["gents-ai contributors"],
                "kind": "plugins",
                "assets": ["README.md", "plugins/prebuilt.afb"],
                "plugins": [
                    {
                        "name": "prebuilt",
                        "description": "Ships only its artifact",
                        "artifact": "plugins/prebuilt.afb",
                        "language": "rust",
                        "input_schema": {"type": "object", "properties": {}},
                    },
                    {
                        "name": "built",
                        "description": "Has source but no built artifact",
                        "artifact": "plugins/built.afb",
                        "source": "plugin_src",
                        "language": "rust",
                        "input_schema": {"type": "object", "properties": {}},
                    }
                ],
            }))
            .unwrap(),
        )
        .unwrap();
        // No artifact on disk: reading it is the refusal, and it names the
        // declared path.
        let error = run_plugin_cases(&root).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("plugins/built.afb"),
            "{error:#}"
        );
    }
}
