//! One turn with the author: a user message on the author's session, and
//! the text it answered with. The interview and the pilot take a [`Turn`];
//! production sends it through the chat turn function, tests script it.

use anyhow::{Context, Result};

use crate::commands::chat::{
    chat_turn_text_content, load_existing_tool_call_keys, stream_turn_progress,
};
use crate::request_helpers::wait_for_terminal_response;
use crate::{create_agent_request, RequestSubmitOptions};

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
/// session with `behavior_id`, followed until the response lands. Its
/// progress and the reply print as they arrive, unless `quiet`: then the
/// turn is followed without writing to stdout, as `gents chat --json` does.
pub(crate) struct LiveTurn {
    pub(crate) graphql: gents::config_client::GraphqlEndpoint,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: String,
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
            &self.agent_did,
            content,
            Some(&self.session_id),
            Some(&self.behavior_id),
            RequestSubmitOptions::default(),
        )
        .await
        .with_context(|| format!("submitting the {} turn", self.behavior_id))?;
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
                None,
            )
            .await?
        };
        let text = chat_turn_text_content(&response);
        anyhow::ensure!(
            !text.trim().is_empty(),
            "the {} turn ended without a reply; `gents response show {}` shows why",
            self.behavior_id,
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

#[cfg(test)]
mod tests {
    use super::{LiveTurn, Turn};

    fn live(quiet: bool) -> LiveTurn {
        LiveTurn {
            graphql: gents::config_client::GraphqlEndpoint::anonymous("http://localhost:0/graphql"),
            agent_did: "did:key:owner".to_owned(),
            behavior_id: "prompt-proposer".to_owned(),
            session_id: "session".to_owned(),
            timeout_secs: 1,
            poll_secs: 1,
            quiet,
        }
    }

    #[test]
    fn a_quiet_live_turn_shows_no_replies() {
        assert!(live(false).shows_replies());
        assert!(!live(true).shows_replies());
    }
}
