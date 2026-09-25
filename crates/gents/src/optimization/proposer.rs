//! The proposer seam: what a proposer is told, and what it returns.
//!
//! A proposer receives data and returns a value. It holds no `ConfigAccess`,
//! opens no transaction and names no path, so it cannot read a check's
//! parameters, another case's body or the database it is being optimized
//! against; `target.rs` and `subject.rs` build the patch from the text it
//! returns.
//!
//! [`ProposalInput`] is therefore a boundary type, and its field list is the
//! statement of what an optimizer may learn. Feedback reaches it only from the
//! round's train run, and the eval runner has already refused to record
//! feedback on any other split.

use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One acceptance check's advice from the train run. No case, no stage, no
/// parameters: a check's name, what it scored, and what it had to say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFeedback {
    pub check: String,
    pub score_bp: Option<u32>,
    pub feedback: Option<String>,
}

/// A candidate the job already refused, and why. `Inconclusive` and
/// budget-exhausted rounds are never rejections and never appear here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub round: u32,
    pub text: String,
    pub rationale: String,
    pub reason: String,
}

/// Everything a proposer is told. Adding a field here is a decision about what
/// reaches a model, and a test pins the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalInput {
    pub round: u32,
    /// The retained checkpoint's text, which is what the train run evaluated.
    pub current_text: String,
    pub feedback: Vec<CheckFeedback>,
    pub rejections: Vec<Rejection>,
    pub max_text_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub text: String,
    pub rationale: String,
}

#[async_trait::async_trait]
pub trait Proposer: Send + Sync {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal>;
}

/// A proposer that answers from a table instead of thinking, and records every
/// input it was given so a test can assert what did and did not reach it.
pub struct ScriptedProposer {
    script: Vec<(String, String)>,
    echo: bool,
    pub calls: Mutex<Vec<ProposalInput>>,
}

impl ScriptedProposer {
    /// One `(text, rationale)` per round, in round order.
    pub fn new(script: Vec<(String, String)>) -> Self {
        Self {
            script,
            echo: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Always proposes the text it was given, which the structural gate
    /// refuses as a duplicate of the checkpoint.
    pub fn echoing() -> Self {
        Self {
            script: Vec::new(),
            echo: true,
            calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl Proposer for ScriptedProposer {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal> {
        let round = input.round;
        let current = input.current_text.clone();
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(input);
        }
        if self.echo {
            return Ok(Proposal {
                text: current,
                rationale: "unchanged".to_owned(),
            });
        }
        let index = (round as usize).checked_sub(1).unwrap_or(0);
        let (text, rationale) = self.script.get(index).ok_or_else(|| {
            anyhow::anyhow!("the scripted proposer has no answer for round {round}")
        })?;
        Ok(Proposal {
            text: text.clone(),
            rationale: rationale.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(round: u32, current: &str) -> ProposalInput {
        ProposalInput {
            round,
            current_text: current.into(),
            feedback: vec![CheckFeedback {
                check: "captured_rows_count".into(),
                score_bp: Some(0),
                feedback: Some("more rows next time".into()),
            }],
            rejections: Vec::new(),
            max_text_bytes: 32 * 1024,
        }
    }

    #[tokio::test]
    async fn the_scripted_proposer_answers_by_round_and_records_what_it_was_given() {
        let proposer = ScriptedProposer::new(vec![
            ("first candidate".into(), "widen the scope".into()),
            ("second candidate".into(), "name the collection".into()),
        ]);
        let first = proposer.propose(input(1, "baseline")).await.unwrap();
        assert_eq!(
            (first.text.as_str(), first.rationale.as_str()),
            ("first candidate", "widen the scope")
        );
        let second = proposer.propose(input(2, "first candidate")).await.unwrap();
        assert_eq!(second.text, "second candidate");

        let exhausted = proposer
            .propose(input(3, "second candidate"))
            .await
            .unwrap_err();
        assert!(
            format!("{exhausted:#}").contains("round 3"),
            "{exhausted:#}"
        );

        let calls = proposer.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].current_text, "first candidate");
        assert_eq!(
            calls[0].feedback[0].feedback.as_deref(),
            Some("more rows next time")
        );
    }

    #[tokio::test]
    async fn the_echoing_proposer_returns_the_current_text_unchanged() {
        let proposer = ScriptedProposer::echoing();
        let proposal = proposer.propose(input(1, "baseline")).await.unwrap();
        assert_eq!(proposal.text, "baseline");
    }

    /// Constraint 13: the proposer sees texts, scores and check names. It never
    /// sees a case id, a stage prompt, a check's params, a tier, a split label
    /// or a raw verdict payload.
    #[test]
    fn the_proposal_input_carries_nothing_protected() {
        let json = serde_json::to_value(input(1, "baseline")).unwrap();
        let rendered = serde_json::to_string(&json).unwrap();
        for forbidden in [
            "case_id", "stage_id", "prompt", "params", "tier", "split", "raw", "trial",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "ProposalInput must not carry {forbidden}: {rendered}"
            );
        }
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "current_text",
                "feedback",
                "max_text_bytes",
                "rejections",
                "round"
            ],
            "a new field on ProposalInput is a decision about what reaches a model"
        );
    }
}
