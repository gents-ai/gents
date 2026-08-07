use super::*;
use std::collections::HashSet;

#[derive(serde::Deserialize)]
struct CompositeCommitRow {
    cid: String,
    height: i64,
}

async fn composite_commits(node: &EmbeddedNode, doc_id: &str) -> Result<Vec<CompositeCommitRow>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"query {{
            _commits(
                docID: ["{escaped_doc_id}"],
                filter: {{ fieldName: {{ _eq: "_C" }} }}
            ) {{
                cid
                height
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "querying AgentRequest {doc_id} composite commits failed: {:?}",
            response.errors
        );
    }
    response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map(|rows| rows.unwrap_or_default())
        .map_err(Into::into)
}

pub(super) async fn current_composite_commit_cids(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<HashSet<String>> {
    Ok(composite_commits(node, doc_id)
        .await?
        .into_iter()
        .map(|commit| commit.cid)
        .collect())
}

pub(super) async fn resolve_claimed_request_version(
    node: &EmbeddedNode,
    doc_id: &str,
    claimed_at: &str,
    deadline: &str,
    behavior_id: &str,
    backend_id: &str,
    execution_origin: &str,
    commits_before_claim: &HashSet<String>,
) -> Result<(crate::DocumentVersionRef, AgentRequest)> {
    let mut commits = composite_commits(node, doc_id).await?;
    commits.retain(|commit| !commits_before_claim.contains(&commit.cid));
    // The successful conditional mutation is the earliest new snapshot with
    // these claim markers. Later mutations inherit the markers, so choosing a
    // newest match would silently move the execution boundary forward.
    commits.sort_by(|left, right| {
        left.height
            .cmp(&right.height)
            .then_with(|| left.cid.cmp(&right.cid))
    });

    let mut selected: Option<(i64, crate::DocumentVersionRef, AgentRequest)> = None;
    for commit in commits {
        if selected
            .as_ref()
            .is_some_and(|(height, _, _)| commit.height > *height)
        {
            break;
        }
        let Some(snapshot) =
            crate::watcher::load_agent_request_at_cid(node, &commit.cid, doc_id).await?
        else {
            continue;
        };
        let request = &snapshot.request;
        let matches_claim = snapshot.status == "processing"
            && snapshot.lifecycle_state.as_deref() == Some("claimed")
            && snapshot.claimed_at.as_deref() == Some(claimed_at)
            && snapshot.backend_id.as_deref().unwrap_or("") == backend_id
            && request.behavior_id.as_deref().unwrap_or("") == behavior_id
            && request.execution_origin.as_deref().unwrap_or("") == execution_origin
            && request.deadline.as_deref() == Some(deadline);
        if matches_claim {
            if selected.is_some() {
                anyhow::bail!(
                    "AgentRequest {doc_id} has multiple new claim snapshots at height {}; refusing to choose a CID",
                    commit.height
                );
            }
            selected = Some((
                commit.height,
                crate::DocumentVersionRef::new(doc_id, commit.cid),
                snapshot.request,
            ));
        }
    }

    if let Some((_, version, request)) = selected {
        return Ok((version, request));
    }

    anyhow::bail!(
        "AgentRequest {doc_id} was claimed but no new composite commit reconstructs the exact claim snapshot"
    )
}

fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

async fn fetch_interrupt_and_ttl(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                interrupt_requested_at
                valid_until
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("fetch_interrupt_and_ttl for {doc_id}: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array());
    let row = rows
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("AgentRequest {doc_id} not found"))?;
    let interrupt = row
        .get("interrupt_requested_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let valid = row
        .get("valid_until")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok((interrupt, valid))
}

impl RequestLifecycle {
    pub fn set_response_doc_id(&mut self, doc_id: &str) {
        self.ensure_state(
            &[LocalLifecycleState::Claimed, LocalLifecycleState::Streaming],
            "set_response_doc_id",
        )
        .expect("response doc can only be attached after claim");
        self.response_doc_id = Some(doc_id.to_string());
        self.state = LocalLifecycleState::Streaming;
    }

    pub async fn claim(&mut self) -> Result<ClaimOutcome> {
        self.claim_inner(false).await
    }

    pub async fn claim_with_identity(&mut self) -> Result<ClaimOutcome> {
        self.claim_inner(true).await
    }

    pub async fn begin_execution(&mut self) -> Result<()> {
        self.ensure_state(&[LocalLifecycleState::Claimed], "begin_execution")?;
        self.transition_execution_view(
            "processing",
            PersistedLifecycleState::Claimed,
            "processing",
            PersistedLifecycleState::Processing,
        )
        .await
    }

    async fn transition_pending_to_interrupted(&mut self, _interrupt_at: &str) -> Result<()> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "interrupted",
                        lifecycle_state: "interrupted",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let resp = crate::retry::execute_graphql_with_terminal_persistence_retry(
            &self.node,
            &mutation,
            "interrupt_before_claim",
        )
        .await?;
        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            let request_view = self.request_view().await?;
            if request_view.as_ref().is_some_and(|row| {
                row.status == "interrupted" && row.lifecycle_state.as_deref() == Some("interrupted")
            }) {
                return Ok(());
            }
            anyhow::bail!(
                "request {} could not transition pending -> interrupted; current status={} lifecycle_state={}",
                self.request.request_id,
                request_view
                    .as_ref()
                    .map(|row| row.status.as_str())
                    .unwrap_or("missing"),
                request_view
                    .as_ref()
                    .and_then(|row| row.lifecycle_state.as_deref())
                    .unwrap_or("missing")
            );
        }
        Ok(())
    }

    async fn transition_pending_to_dead_stale(&mut self) -> Result<()> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let agent_did = escape_graphql_string(&self.request.agent_did);
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "dead",
                        lifecycle_state: "dead",
                        failure_reason: "Stale",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let resp = crate::retry::execute_graphql_with_terminal_persistence_retry(
            &self.node,
            &mutation,
            "expire_stale",
        )
        .await?;
        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            let request_view = self.request_view().await?;
            if request_view.as_ref().is_some_and(|row| {
                row.status == "dead" && row.lifecycle_state.as_deref() == Some("dead")
            }) {
                return Ok(());
            }
            anyhow::bail!(
                "request {} could not transition pending -> dead; current status={} lifecycle_state={}",
                self.request.request_id,
                request_view
                    .as_ref()
                    .map(|row| row.status.as_str())
                    .unwrap_or("missing"),
                request_view
                    .as_ref()
                    .and_then(|row| row.lifecycle_state.as_deref())
                    .unwrap_or("missing")
            );
        }
        self.failure_reason = Some("Stale".to_string());
        Ok(())
    }

    async fn claim_inner(&mut self, _explicit_did: bool) -> Result<ClaimOutcome> {
        self.ensure_state(&[LocalLifecycleState::Pending], "claim")?;
        let (interrupt_requested_at, valid_until) =
            fetch_interrupt_and_ttl(&self.node, &self.request.doc_id).await?;

        if let Some(interrupt_at) = interrupt_requested_at {
            self.transition_pending_to_interrupted(&interrupt_at)
                .await?;
            self.state = LocalLifecycleState::Interrupted;
            return Ok(ClaimOutcome::Interrupted);
        }

        let valid_until_at_claim = match valid_until.as_deref() {
            Some(s) => {
                let dt = chrono::DateTime::parse_from_rfc3339(s)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "invalid valid_until on request {}: {e}",
                            self.request.doc_id
                        )
                    })?
                    .with_timezone(&chrono::Utc);
                if chrono::Utc::now() > dt {
                    self.transition_pending_to_dead_stale().await?;
                    self.state = LocalLifecycleState::Dead;
                    return Ok(ClaimOutcome::Expired);
                }
                Some(dt)
            }
            None => None,
        };

        let dedup = self.check_deduplication().await?;
        if !dedup.is_earliest {
            tracing::info!(
                request_id = %self.request.request_id,
                session_id = %self.request.session_id,
                blocking_request_id = dedup.blocking_request_id.as_deref().unwrap_or(""),
                "request remains queued behind earlier same-session request"
            );
            return Ok(ClaimOutcome::Queued);
        }

        let now = chrono::Utc::now();
        let claimed_at = now.to_rfc3339();
        let synthesized_deadline_at =
            now + chrono::Duration::seconds(self.deadline_duration_secs as i64);
        let deadline_at = self
            .request
            .deadline
            .as_deref()
            .and_then(parse_rfc3339_utc)
            .unwrap_or(synthesized_deadline_at);
        let deadline = deadline_at.to_rfc3339();
        let doc_id = &self.request.doc_id;
        let commits_before_claim = current_composite_commit_cids(&self.node, doc_id).await?;
        let escaped_doc_id = escape_graphql_string(doc_id);
        let escaped_claimed_at = escape_graphql_string(&claimed_at);
        let escaped_deadline = escape_graphql_string(&deadline);
        let escaped_backend_id = escape_graphql_string(&self.backend_id);
        let escaped_behavior_id = escape_graphql_string(&self.behavior_id);
        let execution_origin = self.execution_origin.as_str();

        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "processing",
                        lifecycle_state: "{lifecycle_state}",
                        behavior_id: "{escaped_behavior_id}",
                        backend_id: "{escaped_backend_id}",
                        execution_origin: "{execution_origin}",
                        claimed_at: "{escaped_claimed_at}",
                        deadline: "{escaped_deadline}"
                    }}
                ) {{ _docID }}
            }}"#,
            lifecycle_state = PersistedLifecycleState::Claimed.as_str(),
        );

        let resp =
            session::execute_mutation_with_retry(&self.node, &mutation, "claim_request").await?;

        if !resp
            .data
            .as_ref()
            .and_then(|d| d.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            match self.request_status().await? {
                Some(status) if status == "processing" => {
                    tracing::debug!(doc_id = %doc_id, "claimed via post-update verification");
                }
                Some(status) => {
                    anyhow::bail!("request {} is no longer pending (status={status})", doc_id)
                }
                None => anyhow::bail!("request {} disappeared while claiming", doc_id),
            }
        } else {
            tracing::debug!(
                doc_id = %doc_id,
                deadline = %deadline,
                backend_id = %self.backend_id,
                execution_origin,
                "claimed agent request with deadline"
            );
        }

        let (request_version, claimed_request) = resolve_claimed_request_version(
            &self.node,
            doc_id,
            &claimed_at,
            &deadline,
            &self.behavior_id,
            &self.backend_id,
            execution_origin,
            &commits_before_claim,
        )
        .await?;
        self.request = claimed_request;
        self.request_version = Some(request_version);

        self.state = LocalLifecycleState::Claimed;
        self.claimed_deadline_at = Some(deadline_at);
        self.valid_until_at_claim = valid_until_at_claim;

        Ok(ClaimOutcome::Claimed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const TEST_AGENT_DID: &str = "did:test:claim-order-test";
    const TEST_BEHAVIOR_ID: &str = "general";
    const TEST_BACKEND_ID: &str = "backend-order";

    async fn test_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        node
    }

    async fn insert_pending_request(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        created_at: &str,
    ) -> AgentRequest {
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_created_at = escape_graphql_string(created_at);
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{escaped_request_id}",
                    agent_did: "{TEST_AGENT_DID}",
                    behavior_id: "{TEST_BEHAVIOR_ID}",
                    session_id: "{escaped_session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{escaped_request_id}",
                    superseded_by_request: "",
                    content: "same-session request",
                    status: "pending",
                    lifecycle_state: "pending",
                    backend_id: "",
                    execution_origin: "interactive",
                    failure_reason: "",
                    created_at: "{escaped_created_at}",
                    retry_count: 0,
                    max_retries: {max_retries},
                    subagent_depth: 0
                }}) {{ _docID }}
            }}"#,
            max_retries = DEFAULT_REQUEST_MAX_RETRIES,
        );
        let response =
            session::execute_mutation_with_retry(node, &mutation, "insert_pending_request")
                .await
                .unwrap();
        let inline_doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("create_AgentRequest"))
            .and_then(|value| {
                value
                    .get("_docID")
                    .and_then(|doc_id| doc_id.as_str())
                    .or_else(|| {
                        value
                            .as_array()
                            .and_then(|rows| rows.first())
                            .and_then(|row| row.get("_docID"))
                            .and_then(|doc_id| doc_id.as_str())
                    })
            })
            .map(ToOwned::to_owned);
        let doc_id = match inline_doc_id {
            Some(doc_id) => doc_id,
            None => lookup_request_doc_id(node, request_id)
                .await
                .expect("created AgentRequest doc id"),
        };

        AgentRequest {
            doc_id,
            request_id: request_id.to_string(),
            agent_did: TEST_AGENT_DID.to_string(),
            requester_did: None,
            behavior_id: Some(TEST_BEHAVIOR_ID.to_string()),
            session_id: session_id.to_string(),
            content: "same-session request".to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            metadata: None,
            execution_origin: Some("interactive".to_string()),
            created_at: created_at.to_string(),
            deadline: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_tool_call_id: None,
        }
    }

    async fn lookup_request_doc_id(
        node: &EmbeddedNode,
        request_id: &str,
    ) -> anyhow::Result<String> {
        let escaped_request_id = escape_graphql_string(request_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{ _docID }}
            }}"#
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("query created AgentRequest failed: {:?}", response.errors);
        }
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|value| value.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("AgentRequest {request_id} not found"))
    }

    #[tokio::test]
    async fn claim_preserves_same_session_ordering() {
        let node = test_node().await;
        let first = insert_pending_request(
            node.as_ref(),
            "same-session-request-1",
            "same-session",
            "2026-01-01T00:00:00Z",
        )
        .await;
        let second = insert_pending_request(
            node.as_ref(),
            "same-session-request-2",
            "same-session",
            "2026-01-01T00:00:01Z",
        )
        .await;

        let mut first_lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            first,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        let mut second_lifecycle = RequestLifecycle::new_with_execution_binding(
            node,
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            second,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );

        assert_eq!(
            first_lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        assert_eq!(
            second_lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Queued
        );
    }

    #[tokio::test]
    async fn claim_reloads_the_exact_composite_snapshot_it_pins() {
        let node = test_node().await;
        let stale_request = insert_pending_request(
            node.as_ref(),
            "claim-version-request",
            "claim-version-session",
            "2026-01-01T00:00:00Z",
        )
        .await;
        let doc_id = stale_request.doc_id.clone();
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{}" }} }},
                    input: {{ content: "edited after watcher read" }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&doc_id),
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "pre-claim edit failed: {:?}",
            response.errors
        );

        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            TEST_BEHAVIOR_ID,
            TEST_AGENT_DID,
            stale_request,
            60,
            ExecutionOrigin::Interactive,
            TEST_BACKEND_ID,
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );

        assert_eq!(lifecycle.request().content, "edited after watcher read");
        let version = lifecycle
            .request_version()
            .expect("claim must pin an AgentRequest version");
        assert_eq!(version.doc_id, doc_id);
        assert!(!version.composite_commit_cid.is_empty());

        let snapshot = crate::watcher::load_agent_request_at_cid(
            node.as_ref(),
            &version.composite_commit_cid,
            &doc_id,
        )
        .await
        .unwrap()
        .expect("pinned claim snapshot");
        assert_eq!(snapshot.status, "processing");
        assert_eq!(snapshot.lifecycle_state.as_deref(), Some("claimed"));
        assert_eq!(snapshot.request.content, lifecycle.request().content);
    }

    #[tokio::test]
    async fn claim_version_resolution_rejects_later_marker_preserving_edits() {
        let node = test_node().await;
        let pending = insert_pending_request(
            node.as_ref(),
            "claim-version-race-request",
            "claim-version-race-session",
            "2026-01-01T00:00:00Z",
        )
        .await;
        let doc_id = pending.doc_id.clone();
        let commits_before_claim = current_composite_commit_cids(node.as_ref(), &doc_id)
            .await
            .unwrap();
        let claimed_at = "2026-01-01T00:00:01Z";
        let deadline = "2026-01-01T00:01:01Z";
        let claim = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "processing",
                        lifecycle_state: "claimed",
                        behavior_id: "{TEST_BEHAVIOR_ID}",
                        backend_id: "{TEST_BACKEND_ID}",
                        execution_origin: "interactive",
                        claimed_at: "{claimed_at}",
                        deadline: "{deadline}"
                    }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&doc_id),
        );
        let response = node.execute(&claim).await;
        assert!(
            !response.has_errors(),
            "claim failed: {:?}",
            response.errors
        );

        // This commit inherits every claim marker. A newest-first scan would
        // therefore pin this later content instead of the claim boundary.
        let edit = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{}" }} }},
                    input: {{ content: "edited after claim" }}
                ) {{ _docID }}
            }}"#,
            escape_graphql_string(&doc_id),
        );
        let response = node.execute(&edit).await;
        assert!(
            !response.has_errors(),
            "post-claim edit failed: {:?}",
            response.errors
        );

        let (version, snapshot) = resolve_claimed_request_version(
            node.as_ref(),
            &doc_id,
            claimed_at,
            deadline,
            TEST_BEHAVIOR_ID,
            TEST_BACKEND_ID,
            "interactive",
            &commits_before_claim,
        )
        .await
        .unwrap();
        assert_eq!(snapshot.content, pending.content);
        assert_ne!(snapshot.content, "edited after claim");

        let reconstructed = crate::watcher::load_agent_request_at_cid(
            node.as_ref(),
            &version.composite_commit_cid,
            &doc_id,
        )
        .await
        .unwrap()
        .expect("pinned claim snapshot");
        assert_eq!(reconstructed.request.content, pending.content);
    }
}
