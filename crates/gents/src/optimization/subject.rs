//! The baseline subject pack and the candidate packs derived from it.
//!
//! A candidate is the baseline pack with exactly one field changed: the system
//! prompt of the context the subject behavior names, or the prompt template of
//! a task of that behavior. When the pack keeps that text in a sidecar asset —
//! the shape every pack in this repository uses — the change is one file's
//! bytes and nothing else, which is what makes the structural gate's "only the
//! target moved" check a file comparison.
//!
//! Both packs are ordinary directory packs, so the runner takes them through
//! `CellSource::Directory` with no special case. They are loaded and digested
//! by the runner's own pack loader, so the digest the optimizer records is the
//! digest the runner freezes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::document_config::{EvalDefinition, EvalSplit, PackConfig, TriggerSource};
use crate::eval::runner::freeze::{load_pack, write_pack_files};
use crate::eval::runner::CellSource;
use crate::optimization::target::{JobTarget, TargetField};
use crate::pack::{declared_paths, interpolate, PackManifest};

/// The canonical config bundle a sidecar reference or an inline prompt lives in.
const CONFIG_ASSET: &str = "pack_config.json";

/// One arm's pack: where it is, what it digests to, what it declares, and the
/// bytes that digest covers.
#[derive(Clone, Debug)]
pub struct MaterializedPack {
    pub dir: PathBuf,
    pub digest: String,
    pub config: PackConfig,
    pub manifest: PackManifest,
    /// Every declared asset, keyed by its path relative to `dir`.
    pub files: BTreeMap<String, Vec<u8>>,
    pub behavior_id: String,
    pub target: TargetField,
    /// The context the subject behavior names, or the task being optimized.
    pub target_id: String,
    /// The declared asset the target document reads its text from, when the
    /// pack stores it as a sidecar rather than inline.
    pub prompt_asset: Option<String>,
}

/// Read the pack at `dir` as the subject of `behavior_id`, optimizing the
/// behavior's context or, for a task target, that task.
pub fn materialize_pack(
    dir: &Path,
    owner: &str,
    behavior_id: &str,
    target: &JobTarget,
) -> Result<MaterializedPack> {
    let pack = load_pack(&CellSource::Directory(dir.to_path_buf()), owner)
        .with_context(|| format!("loading pack {}", dir.display()))?;

    let behavior = pack
        .config
        .agent_behaviors
        .iter()
        .find(|behavior| behavior.behavior_id == behavior_id)
        .with_context(|| format!("pack declares no behavior {behavior_id:?}"))?;
    let target_id = match target {
        JobTarget::Context => behavior
            .context_id
            .clone()
            .with_context(|| format!("behavior {behavior_id:?} names no context to optimize"))?,
        JobTarget::Task(task_id) => {
            pack.config
                .tasks
                .iter()
                .find(|task| &task.task_id == task_id && task.behavior_id == behavior_id)
                .with_context(|| {
                    format!("pack declares no task {task_id:?} of behavior {behavior_id:?}")
                })?;
            task_id.clone()
        }
    };
    let target = target.field();

    let prompt_asset = sidecar_prompt_asset(&pack.manifest, &pack.files, target, &target_id)?;
    if let Some(asset) = &prompt_asset {
        reject_shared_sidecar(&pack.files, asset)?;
    }

    Ok(MaterializedPack {
        dir: dir.to_path_buf(),
        digest: pack.digest,
        config: pack.config,
        manifest: pack.manifest,
        files: pack.files,
        behavior_id: behavior_id.to_owned(),
        target,
        target_id,
        prompt_asset,
    })
}

