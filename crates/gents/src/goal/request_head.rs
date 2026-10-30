use super::*;
use crate::request::admission::{
    verify_request_receipt_signature, verify_runtime_local_control_receipt,
};
use crate::request::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequestRow;

pub(crate) fn verify_goal_continuation_edge(
    node_did: &str,
    session_id: &str,
    goal_id: &str,
    parent_row: &AgentRequestRow,
    child: &AgentRequestRow,
) -> Result<(i64, bool)> {
    anyhow::ensure!(
        parent_row.purpose == Some(RequestPurpose::Normal)
            && child.purpose == Some(RequestPurpose::Normal)
            && parent_row.node_did.as_deref() == Some(node_did)
            && parent_row.session_id.as_deref() == Some(session_id)
            && child.session_id.as_deref() == Some(session_id),
        "continuation predecessor is outside the goal owner/session"
    );
    verify_request_receipt_signature(parent_row)?;
    let parent_doc = parent_row
        .doc_id
        .as_deref()
        .context("continuation predecessor has no document ID")?;
    verify_runtime_local_control_receipt(
        child,
        node_did,
        &parent_row.request_id,
        parent_row.requester_did.as_deref().unwrap_or(node_did),
    )?;
    anyhow::ensure!(
        child.caused_by_parent_request_doc_id.as_deref() == Some(parent_doc)
            && (parent_row.caused_by_trigger_kind.as_deref() != Some(GOAL_TRIGGER_KIND)
                || parent_row.caused_by_trigger_id.as_deref() == Some(goal_id))
            && child
                .doc_id
                .as_deref()
                .is_some_and(|id| !id.is_empty() && id != parent_doc)
            && child.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
            && child.caused_by_trigger_id.as_deref() == Some(goal_id),
        "continuation receipt has a different physical goal edge"
    );
    let facts = child
        .input
        .as_ref()
        .and_then(|input| input.goal_continuation.as_ref())
        .context("continuation receipt lacks its original issuance facts")?;
    let parent_reference = parent_row
        .input
        .as_ref()
        .and_then(|input| input.goal_assignment.as_ref());
    let child_reference = child
        .input
        .as_ref()
        .and_then(|input| input.goal_assignment.as_ref());
    anyhow::ensure!(
        continuation_reference_matches(goal_id, parent_doc, parent_reference, child_reference),
        "continuation receipt changed its signed goal assignment"
    );
    let sequence = facts.sequence;
    let identity = goal_continuation_identity(goal_id, &parent_row.request_id, sequence)?;
    anyhow::ensure!(
        child.request_id == identity.request_id
            && child.retry_key.as_deref() == Some(identity.retry_key.as_str()),
        "continuation receipt has a different deterministic identity"
    );
    let wrapup = facts.wrapup;
    Ok((sequence, wrapup))
}

pub(super) fn verify_goal_continuation_receipt(
    goal: &GoalDocument,
    parent_row: &AgentRequestRow,
    child: &AgentRequestRow,
) -> Result<()> {
    let (sequence, wrapup) = verify_goal_continuation_edge(
        &goal.node_did,
        &goal.session_id,
        &goal.goal_id,
        parent_row,
        child,
    )?;
    let parent = crate::request::watcher::AgentRequest::try_from(parent_row.clone())?;
    let agent_id = parent.agent_id.clone();
    let mut expected = prepare_goal_continuation(
        &parent,
        agent_id,
        &goal.goal_id,
        child
            .content
            .as_deref()
            .context("continuation receipt lacks content")?,
        sequence,
        wrapup,
        child
            .created_at
            .as_deref()
            .context("continuation receipt lacks creation time")?,
        child
            .request_hop
            .and_then(|hop| u32::try_from(hop).ok())
            .context("continuation receipt lacks its hop")?,
    )?;
    expected.input.goal_assignment = Some(continuation_assignment(
        goal,
        &parent,
        child.doc_id.as_deref(),
    )?);
    let actual: GoalBackedRequestFingerprint =
        serde_json::from_value(serde_json::to_value(child)?)?;
    anyhow::ensure!(
        actual == GoalBackedRequestFingerprint::from_create(&expected)?,
        "continuation receipt conflicts with the goal and physical predecessor binding"
    );
    Ok(())
}

