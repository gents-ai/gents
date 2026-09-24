//! `--pilot`: the written draft runs once against its subject, one trial per
//! case and one run per populated split, and the author reads the evidence
//! and either keeps the pack or revises it once. A pilot is a look at cases
//! that are about to change, not evidence: its runs carry purpose `pilot`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;

use anyhow::{anyhow, Context, Result};
use gents::document_config::EvalSplit;
use gents::eval::report::{load_report, EvalReport, SlotClass};
use gents::eval::runner::{self, Capture, TrialLocator};
use gents::eval::scoring::PILOT_PURPOSE;
use gents::eval::OutcomeKind;
use serde_json::{json, Value};

use super::draft::parse_reply;
use super::turn::Turn;
use super::validate::Assembled;
use super::write::{commit, rewrite_readme, split_name, PilotNote};
use super::{checked, print_written, InitContext, InitOutcome};
use crate::cli::{CellArg, EvalRunArgs, EvalScopeArgs};
use crate::commands::eval::render::wire;
use crate::commands::eval::run::{follow, run_request};
use crate::commands::eval::{Deps, EvalContext};

/// The cell a pilot runs: the subject, on the init's profile.
const PILOT_CELL: &str = "pilot";

/// Captured rows the digest shows per case, in bytes of JSON.
const ROWS_BUDGET_BYTES: usize = 2048;

/// What the author is told to make of the digest.
const INSTRUCTION: &str = "A failing check may mean a wrong case (the case expects what the behavior should not do, or its capture or params miss what the behavior did: a matcher miss, empty versus absent rows, a keyword that did not survive) or a wrong subject (the behavior really fails it). One trial per case cannot tell noise from either; judge each failure from its rows.";

/// What the pilot ran and whether the pack changed after it.
pub(crate) struct PilotOutcome {
    /// One per populated split, in train, validation, held-out order.
    pub(crate) run_ids: Vec<String>,
    pub(crate) revised: bool,
}

/// Pilot what `drafted` wrote. `confirm` is asked with the case count
/// before anything is spent; a refusal is an error and nothing runs.
pub(crate) async fn pilot(
    ctx: &EvalContext,
    deps: &Deps<'_>,
    init: &InitContext<'_>,
    turn: &mut dyn Turn,
    drafted: &InitOutcome,
    confirm: &mut dyn FnMut(usize) -> bool,
    out: &mut dyn Write,
) -> Result<PilotOutcome> {
    let (Some(written), Some(assembled)) = (&drafted.written, &drafted.assembled) else {
        anyhow::bail!("nothing was written to pilot");
    };
    let definition = &assembled.definition;
    anyhow::ensure!(
        confirm(definition.cases.len()),
        "the pilot was declined; the pack stays as written at {}",
        written.out.display()
    );
    install_definition(ctx, &written.out).await?;

    let unix_ms = chrono::Utc::now().timestamp_millis();
    // Every run is named here as it is created, a stopped one included, so
    // the README names it whatever happens after.
    let mut run_ids = Vec::new();
    let result = run_and_answer(ctx, deps, init, turn, drafted, unix_ms, &mut run_ids, out).await;
    let revised = match result {
        Ok(revised) => revised,
        Err(error) if run_ids.is_empty() => return Err(error),
        Err(error) => {
            // The pilot's own failure is what returns; a README that could
            // not be written as well is said beside it, never instead.
            let note = PilotNote {
                run_ids: run_ids.clone(),
                revised: false,
            };
            return match rewrite_readme(
                &written.out,
                assembled,
                &drafted.summary,
                &init.dossier,
                &note,
            ) {
                Ok(()) => Err(error),
                Err(readme) => Err(error.context(format!(
                    "the README could not record the pilot runs either: {readme:#}"
                ))),
            };
        }
    };
    if !revised {
        rewrite_readme(
            &written.out,
            assembled,
            &drafted.summary,
            &init.dossier,
            &PilotNote {
                run_ids: run_ids.clone(),
                revised: false,
            },
        )?;
    }
    Ok(PilotOutcome { run_ids, revised })
}

