use std::collections::BTreeSet;

use gents_protocol::error_chain::ErrorChain;
use serde_json::Value;

use crate::config_client::DesiredStateApplyPlan;
use crate::optimization::subject::{baseline_text, MaterializedPack};
use crate::optimization::target::TargetField;
use crate::pack::interpolate;
use crate::template::parse_template_for_validation;
use crate::{Collection, ConfigReferences};

mod seeds;
pub(crate) use seeds::seed_gate;

const CONFIG_ASSET: &str = "pack_config.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralRejection {
    pub reason: &'static str,
    pub detail: String,
}

impl StructuralRejection {
    pub fn diagnostics(&self) -> String {
        format!("{}: {}", self.reason, self.detail)
    }
}

impl std::fmt::Display for StructuralRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostics())
    }
}

impl std::error::Error for StructuralRejection {}

fn reject(reason: &'static str, detail: impl Into<String>) -> StructuralRejection {
    StructuralRejection {
        reason,
        detail: detail.into(),
    }
}

fn raw_config(pack: &MaterializedPack) -> Result<Value, StructuralRejection> {
    let bytes = pack.files.get(CONFIG_ASSET).ok_or_else(|| {
        reject(
            "unexpected_change",
            format!("the pack has no {CONFIG_ASSET}"),
        )
    })?;
    serde_json::from_slice(bytes).map_err(|error| {
        reject(
            "unexpected_change",
            format!("{CONFIG_ASSET} does not parse: {error}"),
        )
    })
}

fn split_prompt(
    mut raw: Value,
    pack: &MaterializedPack,
) -> Result<(Value, Value), StructuralRejection> {
    let (array, id_key, field) = pack.target.pack_slot();
    let document = raw[array]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|document| document[id_key].as_str() == Some(pack.target_id.as_str()))
        .ok_or_else(|| {
            reject(
                "unexpected_change",
                format!("no {array} entry {:?}", pack.target_id),
            )
        })?;
    let prompt = document[field].take();
    Ok((prompt, raw))
}

pub fn text_gate(
    baseline: &MaterializedPack,
    text: &str,
    max_text_bytes: usize,
) -> Result<(), StructuralRejection> {
    if text.trim().is_empty() {
        return Err(reject(
            "empty_text",
            "a candidate prompt must say something",
        ));
    }
    if text.len() > max_text_bytes {
        return Err(reject(
            "text_too_long",
            format!("{} bytes exceeds the {max_text_bytes} byte cap", text.len()),
        ));
    }
    if baseline.prompt_asset.is_none() && text.starts_with("./") {
        return Err(reject(
            "unexpected_change",
            "an inline prompt beginning with ./ is read by the pack loader as a sidecar path",
        ));
    }
    if baseline.target == TargetField::TaskPromptTemplate {
        let current = baseline_text(baseline)
            .map_err(|error| reject("unexpected_change", ErrorChain(&error).to_string()))?;
        let (current, candidate) = (template_variables(&current)?, template_variables(text)?);
        let dropped: Vec<&String> = current.difference(&candidate).collect();
        if !dropped.is_empty() {
            return Err(reject(
                "template_variables_dropped",
                format!("the candidate template no longer references {dropped:?}"),
            ));
        }
        owner_validation(baseline, text)?;
    }
    Ok(())
}

fn owner_validation(baseline: &MaterializedPack, text: &str) -> Result<(), StructuralRejection> {
    let invalid = |error: &(dyn std::error::Error + 'static)| {
        reject("template_invalid", format!("{}", ErrorChain(error)))
    };
    let mut config = baseline.config.clone();
    let task = config
        .tasks
        .iter_mut()
        .find(|task| task.task_id == baseline.target_id)
        .ok_or_else(|| {
            reject(
                "unexpected_change",
                format!("pack declares no task {:?}", baseline.target_id),
            )
        })?;
    task.prompt_template = text.to_owned();
    task.validate().map_err(|error| invalid(&error))?;
    let plan = DesiredStateApplyPlan::from_pack_config(&config).map_err(|error| invalid(&error))?;
    let references = ConfigReferences::from_documents(
        &config.node.node_did,
        plan.documents()
            .iter()
            .map(|document| (document.collection, document.add.clone())),
    )
    .map_err(|error| invalid(&error))?;
    plan.documents()
        .iter()
        .filter(|document| document.collection == Collection::Trigger)
        .try_for_each(|document| {
            references
                .validate_document(Collection::Trigger, &document.add)
                .map_err(|error| invalid(&error))
        })
}

