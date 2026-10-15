//! What a case expects a trial's home to hold, for a watcher to measure a
//! running or finished trial against.
//!
//! The goal is derived, never graded: each `captured_rows_count` check
//! contributes the range its params require of its capture, and a case's
//! own `goal` map replaces what the checks imply for the collections it
//! names. The checks remain the only verdicts.

use serde::{Deserialize, Serialize};

use crate::document_config::{EvalCapture, EvalCase};
use crate::eval::checks::captured_rows_count::{required_rows, CapturedRowsCount};
use crate::eval::checks::{range_label, Check};
use crate::eval::runner::executor::Capture;
use crate::eval::runner::progress::LiveSnapshot;

/// The key of a case goal that counts the schemas the subject registered.
pub const SCHEMAS_GOAL: &str = "schemas";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalEntry {
    /// A collection name, or [`SCHEMAS_GOAL`].
    pub collection: String,
    /// The documents capture a `captured_rows_count` check counts; `None`
    /// for an entry of the case's own goal, which counts the collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    pub min: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

impl GoalEntry {
    /// The snapshot's count for this entry; `None` when the snapshot has not
    /// observed it, as for a collection not registered yet.
    pub fn observed(&self, snapshot: &LiveSnapshot) -> Option<u64> {
        if let Some(capture) = &self.capture {
            return snapshot.captures.get(capture).copied();
        }
        if self.collection == SCHEMAS_GOAL {
            return Some(snapshot.schemas.len() as u64);
        }
        snapshot.documents.get(&self.collection).copied()
    }

    pub fn met(&self, observed: u64) -> bool {
        observed >= self.min && self.max.is_none_or(|max| observed <= max)
    }

    /// `7/9`, `2/≤4`, `3/2..5`: observed over expected, `?` when unobserved.
    pub fn label(&self, observed: Option<u64>) -> String {
        let observed = observed.map_or_else(|| "?".to_owned(), |count| count.to_string());
        let expected = match self.max {
            None => self.min.to_string(),
            max => range_label(self.min, max),
        };
        format!("{observed}/{expected}")
    }
}

/// The case's goal, in the order its stages last count each capture, then
/// the case's own entries by name.
/// `fallback` is the run's request-level capture list, which a stage that
/// declares no captures reads.
pub fn case_goal(case: &EvalCase, fallback: &[Capture]) -> Vec<GoalEntry> {
    let mut goal: Vec<GoalEntry> = Vec::new();
    for stage in &case.stages {
        for check in &stage.checks {
            if check.check != CapturedRowsCount.name() {
                continue;
            }
            let Some((capture, min, max)) = required_rows(&check.params) else {
                continue;
            };
            let Some(collection) = capture_collection(&stage.capture, fallback, &capture) else {
                continue;
            };
            if case.goal.contains_key(&collection) {
                continue;
            }
            // A capture counted by several stages is measured against the
            // last: it is what the home holds when the case is done.
            goal.retain(|entry| entry.capture.as_deref() != Some(capture.as_str()));
            goal.push(GoalEntry {
                collection,
                capture: Some(capture),
                min,
                max,
            });
        }
    }
    goal.extend(case.goal.iter().map(|(collection, count)| GoalEntry {
        collection: collection.clone(),
        capture: None,
        min: *count,
        max: None,
    }));
    goal
}

/// The collection a stage's documents capture reads.
fn capture_collection(
    declared: &[EvalCapture],
    fallback: &[Capture],
    name: &str,
) -> Option<String> {
    if declared.is_empty() {
        return fallback.iter().find_map(|capture| match capture {
            Capture::Documents {
                name: named,
                collection,
                ..
            } if named == name => Some(collection.clone()),
            _ => None,
        });
    }
    declared.iter().find_map(|capture| match capture {
        EvalCapture::Documents {
            name: named,
            collection,
            ..
        } if named == name => Some(collection.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    fn case(goal: BTreeMap<String, u64>) -> EvalCase {
        serde_json::from_value(json!({
            "case_id": "setup",
            "split": "validation",
            "goal": goal,
            "stages": [{
                "stage_id": "build",
                "prompt": "Build the crew.",
                "deadline_secs": 60,
                "capture": [
                    {"kind": "documents", "name": "agents", "collection": "Agent", "filter": {}},
                    {"kind": "documents", "name": "tasks", "collection": "Task", "filter": {}},
                    {"kind": "file", "name": "notes", "glob": "*.md"}
                ],
                "checks": [
                    {"check": "captured_rows_count", "params": {"name": "agents", "min": 9}, "tier": "acceptance"},
                    {"check": "tool_calls_expected", "params": {"required": ["config"]}, "tier": "acceptance"}
                ]
            }, {
                "stage_id": "verify",
                "prompt": "Check it.",
                "deadline_secs": 60,
                "checks": [
                    {"check": "captured_rows_count", "params": {"name": "tasks", "min": 20, "max": 22}, "tier": "acceptance"}
                ]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn the_goal_is_each_row_count_check_against_its_capture_and_the_cases_own_entries() {
        let fallback = [Capture::Documents {
            name: "tasks".into(),
            collection: "Task".into(),
            filter: json!({}),
            fields: Vec::new(),
        }];
        let goal = case_goal(&case(BTreeMap::new()), &fallback);
        assert_eq!(
            goal,
            vec![
                GoalEntry {
                    collection: "Agent".into(),
                    capture: Some("agents".into()),
                    min: 9,
                    max: None
                },
                GoalEntry {
                    collection: "Task".into(),
                    capture: Some("tasks".into()),
                    min: 20,
                    max: Some(22)
                },
            ]
        );
        // Without the run's fallback list the second stage captures nothing.
        assert_eq!(case_goal(&case(BTreeMap::new()), &[]).len(), 1);

        let overridden = case_goal(
            &case(BTreeMap::from([
                ("Agent".to_owned(), 7),
                (SCHEMAS_GOAL.to_owned(), 9),
            ])),
            &fallback,
        );
        assert_eq!(
            overridden
                .iter()
                .map(|entry| (
                    entry.collection.as_str(),
                    entry.capture.is_some(),
                    entry.min
                ))
                .collect::<Vec<_>>(),
            vec![
                ("Task", true, 20),
                ("Agent", false, 7),
                ("schemas", false, 9)
            ]
        );
    }

    #[test]
    fn an_entry_reads_its_capture_its_collection_or_the_registered_schemas() {
        let snapshot = LiveSnapshot {
            documents: BTreeMap::from([("Agent".to_owned(), 7)]),
            captures: BTreeMap::from([("tasks".to_owned(), 21)]),
            schemas: vec!["RunStart".into(), "GateResult".into()],
            ..LiveSnapshot::default()
        };
        let entry = |collection: &str, capture: Option<&str>, min, max| GoalEntry {
            collection: collection.into(),
            capture: capture.map(Into::into),
            min,
            max,
        };
        let agents = entry("Agent", None, 9, None);
        let tasks = entry("Task", Some("tasks"), 20, Some(22));
        let schemas = entry(SCHEMAS_GOAL, None, 9, None);
        let absent = entry("RunStart", None, 1, None);
        assert_eq!(agents.observed(&snapshot), Some(7));
        assert_eq!(tasks.observed(&snapshot), Some(21));
        assert_eq!(schemas.observed(&snapshot), Some(2));
        assert_eq!(absent.observed(&snapshot), None);
        assert!(!agents.met(7) && agents.met(9));
        assert!(tasks.met(21) && !tasks.met(23));
        assert_eq!(agents.label(Some(7)), "7/9");
        assert_eq!(tasks.label(Some(21)), "21/20..22");
        assert_eq!(absent.label(None), "?/1");
        assert_eq!(entry("Task", None, 0, Some(4)).label(Some(1)), "1/≤4");
    }
}
