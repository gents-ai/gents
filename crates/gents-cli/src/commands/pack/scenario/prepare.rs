use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use gents::plugin::authority::{declared_manifold, describe_manifold};
use gents::plugin::executor::call_runner;
use gents::plugin::{BoundDir, PluginBudget, PluginRunner, PluginVerdict};

use super::ScenarioManifest;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioPrepareStep {
    pub(super) plugin: String,
    #[serde(default = "default_prepare_input")]
    pub(super) input: Value,
    #[serde(default)]
    pub(super) bind_dir: Option<String>,
    pub(super) seed_fields: BTreeMap<String, String>,
}

fn default_prepare_input() -> Value {
    Value::Object(serde_json::Map::new())
}

pub(super) fn validate_prepare_steps(manifest: &ScenarioManifest) -> Result<()> {
    let mut prepared_seed_fields = BTreeSet::new();
    for step in &manifest.prepare {
        anyhow::ensure!(
            step.input.is_object(),
            "prepare step for plugin {} input must be a JSON object",
            step.plugin
        );
        let plugin = manifest
            .plugins
            .iter()
            .find(|declared| declared.name == step.plugin)
            .with_context(|| format!("prepare step names undeclared plugin {}", step.plugin))?;
        match (&step.bind_dir, &plugin.bind_dir) {
            (Some(_), Some(_)) | (None, None) => {}
            (Some(_), None) => bail!(
                "prepare step for plugin {} sets bind_dir, but the plugin declares none",
                step.plugin
            ),
            (None, Some(_)) => bail!(
                "prepare step for plugin {} must set bind_dir; the plugin declares one",
                step.plugin
            ),
        }
        for (field, pointer) in &step.seed_fields {
            gents::graphql::validate_graphql_name(field).with_context(|| {
                format!(
                    "prepare step for plugin {} seed field {field:?}",
                    step.plugin
                )
            })?;
            if manifest.seed.fields.contains_key(field)
                || field == &manifest.seed.job_id_field
                || field == &manifest.seed.prompt_field
            {
                bail!(
                    "prepare step for plugin {} seed field {field} collides with a seed field \
                     already claimed by seed.fields, seed.job_id_field, or seed.prompt_field",
                    step.plugin
                );
            }
            if !prepared_seed_fields.insert(field.as_str()) {
                bail!("prepare steps declare seed field {field} more than once");
            }
            if !pointer.is_empty() && !pointer.starts_with('/') {
                bail!(
                    "prepare step for plugin {} seed field {field} pointer {pointer:?} must be \
                     \"\" or start with \"/\"",
                    step.plugin
                );
            }
        }
    }
    Ok(())
}

pub(super) fn prepare_seed_fields(
    plugin: &str,
    output: &Value,
    seed_fields: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for (field, pointer) in seed_fields {
        let value = output.pointer(pointer).with_context(|| {
            format!("prepare plugin {plugin} output has no value at {pointer:?}")
        })?;
        let rendered = match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            Value::Bool(boolean) => boolean.to_string(),
            other => bail!(
                "prepare plugin {plugin} field {field} at {pointer:?} must be a string, number, \
                 or bool; got {other}"
            ),
        };
        fields.insert(field.clone(), rendered);
    }
    Ok(fields)
}

pub(super) async fn run_prepare_steps(
    pack: PathBuf,
    distribution: gents::pack::PackManifest,
    steps: Vec<ScenarioPrepareStep>,
    tool_root: Option<PathBuf>,
    grant_authority: bool,
) -> Result<BTreeMap<String, String>> {
    if steps.is_empty() {
        return Ok(BTreeMap::new());
    }
    let started = Instant::now();
    let distribution = Arc::new(distribution);
    let mut fields = BTreeMap::new();
    for step in steps {
        let coordinate = format!("{}/{}", distribution.metadata.namespace, step.plugin);
        let (pack, distribution, tool_root, admitted) = (
            pack.clone(),
            distribution.clone(),
            tool_root.clone(),
            step.clone(),
        );
        let (runner, budget, bound) = tokio::task::spawn_blocking(move || {
            admit_prepare_step(
                &pack,
                &distribution,
                &admitted,
                tool_root.as_deref(),
                grant_authority,
            )
        })
        .await
        .context("admitting a scenario prepare step")??;
        match &bound {
            Some(bound) => {
                tracing::info!(
                    plugin = %step.plugin,
                    bound = %bound.path().display(),
                    "running scenario prepare step"
                );
            }
            None => tracing::info!(plugin = %step.plugin, "running scenario prepare step"),
        }
        let outcome = call_runner(&coordinate, runner, step.input, budget, bound)
            .await
            .with_context(|| format!("calling prepare plugin {}", step.plugin))?;
        anyhow::ensure!(
            outcome.verdict == PluginVerdict::Success,
            "prepare plugin {} did not succeed: {:?}: {}",
            step.plugin,
            outcome.verdict,
            outcome.diagnostics
        );
        fields.extend(prepare_seed_fields(
            &step.plugin,
            &outcome.output,
            &step.seed_fields,
        )?);
    }
    tracing::info!(
        fields = fields.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "scenario prepare steps completed"
    );
    Ok(fields)
}

