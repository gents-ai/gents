use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[test]
fn generated_session_recovery_cases_cover_retry_guards_and_preservation() {
    let legal = lean_session_recovery_case("legal_initial_retry_slot");
    assert!(legal.legal);
    assert_eq!(legal.action.as_str(), "reissueFailed");
    assert_eq!(legal.pre_latest_state.as_str(), "failed");
    assert_eq!(legal.pre_failed_state.as_str(), "failed");
    assert_eq!(legal.post_latest_state.as_str(), "pending");
    assert_eq!(legal.post_failed_state.as_str(), "failed");
    assert_eq!(legal.post_new_state.as_str(), "pending");
    assert_eq!(legal.pre_latest_admission.as_str(), "released");
    assert_eq!(legal.post_latest_admission.as_str(), "released");
    assert_eq!(legal.pre_failed_admission.as_str(), "released");
    assert_eq!(legal.post_failed_admission.as_str(), "released");
    assert_eq!(legal.post_new_admission.as_str(), "released");
    assert_eq!(legal.pre_origin.as_str(), "interactive");
    assert_eq!(legal.pre_backend.as_str(), "contract-backend");
    assert_eq!(legal.pre_origin.as_str(), legal.post_new_origin.as_str());
    assert_eq!(legal.pre_backend.as_str(), legal.post_new_backend.as_str());
    assert_eq!(legal.pre_retry_count + 1, legal.post_retry_count);
    assert!(legal.post_retry_count <= legal.max_retries);
    assert_eq!(legal.pre_session_id, legal.post_session_id);
    assert_eq!(legal.pre_behavior_id, legal.post_behavior_id);
    assert_eq!(legal.pre_request_count + 1, legal.post_request_count);
    assert_eq!(legal.post_latest_id, legal.new_id);
    assert!(legal.pre_failed_is_latest);
    assert!(!legal.post_failed_is_latest);
    assert!(legal.post_new_is_latest);
    assert!(legal.pre_failed_exists);
    assert!(legal.pre_latest_exists);
    assert!(legal.pre_request_ids.contains(&legal.failed_id));
    assert!(!legal.pre_new_request_exists);
    assert!(legal.old_request_retained);
    assert!(legal.new_request_inserted);
    assert!(legal.origin_preserved);
    assert!(legal.backend_preserved);

    let automated = lean_session_recovery_case("illegal_automated_origin");
    assert!(!automated.legal);
    assert_eq!(automated.pre_origin.as_str(), "scheduled");
    assert!(automated.post_latest_state.is_empty());

    let last_slot = lean_session_recovery_case("legal_last_retry_slot");
    assert!(last_slot.legal);
    assert_eq!(last_slot.post_retry_count, last_slot.max_retries);

    let initial_slot = lean_session_recovery_case("legal_initial_retry_slot");
    assert!(initial_slot.legal);
    assert_eq!(initial_slot.pre_retry_count, 0);
    assert_eq!(initial_slot.post_retry_count, 1);

    let duplicate_new_id = lean_session_recovery_case("illegal_new_request_id_already_exists");
    assert!(!duplicate_new_id.legal);
    assert!(duplicate_new_id.pre_new_request_exists);
    assert_eq!(duplicate_new_id.pre_failed_admission.as_str(), "released");

    let duplicate_failed_id =
        lean_session_recovery_case("illegal_new_request_id_matches_failed_id");
    assert!(!duplicate_failed_id.legal);
    assert_eq!(duplicate_failed_id.failed_id, duplicate_failed_id.new_id);
    assert!(duplicate_failed_id.pre_new_request_exists);
    assert_eq!(
        duplicate_failed_id.pre_failed_admission.as_str(),
        "released"
    );

    let non_latest = lean_session_recovery_case("illegal_non_latest_failed_with_pending_latest");
    assert!(!non_latest.legal);
    assert_eq!(non_latest.pre_failed_state.as_str(), "failed");
    assert_eq!(non_latest.pre_latest_state.as_str(), "pending");
    assert!(!non_latest.pre_failed_is_latest);

    let missing = lean_session_recovery_case("illegal_missing_failed_request");
    assert!(!missing.legal);
    assert!(!missing.pre_failed_exists);
    assert!(!missing.pre_latest_exists);

    for name in [
        "illegal_retry_budget_exhausted",
        "illegal_deadline_closed",
        "illegal_non_latest_failed_request",
        "illegal_non_latest_failed_with_pending_latest",
        "illegal_new_request_id_already_exists",
        "illegal_new_request_id_matches_failed_id",
        "illegal_source_not_failed",
        "illegal_source_completed_terminal",
        "illegal_source_dead_stale_terminal",
        "illegal_source_superseded_terminal",
        "illegal_source_interrupted_terminal",
        "illegal_source_input_required_reserved",
        "illegal_source_processing_active_runtime",
        "illegal_missing_failed_request",
    ] {
        let case = lean_session_recovery_case(name);
        assert!(!case.legal, "{name} must be rejected by Lean");
        assert!(case.post_latest_state.is_empty());
    }
}

