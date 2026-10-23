use anyhow::{Context, Result};

use crate::commands::chat::{
    chat_turn_text_content, load_existing_tool_call_keys, stream_turn_progress,
};
use crate::request_helpers::wait_for_terminal_response;
use crate::{create_agent_request, RequestSubmitOptions};

#[async_trait::async_trait]
pub(crate) trait Turn {
    async fn send(&mut self, content: &str) -> Result<String>;
    fn session_id(&self) -> &str;
    fn shows_replies(&self) -> bool;
}

pub(crate) struct LiveTurn {
    pub(crate) graphql: gents::config_client::GraphqlEndpoint,
    pub(crate) node_did: String,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) timeout_secs: u64,
    pub(crate) poll_secs: u64,
    pub(crate) quiet: bool,
}

#[async_trait::async_trait]
impl Turn for LiveTurn {
    async fn send(&mut self, content: &str) -> Result<String> {
        let submitted = create_agent_request(
            &self.graphql,
            &self.node_did,
            content,
            Some(&self.session_id),
            Some(&self.agent_id),
            RequestSubmitOptions::default(),
        )
        .await
        .with_context(|| format!("submitting the {} turn", self.agent_id))?;
        let response = if self.quiet {
            wait_for_terminal_response(
                &self.graphql,
                &submitted.request_id,
                self.timeout_secs,
                self.poll_secs,
            )
            .await?
        } else {
            let known = load_existing_tool_call_keys(&self.graphql, &self.session_id).await?;
            stream_turn_progress(
                &self.graphql,
                &submitted,
                known,
                self.timeout_secs,
                self.poll_secs,
                false,
                crate::commands::chat::ApprovalHandling::None,
                true,
            )
            .await?
        };
        let text = chat_turn_text_content(&response);
        anyhow::ensure!(
            !text.trim().is_empty(),
            "the {} turn ended without a reply; `gents response show {}` shows why",
            self.agent_id,
            submitted.request_id
        );
        Ok(text.to_owned())
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn shows_replies(&self) -> bool {
        !self.quiet
    }
}

#[cfg(test)]
pub(crate) struct ScriptedTurn {
    replies: std::collections::VecDeque<String>,
    pub(crate) sent: Vec<String>,
}

#[cfg(test)]
impl ScriptedTurn {
    pub(crate) fn new<S>(replies: impl IntoIterator<Item = S>) -> Self
    where
        S: Into<String>,
    {
        Self {
            replies: replies.into_iter().map(Into::into).collect(),
            sent: Vec::new(),
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Turn for ScriptedTurn {
    async fn send(&mut self, content: &str) -> Result<String> {
        self.sent.push(content.to_owned());
        self.replies
            .pop_front()
            .context("the scripted author has no reply left")
    }

    fn session_id(&self) -> &str {
        "scripted-session"
    }

    fn shows_replies(&self) -> bool {
        false
    }
}