/// `Optimization.missingTaskSeedSplits` over the baseline pack's configured
/// seed routes. Filter matching and group completion remain runtime evidence.
pub(crate) fn missing_task_seed_splits(
    config: &PackConfig,
    target: &JobTarget,
    definition: &EvalDefinition,
) -> Vec<EvalSplit> {
    let JobTarget::Task(task_id) = target else {
        return Vec::new();
    };
    let task_enabled = config
        .tasks
        .iter()
        .any(|task| task.task_id == *task_id && task.enabled);
    let collections: Vec<&str> = config
        .triggers
        .iter()
        .filter(|trigger| trigger.enabled && trigger.task_id == *task_id)
        .filter_map(|trigger| match &trigger.source {
            TriggerSource::Event { event_source_id } => Some(event_source_id),
            TriggerSource::Schedule { .. } => None,
        })
        .flat_map(|id| {
            config.event_sources.iter().filter_map(move |source| {
                (source.event_source_id == *id
                    && source.event_kind.as_deref().unwrap_or("created") == "created")
                    .then_some(source.source_collection.as_str())
            })
        })
        .collect();
    [EvalSplit::Train, EvalSplit::Validation]
        .into_iter()
        .filter(|split| {
            !task_enabled
                || !definition.cases.iter().any(|case| {
                    case.split == *split
                        && case.stages.iter().any(|stage| {
                            stage
                                .seed
                                .as_ref()
                                .is_some_and(|seed| collections.contains(&seed.collection.as_str()))
                        })
                })
        })
        .collect()
}

/// The raw target field of `target_id`'s document in `pack_config.json`.
fn raw_target<'a>(
    raw: &'a mut Value,
    target: TargetField,
    target_id: &str,
) -> Option<&'a mut Value> {
    let (array, id_key, field) = target.pack_slot();
    raw[array]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|document| document[id_key].as_str() == Some(target_id))
        .map(|document| &mut document[field])
}

/// A target sidecar another config field also reads would change that
/// document too in a trial, while promotion writes only the target.
fn reject_shared_sidecar(files: &BTreeMap<String, Vec<u8>>, asset: &str) -> Result<()> {
    fn uses(value: &Value, asset: &str) -> usize {
        match value {
            Value::String(text) => {
                usize::from(text.starts_with("./") && text.trim_start_matches("./") == asset)
            }
            Value::Array(items) => items.iter().map(|item| uses(item, asset)).sum(),
            Value::Object(fields) => fields.values().map(|field| uses(field, asset)).sum(),
            _ => 0,
        }
    }
    let Some(bytes) = files.get(CONFIG_ASSET) else {
        return Ok(());
    };
    let raw: Value = serde_json::from_slice(bytes).context("parsing pack_config.json")?;
    anyhow::ensure!(
        uses(&raw, asset) <= 1,
        "{CONFIG_ASSET} reads the target's sidecar {asset:?} from more than one field; a candidate could not change one document alone"
    );
    Ok(())
}

/// The declared asset the target field points at, when it points at one. The
/// raw `pack_config.json` is read rather than the loaded [`PackConfig`],
/// because the loader resolves a sidecar reference into the text it holds and
/// the reference itself is what has to be rewritten.
fn sidecar_prompt_asset(
    manifest: &PackManifest,
    files: &BTreeMap<String, Vec<u8>>,
    target: TargetField,
    target_id: &str,
) -> Result<Option<String>> {
    let Some(bytes) = files.get(CONFIG_ASSET) else {
        return Ok(None);
    };
    let mut raw: Value = serde_json::from_slice(bytes).context("parsing pack_config.json")?;
    let Some(reference) = raw_target(&mut raw, target, target_id).and_then(|value| value.as_str())
    else {
        return Ok(None);
    };
    let normalized = reference.trim_start_matches("./");
    Ok(declared_paths(manifest)
        .into_iter()
        .find(|path| path == normalized))
}