fn admit_prepare_step(
    pack: &Path,
    distribution: &gents::pack::PackManifest,
    step: &ScenarioPrepareStep,
    tool_root: Option<&Path>,
    grant_authority: bool,
) -> Result<(Arc<PluginRunner>, PluginBudget, Option<BoundDir>)> {
    let plugin = distribution
        .metadata
        .plugins
        .iter()
        .find(|declared| declared.name == step.plugin)
        .with_context(|| format!("prepare step names undeclared plugin {}", step.plugin))?;
    let artifact_path = pack.join(&plugin.artifact);
    let source_entry = plugin.source.as_deref().and_then(|source| {
        crate::commands::pack::build::plugin_entry(&plugin.language)
            .map(|entry| pack.join(source).join(entry))
    });
    if source_entry.is_some_and(|entry| entry.is_file()) {
        crate::commands::pack::build::build_plugin(pack, distribution, plugin)
            .with_context(|| format!("building prepare plugin {}", step.plugin))?;
    } else if !artifact_path.is_file() {
        bail!(
            "prepare plugin {} has no source and its artifact is missing: {}",
            step.plugin,
            artifact_path.display()
        );
    }
    let artifact = std::fs::read(&artifact_path)
        .with_context(|| format!("reading {}", artifact_path.display()))?;
    // Explicit consent gates the plugin's declared authority.
    let declared = declared_manifold(plugin)?;
    if let Some(asks) = describe_manifold(&declared) {
        anyhow::ensure!(
            grant_authority,
            "prepare plugin {} asks for {asks}; run with --grant-authority to allow that",
            step.plugin
        );
    }
    let runner = PluginRunner::compile_within(&artifact, plugin, &declared)
        .with_context(|| format!("admitting prepare plugin {}", step.plugin))?;
    let afb = afterburner_cloud::Afb::from_bytes(&artifact)
        .with_context(|| format!("{} is not a readable plugin", plugin.artifact))?;
    let budget = PluginBudget::for_plugin(&afb, plugin);
    let bound = step
        .bind_dir
        .as_ref()
        .map(|dir| {
            BoundDir::new(Path::new(dir), tool_root)
                .with_context(|| format!("binding {dir:?} for prepare plugin {}", step.plugin))
        })
        .transpose()?;
    Ok((Arc::new(runner), budget, bound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::error_chain::ErrorChain;
    use serde_json::json;

    #[tokio::test]
    async fn a_prepare_step_binds_only_the_operator_directory() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();

        let within = tempfile::tempdir().unwrap();
        let allowed = within.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::write(allowed.join("a.txt"), b"a").unwrap();
        let outside = tempfile::tempdir().unwrap();

        let step = |bind_dir: &std::path::Path| ScenarioPrepareStep {
            plugin: "list_files".to_string(),
            input: json!({"root": "unused"}),
            bind_dir: Some(bind_dir.to_string_lossy().into_owned()),
            seed_fields: BTreeMap::from([("files".to_string(), "/files/0".to_string())]),
        };

        let fields = run_prepare_steps(
            fixture.path().to_owned(),
            distribution.clone(),
            vec![step(&allowed)],
            Some(within.path().to_owned()),
            false,
        )
        .await
        .expect("binding a directory inside the resolved tool root must succeed");
        assert_eq!(fields.get("files").map(String::as_str), Some("a.txt"));

        let error = run_prepare_steps(
            fixture.path().to_owned(),
            distribution.clone(),
            vec![step(outside.path())],
            Some(within.path().to_owned()),
            false,
        )
        .await
        .expect_err("a directory outside the resolved tool root must be refused");
        assert!(
            ErrorChain(error.as_ref()).to_string().contains("outside"),
            "{}",
            ErrorChain(error.as_ref())
        );

        let escape = within.path().join("allowed").join("..").join("..");
        let error = run_prepare_steps(
            fixture.path().to_owned(),
            distribution,
            vec![step(&escape)],
            Some(within.path().to_owned()),
            false,
        )
        .await
        .expect_err("a `..` escape out of the resolved tool root must be refused");
        assert!(
            ErrorChain(error.as_ref()).to_string().contains("outside"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[test]
    fn a_missing_prepare_plugin_artifact_is_built_from_source_on_demand() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        let artifact_path = fixture.path().join("plugins/list_files.afb");
        assert!(
            artifact_path.is_file(),
            "fixture must build the artifact first"
        );
        std::fs::remove_file(&artifact_path).unwrap();

        let within = tempfile::tempdir().unwrap();
        let allowed = within.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::write(allowed.join("a.txt"), b"a").unwrap();

        let step = ScenarioPrepareStep {
            plugin: "list_files".to_string(),
            input: json!({"root": "unused"}),
            bind_dir: Some(allowed.to_string_lossy().into_owned()),
            seed_fields: BTreeMap::from([("files".to_string(), "/files/0".to_string())]),
        };

        let _guard = crate::commands::afterburner_build::compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_, _, bound) = admit_prepare_step(
            fixture.path(),
            &distribution,
            &step,
            Some(within.path()),
            false,
        )
        .expect("a missing artifact with a declared source must be built on demand");
        drop(_guard);

        assert!(
            artifact_path.is_file(),
            "the missing artifact must have been rebuilt"
        );
        assert!(bound.is_some());
    }

    #[tokio::test]
    async fn a_prepare_step_runs_from_an_asset_only_pack_copy() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();

        let asset_only = tempfile::tempdir().unwrap();
        std::fs::copy(
            fixture.path().join("manifest.json"),
            asset_only.path().join("manifest.json"),
        )
        .unwrap();
        for asset in &distribution.metadata.assets {
            let from = fixture.path().join(asset);
            let to = asset_only.path().join(asset);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(&from, &to).unwrap();
        }
        assert!(
            !asset_only.path().join("plugins/list_files").is_dir(),
            "the asset-only copy must not ship the plugin's source tree"
        );

        let within = tempfile::tempdir().unwrap();
        let allowed = within.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::write(allowed.join("a.txt"), b"a").unwrap();

        let step = ScenarioPrepareStep {
            plugin: "list_files".to_string(),
            input: json!({"root": "unused"}),
            bind_dir: Some(allowed.to_string_lossy().into_owned()),
            seed_fields: BTreeMap::from([("files".to_string(), "/files/0".to_string())]),
        };

        let fields = run_prepare_steps(
            asset_only.path().to_owned(),
            distribution,
            vec![step],
            Some(within.path().to_owned()),
            false,
        )
        .await
        .expect("an asset-only copy must run its shipped artifact, not attempt a build");
        assert_eq!(fields.get("files").map(String::as_str), Some("a.txt"));
    }

    #[test]
    fn a_missing_artifact_with_no_source_is_refused() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let mut distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        distribution.metadata.plugins[0].source = None;
        std::fs::remove_file(fixture.path().join("plugins/list_files.afb")).unwrap();

        let step = ScenarioPrepareStep {
            plugin: "list_files".to_string(),
            input: json!({"root": "unused"}),
            bind_dir: None,
            seed_fields: BTreeMap::new(),
        };

        let error = admit_prepare_step(fixture.path(), &distribution, &step, None, false)
            .expect_err("no source and a missing artifact must be refused");
        assert!(
            ErrorChain(error.as_ref())
                .to_string()
                .contains("has no source and its artifact is missing"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn a_prepare_step_fetches_through_the_host_from_a_granted_host() {
        let port = crate::commands::plugin::testing::hello_server().await;
        let dir = crate::commands::plugin::testing::http_plugin_pack(port);
        let distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap())
                .unwrap();
        let step = ScenarioPrepareStep {
            plugin: "fetch".to_string(),
            input: json!({"url": format!("http://127.0.0.1:{port}/hello")}),
            bind_dir: None,
            seed_fields: BTreeMap::from([("hello".to_string(), "/hello".to_string())]),
        };
        let fields = run_prepare_steps(dir.path().to_owned(), distribution, vec![step], None, true)
            .await
            .unwrap();
        assert_eq!(fields.get("hello").map(String::as_str), Some("true"));
    }

    #[tokio::test]
    async fn a_prepare_plugin_declaring_authority_requires_grant_authority() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let mut distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        distribution.metadata.plugins[0].manifold = Some(json!({
            "fs": "None",
            "net": "None",
            "crypto": false,
            "child_process": false,
            "env": {"AllowList": ["SOME_VAR"]},
            "allow_exit": false,
            "http_timeout_ms": null,
            "listen": "None"
        }));

        let within = tempfile::tempdir().unwrap();
        let allowed = within.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::write(allowed.join("a.txt"), b"a").unwrap();

        let step = ScenarioPrepareStep {
            plugin: "list_files".to_string(),
            input: json!({"root": "unused"}),
            bind_dir: Some(allowed.to_string_lossy().into_owned()),
            seed_fields: BTreeMap::from([("files".to_string(), "/files/0".to_string())]),
        };

        let error = run_prepare_steps(
            fixture.path().to_owned(),
            distribution.clone(),
            vec![step.clone()],
            Some(within.path().to_owned()),
            false,
        )
        .await
        .expect_err("a plugin declaring authority must be refused without --grant-authority");
        assert!(
            ErrorChain(error.as_ref())
                .to_string()
                .contains("--grant-authority"),
            "{}",
            ErrorChain(error.as_ref())
        );

        let fields = run_prepare_steps(
            fixture.path().to_owned(),
            distribution,
            vec![step],
            Some(within.path().to_owned()),
            true,
        )
        .await
        .expect("--grant-authority admits a plugin under its own declared authority");
        assert_eq!(fields.get("files").map(String::as_str), Some("a.txt"));
    }
}
