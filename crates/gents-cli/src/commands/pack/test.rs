use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::{PackRunArgs, PackTestArgs};

pub(crate) type PluginCases = gents::pack::authoring::PluginCases;

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
    let plugins = run_plugin_cases(&dir).await?;
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
    args: PackRunArgs,
}

pub(crate) async fn run_plugin_cases(dir: &Path) -> Result<Vec<PluginCases>> {
    Ok(gents::pack::authoring::run_plugin_cases(dir).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scaffolded_plugin_passes_its_case_and_a_wrong_expectation_fails() {
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
                ux: false,
            },
        )
        .unwrap();
        {
            let _guard = crate::commands::afterburner_build::compile_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            super::super::build::build_pack(&dir, Some(&root.path().join("out.pack"))).unwrap();
        }
        let results = run_plugin_cases(&dir).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].passed, 1, "{:?}", results[0].failures);
        assert!(results[0].failures.is_empty());

        std::fs::write(
            dir.join("plugins/format_check/tests/wrong.json"),
            r#"{"input": {"a": 1}, "expect": {"a": 2}}"#,
        )
        .unwrap();
        let results = run_plugin_cases(&dir).await.unwrap();
        assert_eq!(results[0].passed, 1);
        assert_eq!(results[0].failures.len(), 1);
        assert!(
            results[0].failures[0].starts_with("wrong.json: returned"),
            "{:?}",
            results[0].failures
        );
    }

    #[tokio::test]
    async fn a_case_fetches_through_the_host_from_a_granted_host() {
        let port = crate::commands::plugin::testing::hello_server().await;
        let dir = crate::commands::plugin::testing::http_plugin_pack(port);
        let results = run_plugin_cases(dir.path()).await.unwrap();
        assert_eq!(results[0].passed, 1, "{:?}", results[0].failures);
    }

    #[tokio::test]
    async fn a_bind_case_binds_its_fixture_directory() {
        let dir = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let results = run_plugin_cases(dir.path()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].passed, 1, "{:?}", results[0].failures);
        assert!(results[0].failures.is_empty());
    }
}