/// The target's current text.
pub fn baseline_text(pack: &MaterializedPack) -> Result<String> {
    if let Some(path) = &pack.prompt_asset {
        let bytes = pack
            .files
            .get(path)
            .with_context(|| format!("pack has no asset {path:?}"))?;
        return String::from_utf8(bytes.clone())
            .with_context(|| format!("asset {path:?} is not UTF-8"));
    }
    Ok(match pack.target {
        TargetField::AgentContextSystemPrompt => pack
            .config
            .contexts
            .iter()
            .find(|context| context.context_id == pack.target_id)
            .and_then(|context| context.system_prompt.clone()),
        TargetField::TaskPromptTemplate => pack
            .config
            .tasks
            .iter()
            .find(|task| task.task_id == pack.target_id)
            .map(|task| task.prompt_template.clone()),
    }
    .unwrap_or_default())
}

/// Write the baseline's declared assets into `dir` with the target's text
/// replaced by `text`, and read the result back as a pack.
///
/// A sidecar asset holds `text` byte for byte, since sidecars are never
/// interpolated. An inline prompt is written in its escaped form
/// ([`interpolate::escape`]) so that the loader reads back exactly `text`.
///
/// `dir` is created; an existing one is refused rather than written over, so a
/// round can never evaluate a directory another round left behind.
pub fn materialize_candidate(
    baseline: &MaterializedPack,
    owner: &str,
    text: &str,
    dir: &Path,
) -> Result<MaterializedPack> {
    anyhow::ensure!(
        !dir.exists(),
        "candidate directory {} already exists",
        dir.display()
    );
    let mut files = baseline.files.clone();
    match &baseline.prompt_asset {
        Some(path) => {
            files.insert(path.clone(), text.as_bytes().to_vec());
        }
        None => {
            let bytes = files
                .get(CONFIG_ASSET)
                .with_context(|| format!("pack has no asset {CONFIG_ASSET:?}"))?;
            let mut raw: Value =
                serde_json::from_slice(bytes).context("parsing pack_config.json")?;
            let field =
                raw_target(&mut raw, baseline.target, &baseline.target_id).with_context(|| {
                    format!(
                        "pack declares no {} {:?}",
                        baseline.target.collection().graphql_type(),
                        baseline.target_id
                    )
                })?;
            // The loader interpolates every string in pack_config.json, so
            // the text is written escaped to be read back as exactly itself.
            *field = Value::String(interpolate::escape(text));
            files.insert(CONFIG_ASSET.to_owned(), serde_json::to_vec_pretty(&raw)?);
        }
    }
    write_pack_files(dir, &files)?;
    materialize_pack(
        dir,
        owner,
        &baseline.behavior_id,
        &baseline.target.job_target(&baseline.target_id),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "did:key:subject-owner";
    pub(crate) use crate::eval::runner::freeze::tests::FIXTURE_PROMPT;

    /// The eval runner's own fixture pack: a manifest, a README, the canonical
    /// config bundle and one behavior sidecar holding [`FIXTURE_PROMPT`].
    pub(crate) fn write_fixture_pack(root: &Path) {
        crate::eval::runner::freeze::tests::write_fixture_pack(root, "Off");
    }

    pub(crate) fn context_pack(dir: &Path) -> Result<MaterializedPack> {
        materialize_pack(dir, OWNER, "monitor", &JobTarget::Context)
    }

    pub(crate) const FIXTURE_TEMPLATE: &str = "Plan {{ doc.goal }} for {{ doc.owner }}.\n";

    #[test]
    fn task_seed_coverage_matches_lean_for_canonical_pack_and_definition_inputs() {
        let rows = &crate::lean_vocab_test::lean_optimization_cases().seed_coverage;
        assert!(!rows.is_empty());
        for row in rows {
            let triggers: Vec<Value> = row
                .triggers
                .iter()
                .enumerate()
                .map(|(index, trigger)| {
                    let source = match &trigger.event_source_id {
                        Some(id) => json!({"kind": "event", "event_source_id": id}),
                        None => json!({"kind": "schedule", "schedule_id": "scheduled"}),
                    };
                    json!({
                        "agent_did": OWNER,
                        "trigger_id": format!("trigger-{index}"),
                        "task_id": trigger.task_id,
                        "enabled": trigger.enabled,
                        "source": source,
                    })
                })
                .collect();
            let sources: Vec<Value> = row
                .sources
                .iter()
                .map(|source| {
                    json!({
                        "agent_did": OWNER,
                        "event_source_id": source.event_source_id,
                        "source_collection": source.source_collection,
                        "event_kind": source.event_kind,
                    })
                })
                .collect();
            let config: PackConfig = serde_json::from_value(json!({
                "agent_principal": {"agent_did": OWNER},
                "tasks": [{
                    "agent_did": OWNER, "task_id": "target", "behavior_id": "monitor",
                    "prompt_template": "Work", "enabled": row.task_enabled,
                }],
                "triggers": triggers,
                "event_sources": sources,
            }))
            .unwrap();
            let cases: Vec<Value> = row
                .cases
                .iter()
                .enumerate()
                .map(|(index, case)| {
                    let mut stages = vec![json!({
                        "stage_id": "prompt", "prompt": "Work", "deadline_secs": 1,
                    })];
                    stages.extend(case.seed_collections.iter().enumerate().map(
                        |(index, collection)| {
                            json!({
                                "stage_id": format!("seed-{index}"), "deadline_secs": 1,
                                "seed": {"collection": collection, "document": {"value": "work"}},
                            })
                        },
                    ));
                    json!({
                        "case_id": format!("case-{index}"), "split": case.split,
                        "fixtures": {"documents": [{"collection": "Work", "document": {"value": "fixture"}}]},
                        "stages": stages,
                    })
                })
                .collect();
            let definition = serde_json::from_value(json!({
                "definition_id": "coverage", "agent_did": OWNER, "comparability_version": 1,
                "subject": {"kind": "behavior", "inference_slots": ["primary"]},
                "fixtures": {"documents": [{"collection": "Work", "document": {"value": "shared fixture"}}]},
                "cases": cases,
            }))
            .unwrap();
            let target = row
                .target_task
                .as_ref()
                .map_or(JobTarget::Context, |task| JobTarget::Task(task.clone()));
            assert_eq!(
                serde_json::to_value(missing_task_seed_splits(&config, &target, &definition))
                    .unwrap(),
                json!(row.missing_splits),
                "{}",
                row.name,
            );
        }
    }

    /// The fixture pack with one task of the monitor behavior, its prompt
    /// template in a sidecar or inline in `pack_config.json`, fired by an
    /// event trigger whose source has no group.
    pub(crate) fn write_task_fixture_pack(root: &Path, inline: bool) {
        write_fixture_pack(root);
        let manifest_path = root.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let template = if inline {
            json!(FIXTURE_TEMPLATE)
        } else {
            std::fs::create_dir_all(root.join("tasks/plan")).unwrap();
            std::fs::write(root.join("tasks/plan/prompt.md"), FIXTURE_TEMPLATE).unwrap();
            manifest["assets"]
                .as_array_mut()
                .unwrap()
                .push(json!("tasks/plan/prompt.md"));
            json!("./tasks/plan/prompt.md")
        };
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let config_path = root.join("pack_config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["tasks"] = json!([{
            "task_id": "plan",
            "behavior_id": "monitor",
            "prompt_template": template,
        }]);
        config["event_sources"] = json!([{
            "event_source_id": "plan-source",
            "source_collection": "PlanItem",
        }]);
        config["triggers"] = json!([{
            "trigger_id": "plan-trigger",
            "task_id": "plan",
            "source": {"kind": "event", "event_source_id": "plan-source"},
        }]);
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    }

    fn task_pack(dir: &Path) -> Result<MaterializedPack> {
        materialize_pack(dir, OWNER, "monitor", &JobTarget::Task("plan".into()))
    }

    #[test]
    fn a_task_candidate_rewrites_the_task_sidecar_and_nothing_else() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_task_fixture_pack(&baseline_dir, false);
        let baseline = task_pack(&baseline_dir).unwrap();
        assert_eq!(baseline.target, TargetField::TaskPromptTemplate);
        assert_eq!(baseline.target_id, "plan");
        assert_eq!(
            baseline.prompt_asset.as_deref(),
            Some("tasks/plan/prompt.md")
        );
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_TEMPLATE);

        let text = "Do {{ args.goal }} for {{ doc.owner }}.\n";
        let candidate =
            materialize_candidate(&baseline, OWNER, text, &dirs.path().join("candidate")).unwrap();
        let differing: Vec<&String> = baseline
            .files
            .iter()
            .filter(|(path, bytes)| candidate.files.get(*path) != Some(*bytes))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(differing, vec!["tasks/plan/prompt.md"]);
        assert_eq!(baseline_text(&candidate).unwrap(), text);
        assert_eq!(candidate.target_id, "plan");

        // The same pack as a context subject leaves the task alone.
        let context = context_pack(&baseline_dir).unwrap();
        assert_eq!(context.target_id, "monitor-context");
        assert_eq!(baseline_text(&context).unwrap(), FIXTURE_PROMPT);
    }

    #[test]
    fn an_inline_task_template_is_rewritten_in_the_config() {
        let dirs = tempfile::tempdir().unwrap();
        write_task_fixture_pack(&dirs.path().join("baseline"), true);
        let baseline = task_pack(&dirs.path().join("baseline")).unwrap();
        assert_eq!(baseline.prompt_asset, None);
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_TEMPLATE);
        let candidate = materialize_candidate(
            &baseline,
            OWNER,
            "New {{ args.goal }}.\n",
            &dirs.path().join("c"),
        )
        .unwrap();
        assert_eq!(baseline_text(&candidate).unwrap(), "New {{ args.goal }}.\n");
        let loaded = load_pack(&CellSource::Directory(candidate.dir.clone()), OWNER).unwrap();
        assert_eq!(
            loaded.config.tasks[0].prompt_template,
            "New {{ args.goal }}.\n"
        );
    }

    /// A sidecar two fields read would change both documents in a trial,
    /// while promotion writes only the target; a shared sidecar the target
    /// does not read never changes.
    #[test]
    fn only_a_target_sidecar_referenced_from_two_fields_is_an_error() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_task_fixture_pack(&baseline_dir, false);
        let config_path = baseline_dir.join("pack_config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["tasks"].as_array_mut().unwrap().push(json!({
            "task_id": "review",
            "behavior_id": "monitor",
            "prompt_template": "./tasks/plan/prompt.md",
        }));
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        let error = task_pack(&baseline_dir).unwrap_err();
        assert!(
            format!("{error:#}").contains("tasks/plan/prompt.md"),
            "{error:#}"
        );
        let context = context_pack(&baseline_dir).unwrap();
        assert_eq!(
            context.prompt_asset.as_deref(),
            Some("agent_behaviors/monitor/system_prompt.md")
        );
    }

    #[test]
    fn a_task_the_pack_does_not_declare_is_an_error() {
        let dirs = tempfile::tempdir().unwrap();
        write_fixture_pack(&dirs.path().join("baseline"));
        let error = task_pack(&dirs.path().join("baseline")).unwrap_err();
        assert!(format!("{error:#}").contains("plan"), "{error:#}");
    }

    #[test]
    fn a_candidate_is_the_baseline_pack_with_one_file_rewritten() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let baseline = context_pack(&baseline_dir).unwrap();

        assert_eq!(baseline.target_id, "monitor-context");
        assert_eq!(
            baseline.prompt_asset.as_deref(),
            Some("agent_behaviors/monitor/system_prompt.md")
        );
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_PROMPT);
        assert!(baseline.digest.starts_with("sha256:"));

        let candidate_dir = dirs.path().join("candidate");
        let candidate = materialize_candidate(
            &baseline,
            OWNER,
            "Watch the mailbox, and say why.\n",
            &candidate_dir,
        )
        .unwrap();

        assert_eq!(
            baseline.files.keys().collect::<Vec<_>>(),
            candidate.files.keys().collect::<Vec<_>>(),
            "a candidate declares exactly the baseline's assets"
        );
        let differing: Vec<&String> = baseline
            .files
            .iter()
            .filter(|(path, bytes)| candidate.files.get(*path) != Some(*bytes))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(differing, vec!["agent_behaviors/monitor/system_prompt.md"]);
        assert_eq!(
            baseline_text(&candidate).unwrap(),
            "Watch the mailbox, and say why.\n"
        );
        assert_ne!(
            candidate.digest, baseline.digest,
            "one changed byte is a new pack"
        );
        assert!(candidate_dir.join("manifest.json").exists());
    }

    #[test]
    fn the_same_text_materializes_to_the_same_digest() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let baseline = context_pack(&baseline_dir).unwrap();

        let one = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("one"))
            .unwrap();
        let two = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("two"))
            .unwrap();
        assert_eq!(
            one.digest, two.digest,
            "the digest is over content, not location"
        );

        let same =
            materialize_candidate(&baseline, OWNER, FIXTURE_PROMPT, &dirs.path().join("same"))
                .unwrap();
        assert_eq!(
            same.digest, baseline.digest,
            "rewriting the prompt with its own text is the baseline pack"
        );
    }

    #[test]
    fn a_behavior_the_pack_does_not_declare_is_an_error() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let error = materialize_pack(
            &baseline_dir,
            OWNER,
            "no-such-behavior",
            &JobTarget::Context,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("no-such-behavior"),
            "{error:#}"
        );
    }

    /// The same pack with the prompt inline in `pack_config.json` and no
    /// sidecar asset, for the structural gate's inline branch.
    pub(crate) fn write_inline_fixture_pack(root: &Path) {
        write_fixture_pack(root);
        std::fs::remove_file(root.join("agent_behaviors/monitor/system_prompt.md")).unwrap();
        let manifest_path = root.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["assets"] = json!(["README.md", "pack_config.json"]);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let config_path = root.join("pack_config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["contexts"][0]["system_prompt"] = json!(FIXTURE_PROMPT);
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    }

    #[test]
    fn an_inline_prompt_is_rewritten_in_the_config_and_nowhere_else() {
        let dirs = tempfile::tempdir().unwrap();
        write_inline_fixture_pack(&dirs.path().join("baseline"));
        let baseline = context_pack(&dirs.path().join("baseline")).unwrap();
        assert_eq!(baseline.prompt_asset, None);
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_PROMPT);
        let candidate =
            materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("c")).unwrap();
        assert_eq!(baseline_text(&candidate).unwrap(), "New text.\n");
        assert_ne!(candidate.digest, baseline.digest);
    }

    /// An inline prompt is written into `pack_config.json`, which the loader
    /// interpolates; the candidate must still evaluate exactly the text.
    #[test]
    fn an_inline_prompt_with_dollar_signs_loads_back_as_itself() {
        let text = "Home is ${HOME}, pay $$5, and ${GENTS_T21_SURELY_UNSET}.\n";
        assert!(std::env::var("GENTS_T21_SURELY_UNSET").is_err());
        let dirs = tempfile::tempdir().unwrap();
        write_inline_fixture_pack(&dirs.path().join("baseline"));
        let baseline = context_pack(&dirs.path().join("baseline")).unwrap();
        let candidate =
            materialize_candidate(&baseline, OWNER, text, &dirs.path().join("c")).unwrap();
        assert_eq!(baseline_text(&candidate).unwrap(), text);

        let loaded = load_pack(&CellSource::Directory(candidate.dir.clone()), OWNER).unwrap();
        let prompt = loaded
            .config
            .contexts
            .iter()
            .find(|context| context.context_id == candidate.target_id)
            .and_then(|context| context.system_prompt.clone());
        assert_eq!(prompt.as_deref(), Some(text));
    }
}
