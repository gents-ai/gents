use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFeedback {
    pub check: String,
    pub score_bp: Option<u32>,
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub round: u32,
    pub text: String,
    pub rationale: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalInput {
    pub round: u32,
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

pub struct ScriptedProposer {
    script: Vec<(String, String)>,
    echo: bool,
    pub calls: Mutex<Vec<ProposalInput>>,
}

impl ScriptedProposer {
    pub fn new(script: Vec<(String, String)>) -> Self {
        Self {
            script,
            echo: false,
            calls: Mutex::new(Vec::new()),
        }
    }

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
        let index = (round as usize).saturating_sub(1);
        let (text, rationale) = self.script.get(index).ok_or_else(|| {
            anyhow::anyhow!("the scripted proposer has no answer for round {round}")
        })?;
        Ok(Proposal {
            text: text.clone(),
            rationale: rationale.clone(),
        })
    }
}
