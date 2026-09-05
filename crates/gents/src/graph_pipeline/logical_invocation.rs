//! Derive logical invocations from authenticated physical request ancestry.
//! Goal remains the continuation owner; this projection observes its committed
//! obligation without replaying retry, activity, or budget decisions.
use super::*;
use crate::goal::{GoalDocument, GOAL_FIELDS};
use crate::request_admission::{verify_request_receipt_signature, SIGNED_REQUEST_FIELDS};

pub(super) struct Invocation {
    pub root_request_id: String,
    pub member_doc_ids: BTreeSet<String>,
    pub node_id: String,
    pub tip: Option<GraphRunRequestView>,
    pub outstanding: bool,
    pub invalid: bool,
}

pub(super) struct Projection {
    pub requests: Vec<GraphRunRequestView>,
    pub invocations: Vec<Invocation>,
}

fn request_view(row: &AgentRequestRow, node_id: Option<String>) -> GraphRunRequestView {
    GraphRunRequestView {
        request_id: row.request_id.clone(),
        session_id: row.session_id.clone(),
        node_id,
        behavior_id: row.behavior_id.clone().unwrap_or_default(),
        lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_owned()),
        failure_reason: row.failure_reason.clone(),
        terminal: row
            .lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal),
        succeeded: row.lifecycle_state == Some(RequestLifecycleState::Completed),
    }
}

fn authentic_root(row: &AgentRequestRow, owner_did: &str) -> bool {
    let Some(target) = row.agent_did.as_deref().filter(|s| !s.is_empty()) else {
        return false;
    };
    target == owner_did
        && verify_request_receipt_signature(row).is_ok()
        && row.doc_id.as_deref().is_some_and(|id| !id.is_empty())
        && row.session_id.as_deref().is_some_and(|id| !id.is_empty())
        && row.admission_kind.as_deref() == Some("runtime-internal")
        && row.runtime_source_kind.as_deref() == Some("automated-trigger")
        && row.admission_signer_did.as_deref() == Some(target)
        && row.runtime_issuer_did.as_deref() == Some(target)
        && row.requester_did.as_deref() == Some(target)
        && row.runtime_source_request_id == row.caused_by_trigger_id
        // Historical seal coherence from automated-trigger admission. Current
        // trigger/task existence and enablement belong to fresh admission only.
        && matches!(row.caused_by_trigger_kind.as_deref(), Some("event" | "schedule"))
        && row.caused_by_trigger_doc_id.as_deref().is_some_and(|id| !id.is_empty())
}