/// The pilot from its first run to the author's answer: every populated
/// split's run (each id pushed to `run_ids` before it starts), the digest
/// turn, and the one revision round. Whether the pack was revised.
#[allow(clippy::too_many_arguments)]
async fn run_and_answer(
    ctx: &EvalContext,
    deps: &Deps<'_>,
    init: &InitContext<'_>,
    turn: &mut dyn Turn,
    drafted: &InitOutcome,
    unix_ms: i64,
    run_ids: &mut Vec<String>,
    out: &mut dyn Write,
) -> Result<bool> {
    let assembled = drafted
        .assembled
        .as_ref()
        .ok_or_else(|| anyhow!("nothing was written to pilot"))?;
    let definition = &assembled.definition;
    for split in [EvalSplit::Train, EvalSplit::Validation, EvalSplit::HeldOut] {
        if !definition.cases.iter().any(|case| case.split == split) {
            continue;
        }
        let run_id = format!(
            "{}-pilot-{unix_ms}-{}",
            definition.definition_id,
            split_name(split)
        );
        run_ids.push(run_id.clone());
        run_split(
            ctx,
            deps,
            init,
            &definition.definition_id,
            split,
            &run_id,
            out,
        )
        .await?;
    }

    let mut reports = Vec::with_capacity(run_ids.len());
    for run_id in run_ids.iter() {
        reports.push(load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), run_id).await?);
    }
    let rows = failing_rows(deps, assembled, &reports).await;
    let digest = digest_turn(&reports, &rows);
    writeln!(out, "{digest}")?;
    let reply = turn.send(&digest).await?;
    if !turn.shows_replies() {
        writeln!(out, "{reply}")?;
    }
    revise(init, drafted, run_ids, &reply, out).await
}

/// Install the pack at `dir` into the home as a directory pack, through the
/// owner `gents pack install` uses. Its definition is what the runs freeze.
async fn install_definition(ctx: &EvalContext, dir: &std::path::Path) -> Result<()> {
    let (manifest, assets) = super::dossier::read_pack(dir)?;
    let config = gents::pack::load_pack_config(
        &manifest,
        &gents::pack::PackInstallOptions {
            agent_did: ctx.owner.clone(),
        },
        &|path| {
            assets
                .get(path)
                .cloned()
                .with_context(|| format!("pack has no asset {path:?}"))
        },
        &|_name| None,
    )?;
    gents::pack::install_pack_documents(&ctx.access, &config)
        .await
        .context("installing the drafted definition pack into the home")?;
    Ok(())
}

/// One pilot run: `split`'s cases, one trial each, on the subject, built and
/// followed as `gents eval run` builds and follows one.
async fn run_split(
    ctx: &EvalContext,
    deps: &Deps<'_>,
    init: &InitContext<'_>,
    definition_id: &str,
    split: EvalSplit,
    run_id: &str,
    out: &mut dyn Write,
) -> Result<()> {
    let args = EvalRunArgs {
        definition_id: definition_id.to_owned(),
        cells: vec![CellArg {
            cell_id: PILOT_CELL.to_owned(),
            pack: init.subject_dir.display().to_string(),
            behavior: Some(init.dossier.behavior_id.clone()),
        }],
        profiles: vec![(PILOT_CELL.to_owned(), init.profile.clone())],
        split,
        trials: 1,
        // `gents eval run`'s defaults.
        seed_base: 1000,
        concurrency: 1,
        purpose: PILOT_PURPOSE.to_owned(),
        run_id: Some(run_id.to_owned()),
        max_infra_retries: 1,
        registry: None,
        json: false,
        scope: EvalScopeArgs::default(),
    };
    // The packs are held until the run ends: a cache entry stays locked.
    let (request, _packs) = run_request(ctx, &args).await?;
    let outcome = follow(
        ctx,
        run_id,
        BTreeSet::new(),
        true,
        out,
        runner::run(
            &ctx.access,
            &request,
            deps.executor,
            deps.registry,
            deps.cancel.clone(),
            &deps.options,
        ),
    )
    .await?;
    anyhow::ensure!(
        !outcome.cancelled && outcome.abandoned == 0,
        "pilot run {run_id} stopped before it finished; the pack stays as written; `gents eval resume {run_id}` continues it"
    );
    Ok(())
}

