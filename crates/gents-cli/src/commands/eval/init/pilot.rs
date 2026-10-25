use gents_protocol::error_chain::ErrorChain;
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

const PILOT_CELL: &str = "pilot";

const ROWS_BUDGET_BYTES: usize = 2048;

const INSTRUCTION: &str = "A failing check may mean a wrong case (the case expects what the agent should not do, or its capture or params miss what the agent did: a matcher miss, empty versus absent rows, a keyword that did not survive) or a wrong subject (the agent really fails it). One trial per case cannot tell noise from either; judge each failure from its rows.";

pub(crate) struct PilotOutcome {
    pub(crate) run_ids: Vec<String>,
    pub(crate) revised: bool,
}

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
    let mut run_ids = Vec::new();
    let result = run_and_answer(ctx, deps, init, turn, drafted, unix_ms, &mut run_ids, out).await;
    let revised = match result {
        Ok(revised) => revised,
        Err(error) if run_ids.is_empty() => return Err(error),
        Err(error) => {
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
                    "the README could not record the pilot runs either: {}",
                    ErrorChain(&readme)
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

async fn install_definition(ctx: &EvalContext, dir: &std::path::Path) -> Result<()> {
    let (manifest, assets) = super::dossier::read_pack(dir)?;
    let config = gents::pack::load_pack_config(
        &manifest,
        &gents::pack::PackInstallOptions {
            node_did: ctx.owner.clone(),
        },
        &|path| {
            assets
                .get(path)
                .cloned()
                .with_context(|| format!("pack has no asset {path:?}"))
        },
        &|_name| None,
    )?;
    let digest =
        gents::pack::digest_declared_assets(&manifest, |path| assets.get(path).map(Vec::as_slice))?;
    let identity = gents::pack::PackIdentity::new(&manifest, digest, Vec::new());
    let expected = config
        .eval_definitions
        .iter()
        .map(|definition| gents::config_client::DesiredStateExpectation {
            collection: gents::Collection::EvalDefinition,
            owner: ctx.owner.clone(),
            id: definition.definition_id.clone(),
            digest: None,
        })
        .collect();
    gents::pack::install_pack_documents_with_expectations(
        &ctx.access,
        &ctx.owner,
        &identity,
        &config,
        gents::pack::DriftPolicy::Refuse,
        expected,
    )
    .await
    .context(
        "installing the pilot definition; choose a new --definition-id if this home already has it",
    )?;
    Ok(())
}

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
            agent: Some(init.dossier.agent_id.clone()),
        }],
        profiles: vec![(PILOT_CELL.to_owned(), init.profile.clone())],
        split,
        trials: 1,
        seed_base: 1000,
        concurrency: 1,
        purpose: PILOT_PURPOSE.to_owned(),
        run_id: Some(run_id.to_owned()),
        max_infra_retries: 1,
        registry: None,
        json: false,
        scope: EvalScopeArgs::default(),
    };
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
            trial_node_did: latest.trial_node_did.clone(),
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
    let definition_id = drafted
        .assembled
        .as_ref()
        .map(|assembled| assembled.definition.definition_id.as_str())
        .or(init.definition_id.as_deref());
    match checked(parsed, init, definition_id, &drafted.summary, Some(&note)).await {
        Ok((assembled, staged)) => {
            let written = drafted
                .written
                .as_ref()
                .ok_or_else(|| anyhow!("nothing was written to revise"))?;
            let written = commit(
                staged,
                &written.out,
                true,
                &crate::home_state::resolve_home_dir(init.scope.home.as_deref()),
            )?;
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
    use gents::eval::report::load_runs;
    use serde_json::json;

    use super::super::tests::draft_block;
    use super::super::validate::tests::dossier;
    use super::super::validate::{assemble, validate, Floors};
    use super::super::write::{commit, stage};
    use super::*;
    use crate::commands::eval::testing::Fixture;

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
        subject.agent_id = "monitor".into();
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
            scope: EvalScopeArgs::default(),
        }
    }

    async fn drafted(init: &InitContext<'_>) -> InitOutcome {
        let draft = super::super::draft::parse_reply(&block(&cases()))
            .unwrap()
            .unwrap();
        let assembled = assemble(&draft, None, &init.owner, &init.dossier.slot).unwrap();
        validate(&assembled, init.registry, &init.dossier, &init.floors).unwrap();
        let staged = stage(&assembled, SUMMARY, &init.dossier, None)
            .await
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let written = commit(staged, &init.out, false, home.path()).unwrap();
        InitOutcome {
            written: Some(written),
            assembled: Some(assembled),
            summary: SUMMARY.into(),
            rounds: 1,
        }
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
    async fn pilot_refuses_an_installed_definition_without_changing_it() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let dir = &drafted.written.as_ref().unwrap().out;
        let mut existing =
            serde_json::to_value(&drafted.assembled.as_ref().unwrap().definition).unwrap();
        existing["title"] = json!("Existing instrument");
        let plan = gents::config_client::DesiredStateApplyPlan::new(vec![
            gents::config_client::DesiredStateApplyDocument {
                collection: gents::Collection::EvalDefinition,
                add: existing.clone(),
                update: existing,
            },
        ])
        .unwrap();
        fixture
            .ctx
            .access
            .transact_write("eval.pilot.existing_definition_fixture", |txn| {
                let plan = &plan;
                Box::pin(
                    async move { gents::config_client::apply_desired_state_plan(txn, plan).await },
                )
            })
            .await
            .unwrap();
        let query = "{ EvalDefinition { definition_id title cases } PackInstallation { coordinate digest documents history } }";
        let before = fixture.ctx.access.execute(query).await.unwrap();
        let error = install_definition(&fixture.ctx, dir).await.unwrap_err();
        assert!(
            gents::config_client::stale_expectation(error.as_ref()).is_some(),
            "{}",
            ErrorChain(error.as_ref())
        );
        let after = fixture.ctx.access.execute(query).await.unwrap();
        assert_eq!(before, after);
        assert!(pilot_runs(&fixture).await.is_empty());
    }

    #[tokio::test]
    async fn concurrent_pilots_cannot_replace_each_others_definition() {
        let fixture = Fixture::new().await;
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let init = context(&fixture, &registry, &root.path().join("out"));
        let drafted = drafted(&init).await;
        let dir = &drafted.written.as_ref().unwrap().out;
        let (left, right) = tokio::join!(
            install_definition(&fixture.ctx, dir),
            install_definition(&fixture.ctx, dir)
        );
        assert_ne!(left.is_ok(), right.is_ok());
        let error = left.err().or_else(|| right.err()).unwrap();
        assert!(
            gents::config_client::stale_expectation(error.as_ref()).is_some(),
            "{}",
            ErrorChain(error.as_ref())
        );
    }
}