pub(super) async fn generated_session_recovery_cases_drive_db_backed_reissue_contract() {
    let cases = &lean_contract_snapshot().session_recovery_cases;
    assert_eq!(cases.iter().filter(|case| case.legal).count(), 2);
    assert_eq!(cases.len(), 17);

    let db = test_db("session-recovery-generated-contract").await;
    for case in cases {
        let pre = seed_session_recovery_case(&db.node, case).await;
        assert_eq!(
            request_count_for_session(&db.node, &pre.session_id).await,
            case.pre_request_count,
            "pre request count must match Lean case {}",
            case.name
        );
        assert_eq!(
            latest_request_id_for_session(&db.node, &pre.session_id).await,
            pre.pre_latest_request_id,
            "pre latest binding must match Lean case {}",
            case.name
        );

        let before_failed = fetch_recovery_request(&db.node, &pre.failed_request_id).await;
        let before_new_count = request_count_by_id(&db.node, &pre.new_request_id).await;
        let result = reissue_failed_request_for_contract(&db.node, &pre).await;

        if case.legal {
            assert_eq!(
                result.as_deref(),
                Ok(pre.new_request_id.as_str()),
                "legal Lean case {} must reissue",
                case.name
            );
            assert_legal_reissue_postconditions(&db.node, case, &pre).await;
        } else {
            let error = result.expect_err("illegal Lean case must be denied");
            assert!(
                error.contains(expected_reissue_denial_fragment(case)),
                "illegal case {} failed with unexpected error: {error}",
                case.name
            );
            assert_eq!(
                request_count_for_session(&db.node, &pre.session_id).await,
                case.pre_request_count,
                "illegal case {} must not insert a successor",
                case.name
            );
            assert_eq!(
                latest_request_id_for_session(&db.node, &pre.session_id).await,
                pre.pre_latest_request_id,
                "illegal case {} must not change latest request",
                case.name
            );
            assert_eq!(
                request_count_by_id(&db.node, &pre.new_request_id).await,
                before_new_count,
                "illegal case {} must not create the requested successor id",
                case.name
            );
            assert_eq!(
                fetch_recovery_request(&db.node, &pre.failed_request_id).await,
                before_failed,
                "illegal case {} must leave the source request unchanged",
                case.name
            );
        }
    }
}

