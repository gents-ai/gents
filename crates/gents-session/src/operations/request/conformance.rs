use gents_lean_vocab::families::session_operations_resolve_delivery::Input as DeliveryInput;
use gents_protocol::operations::request::DeliveryChoice;

use super::{reissue_decision, request_lookup};

#[test]
fn request_lookup_matches_lean() {
    gents_lean_vocab::families::session_operations_request_lookup::replay(|i| {
        request_lookup(i.found)
    });
}

#[test]
fn reissue_decision_matches_lean() {
    gents_lean_vocab::families::session_operations_reissue_decision::replay(|i| {
        reissue_decision(
            i.kind,
            i.parent,
            i.interactive,
            i.budget_remains,
            i.deadline_open,
            i.latest,
        )
    });
}

#[test]
fn delivery_resolution_matches_lean() {
    gents_lean_vocab::families::session_operations_resolve_delivery::replay(
        |i: &DeliveryInput<DeliveryChoice>| i.choice.resolve(),
    );
}

#[tokio::test]
async fn an_unknown_outcome_is_rechecked_under_its_own_identity() -> anyhow::Result<()> {
    use gents_lean_vocab::families::storage_failure_contracts_unknown_outcome as family;
    use gents_protocol::operations::request::{MessageSendInput, RequestRef};
    use gents_store::identity::NodeIdentity;
    let unknown = family::cases();
    let signed_home = gents_documents::test_support::SignedHome::start(
        "unknown-send-outcome",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let agent = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, user, access) = signed_home.into_parts();
    let request_id = "storage-unknown-outcome";
    let session_id = "storage-unknown-outcome-session";
    let input = MessageSendInput {
        node_did: db.node_identity.did().to_owned(),
        session_id: Some(session_id.to_owned()),
        agent_did: Some(agent.agent_did),
        request_id: Some(request_id.to_owned()),
        content: "delivery whose outcome is unknown".to_owned(),
        ..Default::default()
    };
    let reference = RequestRef {
        node_did: input.node_did.clone(),
        requester_did: Some(user.did().to_owned()),
        request_id: request_id.to_owned(),
    };
    let first = super::send(&access, &user, input.clone()).await?;
    let committed = super::get(&access, &reference).await?;
    assert_eq!(
        committed.retry_key.as_deref(),
        Some(crate::request::lifecycle::client_request_key(user.did(), request_id).as_str())
    );
    assert!(committed.admission_signature.is_some());
    let rechecked = super::send(&access, &user, input).await?;
    assert_eq!(first, rechecked);
    let after = super::get(&access, &reference).await?;
    assert_eq!(after.request_id, committed.request_id);
    assert_eq!(after.retry_key, committed.retry_key);
    assert_eq!(after.admission_signature, committed.admission_signature);
    assert_eq!(after.content, committed.content);
    let request_rows = super::read::requests_by_id(
        &access,
        &reference.node_did,
        reference.requester_did.as_deref(),
        request_id,
    )
    .await?;
    assert_eq!(request_rows.len(), 1);
    use gents_protocol::document_query::{
        fields::agent_request as fields, rows, session_scope, Query, Selection,
    };
    let query = Query::select(Selection::new().field(fields::DOC_ID))
        .filter(session_scope(
            &reference.node_did,
            session_id,
            reference.requester_did.as_deref(),
        ))
        .render();
    let data = access.read(&query).await?;
    assert_eq!(rows(&data, "AgentRequest").len(), 1);
    let same_request =
        after.request_id == committed.request_id && after.retry_key == committed.retry_key;
    let same_bytes = after.admission_signature == committed.admission_signature
        && after.content == committed.content;
    let stop_automatic_retry = request_rows.len() == 1 && rows(&data, "AgentRequest").len() == 1;
    for case in &unknown {
        case.check(family::RecheckOutput {
            same_request,
            same_bytes,
            stop_automatic_retry,
        });
    }
    db.node.shutdown().await?;
    Ok(())
}

#[test]
fn interrupt_settled_matches_lean() {
    gents_lean_vocab::families::session_operations_interrupt_settled::replay(|i| {
        super::interrupt_settled(i.terminal, i.interrupted)
    });
}