/// The captured rows of every failing stage, read back from the trial's
/// home by `(case_id, stage_id)`. Unreadable homes give nothing.
async fn failing_rows(
    deps: &Deps<'_>,
    assembled: &Assembled,
    reports: &[EvalReport],
) -> BTreeMap<(String, String), Vec<Value>> {
    let mut rows = BTreeMap::new();
    let slots = reports
        .iter()
        .flat_map(|report| &report.cells)
        .flat_map(|cell| &cell.slots)
        .filter(|slot| slot.class != SlotClass::Pass);
    for slot in slots {
        let (Some(latest), Some(case)) = (
            slot.latest.as_ref(),
            assembled
                .definition
                .cases
                .iter()
                .find(|case| case.case_id == slot.case_id),
        ) else {
            continue;
        };
        let failing: BTreeSet<&str> = slot
            .verdicts
            .iter()
            .filter(|verdict| verdict.kind != OutcomeKind::Passed)
            .map(|verdict| verdict.stage_id.as_str())
            .collect();
        let locator = TrialLocator {
            trial_agent_did: latest.trial_agent_did.clone(),
            session_id: latest.session_id.clone(),
            home_hint: latest.home_hint.clone(),
        };
        for (index, stage) in case.stages.iter().enumerate() {
            if !failing.contains(stage.stage_id.as_str()) || stage.capture.is_empty() {
                continue;
            }
            let captures: Vec<Capture> = stage.capture.iter().map(Capture::from).collect();
            let Some(evidence) = deps.executor.recollect(&locator, &captures).await else {
                continue;
            };
            // Recollected stages are named by their position in the session;
            // a session without this stage's position shows no rows rather
            // than another stage's.
            let Some(read) = evidence.stages.get(index) else {
                continue;
            };
            let captured = read
                .captures
                .iter()
                .map(|(name, result)| json!({"capture": name, "result": result}))
                .collect();
            rows.insert((case.case_id.clone(), stage.stage_id.clone()), captured);
        }
    }
    rows
}

