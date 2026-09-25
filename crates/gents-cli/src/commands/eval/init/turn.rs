//! One turn with the author: a user message on the author's session, and
//! the text it answered with. The interview and the pilot take a [`Turn`];
//! production sends it through the chat turn function, tests script it.

use anyhow::{Context, Result};

use crate::commands::chat::{
    chat_turn_text_content, load_existing_tool_call_keys, stream_turn_progress,
};
use crate::{create_agent_request, RequestSubmitOptions};

/// The behavior the `eval_author` pack installs.
pub(crate) const AUTHOR_BEHAVIOR: &str = "eval-author";

#[async_trait::async_trait]
pub(crate) trait Turn {
    /// Send one user turn on the author's session; return the author's text.
    async fn send(&mut self, content: &str) -> Result<String>;
    fn session_id(&self) -> &str;
    /// Whether the author's reply already reached the operator's terminal
    /// while it arrived; the interview prints it otherwise.
    fn shows_replies(&self) -> bool;
}

/// A turn on the served home, as `gents chat` sends one: submitted on the
/// author's session with behavior `eval-author`, followed until the
/// response lands. Its progress and the reply print as they arrive.
pub(crate) struct LiveTurn {
    pub(crate) graphql: String,
    pub(crate) agent_did: String,
    pub(crate) session_id: String,
    pub(crate) timeout_secs: u64,
    pub(crate) poll_secs: u64,
}

#[async_trait::async_trait]
impl Turn for LiveTurn {
    async fn send(&mut self, content: &str) -> Result<String> {
        let known = load_existing_tool_call_keys(&self.graphql, &self.session_id).await?;
        let submitted = create_agent_request(
            &self.graphql,
            &self.agent_did,
            content,
            Some(&self.session_id),
            Some(AUTHOR_BEHAVIOR),
            RequestSubmitOptions::default(),
        )
        .await
        .context("submitting the author's turn")?;
        let response = stream_turn_progress(
            &self.graphql,
            &submitted,
            known,
            self.timeout_secs,
            self.poll_secs,
            false,
        )
        .await?;
        let text = chat_turn_text_content(&response);
        anyhow::ensure!(
            !text.trim().is_empty(),
            "the author's turn ended without a reply; `gents response show {}` shows why",
            submitted.request_id
        );
        Ok(text.to_owned())
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn shows_replies(&self) -> bool {
        true
    }
}

/// Replies stated up front, and every turn sent, for the loop's tests.
#[cfg(test)]
pub(crate) struct ScriptedTurn {
    replies: std::collections::VecDeque<String>,
    pub(crate) sent: Vec<String>,
}

#[cfg(test)]
impl ScriptedTurn {
    pub(crate) fn new<S: Into<String>>(replies: impl IntoIterator<Item = S>) -> Self {
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