pub(super) async fn load(
    executor: &(impl GraphRunQuery + ?Sized),
    correlation: &str,
    plan: &GraphPlan,
    owner_did: &str,
) -> Result<Projection> {
    let routes = planned_trigger_nodes(plan)?;
    let response = executor
        .execute_graph_query(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_correlation: {{ _eq: "{}" }},
        caused_by_trigger_id: {{ _in: {} }} }}) {{ {SIGNED_REQUEST_FIELDS} failure_reason }} }}"#,
            escape_graphql_string(correlation),
            graphql_string_list_literal(&routes.keys().cloned().collect::<Vec<_>>()),
        ))
        .await?;
    let root_rows: Vec<AgentRequestRow> =
        serde_json::from_value(Value::Array(rows(&response, "AgentRequest").to_vec()))?;
    let mut physical = BTreeMap::<String, GraphRunRequestView>::new();
    let mut invocations = Vec::new();
    let mut sessions = BTreeMap::new();
    for root in &root_rows {
        let node_id = routes
            .get(root.caused_by_trigger_id.as_deref().unwrap_or_default())
            .context("selected graph route is absent from pinned plan")?
            .clone();
        if !authentic_root(root, owner_did) {
            // Owner DID comes from the verified GraphRun/revision, never the
            // candidate row or mutable Task/AgentBehavior configuration.
            continue;
        }
        let owner = root.agent_did.as_deref().unwrap();
        let session = root.session_id.as_deref().unwrap();
        let key = (owner.to_owned(), session.to_owned());
        if !sessions.contains_key(&key) {
            let response = executor.execute_graph_query(&format!(
                r#"{{ AgentRequest(filter: {{ agent_did: {{ _eq: "{}" }},
                session_id: {{ _eq: "{}" }} }},
                order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} failure_reason }}
                Goal(filter: {{ agent_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ {GOAL_FIELDS} }} }}"#,
                escape_graphql_string(owner), escape_graphql_string(session),
                escape_graphql_string(owner), escape_graphql_string(session),
            )).await?;
            let requests: Vec<AgentRequestRow> =
                serde_json::from_value(Value::Array(rows(&response, "AgentRequest").to_vec()))?;
            let mut goals: Vec<GoalDocument> =
                serde_json::from_value(Value::Array(rows(&response, "Goal").to_vec()))?;
            crate::goal::sort_goals_canonical(&mut goals);
            sessions.insert(key.clone(), (requests, goals.into_iter().next()));
        }
        let (requests, goal) = &sessions[&key];
        let root_doc = root.doc_id.as_deref().unwrap();
        let mut members = BTreeSet::from([root_doc.to_owned()]);
        let mut parents = BTreeMap::<String, String>::new();
        let mut invalid = false;
        // Finite monotone closure; no timestamp ordering enters ancestry.
        for _ in 0..requests.len() {
            let mut changed = false;
            for child in requests {
                let Some(parent_doc) = child.caused_by_parent_request_doc_id.as_deref() else {
                    continue;
                };
                if !members.contains(parent_doc) {
                    continue;
                }
                let Some(child_doc) = child.doc_id.as_deref() else {
                    continue;
                };
                let Some(goal_id) = child.caused_by_trigger_id.as_deref() else {
                    continue;
                };
                let Some(parent) = requests
                    .iter()
                    .find(|r| r.doc_id.as_deref() == Some(parent_doc))
                else {
                    continue;
                };
                if crate::goal::verify_goal_continuation_edge(
                    owner, session, goal_id, parent, child,
                )
                .is_err()
                {
                    continue;
                }
                if child_doc == root_doc
                    || root_rows
                        .iter()
                        .any(|r| r.doc_id.as_deref() == Some(child_doc))
                {
                    invalid = true;
                }
                if parents.get(child_doc).is_some_and(|p| p != parent_doc) {
                    invalid = true;
                }
                parents.insert(child_doc.to_owned(), parent_doc.to_owned());
                changed |= members.insert(child_doc.to_owned());
            }
            if !changed {
                break;
            }
        }
        let member_rows = requests
            .iter()
            .filter(|r| r.doc_id.as_ref().is_some_and(|id| members.contains(id)))
            .collect::<Vec<_>>();
        let tips = member_rows
            .iter()
            .filter(|r| {
                !parents
                    .values()
                    .any(|p| Some(p.as_str()) == r.doc_id.as_deref())
            })
            .collect::<Vec<_>>();
        let physically_active = member_rows.iter().any(|r| {
            !r.lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
        });
        invalid |= !physically_active && tips.len() != 1;
        let obligation = goal.as_ref().is_some_and(|goal| {
            let open = goal
                .state()
                .is_some_and(crate::goal::GoalState::has_continuation_obligation);
            // Keep invocation ancestry and other authenticated Goal chains as
            // association evidence. Ordinary interactive rows cannot erase an
            // obligation; a replacement Goal on another chain cannot acquire it.
            let invocation_rows = requests
                .iter()
                .filter(|row| {
                    if row.doc_id.as_ref().is_some_and(|id| members.contains(id)) {
                        return true;
                    }
                    let Some(goal_id) = row
                        .caused_by_trigger_id
                        .as_deref()
                        .filter(|_| row.caused_by_trigger_kind.as_deref() == Some("goal"))
                    else {
                        return false;
                    };
                    let Some(parent) = requests.iter().find(|parent| {
                        parent.doc_id.is_some()
                            && parent.doc_id == row.caused_by_parent_request_doc_id
                    }) else {
                        return false;
                    };
                    crate::goal::verify_goal_continuation_edge(owner, session, goal_id, parent, row)
                        .is_ok()
                })
                .cloned()
                .collect::<Vec<_>>();
            open && crate::goal::latest_authenticated_session_request(
                owner,
                session,
                &invocation_rows,
            )
            .is_some_and(|head| {
                head.doc_id.as_ref().is_some_and(|id| members.contains(id))
                    && (head.caused_by_trigger_kind.as_deref() != Some("goal")
                        || head.caused_by_trigger_id.as_deref() == Some(goal.goal_id.as_str()))
            })
        });
        let outstanding = obligation || physically_active;
        for row in &member_rows {
            let doc = row.doc_id.as_ref().unwrap();
            if physical.contains_key(doc) {
                invalid = true;
            }
            physical.insert(doc.clone(), request_view(row, Some(node_id.clone())));
        }
        invocations.push(Invocation {
            root_request_id: root.request_id.clone(),
            member_doc_ids: members,
            node_id,
            tip: tips.first().map(|r| {
                request_view(
                    r,
                    routes
                        .get(root.caused_by_trigger_id.as_deref().unwrap())
                        .cloned(),
                )
            }),
            outstanding,
            invalid,
        });
    }
    let mut requests = physical.into_values().collect::<Vec<_>>();
    requests.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    invocations.sort_by(|a, b| a.root_request_id.cmp(&b.root_request_id));
    Ok(Projection {
        requests,
        invocations,
    })
}