fn template_variables(template: &str) -> Result<BTreeSet<String>, StructuralRejection> {
    parse_template_for_validation(template)
        .map(|references| {
            references
                .into_iter()
                .map(|reference| reference.path.join("."))
                .collect()
        })
        .map_err(|error| {
            reject(
                "template_invalid",
                format!("the template does not parse: {error}"),
            )
        })
}

pub fn structural_gate(
    baseline: &MaterializedPack,
    candidate: &MaterializedPack,
    text: &str,
    max_text_bytes: usize,
    seen_digests: &[String],
) -> Result<(), StructuralRejection> {
    text_gate(baseline, text, max_text_bytes)?;

    if seen_digests
        .iter()
        .any(|digest| digest == &candidate.digest)
    {
        return Err(reject(
            "duplicate_candidate",
            format!(
                "candidate digests to {}, which the job has already evaluated",
                candidate.digest
            ),
        ));
    }

    let baseline_paths: BTreeSet<&String> = baseline.files.keys().collect();
    let candidate_paths: BTreeSet<&String> = candidate.files.keys().collect();
    if baseline_paths != candidate_paths {
        let added: Vec<&&String> = candidate_paths.difference(&baseline_paths).collect();
        let removed: Vec<&&String> = baseline_paths.difference(&candidate_paths).collect();
        return Err(reject(
            "unexpected_change",
            format!("declared assets changed: added {added:?}, removed {removed:?}"),
        ));
    }
    let changed: Vec<&String> = baseline
        .files
        .iter()
        .filter(|(path, bytes)| candidate.files.get(*path) != Some(*bytes))
        .map(|(path, _)| path)
        .collect();
    let allowed = baseline
        .prompt_asset
        .clone()
        .unwrap_or_else(|| CONFIG_ASSET.to_owned());
    if changed.len() != 1 || changed[0] != &allowed {
        return Err(reject(
            "unexpected_change",
            format!("expected only {allowed:?} to change, but {changed:?} did"),
        ));
    }
    if (candidate.target, &candidate.target_id) != (baseline.target, &baseline.target_id) {
        return Err(reject(
            "unexpected_change",
            format!(
                "the target moved from {:?} to {:?}",
                baseline.target_id, candidate.target_id
            ),
        ));
    }

    match &baseline.prompt_asset {
        Some(asset) => {
            if candidate.files.get(asset).map(Vec::as_slice) != Some(text.as_bytes()) {
                return Err(reject(
                    "text_mismatch",
                    format!("{asset:?} does not hold the proposed text"),
                ));
            }
        }
        None => {
            let (_, baseline_rest) = split_prompt(raw_config(baseline)?, baseline)?;
            let (prompt, candidate_rest) = split_prompt(raw_config(candidate)?, candidate)?;
            if baseline_rest != candidate_rest {
                let (array, _, field) = baseline.target.pack_slot();
                return Err(reject(
                    "unexpected_change",
                    format!(
                        "{CONFIG_ASSET} changed besides {array}[{:?}].{field}",
                        baseline.target_id
                    ),
                ));
            }
            if prompt.as_str() != Some(interpolate::escape(text).as_str()) {
                return Err(reject(
                    "text_mismatch",
                    "the inline target field is not the proposed text",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimization::subject::tests::{
        write_fixture_pack, write_inline_fixture_pack, write_task_fixture_pack, FIXTURE_PROMPT,
        FIXTURE_TEMPLATE,
    };
    use crate::optimization::subject::{materialize_candidate, materialize_pack};
    use crate::optimization::target::JobTarget;

    const OWNER: &str = "did:key:gate-owner";
    const TEXT: &str = "Watch the mailbox, and say why.\n";

    struct Fixture {
        _dirs: tempfile::TempDir,
        root: std::path::PathBuf,
        baseline: MaterializedPack,
    }

    fn fixture(inline: bool) -> Fixture {
        let dirs = tempfile::tempdir().unwrap();
        let root = dirs.path().to_path_buf();
        if inline {
            write_inline_fixture_pack(&root.join("baseline"));
        } else {
            write_fixture_pack(&root.join("baseline"));
        }
        let baseline = materialize_pack(
            &root.join("baseline"),
            OWNER,
            "monitor",
            &JobTarget::Context,
        )
        .unwrap();
        Fixture {
            _dirs: dirs,
            root,
            baseline,
        }
    }

    fn candidate(fixture: &Fixture, text: &str, name: &str) -> MaterializedPack {
        materialize_candidate(&fixture.baseline, OWNER, text, &fixture.root.join(name)).unwrap()
    }

    fn gate(
        fixture: &Fixture,
        candidate: &MaterializedPack,
        text: &str,
    ) -> Result<(), StructuralRejection> {
        structural_gate(
            &fixture.baseline,
            candidate,
            text,
            32 * 1024,
            std::slice::from_ref(&fixture.baseline.digest),
        )
    }

    fn task_fixture(inline: bool) -> Fixture {
        let dirs = tempfile::tempdir().unwrap();
        let root = dirs.path().to_path_buf();
        write_task_fixture_pack(&root.join("baseline"), inline);
        let baseline = materialize_pack(
            &root.join("baseline"),
            OWNER,
            "monitor",
            &JobTarget::Task("plan".into()),
        )
        .unwrap();
        Fixture {
            _dirs: dirs,
            root,
            baseline,
        }
    }

    const TEMPLATE: &str = "Do {{ doc.goal }} for {{ doc.owner }}, and say why.\n";

    #[test]
    fn a_one_field_task_candidate_passes_in_a_sidecar_and_inline() {
        for inline in [false, true] {
            let fixture = task_fixture(inline);
            assert_eq!(fixture.baseline.prompt_asset.is_none(), inline);
            let candidate = candidate(&fixture, TEMPLATE, "c1");
            gate(&fixture, &candidate, TEMPLATE).unwrap();
            let rejection = gate(&fixture, &candidate, FIXTURE_TEMPLATE).unwrap_err();
            assert_eq!(rejection.reason, "text_mismatch");
        }
    }

    #[test]
    fn an_inline_task_candidate_that_changed_another_task_field_is_rejected() {
        let fixture = task_fixture(true);
        let mut tampered = candidate(&fixture, TEMPLATE, "c2");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&tampered.files["pack_config.json"]).unwrap();
        raw["tasks"][0]["display_name"] = serde_json::json!("Renamed");
        tampered.files.insert(
            "pack_config.json".into(),
            serde_json::to_vec_pretty(&raw).unwrap(),
        );
        let rejection = gate(&fixture, &tampered, TEMPLATE).unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(
            rejection.detail.contains("besides tasks"),
            "{}",
            rejection.detail
        );
    }

    #[test]
    fn a_task_candidate_that_does_not_parse_is_rejected() {
        let task = task_fixture(false);
        let rejection = text_gate(&task.baseline, "Do {{ args.goal ", 32 * 1024).unwrap_err();
        assert_eq!(rejection.reason, "template_invalid");
        assert!(
            rejection.detail.contains("does not parse"),
            "{}",
            rejection.detail
        );
    }

    #[test]
    fn a_task_candidate_that_drops_a_template_variable_is_rejected() {
        let task = task_fixture(false);
        let rejection = text_gate(&task.baseline, "Do {{ doc.goal }}.\n", 32 * 1024).unwrap_err();
        assert_eq!(rejection.reason, "template_variables_dropped");
        assert!(
            rejection.detail.contains("doc.owner"),
            "{}",
            rejection.detail
        );
        assert!(
            !rejection.detail.contains("doc.goal"),
            "{}",
            rejection.detail
        );

        text_gate(&task.baseline, TEMPLATE, 32 * 1024).unwrap();
        text_gate(
            &task.baseline,
            "{{ doc.owner }}: {{ doc.goal }} at {{ ctx.now }}\n",
            32 * 1024,
        )
        .unwrap();

        let context = fixture(false);
        text_gate(&context.baseline, "Watch {{ nothing }}.\n", 32 * 1024).unwrap();
    }

    #[test]
    fn a_task_candidate_the_owner_would_refuse_at_install_is_rejected() {
        let task = task_fixture(false);
        for (text, variable) in [
            (
                "Do {{ doc.goal }} for {{ doc.owner }} on {{ node.bogus }}.\n",
                "node.bogus",
            ),
            (
                "Do {{ doc.goal }} for {{ doc.owner }} as {{ args.mode }}.\n",
                "args.mode",
            ),
        ] {
            let rejection = text_gate(&task.baseline, text, 32 * 1024).unwrap_err();
            assert_eq!(rejection.reason, "template_invalid", "{text}");
            assert!(rejection.detail.contains(variable), "{}", rejection.detail);
        }
    }

    #[test]
    fn a_one_field_sidecar_candidate_of_a_new_digest_passes() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, TEXT, "c1");
        gate(&fixture, &candidate, TEXT).unwrap();
    }

    #[test]
    fn a_one_field_inline_candidate_of_a_new_digest_passes() {
        let fixture = fixture(true);
        assert_eq!(fixture.baseline.prompt_asset, None);
        let candidate = candidate(&fixture, TEXT, "c1");
        gate(&fixture, &candidate, TEXT).unwrap();
    }

    #[test]
    fn a_candidate_that_repeats_a_digest_is_rejected_as_a_duplicate() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, FIXTURE_PROMPT, "c2");
        let rejection = gate(&fixture, &candidate, FIXTURE_PROMPT).unwrap_err();
        assert_eq!(rejection.reason, "duplicate_candidate");
        assert!(rejection.diagnostics().starts_with("duplicate_candidate: "));
    }

    #[test]
    fn an_empty_or_oversized_text_is_rejected_before_anything_else() {
        let fixture = fixture(false);
        let empty = candidate(&fixture, "", "c3");
        assert_eq!(gate(&fixture, &empty, "").unwrap_err().reason, "empty_text");

        let long = "x".repeat(33);
        let oversized = candidate(&fixture, &long, "c4");
        let rejection = structural_gate(
            &fixture.baseline,
            &oversized,
            &long,
            32,
            std::slice::from_ref(&fixture.baseline.digest),
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "text_too_long");
        assert!(rejection.detail.contains("33"), "{}", rejection.detail);
    }

    #[test]
    fn a_sidecar_candidate_that_changed_another_asset_is_rejected() {
        let fixture = fixture(false);
        let mut tampered = candidate(&fixture, TEXT, "c5");
        tampered
            .files
            .insert("README.md".into(), b"# tampered\n".to_vec());
        let rejection = gate(&fixture, &tampered, TEXT).unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(
            rejection.detail.contains("README.md"),
            "{}",
            rejection.detail
        );
    }

    #[test]
    fn a_sidecar_whose_bytes_are_not_the_proposed_text_is_rejected() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, TEXT, "c6");
        let rejection = gate(&fixture, &candidate, "Some other text.\n").unwrap_err();
        assert_eq!(rejection.reason, "text_mismatch");
    }

    #[test]
    fn an_inline_candidate_that_changed_another_config_field_is_rejected() {
        let fixture = fixture(true);
        let mut tampered = candidate(&fixture, TEXT, "c7");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&tampered.files["pack_config.json"]).unwrap();
        raw["contexts"][0]["display_name"] = serde_json::json!("Renamed");
        tampered.files.insert(
            "pack_config.json".into(),
            serde_json::to_vec_pretty(&raw).unwrap(),
        );
        let rejection = gate(&fixture, &tampered, TEXT).unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(
            rejection.detail.contains("besides contexts"),
            "{}",
            rejection.detail
        );

        let untampered = candidate(&fixture, TEXT, "c8");
        let rejection = gate(&fixture, &untampered, "Not what was written.\n").unwrap_err();
        assert_eq!(rejection.reason, "text_mismatch");
    }

    #[test]
    fn an_inline_text_that_reads_as_a_sidecar_path_is_rejected() {
        let fixture = fixture(true);
        let rejection = gate(&fixture, &fixture.baseline.clone(), "./README.md").unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(rejection.detail.contains("./"), "{}", rejection.detail);
    }

    #[test]
    fn the_text_gate_decides_before_materialization() {
        let sidecar = fixture(false);
        let inline = fixture(true);
        text_gate(&sidecar.baseline, TEXT, 32 * 1024).unwrap();
        text_gate(&inline.baseline, TEXT, 32 * 1024).unwrap();

        assert_eq!(
            text_gate(&sidecar.baseline, " \n\t", 32 * 1024)
                .unwrap_err()
                .reason,
            "empty_text"
        );
        let rejection = text_gate(&sidecar.baseline, "four", 3).unwrap_err();
        assert_eq!(rejection.reason, "text_too_long");
        assert!(rejection.detail.contains('4'), "{}", rejection.detail);

        text_gate(&sidecar.baseline, "./README.md", 32 * 1024).unwrap();
        assert_eq!(
            text_gate(&inline.baseline, "./README.md", 32 * 1024)
                .unwrap_err()
                .reason,
            "unexpected_change"
        );
    }

    #[test]
    fn an_inline_candidate_with_dollar_signs_passes() {
        let text = "Home is ${HOME}, pay $$5, and ${GENTS_T21_SURELY_UNSET}.\n";
        let fixture = fixture(true);
        let candidate = candidate(&fixture, text, "c10");
        gate(&fixture, &candidate, text).unwrap();
        let rejection = gate(&fixture, &candidate, "Home is ${HOME}.\n").unwrap_err();
        assert_eq!(rejection.reason, "text_mismatch");
    }
}