#[derive(Debug)]
struct SessionRecoveryDbPre {
    session_id: String,
    failed_request_id: String,
    new_request_id: String,
    pre_latest_request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecoveryRequest {
    request_id: String,
    agent_did: String,
    behavior_id: String,
    session_id: String,
    content: String,
    lifecycle_state: RequestLifecycleState,
    backend_id: String,
    execution_origin: String,
    retry_parent_request: String,
    retry_root_request: String,
    retry_count: i64,
    max_retries: i64,
    deadline: Option<String>,
}

impl From<gents_protocol::row::AgentRequestRow> for RecoveryRequest {
    fn from(row: gents_protocol::row::AgentRequestRow) -> Self {
        Self {
            request_id: row.request_id,
            agent_did: row.agent_did.expect("AgentRequest.agent_did"),
            behavior_id: row.behavior_id.expect("AgentRequest.behavior_id"),
            session_id: row.session_id.expect("AgentRequest.session_id"),
            content: row.content.expect("AgentRequest.content"),
            lifecycle_state: row.lifecycle_state.expect("AgentRequest.lifecycle_state"),
            backend_id: row.backend_id.expect("AgentRequest.backend_id"),
            execution_origin: row.execution_origin.expect("AgentRequest.execution_origin"),
            retry_parent_request: row.retry_parent_request.unwrap_or_default(),
            retry_root_request: row.retry_root_request.unwrap_or_default(),
            retry_count: row.retry_count.unwrap_or_default(),
            max_retries: row.max_retries.unwrap_or_default(),
            deadline: row.deadline,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RecoveryConversationLatestRow {
    latest_request_id: String,
}

async fn seed_session_recovery_case(
    node: &EmbeddedNode,
    case: &lean_vocab_test::LeanSessionRecoveryCase,
) -> SessionRecoveryDbPre {
    let session_id = format!("sr-{}-session", case.name);
    let failed_request_id = recovery_request_id(case, case.failed_id);
    let new_request_id = recovery_request_id(case, case.new_id);
    let pre_latest_request_id = recovery_request_id(case, case.pre_latest_id);

    create_agent_session(node, &session_id, AGENT_NAME, "2026-03-23T00:00:00Z").await;
    for request_id in &case.pre_request_ids {
        let request_id_string = recovery_request_id(case, *request_id);
        let (state, retry_count, max_retries, deadline, backend, origin) =
            if *request_id == case.failed_id {
                (
                    case.pre_failed_state.as_str(),
                    case.pre_retry_count as i64,
                    case.max_retries as i64,
                    recovery_deadline(case.pre_deadline_exceeded),
                    case.pre_backend.as_str(),
                    case.pre_origin.as_str(),
                )
            } else if *request_id == case.pre_latest_id {
                (
                    case.pre_latest_state.as_str(),
                    0,
                    case.max_retries as i64,
                    recovery_deadline(false),
                    case.pre_backend.as_str(),
                    case.pre_origin.as_str(),
                )
            } else {
                (
                    "pending",
                    0,
                    case.max_retries as i64,
                    recovery_deadline(false),
                    case.pre_backend.as_str(),
                    case.pre_origin.as_str(),
                )
            };

        create_session_recovery_request(
            node,
            &request_id_string,
            &session_id,
            state,
            retry_count,
            max_retries,
            &deadline,
            backend,
            origin,
            if state == "dead" { "Stale" } else { "" },
            None,
            None,
        )
        .await;
    }
    upsert_conversation(
        node,
        &session_id,
        &pre_latest_request_id,
        "session recovery contract",
        "active",
    )
    .await;

    SessionRecoveryDbPre {
        session_id,
        failed_request_id,
        new_request_id,
        pre_latest_request_id,
    }
}

#[allow(clippy::too_many_arguments)]
async fn create_session_recovery_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    lifecycle_state: &str,
    retry_count: i64,
    max_retries: i64,
    deadline: &str,
    backend_id: &str,
    execution_origin: &str,
    failure_reason: &str,
    retry_parent_request: Option<&str>,
    retry_root_request: Option<&str>,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let deadline = escape_graphql_string(deadline);
    let backend_id = escape_graphql_string(backend_id);
    let execution_origin = escape_graphql_string(execution_origin);
    let failure_reason = escape_graphql_string(failure_reason);
    let retry_parent_request = retry_parent_request
        .map(escape_graphql_string)
        .unwrap_or_default();
    let retry_root_request = retry_root_request
        .map(escape_graphql_string)
        .unwrap_or_else(|| request_id.clone());
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                retry_parent_request: "{retry_parent_request}",
                retry_root_request: "{retry_root_request}",
                superseded_by_request: "",
                content: "session recovery contract",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "{backend_id}",
                execution_origin: "{execution_origin}",
                failure_reason: "{failure_reason}",
                created_at: "2026-03-23T00:00:00Z",
                deadline: "{deadline}",
                retry_count: {retry_count},
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create session recovery request failed: {:?}",
        resp.errors
    );
}

async fn reissue_failed_request_for_contract(
    node: &EmbeddedNode,
    pre: &SessionRecoveryDbPre,
) -> Result<String, String> {
    let Some(parent) = fetch_recovery_request(node, &pre.failed_request_id).await else {
        return Err(format!(
            "retry parent request not found: request_id={}",
            pre.failed_request_id
        ));
    };
    if parent.lifecycle_state != RequestLifecycleState::Failed {
        return Err(format!(
            "retry parent request must be failed, got lifecycle_state={}",
            parent.lifecycle_state
        ));
    }
    if parent.execution_origin != "interactive" {
        return Err(format!(
            "client retry parent request must be interactive, got execution_origin={}",
            parent.execution_origin
        ));
    }
    if parent.retry_count >= parent.max_retries {
        return Err(format!(
            "retry parent request exhausted retry budget: retry_count={} max_retries={}",
            parent.retry_count, parent.max_retries
        ));
    }
    if parent
        .deadline
        .as_deref()
        .filter(|deadline| !deadline.is_empty())
        .is_some_and(|deadline| {
            chrono::DateTime::parse_from_rfc3339(deadline)
                .map(|parsed| chrono::Utc::now() > parsed.with_timezone(&chrono::Utc))
                .unwrap_or(true)
        })
    {
        return Err("retry parent request deadline is closed".to_string());
    }
    let latest_request_id = latest_request_id_for_session(node, &pre.session_id).await;
    if latest_request_id != parent.request_id {
        return Err(format!(
            "retry parent request must be latest for session {}, got latest_request_id={latest_request_id}",
            pre.session_id
        ));
    }
    if fetch_recovery_request(node, &pre.new_request_id)
        .await
        .is_some()
    {
        return Err(format!(
            "retry new request id already exists: request_id={}",
            pre.new_request_id
        ));
    }

    let retry_root_request = if parent.retry_root_request.is_empty() {
        parent.request_id.as_str()
    } else {
        parent.retry_root_request.as_str()
    };
    create_session_recovery_request(
        node,
        &pre.new_request_id,
        &pre.session_id,
        "pending",
        parent.retry_count + 1,
        parent.max_retries,
        &recovery_deadline(false),
        &parent.backend_id,
        &parent.execution_origin,
        "",
        Some(&parent.request_id),
        Some(retry_root_request),
    )
    .await;
    upsert_conversation(
        node,
        &pre.session_id,
        &pre.new_request_id,
        &parent.content,
        "active",
    )
    .await;

    Ok(pre.new_request_id.clone())
}

async fn assert_legal_reissue_postconditions(
    node: &EmbeddedNode,
    case: &lean_vocab_test::LeanSessionRecoveryCase,
    pre: &SessionRecoveryDbPre,
) {
    assert_eq!(
        request_count_for_session(node, &pre.session_id).await,
        case.post_request_count
    );
    assert_eq!(
        latest_request_id_for_session(node, &pre.session_id).await,
        pre.new_request_id
    );

    let new_request = fetch_recovery_request(node, &pre.new_request_id)
        .await
        .expect("legal reissue must insert successor");
    assert_eq!(new_request.session_id, pre.session_id);
    assert_eq!(new_request.agent_did, AGENT_DID);
    assert_eq!(new_request.behavior_id, AGENT_NAME);
    assert_eq!(
        new_request.lifecycle_state,
        RequestLifecycleState::parse(&case.post_new_state).expect("Lean request lifecycle state")
    );
    assert_eq!(new_request.retry_parent_request, pre.failed_request_id);
    assert_eq!(new_request.retry_root_request, pre.failed_request_id);
    assert_eq!(new_request.retry_count, case.post_retry_count as i64);
    assert_eq!(new_request.max_retries, case.max_retries as i64);
    assert_eq!(new_request.backend_id, case.post_new_backend);
    assert_eq!(new_request.execution_origin, case.post_new_origin);
    assert!(case.origin_preserved);
    assert!(case.backend_preserved);

    let failed_request = fetch_recovery_request(node, &pre.failed_request_id)
        .await
        .expect("legal reissue must retain source request");
    assert_eq!(
        failed_request.lifecycle_state,
        RequestLifecycleState::parse(&case.post_failed_state)
            .expect("Lean request lifecycle state")
    );
    assert_eq!(failed_request.retry_count, case.pre_retry_count as i64);
    assert_eq!(failed_request.max_retries, case.max_retries as i64);
    assert_eq!(failed_request.backend_id, case.pre_backend);
    assert_eq!(failed_request.execution_origin, case.pre_origin);
    assert_eq!(
        request_count_by_id(node, &pre.failed_request_id).await,
        if case.old_request_retained { 1 } else { 0 }
    );
    assert_eq!(
        request_count_by_id(node, &pre.new_request_id).await,
        if case.new_request_inserted { 1 } else { 0 }
    );
}

async fn fetch_recovery_request(node: &EmbeddedNode, request_id: &str) -> Option<RecoveryRequest> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                lifecycle_state
                backend_id
                execution_origin
                retry_parent_request
                retry_root_request
                retry_count
                max_retries
                deadline
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<gents_protocol::row::AgentRequestRow>(&resp, "AgentRequest")
        .map(RecoveryRequest::from)
}

async fn latest_request_id_for_session(node: &EmbeddedNode, session_id: &str) -> String {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                limit: 1
            ) {{
                latest_request_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<RecoveryConversationLatestRow>(&resp, "AgentConversation")
        .map(|row| row.latest_request_id)
        .unwrap_or_default()
}

async fn request_count_for_session(node: &EmbeddedNode, session_id: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    request_count_query(
        node,
        &format!(
            r#"{{
                AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                    _docID
                }}
            }}"#
        ),
    )
    .await
}

