use gents_lean_vocab::families::client_shell_contracts_session_turn_cases_selection as family;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

fn request_id(request: u64) -> String {
    format!("req-{request}")
}

fn doc_id(doc: u64) -> String {
    format!("doc-{doc}")
}

fn doc_number(row: &AgentRequestRow) -> u64 {
    row.doc_id
        .as_deref()
        .and_then(|doc| doc.strip_prefix("doc-"))
        .and_then(|doc| doc.parse().ok())
        .unwrap_or_else(|| panic!("{:?} is not a modeled row", row.doc_id))
}

fn modeled_rows(rows: &[family::Row<String>]) -> Vec<AgentRequestRow> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let owner_doc = row.folded_into.or(row.replaced_by);
            let lifecycle = match (row.state.as_str(), owner_doc, row.completed) {
                ("unclaimed", _, _) => RequestLifecycleState::Pending,
                ("active", _, _) => RequestLifecycleState::Processing,
                ("terminal", Some(_), _) => RequestLifecycleState::Superseded,
                ("terminal", None, true) => RequestLifecycleState::Completed,
                ("terminal", None, false) => RequestLifecycleState::Failed,
                (other, _, _) => panic!("unsupported Lean row state {other:?}"),
            };
            AgentRequestRow {
                purpose: Some(if row.public_input {
                    gents_protocol::request_admission::RequestPurpose::Normal
                } else {
                    gents_protocol::request_admission::RequestPurpose::TitleAudit
                }),
                doc_id: Some(doc_id(row.doc)),
                request_id: request_id(row.request),
                node_did: Some("did:test:contract-agent".into()),
                requester_did: (row.requester != 1)
                    .then(|| format!("did:test:requester-{}", row.requester)),
                agent_did: Some("contract-agent".into()),
                session_id: Some("session-1".into()),
                content: Some(format!("input {}", row.request)),
                lifecycle_state: Some(lifecycle),
                execution_origin: Some("interactive".into()),
                created_at: Some(format!("2026-04-21T12:00:{index:02}Z")),
                retry_parent_request: row.retry_parent.map(request_id),
                retry_parent_request_doc_id: row.retry_parent_doc.map(doc_id),
                superseded_by_request: owner_doc.map(|owner| {
                    rows.iter()
                        .find(|candidate| candidate.doc == owner)
                        .map_or("req-missing".into(), |owner| request_id(owner.request))
                }),
                superseded_by_request_doc_id: owner_doc.map(doc_id),
                failure_reason: row
                    .folded_into
                    .map(|_| gents_session::request::lifecycle::FOLDED_REASON.to_owned())
                    .or_else(|| {
                        row.replaced_by
                            .map(|_| gents_session::request::lifecycle::REPLACED_REASON.to_owned())
                    }),
                input: (row.queued_after.is_some() || row.background_wake).then(|| RequestInput {
                    queue: Some(RequestQueue {
                        delivery: Default::default(),
                        position: None,
                        source: if row.background_wake {
                            QueueSource::BackgroundCompletion
                        } else {
                            QueueSource::User
                        },
                        policy: QueuePolicy::Append,
                        key: None,
                        queued_after_request_id: row.queued_after.map(request_id),
                        interrupted_request_id: None,
                        background_completion_wake_version: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }
        })
        .collect()
}

#[test]
fn session_turn_selection_matches_the_lean_model() {
    gents_lean_vocab::families::client_shell_contracts_session_turn_cases_selection::replay::<
        String,
    >(|input| {
        let rows = modeled_rows(&input.rows);
        let selected = rows
            .iter()
            .find(|row| row.doc_id.as_deref() == Some(doc_id(input.selected_doc).as_str()))
            .expect("selected case row");
        let refs = rows.iter().collect::<Vec<_>>();
        family::SelectionOutput {
            display_turn_doc: doc_number(crate::client::session_display_request(&refs, selected)),
            turn_doc: doc_number(crate::client::session_turn_request(&refs, selected)),
            answer_doc: doc_number(crate::client::request_answer_owner(&refs, selected)),
        }
    });
}
