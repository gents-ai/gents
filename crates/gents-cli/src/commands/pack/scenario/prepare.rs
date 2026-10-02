//! `experiment.json` `prepare` steps: host-run plugin calls before the seed.
//!
//! Each step admits a pack plugin under its own declared authority, calls
//! it (optionally bound read-only to an operator-resolved ceiling
//! directory), and maps its own output onto new `seed.fields` entries by
//! JSON pointer. Steps run in manifest order, after every event source is
//! observed and before the seed.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use gents::plugin::authority::{declared_manifold, describe_manifold};
use gents::plugin::{BoundDir, PluginBudget, PluginRunner, PluginVerdict};

#[cfg(test)]
use super::validate_manifest;
use super::ScenarioManifest;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioPrepareStep {
    pub(super) plugin: String,
    #[serde(default = "default_prepare_input")]
    pub(super) input: Value,
    /// A directory this step binds read-only into the named plugin's
    /// declared `bind_dir` field, resolved against the process cwd and,
    /// when `init.tool_root` is set, required to stay inside it (see
    /// [`gents::plugin::BoundDir::new`]). Present exactly when the plugin
    /// declares `bind_dir` ([`validate_prepare_steps`]).
    #[serde(default)]
    pub(super) bind_dir: Option<String>,
    /// Maps a JSON Pointer into the plugin's output onto a new `seed.fields`
    /// entry; `""` selects the whole output value.
    pub(super) seed_fields: BTreeMap<String, String>,
}

fn default_prepare_input() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Validates every `prepare` step against the pack's own declared plugins
/// and the seed fields `seed.fields`, `seed.job_id_field`, and
/// `seed.prompt_field` already claim. A `prepare` step's `input` must be an
/// object: the plugin ABI accepts nothing else, and refusing it here fails
/// before the runtime starts rather than deep inside `call`/`call_bound`.
/// Duplicate seed field names, whether between two `prepare` steps or
/// against a seed field the manifest already declares, are refused here so
/// insertion order never has to decide a winner.
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

/// Maps a prepare step's `seed_fields` pointers onto its plugin's own JSON
/// output, coercing to the same string values `seed.fields` and the seed's
/// GraphQL create mutation always carry. A pointer that does not resolve, or
/// resolves to anything but a string, number, or bool, is refused: a
/// mismatched field fails the run here rather than seeding an unusable
/// value.
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

/// Runs every `prepare` step in manifest order, returning the seed fields
/// their outputs mapped ([`validate_prepare_steps`] refuses two steps from
/// declaring the same seed field, so insertion order never matters here). A
/// no-op with no steps: neither the admission nor the blocking hand-off
/// below has anything to do.
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
    let fields = tokio::task::spawn_blocking(move || {
        run_prepare_steps_blocking(
            &pack,
            &distribution,
            &steps,
            tool_root.as_deref(),
            grant_authority,
        )
    })
    .await
    .context("running scenario prepare steps")??;
    tracing::info!(
        fields = fields.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "scenario prepare steps completed"
    );
    Ok(fields)
}