async fn request_count_by_id(node: &EmbeddedNode, request_id: &str) -> usize {
    let request_id = escape_graphql_string(request_id);
    request_count_query(
        node,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                    _docID
                }}
            }}"#
        ),
    )
    .await
}

async fn request_count_query(node: &EmbeddedNode, query: &str) -> usize {
    let resp = node.execute(query).await;
    assert!(
        !resp.has_errors(),
        "request count failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .map(Vec::len)
        .unwrap_or_default()
}

fn recovery_request_id(case: &lean_vocab_test::LeanSessionRecoveryCase, id: usize) -> String {
    format!("sr-{}-{id}", case.name)
}

fn recovery_deadline(exceeded: bool) -> String {
    let deadline = if exceeded {
        chrono::Utc::now() - chrono::Duration::seconds(30)
    } else {
        chrono::Utc::now() + chrono::Duration::minutes(5)
    };
    deadline.to_rfc3339()
}

fn expected_reissue_denial_fragment(
    case: &lean_vocab_test::LeanSessionRecoveryCase,
) -> &'static str {
    if !case.pre_failed_exists {
        "not found"
    } else if case.pre_failed_state != "failed" || case.pre_failed_admission != "released" {
        "must be failed"
    } else if case.pre_origin != "interactive" {
        "must be interactive"
    } else if case.pre_retry_count >= case.max_retries {
        "exhausted retry budget"
    } else if case.pre_deadline_exceeded {
        "deadline is closed"
    } else if !case.pre_failed_is_latest {
        "must be latest"
    } else if case.pre_new_request_exists {
        "already exists"
    } else {
        panic!("unhandled illegal SessionRecovery case: {}", case.name);
    }
}