pub(crate) fn request_selection_fields() -> String {
    format!(
        "{} failure_reason superseded_by_request superseded_by_request_doc_id",
        crate::request::admission::SIGNED_REQUEST_FIELDS
    )
}

pub(crate) fn latest_goal_request<'a>(
    goal: &GoalDocument,
    rows: &'a [AgentRequestRow],
) -> Option<&'a AgentRequestRow> {
    let assignment = if goal.assignment_root_request_doc_id.is_some() {
        Some((
            request_rows_by_doc(&goal.node_did, &goal.session_id, rows).ok()?,
            assignment_openings(std::slice::from_ref(goal)),
        ))
    } else {
        None
    };
    latest_scoped_request(
        &goal.node_did,
        &goal.session_id,
        &goal.goal_id,
        rows,
        |row| {
            let Some((by_doc, openings)) = assignment.as_ref() else {
                return true;
            };
            authenticated_assignment_entry(
                &goal.node_did,
                &goal.session_id,
                row,
                by_doc,
                Some(&goal.goal_id),
                openings,
            )
            .inspect_err(|error| {
                tracing::debug!(
                    goal_id = %goal.goal_id,
                    request_doc_id = ?row.doc_id,
                    %error,
                    "Goal assignment ancestry is unavailable"
                );
            })
            .is_ok_and(|entry| super::request_assignment::request_belongs_to_goal(goal, entry))
        },
    )
}

fn latest_scoped_request<'a, F>(
    node_did: &str,
    session_id: &str,
    goal_id: &str,
    rows: &'a [AgentRequestRow],
    allowed: F,
) -> Option<&'a AgentRequestRow>
where
    F: Fn(&AgentRequestRow) -> bool,
{
    let in_scope = |row: &AgentRequestRow| {
        row.purpose == Some(RequestPurpose::Normal)
            && row.node_did.as_deref() == Some(node_did)
            && row.session_id.as_deref() == Some(session_id)
            && allowed(row)
    };
    rows.iter().filter(|row| in_scope(row)).find(|parent| {
        if crate::request::lifecycle::folded_into(parent).is_some()
            || crate::request::lifecycle::retired_by_author(parent)
        {
            return false;
        }
        !rows.iter().any(|child| {
            let Some(original_goal_id) = child.caused_by_trigger_id.as_deref() else {
                return false;
            };
            parent.doc_id.is_some()
                && child.doc_id != parent.doc_id
                && child.caused_by_parent_request_doc_id == parent.doc_id
                && child.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
                && child.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                && goal_id == original_goal_id
                && in_scope(child)
                && verify_goal_continuation_edge(
                    node_did,
                    session_id,
                    original_goal_id,
                    parent,
                    child,
                )
                .is_ok()
        })
    })
}

pub(crate) struct AuthenticatedGoalRequestMembers<'a> {
    pub(crate) entry: &'a AgentRequestRow,
    pub(crate) member_doc_ids: Vec<String>,
}

pub(crate) fn authenticated_goal_request_members<'a>(
    node_did: &str,
    session_id: &str,
    current_doc_id: &str,
    rows: &'a [AgentRequestRow],
) -> Result<AuthenticatedGoalRequestMembers<'a>> {
    let by_doc = request_rows_by_doc(node_did, session_id, rows)?;
    let current = *by_doc
        .get(current_doc_id)
        .context("current request is absent from its owner/session observation")?;
    let mut entries = rapidhash::RapidHashMap::default();
    let entry = authenticated_physical_entry(
        node_did,
        session_id,
        current,
        &by_doc,
        None,
        None,
        Some(&mut entries),
    )?;
    let entry_doc = entry
        .doc_id
        .as_deref()
        .context("entry has no document ID")?;
    let mut member_doc_ids = by_doc
        .iter()
        .filter_map(|(doc, row)| {
            authenticated_physical_entry(
                node_did,
                session_id,
                row,
                &by_doc,
                None,
                None,
                Some(&mut entries),
            )
            .ok()
            .filter(|root| root.doc_id.as_deref() == Some(entry_doc))
            .map(|_| (*doc).to_owned())
        })
        .collect::<Vec<_>>();
    member_doc_ids.sort_unstable();
    for doc in &member_doc_ids {
        if doc == entry_doc {
            continue;
        }
        by_doc[doc.as_str()]
            .caused_by_parent_request_doc_id
            .as_deref()
            .context("authenticated Goal member has no physical parent")?;
    }
    Ok(AuthenticatedGoalRequestMembers {
        entry,
        member_doc_ids,
    })
}