/// The author's pilot turn: per case and stage each check's kind, score and
/// reason code, the captured rows of failing stages within a per-case
/// budget, and what to reply.
pub(crate) fn digest_turn(
    reports: &[EvalReport],
    rows: &BTreeMap<(String, String), Vec<Value>>,
) -> String {
    let mut text = String::from("## Pilot results\n\n");
    let _ = writeln!(
        text,
        "One trial per case, one run per split: {}.",
        reports
            .iter()
            .map(|report| report.run.run_id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for report in reports {
        let split = wire(&report.run.split);
        for slot in report.cells.iter().flat_map(|cell| &cell.slots) {
            let _ = writeln!(text, "\n### {} ({split})\n", slot.case_id);
            let _ = writeln!(text, "slot: {}", wire(&slot.class));
            let mut stages: Vec<&str> = Vec::new();
            for verdict in &slot.verdicts {
                if !stages.contains(&verdict.stage_id.as_str()) {
                    stages.push(&verdict.stage_id);
                }
            }
            let mut budget = ROWS_BUDGET_BYTES;
            for stage in stages {
                let _ = writeln!(text, "stage {stage}:");
                let mut failing = false;
                for verdict in slot.verdicts.iter().filter(|v| v.stage_id == stage) {
                    failing |= verdict.kind != OutcomeKind::Passed;
                    let _ = writeln!(
                        text,
                        "- {} {} {} {}",
                        verdict.check,
                        wire(&verdict.kind),
                        verdict
                            .score_bp
                            .map_or_else(|| "-".to_owned(), |bp| bp.to_string()),
                        verdict.reason_code.as_deref().unwrap_or("-")
                    );
                }
                if !failing {
                    continue;
                }
                match rows.get(&(slot.case_id.clone(), stage.to_owned())) {
                    Some(rows) => {
                        let _ =
                            writeln!(text, "captured rows:\n{}", render_rows(rows, &mut budget));
                    }
                    None => text.push_str("captured rows: none could be read back\n"),
                }
            }
        }
    }
    let _ = write!(
        text,
        "\n{INSTRUCTION}\n\nReply with a full revised draft, or say 'keep'."
    );
    text
}

/// `rows` as JSON within `budget` bytes, cut at a character boundary and
/// marked `…` when it did not fit; `budget` loses what was rendered.
fn render_rows(rows: &[Value], budget: &mut usize) -> String {
    let json = serde_json::to_string(rows).unwrap_or_default();
    if json.len() <= *budget {
        *budget -= json.len();
        return json;
    }
    let mut cut = *budget;
    while !json.is_char_boundary(cut) {
        cut -= 1;
    }
    *budget = 0;
    format!("{}…", &json[..cut])
}

/// The author's answer to the digest: `keep`, or one revised draft. A valid
/// revision replaces the pack at `--out`; a failing one is reported and the
/// pack stays as written. Whether the pack was replaced.
async fn revise(
    init: &InitContext<'_>,
    drafted: &InitOutcome,
    run_ids: &[String],
    reply: &str,
    out: &mut dyn Write,
) -> Result<bool> {
    let answer = reply
        .trim()
        .trim_matches(|c: char| matches!(c, '\'' | '"' | '`' | '.'));
    if answer.eq_ignore_ascii_case("keep") {
        writeln!(out, "the author kept the draft")?;
        return Ok(false);
    }
    let parsed = parse_reply(reply);
    if let Ok(None) = parsed {
        writeln!(
            out,
            "the author replied with neither a draft nor 'keep'; the pack stays as written"
        )?;
        return Ok(false);
    }
    let note = PilotNote {
        run_ids: run_ids.to_vec(),
        revised: true,
    };
    match checked(parsed, init, &drafted.summary, Some(&note)).await {
        Ok((assembled, staged)) => {
            let written = drafted
                .written
                .as_ref()
                .ok_or_else(|| anyhow!("nothing was written to revise"))?;
            let written = commit(staged, &written.out, true)?;
            writeln!(out, "the author revised the draft after the pilot")?;
            print_written(init, &assembled, &written, out)?;
            Ok(true)
        }
        Err(messages) => {
            writeln!(
                out,
                "the revised draft did not validate; the pack stays as written:\n- {}",
                messages.join("\n- ")
            )?;
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gents::eval::checks::CheckRegistry;
    use gents::eval::report::{load_report, load_runs};
    use gents::eval::runner::{ScriptKey, ScriptedExecutor};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::super::tests::draft_block;
    use super::super::turn::ScriptedTurn;
    use super::super::validate::tests::dossier;
    use super::super::validate::{assemble, validate, Floors};
    use super::super::write::write_pack;
    use super::*;
    use crate::commands::eval::testing::{deps, eval_with, fail, pass, Fixture};

    const DEFINITION: &str = "pilot-def";
    const SUMMARY: &str = "It must record findings.";

    fn case(case_id: &str, split: &str) -> Value {
        json!({
            "case_id": case_id,
            "split": split,
            "stages": [{
                "stage_id": "check",
                "prompt": "Run the monitor.",
                "deadline_secs": 600,
                "checks": [{
                    "check": "captured_rows_count",
                    "params": {"name": "findings", "min": 1},
                    "tier": "acceptance",
                }],
            }],
        })
    }

    fn cases() -> Vec<Value> {
        vec![
            case("train-a", "train"),
            case("val-a", "validation"),
            case("val-b", "validation"),
            case("ho-a", "held_out"),
        ]
    }

    fn block(cases: &[Value]) -> String {
        draft_block(cases).replace("canary-quality", DEFINITION)
    }

    fn context<'a>(fixture: &Fixture, registry: &'a CheckRegistry, out: &Path) -> InitContext<'a> {
        let mut subject = dossier();
        subject.behavior_id = "monitor".into();
        InitContext {
            dossier: subject,
            registry,
            floors: Floors { validation_min: 1 },
            definition_id: None,
            owner: fixture.ctx.owner.clone(),
            out: out.to_path_buf(),
            force: false,
            subject: fixture.pack_arg(),
            subject_dir: fixture.pack.clone(),
            profile: "local".into(),
        }
    }

    /// What the interview hands the pilot: the pack of [`cases`], written.
    async fn drafted(init: &InitContext<'_>) -> InitOutcome {
        let draft = super::super::draft::parse_reply(&block(&cases()))
            .unwrap()
            .unwrap();
        let assembled = assemble(&draft, None, &init.owner, &init.dossier.slot).unwrap();
        validate(&assembled, init.registry, &init.dossier, &init.floors).unwrap();
        let written = write_pack(&assembled, SUMMARY, &init.dossier, &init.out, false)
            .await
            .unwrap();
        InitOutcome {
            written: Some(written),
            assembled: Some(assembled),
            summary: SUMMARY.into(),
            rounds: 1,
        }
    }

    /// Every trial passes but `val-a`'s.
    fn failing_val_a() -> ScriptedExecutor {
        ScriptedExecutor::new().with_default(pass()).with(
            ScriptKey {
                cell_label: "pilot".into(),
                case_id: "val-a".into(),
                trial_index: 0,
                attempt: 1,
            },
            fail(),
        )
    }

    async fn pilot_runs(fixture: &Fixture) -> Vec<gents::eval::RunRecord> {
        load_runs(&fixture.ctx.access, &fixture.ctx.owner)
            .await
            .unwrap()
            .into_iter()
            .filter(|run| run.origin.definition.definition_id == DEFINITION)
            .collect()
    }

    #[tokio::test]
    async fn a_pilot_runs_every_split_once_records_its_purpose_and_digests_the_failures() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let executor = failing_val_a();
        let mut turn = ScriptedTurn::new(["keep"]);
        let mut asked = Vec::new();
        let mut out = Vec::new();
        let piloted = pilot(
            &fixture.ctx,
            &deps(&executor, &registry, CancellationToken::new()),
            &init,
            &mut turn,
            &drafted,
            &mut |count| {
                asked.push(count);
                true
            },
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(asked, vec![4]);
        assert!(!piloted.revised);

        let runs = pilot_runs(&fixture).await;
        assert_eq!(runs.len(), 3, "one run per populated split");
        let mut covered = Vec::new();
        for run in &runs {
            assert_eq!(run.origin.purpose, "pilot");
            assert_eq!(run.origin.trials_per_case, 1);
            let split = crate::commands::eval::render::wire(&run.origin.split);
            assert!(
                run.run_id.starts_with(&format!("{DEFINITION}-pilot-"))
                    && run.run_id.ends_with(&format!("-{split}")),
                "{}",
                run.run_id
            );
            assert!(piloted.run_ids.contains(&run.run_id));
            covered.extend(run.origin.case_ids.iter().cloned());
        }
        covered.sort();
        assert_eq!(covered, vec!["ho-a", "train-a", "val-a", "val-b"]);

        let digest = &turn.sent[0];
        assert!(digest.starts_with("## Pilot results"), "{digest}");
        assert!(digest.contains("### val-a (validation)"), "{digest}");
        assert!(digest.contains("captured_rows_count"), "{digest}");
        assert!(digest.contains("below_min"), "{digest}");
        assert!(
            digest.contains("wrong case") && digest.contains("wrong subject"),
            "{digest}"
        );
        assert!(digest.contains("say 'keep'"), "{digest}");
        // The scripted executor keeps no home to read rows back from.
        assert!(
            digest.contains("captured rows: none could be read back"),
            "{digest}"
        );
        let validation = piloted
            .run_ids
            .iter()
            .find(|run_id| run_id.ends_with("-validation"))
            .unwrap();
        let report = load_report(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            &fixture.ctx.runs_dir(),
            validation,
        )
        .await
        .unwrap();
        let rows = BTreeMap::from([(
            ("val-a".to_owned(), "check".to_owned()),
            vec![json!({"capture": "findings", "result": {"kind": "documents", "rows": []}})],
        )]);
        let with_rows = digest_turn(&[report], &rows);
        assert!(
            with_rows.contains("captured rows:\n[{\"capture\":\"findings\""),
            "{with_rows}"
        );
        // A passing stage shows its verdicts and no rows.
        let val_b = with_rows
            .split("### val-b (validation)")
            .nth(1)
            .and_then(|rest| rest.split("\n\n").nth(1))
            .unwrap();
        assert!(val_b.contains("captured_rows_count"), "{val_b}");
        assert!(!val_b.contains("captured rows"), "{val_b}");
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("landed pilot val-a #0 attempt 1"),
            "{printed}"
        );

        let readme = std::fs::read_to_string(init.out.join("README.md")).unwrap();
        for run_id in &piloted.run_ids {
            assert!(readme.contains(run_id.as_str()), "{readme}");
        }
        assert!(!readme.contains("revised after the pilot"), "{readme}");
    }

    #[tokio::test]
    async fn a_revised_draft_after_the_pilot_overwrites_the_pack() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let executor = failing_val_a();
        let mut revised = cases();
        revised[1]["case_id"] = json!("val-renamed");
        let mut turn = ScriptedTurn::new([block(&revised)]);
        let piloted = pilot(
            &fixture.ctx,
            &deps(&executor, &registry, CancellationToken::new()),
            &init,
            &mut turn,
            &drafted,
            &mut |_| true,
            &mut Vec::new(),
        )
        .await
        .unwrap();
        assert!(piloted.revised);
        assert!(init.out.join("cases/val_renamed.json").is_file());
        assert!(!init.out.join("cases/val_a.json").exists());
        let readme = std::fs::read_to_string(init.out.join("README.md")).unwrap();
        assert!(readme.contains(&piloted.run_ids[0]), "{readme}");
        assert!(readme.contains("revised after the pilot"), "{readme}");
    }

    #[tokio::test]
    async fn a_revision_that_fails_validation_keeps_the_pack_and_notes_the_pilot() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let executor = failing_val_a();
        let mut broken = cases();
        broken[1]["stages"][0]["checks"][0]["check"] = json!("no_such");
        let mut turn = ScriptedTurn::new([block(&broken)]);
        let mut out = Vec::new();
        let piloted = pilot(
            &fixture.ctx,
            &deps(&executor, &registry, CancellationToken::new()),
            &init,
            &mut turn,
            &drafted,
            &mut |_| true,
            &mut out,
        )
        .await
        .unwrap();
        assert!(!piloted.revised);
        assert_eq!(turn.sent.len(), 1, "exactly one revision round");
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("unknown check"), "{printed}");
        assert!(init.out.join("cases/val_a.json").is_file());
        let readme = std::fs::read_to_string(init.out.join("README.md")).unwrap();
        assert!(readme.contains(&piloted.run_ids[0]), "{readme}");
    }

    #[tokio::test]
    async fn a_pilot_that_fails_after_its_runs_still_names_them_in_the_readme() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let executor = failing_val_a();
        // The author never answers: the digest turn fails after the runs.
        let mut turn = ScriptedTurn::new(Vec::<String>::new());
        let error = pilot(
            &fixture.ctx,
            &deps(&executor, &registry, CancellationToken::new()),
            &init,
            &mut turn,
            &drafted,
            &mut |_| true,
            &mut Vec::new(),
        )
        .await
        .err()
        .expect("the failed turn propagates");
        assert!(format!("{error:#}").contains("no reply left"), "{error:#}");
        assert_eq!(turn.sent.len(), 1, "the digest was sent");
        let runs = pilot_runs(&fixture).await;
        assert_eq!(runs.len(), 3);
        let readme = std::fs::read_to_string(init.out.join("README.md")).unwrap();
        for run in &runs {
            assert!(readme.contains(run.run_id.as_str()), "{readme}");
        }
        assert!(!readme.contains("revised after the pilot"), "{readme}");
    }

    #[tokio::test]
    async fn the_pilot_asks_before_spending_unless_told_yes() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let executor = failing_val_a();
        let mut turn = ScriptedTurn::new(["keep"]);
        let error = pilot(
            &fixture.ctx,
            &deps(&executor, &registry, CancellationToken::new()),
            &init,
            &mut turn,
            &drafted,
            &mut |_| false,
            &mut Vec::new(),
        )
        .await
        .err()
        .expect("a declined pilot is refused");
        assert!(error.to_string().contains("declined"), "{error:#}");
        assert!(pilot_runs(&fixture).await.is_empty());
        assert!(turn.sent.is_empty());
    }

    #[tokio::test]
    async fn a_pilot_run_is_not_exposure() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let executor = failing_val_a();
        pilot(
            &fixture.ctx,
            &deps(&executor, &registry, CancellationToken::new()),
            &init,
            &mut ScriptedTurn::new(["keep"]),
            &drafted,
            &mut |_| true,
            &mut Vec::new(),
        )
        .await
        .unwrap();
        let pack = fixture.pack_arg();
        let cell = format!("baseline={pack}");
        eval_with(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--cell",
                &cell,
                "--profile",
                "baseline=local",
                "--run-id",
                "r-eval",
                "--trials",
                "1",
            ],
            &deps(&executor, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        let report = load_report(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            &fixture.ctx.runs_dir(),
            "r-eval",
        )
        .await
        .unwrap();
        assert_eq!(
            report.exposure, 1,
            "the validation pilot run is not exposure"
        );
    }

    #[test]
    fn the_digest_budgets_captured_rows_per_case() {
        let mut rows = BTreeMap::new();
        let long = "x".repeat(4096);
        rows.insert(
            ("val-a".to_owned(), "check".to_owned()),
            vec![json!({"capture": "findings", "rows": [{"text": long}]})],
        );
        let rendered = render_rows(&rows[&("val-a".to_owned(), "check".to_owned())], &mut 2048);
        assert!(
            rendered.len() <= 2048 + '…'.len_utf8(),
            "{}",
            rendered.len()
        );
        assert!(rendered.ends_with('…'), "{rendered}");
        let mut budget = 2048;
        let short = render_rows(&[json!({"id": 1})], &mut budget);
        assert_eq!(short, r#"[{"id":1}]"#);
        assert_eq!(budget, 2048 - short.len());
    }
}
