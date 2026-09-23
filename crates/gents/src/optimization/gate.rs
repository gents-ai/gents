//! The structural gate: everything that can be decided about a candidate
//! before a single validation trial is spent.
//!
//! The gate is pure. [`text_gate`] judges the proposed text alone, so a driver
//! can refuse a bad text before it materializes anything. [`structural_gate`]
//! then compares two materialized packs byte for byte, so "the patch touches
//! only the allowed field" is a statement about files rather than about intent.
//!
//! The gate does not validate the candidate's reference closure. Pack
//! behaviors name inference-slot markers that are bound only when the executor
//! runs, so a pack-level closure check would refuse every real candidate. It is
//! also unnecessary: the byte comparison proves that only the target prompt
//! differs from the baseline, so the candidate's references are exactly the
//! baseline's. The live closure is validated by promotion, where a transaction
//! exists.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::optimization::subject::MaterializedPack;
use crate::pack::interpolate;

const CONFIG_ASSET: &str = "pack_config.json";

/// Why a candidate never reached a validation run. `reason` is a closed
/// vocabulary for the journal — `empty_text`, `text_too_long`,
/// `unexpected_change`, `text_mismatch` or `duplicate_candidate` — and
/// `detail` is diagnostics for an operator.
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

/// The raw `system_prompt` of `context_id`, and the config with it masked.
fn split_prompt(mut raw: Value, context_id: &str) -> Result<(Value, Value), StructuralRejection> {
    let context = raw["contexts"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|context| context["context_id"].as_str() == Some(context_id))
        .ok_or_else(|| reject("unexpected_change", format!("no context {context_id:?}")))?;
    let prompt = context["system_prompt"].take();
    Ok((prompt, raw))
}

/// Decide whether `text` may become a candidate of `baseline` at all, before
/// anything is written to disk.
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
    Ok(())
}

/// Decide whether `candidate` may be evaluated at all.
///
/// The checks run in the order a rejection is cheapest to explain, and the
/// first failure decides. [`text_gate`] runs first. `seen_digests` holds the
/// checkpoint's digest and every earlier candidate's, and it is consulted
/// before the file comparison, so a proposer that repeats itself — including
/// one that returns the checkpoint's own text — is a duplicate and spends
/// nothing. The gate reads nothing owner-specific, because reference validity
/// is inherited from the baseline.
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
    if candidate.context_id != baseline.context_id {
        return Err(reject(
            "unexpected_change",
            format!(
                "the subject behavior's context moved from {:?} to {:?}",
                baseline.context_id, candidate.context_id
            ),
        ));
    }

    match &baseline.prompt_asset {
        // Sidecar: the one changed asset holds exactly the proposed bytes.
        Some(asset) => {
            if candidate.files.get(asset).map(Vec::as_slice) != Some(text.as_bytes()) {
                return Err(reject(
                    "text_mismatch",
                    format!("{asset:?} does not hold the proposed text"),
                ));
            }
        }
        // Inline: the configs agree once the target is masked, and the
        // candidate's target is exactly the proposed text's escaped form.
        None => {
            let (_, baseline_rest) = split_prompt(raw_config(baseline)?, &baseline.context_id)?;
            let (prompt, candidate_rest) =
                split_prompt(raw_config(candidate)?, &candidate.context_id)?;
            if baseline_rest != candidate_rest {
                return Err(reject(
                    "unexpected_change",
                    format!(
                        "{CONFIG_ASSET} changed besides contexts[{:?}].system_prompt",
                        baseline.context_id
                    ),
                ));
            }
            // materialize_candidate writes the text escaped, because the
            // loader interpolates this file.
            if prompt.as_str() != Some(interpolate::escape(text).as_str()) {
                return Err(reject(
                    "text_mismatch",
                    "the inline system_prompt is not the proposed text",
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
        write_fixture_pack, write_inline_fixture_pack, FIXTURE_PROMPT,
    };
    use crate::optimization::subject::{materialize_candidate, materialize_pack};

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
        let baseline = materialize_pack(&root.join("baseline"), OWNER, "monitor").unwrap();
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
            &[fixture.baseline.digest.clone()],
        )
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
            &[fixture.baseline.digest.clone()],
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

    /// F6: the one changed asset must hold exactly the proposed bytes.
    #[test]
    fn a_sidecar_whose_bytes_are_not_the_proposed_text_is_rejected() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, TEXT, "c6");
        let rejection = gate(&fixture, &candidate, "Some other text.\n").unwrap_err();
        assert_eq!(rejection.reason, "text_mismatch");
    }

    /// F6: in an inline pack the whole config may differ only at the target.
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

    /// The pack loader reads an inline value beginning with `./` as a sidecar
    /// path, so such a text would silently become a different field's meaning.
    #[test]
    fn an_inline_text_that_reads_as_a_sidecar_path_is_rejected() {
        let fixture = fixture(true);
        let rejection = gate(&fixture, &fixture.baseline.clone(), "./README.md").unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(rejection.detail.contains("./"), "{}", rejection.detail);
    }

    /// P-N5: the text checks decide before any candidate is materialized.
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

        // A leading ./ is only a sidecar path where the prompt is inline.
        text_gate(&sidecar.baseline, "./README.md", 32 * 1024).unwrap();
        assert_eq!(
            text_gate(&inline.baseline, "./README.md", 32 * 1024)
                .unwrap_err()
                .reason,
            "unexpected_change"
        );
    }

    /// An inline prompt is stored escaped; the gate admits the escaped form of
    /// exactly the proposed text and nothing else.
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