pub(super) fn request_rows_by_doc<'a>(
    node_did: &str,
    session_id: &str,
    rows: &'a [AgentRequestRow],
) -> Result<rapidhash::RapidHashMap<&'a str, &'a AgentRequestRow>> {
    let mut by_doc = rapidhash::RapidHashMap::default();
    for row in rows.iter().filter(|row| {
        row.node_did.as_deref() == Some(node_did) && row.session_id.as_deref() == Some(session_id)
    }) {
        let Some(doc) = row.doc_id.as_deref().filter(|doc| !doc.is_empty()) else {
            continue;
        };
        anyhow::ensure!(
            by_doc.insert(doc, row).is_none(),
            "duplicate physical request document in ancestry observation"
        );
    }
    Ok(by_doc)
}

pub(super) type AssignmentOpenings<'a> =
    rapidhash::RapidHashMap<&'a str, rapidhash::RapidHashSet<&'a str>>;

pub(super) fn assignment_openings(goals: &[GoalDocument]) -> AssignmentOpenings<'_> {
    let mut openings = AssignmentOpenings::default();
    for goal in goals {
        if let Some(root) = goal.assignment_root_request_doc_id.as_deref() {
            openings.entry(root).or_default().insert(&goal.goal_id);
        }
    }
    openings
}

pub(super) fn authenticated_assignment_entry<'a>(
    node_did: &str,
    session_id: &str,
    row: &'a AgentRequestRow,
    by_doc: &rapidhash::RapidHashMap<&'a str, &'a AgentRequestRow>,
    expected_goal: Option<&str>,
    openings: &AssignmentOpenings<'_>,
) -> Result<&'a AgentRequestRow> {
    authenticated_physical_entry(
        node_did,
        session_id,
        row,
        by_doc,
        expected_goal,
        Some(openings),
        None,
    )
}

pub(super) fn authenticated_entry<'a>(
    node_did: &str,
    session_id: &str,
    row: &'a AgentRequestRow,
    by_doc: &rapidhash::RapidHashMap<&'a str, &'a AgentRequestRow>,
    expected_goal: Option<&str>,
) -> Result<&'a AgentRequestRow> {
    authenticated_physical_entry(node_did, session_id, row, by_doc, expected_goal, None, None)
}

/// Authenticated suffixes are reusable only within one immutable owner/session
/// observation. Assignment openings require their own retained-version checks.
struct AuthenticatedSuffix<'a> {
    entry: &'a AgentRequestRow,
    goal_id: Option<&'a str>,
}

