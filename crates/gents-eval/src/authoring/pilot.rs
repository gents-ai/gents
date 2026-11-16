use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use gents::eval::OutcomeKind;
use serde::Serialize;
use serde_json::{json, Value};

use super::validate::Assembled;
use crate::report::{EvalReport, SlotClass};
use crate::runner::{Capture, TrialExecutor, TrialLocator};

const ROWS_BUDGET_BYTES: usize = 2048;

const INSTRUCTION: &str = "A failing check may mean a wrong case (the case expects what the agent should not do, or its capture or params miss what the agent did: a matcher miss, empty versus absent rows, a keyword that did not survive) or a wrong subject (the agent really fails it). One trial per case cannot tell noise from either; judge each failure from its rows.";

/// The serde spelling of a unit-like wire value, as its owner serializes it.
fn wire<T>(value: &T) -> String
where
    T: Serialize,
{
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

pub async fn failing_rows(
    executor: &dyn TrialExecutor,
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
            let Some(evidence) = executor.recollect(&locator, &captures).await else {
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

pub fn digest_turn(
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
