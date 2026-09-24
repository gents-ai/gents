//! `gents pack test`: check a pack, build it, and run every plugin's cases.
//!
//! A plugin case is a JSON file in the plugin source's `tests/` directory:
//! `{"input": <arguments>, "expect": <result>}`. Each case runs through the
//! same runner and bounds a real call uses, against the artifact the build
//! just produced. `expect` absent means the call only has to succeed. The
//! pack's `experiment.json` scenario needs a model endpoint, so it runs only
//! when asked for with `--scenario`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::pack::PackManifest;
use gents::plugin::{PluginBudget, PluginRunner, PluginVerdict};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::PackTestArgs;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginCase {
    input: Value,
    #[serde(default)]
    expect: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginCases {
    pub(crate) plugin: String,
    pub(crate) passed: usize,
    pub(crate) failures: Vec<String>,
}

pub(crate) async fn test(args: PackTestArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let check = super::check::check_dir(&dir).await;
    anyhow::ensure!(
        check.problems.is_empty(),
        "{} failed the check: {}",
        dir.display(),
        check.problems.join("; ")
    );
    let out = tempfile::tempdir().context("creating a build directory")?;
    let built = super::build::build_pack(&dir, Some(&out.path().join("test.pack")))?;
    let plugins = run_plugin_cases(&dir)?;
    let scenario = if args.scenario {
        anyhow::ensure!(
            dir.join("experiment.json").is_file(),
            "{} has no experiment.json scenario",
            dir.display()
        );
        let run =
            <ScenarioRun as clap::Parser>::try_parse_from(["scenario", &dir.to_string_lossy()])?;
        super::scenario::run(run.args).await?;
        "passed"
    } else {
        "not run; pass --scenario"
    };
    let failed: usize = plugins.iter().map(|cases| cases.failures.len()).sum();
    crate::print_json(&json!({
        "pack": check.pack,
        "digest": built.digest,
        "graphs": check.graphs,
        "plugins": plugins,
        "scenario": scenario,
    }))?;
    anyhow::ensure!(failed == 0, "{failed} plugin cases failed");
    Ok(())
}

#[derive(clap::Parser)]
struct ScenarioRun {
    #[command(flatten)]
    args: crate::cli::PackRunArgs,
}

/// Runs every case of every plugin that has a source, against its built
/// artifact.
pub(crate) fn run_plugin_cases(dir: &Path) -> Result<Vec<PluginCases>> {
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
        let runner = PluginRunner::compile_within(
            &artifact,
            plugin,
            &gents::plugin::authority::declared_manifold(plugin)?,
        )?;
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
            match run_case(&runner, &path) {
                Ok(()) => report.passed += 1,
                Err(error) => report.failures.push(format!("{name}: {error:#}")),
            }
        }
        results.push(report);
    }
    Ok(results)
}

fn run_case(runner: &PluginRunner, path: &Path) -> Result<()> {
    let case: PluginCase = serde_json::from_slice(&std::fs::read(path)?)
        .context("a case is {\"input\": ..., \"expect\": ...}")?;
    let outcome = runner.call(&case.input, &PluginBudget::default())?;
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

    #[test]
    fn a_scaffolded_plugin_passes_its_case_and_a_wrong_expectation_fails() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("format_check");
        super::super::scaffold::scaffold(
            &dir,
            "format_check",
            &crate::cli::PackScaffoldArgs {
                kind: None,
                namespace: "acme".into(),
                template: Some(crate::cli::PackTemplate::PluginTool),
                language: None,
            },
        )
        .unwrap();
        {
            let _guard = crate::commands::afterburner_build::compile_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            super::super::build::build_pack(&dir, Some(&root.path().join("out.pack"))).unwrap();
        }
        let results = run_plugin_cases(&dir).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].passed, 1, "{:?}", results[0].failures);
        assert!(results[0].failures.is_empty());

        std::fs::write(
            dir.join("plugins/format_check/tests/wrong.json"),
            r#"{"input": {"a": 1}, "expect": {"a": 2}}"#,
        )
        .unwrap();
        let results = run_plugin_cases(&dir).unwrap();
        assert_eq!(results[0].passed, 1);
        assert_eq!(results[0].failures.len(), 1);
        assert!(
            results[0].failures[0].starts_with("wrong.json: returned"),
            "{:?}",
            results[0].failures
        );
    }
}
