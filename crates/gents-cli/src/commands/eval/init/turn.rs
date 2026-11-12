use anyhow::{Context, Result};

use gents_protocol::operations::request::MessageSendInput;
use gents_store::config_client::ConfigAccess;

use crate::commands::chat::{
    chat_turn_text_content, load_existing_tool_call_keys, stream_turn_progress,
};
use crate::operation_failure::OperationFailure;
use crate::request_helpers::{client_signer, request_ref, wait_for_request};

#[async_trait::async_trait]
pub(crate) trait Turn {
    async fn send(&mut self, content: &str) -> Result<String>;
    fn session_id(&self) -> &str;
    fn shows_replies(&self) -> bool;
}

pub(crate) struct LiveTurn {
    pub(crate) graphql: gents_store::config_client::GraphqlEndpoint,
    pub(crate) node_did: String,
    pub(crate) agent_did: String,
    pub(crate) session_id: String,
    pub(crate) timeout_secs: u64,
    pub(crate) poll_secs: u64,
    pub(crate) quiet: bool,
}

#[async_trait::async_trait]
impl Turn for LiveTurn {
    async fn send(&mut self, content: &str) -> Result<String> {
        let access = ConfigAccess::Graphql(self.graphql.clone());
        let signer = client_signer(&access)?;
        let submitted = gents::operations::request::send(
            &access,
            &signer,
            MessageSendInput {
                node_did: self.node_did.clone(),
                session_id: Some(self.session_id.clone()),
                agent_did: Some(self.agent_did.clone()),
                content: content.to_owned(),
                ..Default::default()
            },
        )
        .await
        .map_err(OperationFailure::from)
        .with_context(|| format!("submitting the {} turn", self.agent_did))?;
        let response = if self.quiet {
            wait_for_request(
                &access,
                request_ref(&submitted.node_did, &submitted.request_id),
                self.timeout_secs,
                self.poll_secs,
                || async { Ok(false) },
            )
            .await?
        } else {
            let known = load_existing_tool_call_keys(&self.graphql, &self.session_id).await?;
            stream_turn_progress(
                &access,
                &submitted,
                known,
                self.timeout_secs,
                self.poll_secs,
                false,
                true,
            )
            .await?
        };
        let text = chat_turn_text_content(&response);
        anyhow::ensure!(
            !text.trim().is_empty(),
            "the {} turn ended without a reply; `gents utils debug response show {}` shows why",
            self.agent_did,
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