fn authenticated_physical_entry<'a>(
    node_did: &str,
    session_id: &str,
    mut row: &'a AgentRequestRow,
    by_doc: &rapidhash::RapidHashMap<&'a str, &'a AgentRequestRow>,
    expected_goal: Option<&str>,
    assignment_openings: Option<&AssignmentOpenings<'_>>,
    mut entries: Option<&mut rapidhash::RapidHashMap<&'a str, AuthenticatedSuffix<'a>>>,
) -> Result<&'a AgentRequestRow> {
    if assignment_openings.is_some() {
        entries = None;
    }
    let mut visited = rapidhash::RapidHashSet::default();
    let mut path_goal: Option<&str> = None;
    let entry = loop {
        let doc = row
            .doc_id
            .as_deref()
            .context("request has no document ID")?;
        anyhow::ensure!(visited.insert(doc), "cyclic Goal request ancestry");
        anyhow::ensure!(
            row.purpose == Some(RequestPurpose::Normal)
                && row.node_did.as_deref() == Some(node_did)
                && row.session_id.as_deref() == Some(session_id),
            "request ancestry is outside its owner/session"
        );
        if let Some(suffix) = entries.as_ref().and_then(|entries| entries.get(doc)) {
            anyhow::ensure!(
                expected_goal
                    .or(path_goal)
                    .is_none_or(|expected| suffix.goal_id.is_none_or(|goal| expected == goal)),
                "Goal continuation belongs to another assignment"
            );
            break suffix.entry;
        }
        verify_request_receipt_signature(row)?;
        if row.caused_by_trigger_kind.as_deref() != Some(GOAL_TRIGGER_KIND) {
            let opening_goals = assignment_openings.and_then(|openings| openings.get(doc));
            anyhow::ensure!(
                opening_goals.is_none_or(|goals| expected_goal
                    .or(path_goal)
                    .is_none_or(|goal| goals.contains(goal))),
                "Goal continuation opening belongs to another assignment"
            );
            let follows_wake = assignment_openings.is_some()
                && opening_goals.is_none()
                && row
                    .input
                    .as_ref()
                    .is_none_or(|input| input.goal_assignment.is_none())
                && crate::request::lifecycle::queue::row_is_automated_wakeup(row);
            if !follows_wake {
                break row;
            }
            let parent_doc = row
                .caused_by_parent_request_doc_id
                .as_deref()
                .context("background wake has no physical parent")?;
            let parent = *by_doc
                .get(parent_doc)
                .context("background wake parent is absent from its owner/session observation")?;
            anyhow::ensure!(
                parent.purpose == Some(RequestPurpose::Normal)
                    && parent.doc_id.as_deref() != Some(doc)
                    && parent.node_did.as_deref() == Some(node_did)
                    && parent.session_id.as_deref() == Some(session_id)
                    && parent
                        .input
                        .as_ref()
                        .is_none_or(|input| input.goal_assignment.is_none())
                    && row.execution_origin.as_deref() == Some("scheduled"),
                "background wake changed its signed assignment or physical owner/session"
            );
            verify_runtime_local_control_receipt(
                row,
                node_did,
                &parent.request_id,
                parent.requester_did.as_deref().unwrap_or(node_did),
            )?;
            row = parent;
            continue;
        }
        let goal_id = row
            .caused_by_trigger_id
            .as_deref()
            .context("Goal continuation has no original Goal ID")?;
        anyhow::ensure!(
            expected_goal.is_none_or(|expected| expected == goal_id)
                && path_goal.is_none_or(|expected| expected == goal_id),
            "Goal continuation belongs to another assignment"
        );
        path_goal = Some(goal_id);
        let parent_doc = row
            .caused_by_parent_request_doc_id
            .as_deref()
            .context("Goal continuation has no physical parent")?;
        let parent = *by_doc
            .get(parent_doc)
            .context("Goal continuation parent is absent from its owner/session observation")?;
        verify_goal_continuation_edge(node_did, session_id, goal_id, parent, row)?;
        if assignment_openings
            .and_then(|openings| openings.get(doc))
            .is_some_and(|goals| goals.contains(goal_id))
        {
            break row;
        }
        row = parent;
    };
    if let Some(entries) = entries {
        for doc in visited {
            let row = by_doc[doc];
            entries.insert(
                doc,
                AuthenticatedSuffix {
                    entry,
                    goal_id: row.caused_by_trigger_id.as_deref().filter(|_| {
                        row.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                    }),
                },
            );
        }
    }
    Ok(entry)
}

pub(crate) fn goal_session_is_idle(rows: &[AgentRequestRow]) -> bool {
    rows.iter().all(|row| {
        row.purpose.is_some_and(RequestPurpose::is_auxiliary)
            || row
                .lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
    })
}