#[tokio::test]
async fn generated_retries_recheck_the_stored_request_rows() -> anyhow::Result<()> {
    use crate::request::lifecycle::{ExecutionOrigin, RequestTerminalOutcome};
    use gents_lean_vocab::families::session_documents_retry as family;
    use gents_protocol::document_query::{
        fields::agent_request as fields, rows, session_scope, Query, Selection,
    };
    use gents_protocol::operations::request::{ReissueKind, RequestRef, RequestReissueInput};
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use gents_store::identity::NodeIdentity;
    let request_id = |n: u64| format!("{n:08}");
    let session_name = |n: u64| format!("retry-session-{n}");
    for case in family::cases() {
        let i = &case.input;
        let signed_home = gents_documents::test_support::SignedHome::start(
            &format!("generated-retry-{}", i.name),
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let binding = gents_inference::test_support::default_agent(&signed_home).await;
        let node_did = signed_home.node_did().to_owned();
        let base = chrono::SubsecRound::trunc_subsecs(chrono::Utc::now(), 0)
            - chrono::TimeDelta::minutes(1);
        let scope = &i
            .rows
            .first()
            .expect("a retry case stores its parent")
            .scope;
        let session = session_name(scope.session);
        let mut parent_doc = None;
        for row in &i.rows {
            assert!(
                (row.scope.node, row.scope.session, row.scope.requester)
                    == (scope.node, scope.session, scope.requester)
                    && row.agent == i.rows[0].agent
                    && row.purpose == "normal",
                "the fixture stores one session's normal requests"
            );
            let created_at = (base + chrono::TimeDelta::seconds(i64::try_from(row.created_at)?))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let mut claimed =
                crate::test_support::ClaimedRequest::client_signed(&signed_home, &binding)
                    .ids(&request_id(row.request_id), &session)
                    .created_at(&created_at)
                    .batch_interval(std::time::Duration::ZERO)
                    .drive(crate::test_support::Stage::Executing)
                    .await;
            let outcome = [
                RequestTerminalOutcome::Completed,
                RequestTerminalOutcome::Failed,
                RequestTerminalOutcome::Interrupted,
                RequestTerminalOutcome::Dead,
                RequestTerminalOutcome::Superseded,
            ]
            .into_iter()
            .find(|outcome| outcome.request_lifecycle_state() == row.state)
            .expect("the fixture stores terminal rows");
            claimed
                .lifecycle
                .terminalize_owned(
                    outcome,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await?;
            if row.doc_id == i.parent_doc {
                parent_doc = Some(claimed.doc_id);
            }
        }
        let (db, user, client) = signed_home.into_parts();
        let parent = super::get(
            &client,
            &RequestRef {
                node_did: node_did.clone(),
                requester_did: Some(user.did().to_owned()),
                request_id: request_id(i.failed_id),
            },
        )
        .await?;
        assert_eq!(
            parent.doc_id, parent_doc,
            "the reissue lookup finds the modeled physical parent"
        );
        let reissued = super::reissue(
            &client,
            &user,
            RequestReissueInput {
                kind: ReissueKind::Retry,
                parent: RequestRef {
                    node_did: node_did.clone(),
                    requester_did: Some(user.did().to_owned()),
                    request_id: request_id(i.failed_id),
                },
                request_id: Some(request_id(i.new_id)),
            },
        )
        .await;
        let stored = match reissued {
            Err(_) => None,
            Ok(receipt) => {
                assert_eq!(receipt.session_id, session);
                let query = Query::select(
                    Selection::new()
                        .field(fields::REQUEST_ID)
                        .field(fields::AGENT_DID)
                        .field(fields::LIFECYCLE_STATE)
                        .field(fields::EXECUTION_ORIGIN)
                        .field(fields::RETRY_COUNT)
                        .field(fields::RETRY_PARENT_REQUEST),
                )
                .filter(session_scope(&node_did, &session, Some(user.did())))
                .render();
                let data = client.read(&query).await?;
                let stored = rows(&data, "AgentRequest");
                let text =
                    |row: &serde_json::Value, field: &str| row[field].as_str().map(str::to_owned);
                let model_id = |id: &str| id.parse::<u64>().expect("a modeled request id");
                let latest =
                    super::read::latest_session_request(&client, &node_did, &session, user.did())
                        .await?
                        .expect("the session has requests");
                assert!(
                    stored
                        .iter()
                        .all(|row| text(row, "agent_did").as_deref()
                            == Some(binding.agent_did.as_str())),
                    "every row names the session's Agent"
                );
                Some(family::RetryStateWire {
                    session_id: scope.session,
                    agent: i.rows[0].agent,
                    latest: model_id(&latest),
                    requests: [i.failed_id, i.new_id]
                        .into_iter()
                        .filter_map(|id| {
                            stored
                                .iter()
                                .find(|row| text(row, "request_id") == Some(request_id(id)))
                        })
                        .map(|row| family::RetryRequestWire {
                            request_id: model_id(&text(row, "request_id").expect("an id")),
                            state: serde_json::from_value::<RequestLifecycleState>(
                                row["lifecycle_state"].clone(),
                            )
                            .expect("a stored lifecycle state"),
                            origin: serde_json::from_value::<ExecutionOrigin>(
                                row["execution_origin"].clone(),
                            )
                            .expect("a stored execution origin"),
                            retry_count: row["retry_count"].as_u64().unwrap_or(0),
                            retry_parent: text(row, "retry_parent_request")
                                .as_deref()
                                .map(model_id),
                        })
                        .collect(),
                })
            }
        };
        case.check(stored);
        db.node.shutdown().await?;
    }
    Ok(())
}