/// The blocking half of [`run_prepare_steps`]: everything from finding the
/// plugin through calling it and mapping its output has to run off the async
/// executor, exactly like `commands::plugin::run::run_plugin` and
/// `commands::pack::test::run_plugin_cases` (running a guest panics when
/// called directly from an async task).
fn run_prepare_steps_blocking(
    pack: &Path,
    distribution: &gents::pack::PackManifest,
    steps: &[ScenarioPrepareStep],
    tool_root: Option<&Path>,
    grant_authority: bool,
) -> Result<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for step in steps {
        let plugin = distribution
            .metadata
            .plugins
            .iter()
            .find(|declared| declared.name == step.plugin)
            .with_context(|| format!("prepare step names undeclared plugin {}", step.plugin))?;
        let artifact_path = pack.join(&plugin.artifact);
        // A pack directory holding only its declared assets (a built
        // `.pack`, the home's store, or a registry fetch) keeps
        // `plugin.source` in the manifest but never ships the source tree
        // itself, so a source is only actually available here when its
        // entry file is really on disk. Rebuild from it when it is; fall
        // back to the shipped artifact otherwise, exactly like a plugin
        // with no declared source at all.
        let source_entry = plugin.source.as_deref().and_then(|source| {
            crate::commands::pack::build::plugin_entry(&plugin.language)
                .map(|entry| pack.join(source).join(entry))
        });
        if source_entry.is_some_and(|entry| entry.is_file()) {
            // Always rebuilt, not just when missing: `build_plugin` stamps
            // the source digest, so an unchanged source costs one digest
            // check, while a source an author edited since the last build
            // never runs stale here.
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
        // The author's own plugin runs with exactly the authority it
        // declares (mirrors `commands::pack::test::run_plugin_cases`);
        // consent is the gate below, not a narrower ceiling.
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
        let outcome = match &bound {
            Some(bound) => runner.call_bound(&step.input, &budget, bound),
            None => runner.call(&step.input, &budget),
        };
        let outcome = outcome.with_context(|| format!("calling prepare plugin {}", step.plugin))?;
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
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal declared plugin, with or without `bind_dir`, for exercising
    /// `validate_prepare_steps` without a real `.afb`.
    fn scenario_plugin(name: &str, bind_dir: bool) -> gents::pack::PackPlugin {
        gents::pack::PackPlugin {
            name: name.to_string(),
            description: String::new(),
            artifact: format!("plugins/{name}.afb"),
            source: None,
            language: "rust".to_string(),
            input_schema: json!({"type": "object"}),
            manifold: None,
            instructions: None,
            bind_dir: bind_dir.then(|| gents::pack::PluginDirBinding {
                input_field: "root".to_string(),
                original_field: None,
                description: "scan target".to_string(),
                access: Default::default(),
            }),
            limits: None,
            model_slot: None,
        }
    }

    #[test]
    fn manifest_parses_prepare_steps_and_refuses_colliding_seed_fields() {
        let mut manifest: ScenarioManifest = serde_json::from_value(serde_json::json!({
            "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
            "seed": {
                "collection": "PrepJob", "job_id_field": "run_id", "prompt_field": "focus",
                "fields": {"existing": "x"}
            },
            "expect": {"trigger_ids": []},
            "prepare": [{
                "plugin": "scanner",
                "input": {"max_payload_chars": "1024"},
                "bind_dir": "/tmp",
                "seed_fields": {"candidates": "/payload"}
            }]
        }))
        .expect("manifest with prepare");
        assert_eq!(manifest.prepare.len(), 1);
        assert_eq!(manifest.prepare[0].plugin, "scanner");
        assert_eq!(manifest.prepare[0].bind_dir.as_deref(), Some("/tmp"));

        let bare: ScenarioManifest = serde_json::from_value(serde_json::json!({
            "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
            "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
            "expect": {"trigger_ids": []}
        }))
        .expect("manifest without prepare");
        assert!(bare.prepare.is_empty());

        manifest.plugins = vec![scenario_plugin("scanner", true)];
        validate_manifest(&manifest).expect("a declared plugin with a matching bind_dir is valid");

        manifest.prepare[0]
            .seed_fields
            .insert("existing".to_string(), "/payload".to_string());
        let error = validate_manifest(&manifest)
            .expect_err("a seed field colliding with seed.fields must be refused");
        assert!(error.to_string().contains("existing"), "{error}");
    }

    #[test]
    fn prepare_step_validation_requires_object_input_and_refuses_seed_identity_field_collisions() {
        fn manifest(prepare_step: serde_json::Value) -> ScenarioManifest {
            serde_json::from_value(serde_json::json!({
                "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
                "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
                "expect": {"trigger_ids": []},
                "prepare": [prepare_step]
            }))
            .unwrap()
        }

        let mut non_object_input = manifest(json!({
            "plugin": "scanner", "input": "not-an-object", "seed_fields": {}
        }));
        non_object_input.plugins = vec![scenario_plugin("scanner", false)];
        let error = validate_manifest(&non_object_input)
            .expect_err("a non-object prepare input must be refused");
        assert!(
            error.to_string().contains("must be a JSON object"),
            "{error}"
        );

        let mut job_id_collision = manifest(json!({
            "plugin": "scanner", "seed_fields": {"run_id": "/id"}
        }));
        job_id_collision.plugins = vec![scenario_plugin("scanner", false)];
        let error = validate_manifest(&job_id_collision)
            .expect_err("a seed field named like seed.job_id_field must be refused");
        assert!(error.to_string().contains("run_id"), "{error}");

        let mut prompt_collision = manifest(json!({
            "plugin": "scanner", "seed_fields": {"focus": "/id"}
        }));
        prompt_collision.plugins = vec![scenario_plugin("scanner", false)];
        let error = validate_manifest(&prompt_collision)
            .expect_err("a seed field named like seed.prompt_field must be refused");
        assert!(error.to_string().contains("focus"), "{error}");
    }

    #[test]
    fn prepare_step_validation_refuses_undeclared_plugins_and_bind_dir_mismatches() {
        fn manifest(prepare_step: serde_json::Value) -> ScenarioManifest {
            serde_json::from_value(serde_json::json!({
                "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
                "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
                "expect": {"trigger_ids": []},
                "prepare": [prepare_step]
            }))
            .unwrap()
        }

        let undeclared = manifest(json!({"plugin": "scanner", "seed_fields": {}}));
        let error =
            validate_manifest(&undeclared).expect_err("an undeclared plugin must be refused");
        assert!(error.to_string().contains("undeclared plugin"), "{error}");

        let mut missing_bind = manifest(json!({"plugin": "scanner", "seed_fields": {}}));
        missing_bind.plugins = vec![scenario_plugin("scanner", true)];
        let error = validate_manifest(&missing_bind)
            .expect_err("a step must set bind_dir when the plugin declares one");
        assert!(error.to_string().contains("must set bind_dir"), "{error}");

        let mut unexpected_bind = manifest(json!({
            "plugin": "scanner", "bind_dir": "/tmp", "seed_fields": {}
        }));
        unexpected_bind.plugins = vec![scenario_plugin("scanner", false)];
        let error = validate_manifest(&unexpected_bind)
            .expect_err("a step must not set bind_dir when the plugin declares none");
        assert!(error.to_string().contains("declares none"), "{error}");

        let mut bad_pointer = manifest(json!({
            "plugin": "scanner", "seed_fields": {"candidates": "payload"}
        }));
        bad_pointer.plugins = vec![scenario_plugin("scanner", false)];
        let error = validate_manifest(&bad_pointer)
            .expect_err("a pointer must be \"\" or start with \"/\"");
        assert!(error.to_string().contains("start with"), "{error}");
    }

    #[test]
    fn prepare_seed_fields_map_output_pointers() {
        let output = json!({
            "manifest": {"page_count": 3, "sealed": true},
            "summary": "PINNED BASE: a\nPINNED HEAD: b",
        });
        let seed_fields = BTreeMap::from([
            ("summary".to_string(), "/summary".to_string()),
            ("page_count".to_string(), "/manifest/page_count".to_string()),
            ("sealed".to_string(), "/manifest/sealed".to_string()),
        ]);
        let fields = prepare_seed_fields("scanner", &output, &seed_fields).unwrap();
        assert_eq!(
            fields.get("summary").map(String::as_str),
            Some("PINNED BASE: a\nPINNED HEAD: b")
        );
        assert_eq!(fields.get("page_count").map(String::as_str), Some("3"));
        assert_eq!(fields.get("sealed").map(String::as_str), Some("true"));

        // "" selects the whole output value, usable only when that whole
        // value is itself a string, number, or bool.
        let scalar_output = json!("bare-string-output");
        let whole = BTreeMap::from([("whole".to_string(), String::new())]);
        let fields = prepare_seed_fields("scanner", &scalar_output, &whole).unwrap();
        assert_eq!(
            fields.get("whole").map(String::as_str),
            Some("bare-string-output")
        );

        let missing = BTreeMap::from([("nope".to_string(), "/absent".to_string())]);
        assert!(prepare_seed_fields("scanner", &output, &missing).is_err());

        let wrong_type = BTreeMap::from([("manifest".to_string(), "/manifest".to_string())]);
        let error = prepare_seed_fields("scanner", &output, &wrong_type).unwrap_err();
        assert!(format!("{error:#}").contains("string, number, or bool"));
    }

    /// Proves the wiring from a scenario's `prepare` step through to
    /// `BoundDir`: the plugin sees exactly the operator-bound directory
    /// (and the seed field it produced), never one it names itself, and a
    /// `bind_dir` stepping outside the resolved `init.tool_root` is refused
    /// before the plugin ever runs (`BoundDir`'s own unit tests, in
    /// `gents::plugin`, cover the canonicalization and symlink cases this
    /// wiring relies on).
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
        assert!(format!("{error:#}").contains("outside"), "{error:#}");

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
        assert!(format!("{error:#}").contains("outside"), "{error:#}");
    }

    /// Regression for the missing-coverage finding on the production path a
    /// fresh packs checkout always takes: `plugins/*.afb` is gitignored, so
    /// the artifact is missing on every clean clone and must be built from
    /// `source` on demand. `run_prepare_steps_blocking` is synchronous and
    /// has no `.await`, so it is called directly here while holding
    /// `afterburner_build::compile_lock()`, exactly like
    /// `build_bind_plugin_fixture` does for its own build.
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
        let fields = run_prepare_steps_blocking(
            fixture.path(),
            &distribution,
            &[step],
            Some(within.path()),
            false,
        )
        .expect("a missing artifact with a declared source must be built on demand");
        drop(_guard);

        assert!(
            artifact_path.is_file(),
            "the missing artifact must have been rebuilt"
        );
        assert_eq!(fields.get("files").map(String::as_str), Some("a.txt"));
    }

    /// A pack directory holding only its declared assets, the shape a built
    /// `.pack`, the home's store, or a registry fetch actually ships, keeps
    /// `plugin.source` in the manifest without shipping the source tree
    /// itself (`bind_plugin_fixture`'s `plugins/list_files/source` is not
    /// in `assets`). The prepare step must run the shipped artifact rather
    /// than attempt a build that has nothing to build from.
    #[test]
    fn a_prepare_step_runs_from_an_asset_only_pack_copy() {
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

        let fields = run_prepare_steps_blocking(
            asset_only.path(),
            &distribution,
            &[step],
            Some(within.path()),
            false,
        )
        .expect("an asset-only copy must run its shipped artifact, not attempt a build");
        assert_eq!(fields.get("files").map(String::as_str), Some("a.txt"));
    }

    /// The other half of the missing-artifact path: a plugin with no
    /// `source` and a missing artifact has nothing to build from, so it is
    /// refused with an actionable message rather than a bare I/O error.
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

        let error = run_prepare_steps_blocking(fixture.path(), &distribution, &[step], None, false)
            .expect_err("no source and a missing artifact must be refused");
        assert!(
            format!("{error:#}").contains("has no source and its artifact is missing"),
            "{error:#}"
        );
    }

    /// The consent gate: a prepare plugin whose manifest declares authority
    /// beyond the sealed default must be refused unless `--grant-authority`
    /// is passed, and passing it admits the plugin under that authority.
    #[tokio::test]
    async fn a_prepare_plugin_declaring_authority_requires_grant_authority() {
        let fixture = crate::commands::plugin::testing::build_bind_plugin_fixture();
        let mut distribution: gents::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(fixture.path().join("manifest.json")).unwrap())
                .unwrap();
        // `fs` is mutually exclusive with `bind_dir` (`PackPlugin::validate`),
        // so `env` is the non-sealed axis exercised here.
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
            format!("{error:#}").contains("--grant-authority"),
            "{error:#}"
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
