//! The goal is derived from the checks, and the checks remain the only verdicts.

use serde::{Deserialize, Serialize};

use crate::documents::{EvalCapture, EvalCase};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    pub min: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

impl GoalEntry {
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

    pub fn label(&self, observed: Option<u64>) -> String {
        let observed = observed.map_or_else(|| "?".to_owned(), |count| count.to_string());
        let expected = match self.max {
            None => self.min.to_string(),
            max => range_label(self.min, max),
        };
        format!("{observed}/{expected}")
    }
}

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
            // A capture counted by several stages: the last one wins.
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
}
