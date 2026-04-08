use super::*;

impl RequestLifecycle {
    pub fn new(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
    ) -> Self {
        Self::new_with_execution_binding(
            node,
            agent_name,
            &format!("did:defra-agent:{agent_name}"),
            request,
            deadline_duration_secs,
            ExecutionOrigin::Interactive,
            "",
        )
    }

    pub fn new_with_agent_did(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
    ) -> Self {
        Self::new_with_execution_binding(
            node,
            agent_name,
            agent_did,
            request,
            deadline_duration_secs,
            ExecutionOrigin::Interactive,
            "",
        )
    }

    pub fn new_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
    ) -> Self {
        Self {
            node,
            agent_name: agent_name.to_string(),
            agent_did: agent_did.to_string(),
            execution_origin,
            backend_id: backend_id.into(),
            request,
            response_doc_id: None,
            progress_seq: 0,
            deadline_duration_secs,
            state: LocalLifecycleState::Pending,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn materialize_claimed_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
    ) -> Result<Self> {
        let backend_id = backend_id.into();
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let claimed_at = created_at.clone();
        let deadline = (chrono::Utc::now()
            + chrono::Duration::seconds(deadline_duration_secs as i64))
        .to_rfc3339();

        session::create_session_with_id(node.as_ref(), &session_id, agent_name).await?;

        let escaped_request_id = escape_graphql_string(&request_id);
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_session_id = escape_graphql_string(&session_id);
        let escaped_content = escape_graphql_string(content);
        let escaped_backend_id = escape_graphql_string(&backend_id);
        let escaped_retry_root_request = graphql_retry_root_request(None, &request_id);
        let escaped_created_at = escape_graphql_string(&created_at);
        let escaped_claimed_at = escape_graphql_string(&claimed_at);
        let escaped_deadline = escape_graphql_string(&deadline);
        let execution_origin_str = execution_origin.as_str();

        let mutation = format!(
            r#"mutation {{
                add_AgentRequest(input: {{
                    request_id: "{escaped_request_id}",
                    agent_did: "{escaped_agent_did}",
                    session_id: "{escaped_session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{escaped_retry_root_request}",
                    superseded_by_request: "",
                    content: "{escaped_content}",
                    status: "processing",
                    lifecycle_state: "{lifecycle_state}",
                    admission_state: "{admission_state}",
                    backend_id: "{escaped_backend_id}",
                    execution_origin: "{execution_origin_str}",
                    created_at: "{escaped_created_at}",
                    claimed_at: "{escaped_claimed_at}",
                    deadline: "{escaped_deadline}",
                    retry_count: 0,
                    max_retries: {max_retries}
                }}) {{ _docID }}
            }}"#,
            lifecycle_state = PersistedLifecycleState::Claimed.as_str(),
            admission_state = PersistedAdmissionState::Waiting.as_str(),
            max_retries = DEFAULT_REQUEST_MAX_RETRIES,
        );

        let resp = node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!("creating claimed AgentRequest failed: {:?}", resp.errors);
        }

        let doc_id = if let Some(doc_id) = resp
            .data
            .as_ref()
            .and_then(|data| data.get("add_AgentRequest"))
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
            }) {
            doc_id.to_string()
        } else {
            let query = format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{ _docID }} }}"#
            );
            let query_resp = node.execute(&query).await;
            if query_resp.has_errors() {
                anyhow::bail!(
                    "querying created AgentRequest doc id failed: {:?}",
                    query_resp.errors
                );
            }

            query_resp
                .data
                .as_ref()
                .and_then(|d| d.get("AgentRequest"))
                .and_then(|v| v.as_array())
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("add_AgentRequest returned no _docID"))?
                .to_string()
        };

        let lineage_mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{
                        retry_parent_request: "",
                        retry_root_request: "{escaped_retry_root_request}",
                        superseded_by_request: ""
                    }}
                ) {{ _docID }}
            }}"#,
        );
        let lineage_resp = node.execute(&lineage_mutation).await;
        if lineage_resp.has_errors() {
            anyhow::bail!(
                "persisting request lineage for materialized AgentRequest failed: {:?}",
                lineage_resp.errors
            );
        }

        let request = AgentRequest {
            doc_id,
            request_id: request_id.clone(),
            agent_did: agent_did.to_string(),
            session_id: session_id.clone(),
            content: content.to_string(),
            created_at,
        };

        session::upsert_conversation_from_request_with_did(
            node.as_ref(),
            &session_id,
            agent_name,
            agent_did,
            &request_id,
            content,
            "processing",
        )
        .await?;

        Ok(Self {
            node,
            agent_name: agent_name.to_string(),
            agent_did: agent_did.to_string(),
            execution_origin,
            backend_id,
            request,
            response_doc_id: None,
            progress_seq: 0,
            deadline_duration_secs,
            state: LocalLifecycleState::Claimed,
        })
    }

    pub fn request(&self) -> &AgentRequest {
        &self.request
    }

    pub fn response_doc_id(&self) -> Option<&str> {
        self.response_doc_id.as_deref()
    }

    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }
}
