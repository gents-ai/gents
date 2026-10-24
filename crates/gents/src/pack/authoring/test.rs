use gents_protocol::error_chain::ErrorChain;
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
    #[serde(default)]
    pub bind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginCases {
    pub plugin: String,
    pub passed: usize,
    pub failures: Vec<String>,
}

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
        let runner = Arc::new(PluginRunner::compile_within(
            &artifact,
            plugin,
            &crate::plugin::authority::declared_manifold(plugin)?,
        )?);
        let coordinate = format!("{}/{}", manifest.metadata.namespace, plugin.name);
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
                Err(error) => report
                    .failures
                    .push(format!("{name}: {}", ErrorChain(error.as_ref()))),
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
