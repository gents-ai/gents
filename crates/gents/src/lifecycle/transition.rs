use anyhow::Context;

use super::*;

impl RequestLifecycle {
    pub async fn advance(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Streaming], "advance")?;
        let response_doc_id = self
            .response_doc_id
            .as_deref()
            .context("advance() called before response doc created")?;
        let next_progress_seq = self
            .progress_seq
            .checked_add(1)
            .context("response progress overflow")?;
        let fence = super::execution_lease::ExecutionWriteFence {
            request_doc_id: self.request.doc_id.clone(),
            execution_generation: self.execution_generation()?.to_owned(),
            lease_duration_secs: self.execution_lease_duration_secs,
        };
        let response_doc_id = escape_graphql_string(response_doc_id);
        let response_mutation = format!(
            r#"mutation {{ update_AgentResponse(
            filter: {{ _docID: {{ _eq: "{response_doc_id}" }}, status: {{ _eq: "streaming" }} }},
            input: {{ progress_seq: {next_progress_seq} }}
        ) {{ _docID }} }}"#
        );
        fence
            .execute_response_write(&self.node, &response_mutation, ExecutionWriteKind::Progress)
            .await?;
        self.progress_seq = next_progress_seq;
        Ok(())
    }

    pub(super) fn ensure_state(
        &self,
        expected: &[LocalLifecycleState],
        action: &str,
    ) -> Result<()> {
        if expected.contains(&self.state) {
            return Ok(());
        }

        anyhow::bail!(
            "cannot {} request_id={} while lifecycle is in {:?}",
            action,
            self.request.request_id,
            self.state
        )
    }
}
