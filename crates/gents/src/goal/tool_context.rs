use super::*;
use gents_protocol::row::AgentRequestRow;

pub(crate) struct GoalToolContext {
    pub request: AgentRequestRow,
    pub assignment: RequestAssignmentObservation,
    current: Option<GoalDocument>,
}

impl GoalToolContext {
    pub(crate) fn can_write(&self) -> bool {
        matches!(&self.assignment, RequestAssignmentObservation::Assigned(goal)
            if self.current.as_ref().is_some_and(|current| same_goal_assignment(goal, current)))
    }

    pub(super) fn can_create(&self) -> bool {
        match self.assignment {
            RequestAssignmentObservation::None => self.current.is_none(),
            RequestAssignmentObservation::Unknown => false,
            RequestAssignmentObservation::Assigned(_) => self.can_write(),
        }
    }
}

pub(super) fn same_goal_assignment(left: &GoalDocument, right: &GoalDocument) -> bool {
    left.doc_id == right.doc_id
        && left.goal_id == right.goal_id
        && left.node_did == right.node_did
        && left.session_id == right.session_id
        && left.assignment_root_request_doc_id == right.assignment_root_request_doc_id
}

pub(crate) async fn load_goal_tool_context(
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    session: &str,
    request_id: &str,
    request_doc_id: Option<&str>,
) -> Result<GoalToolContext> {
    let doc = request_doc_id.context("Goal tool caller has no physical request identity")?;
    crate::ConfigAccess::transact_local_readonly(node, None, "goal.tool_context", |txn| {
        Box::pin(load_goal_tool_context_in_txn(
            txn, owner, session, request_id, doc,
        ))
    })
    .await
}

pub(super) async fn load_goal_tool_context_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    session: &str,
    request_id: &str,
    request_doc_id: &str,
) -> Result<GoalToolContext> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }},
        session_id: {{ _eq: "{}" }}, request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(request_doc_id),
            escape_graphql_string(owner),
            escape_graphql_string(session),
            escape_graphql_string(request_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    let mut requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("Goal tool caller query omitted rows")?,
    )?;
    anyhow::ensure!(
        requests.len() == 1,
        "Goal tool caller is absent or ambiguous in its owner/session"
    );
    let request = requests.pop().expect("one physical caller");
    let assignment = load_request_assignment_in_txn(txn, &request).await?;
    let current = load_canonical_goal_in_txn(txn, owner, session).await?;
    Ok(GoalToolContext {
        request,
        assignment,
        current,
    })
}
