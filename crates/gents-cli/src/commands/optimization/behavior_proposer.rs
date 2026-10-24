use anyhow::Result;
use gents::optimization::{Proposal, ProposalInput, Proposer};
use tokio::sync::Mutex;

use crate::commands::eval::init::draft::json_blocks;
use crate::commands::eval::init::turn::Turn;
use crate::commands::eval::render::percent;

#[derive(Debug, thiserror::Error)]
pub(crate) enum BehaviorProposerError {
    #[error("the agent's second reply is not a proposal: {problem}")]
    SecondReplyNotProposal { problem: String },
}

pub(crate) struct AgentProposer<T> {
    turn: Mutex<T>,
    preamble: Mutex<Option<String>>,
}

impl<T> AgentProposer<T>
where
    T: Turn + Send,
{
    pub(crate) fn with_preamble(turn: T, preamble: String) -> Self {
        Self {
            turn: Mutex::new(turn),
            preamble: Mutex::new(Some(preamble)),
        }
    }
}

#[async_trait::async_trait]
impl<T> Proposer for AgentProposer<T>
where
    T: Turn + Send,
{
    async fn propose(&self, input: ProposalInput) -> Result<Proposal> {
        let mut turn = self.turn.lock().await;
        if let Some(preamble) = self.preamble.lock().await.take() {
            turn.send(&preamble).await?;
        }
        let reply = turn.send(&render(&input)).await?;
        let problem = match parse_reply(&reply) {
            Ok(proposal) => return Ok(proposal),
            Err(problem) => problem,
        };
        let reply = turn
            .send(&format!(
                "Reply with exactly one fenced json block with text and rationale; {problem}"
            ))
            .await?;
        parse_reply(&reply).map_err(|problem| {
            anyhow::Error::from(BehaviorProposerError::SecondReplyNotProposal { problem })
        })
    }
}

pub(crate) fn render(input: &ProposalInput) -> String {
    let mut out = format!(
        "Current instruction:\n{}\n\nFeedback from the train run:\n",
        fenced(&input.current_text)
    );
    let mut feedback: Vec<_> = input.feedback.iter().collect();
    feedback.sort_by(|a, b| a.check.cmp(&b.check));
    if feedback.is_empty() {
        out.push_str("- none\n");
    }
    for item in feedback {
        let score = item
            .score_bp
            .map_or_else(|| "no score".to_owned(), |bp| percent(Some(bp)));
        match item.feedback.as_deref().unwrap_or("no feedback") {
            text if text.contains('\n') => {
                out.push_str(&format!("- {}: {score} -\n{}\n", item.check, fenced(text)));
            }
            text => out.push_str(&format!("- {}: {score} - {text}\n", item.check)),
        }
    }
    if !input.rejections.is_empty() {
        out.push_str("\nRejected so far:\n");
        for rejection in &input.rejections {
            out.push_str(&format!(
                "- round {}, {}:\n{}\n",
                rejection.round,
                rejection.reason,
                fenced(&rejection.text)
            ));
        }
    }
    out.push_str(&format!(
        "\nRules:\n\
         - The text must differ from the current one.\n\
         - The text must not repeat a rejected candidate.\n\
         - The text must be at most {} bytes.\n\
         - Keep the same audience and job.\n\
         - Do not add tools or claims the feedback does not support.\n\n\
         Reply with exactly one fenced json block: {{\"text\": ..., \"rationale\": ...}}\n",
        input.max_text_bytes
    ));
    out
}

fn fenced(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}")
}

fn parse_reply(reply: &str) -> Result<Proposal, String> {
    let blocks = json_blocks(reply);
    let block = match blocks.as_slice() {
        [block] => block,
        blocks => return Err(format!("the reply has {} fenced json blocks", blocks.len())),
    };
    serde_json::from_str(block)
        .map_err(|error| format!("the json block is not {{\"text\", \"rationale\"}}: {error}"))
}