#[cfg(test)]
#[path = "request_head_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn assignment_allows(assigned: Option<&str>, observed: Option<&str>) -> bool {
    assigned.is_none_or(|root| observed == Some(root))
}

pub(super) async fn retire_stale_assignment_continuations_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    only_doc: Option<&str>,
    now: &str,
) -> Result<bool> {
    let Some(root) = goal.assignment_root_request_doc_id.as_deref() else {
        return Ok(true);
    };
    let owner = escape_graphql_string(&goal.node_did);
    let session = escape_graphql_string(&goal.session_id);
    let response = txn.execute_local_response(&format!(
        r#"{{AgentRequest(filter: {{node_did: {{_eq: "{owner}"}}, session_id: {{_eq: "{session}"}}}}) {{{}}}}}"#,
        crate::request::admission::SIGNED_REQUEST_FIELDS
    )).await?;
    let requests: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    let pending = requests
        .iter()
        .filter(|row| {
            row.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                && row.lifecycle_state == Some(RequestLifecycleState::Pending)
                && only_doc.is_none_or(|doc| row.doc_id.as_deref() == Some(doc))
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(true);
    }
    let by_doc = request_rows_by_doc(&goal.node_did, &goal.session_id, &requests)?;
    let openings = assignment_openings(std::slice::from_ref(goal));
    let opening = *by_doc
        .get(root)
        .context("Goal assignment opening is absent from its owner/session")?;
    let entry = authenticated_assignment_entry(
        &goal.node_did,
        &goal.session_id,
        opening,
        &by_doc,
        Some(&goal.goal_id),
        &openings,
    )?;
    anyhow::ensure!(
        entry.doc_id.as_deref() == Some(root)
            && super::request_assignment::request_belongs_to_goal(goal, entry),
        "Goal assignment has no authenticated physical opening"
    );
    let mut allowed = true;
    for row in pending {
        let doc = row
            .doc_id
            .as_deref()
            .context("Goal continuation has no document ID")?;
        let entry = authenticated_assignment_entry(
            &goal.node_did,
            &goal.session_id,
            row,
            &by_doc,
            None,
            &openings,
        )?;
        if super::request_assignment::request_belongs_to_goal(goal, entry)
            && row.caused_by_trigger_id.as_deref() == Some(&goal.goal_id)
        {
            continue;
        }
        allowed = false;
        let reason = "superseded by a newer Task Goal assignment";
        let mutation = crate::request::lifecycle::queue::supersede_pending_mutation(
            doc,
            &goal.node_did,
            &entry.request_id,
            root,
            reason,
        );
        let response = txn.execute_local_response(&mutation).await?;
        if response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(crate::graphql::response_has_documents)
        {
            crate::triggers::engine::durable::publish_request_outcome(
                txn,
                &goal.node_did,
                &row.request_id,
                "superseded",
                reason,
                now,
            )
            .await?;
        }
    }
    Ok(allowed)
}

pub(crate) async fn assignment_request_id_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    goal: &GoalDocument,
) -> Result<Option<String>> {
    let Some(root) = goal.assignment_root_request_doc_id.as_deref() else {
        return Ok(None);
    };
    let response = txn.execute_local_response(&format!(
        r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{request_id}}}}"#,
        escape_graphql_string(root), escape_graphql_string(&goal.node_did), escape_graphql_string(&goal.session_id)
    )).await?;
    let requests: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        requests.len() == 1,
        "Goal assignment has no unique request in its owner/session"
    );
    Ok(Some(requests[0].request_id.clone()))
}

pub(super) async fn load_session_requests(
    txn: &impl crate::config_client::ConfigRead,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<AgentRequestRow>> {
    let fields = request_selection_fields();
    let escaped_did = escape_graphql_string(node_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute_read(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{escaped_did}" }}, session_id: {{ _eq: "{escaped_session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {fields} }} }}"#
        ))
        .await?;
    serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("request query omitted rows")?,
    )
    .map_err(Into::into)
}
