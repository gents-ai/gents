use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::observe::run_timeline::{
    build_run_timeline, RunTimeline, RunTimelineRows, TimelineCompactionRow,
    TimelineGoalVersionRow, TimelineInferenceCallRow, TimelineMessageRow,
    TimelineProviderContextReductionRow, TimelineRenderedRequestRef, TimelineRenderedRequestRow,
    TimelineRequestRow, TimelineSessionRow, TimelineToolCallRow,
};
use crate::observe::run_timeline_fetch::event_loaders::resolve_timeline_messages_for_session;
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;

#[cfg(test)]
pub(crate) async fn load_session_tool_calls(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineToolCallRow>> {
    let observations =
        load_timeline_tool_observations_for_session(access, node_did, session_id, requester_did)
            .await?;
    let messages =
        resolve_timeline_messages_for_session(access, node_did, session_id, requester_did).await?;
    event_loaders::resolve_timeline_tool_observations(
        access,
        node_did,
        session_id,
        requester_did,
        observations,
        &messages,
    )
    .await
}

pub(crate) async fn load_accepted_tool_calls(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    tool_doc_ids: &[String],
) -> Result<Vec<TimelineToolCallRow>> {
    let observations = event_loaders::load_timeline_tool_observations(
        access,
        node_did,
        session_id,
        requester_did,
        tool_doc_ids,
    )
    .await?;
    let sequences = observations
        .iter()
        .filter_map(|observation| observation.row.message_sequence)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let messages = event_loaders::resolve_timeline_messages_at_sequences(
        access,
        node_did,
        session_id,
        requester_did,
        &sequences,
    )
    .await?;
    let mut messages_by_sequence = BTreeMap::<i64, Vec<TimelineMessageRow>>::new();
    for message in messages {
        messages_by_sequence
            .entry(message.sequence)
            .or_default()
            .push(message);
    }
    let mut calls = Vec::with_capacity(observations.len());
    for observation in observations {
        let sequence = observation
            .row
            .message_sequence
            .context("accepted tool lacks its accepted message sequence")?;
        let messages = messages_by_sequence
            .get(&sequence)
            .context("accepted tool sequence has no canonical messages")?;
        let call = event_loaders::resolve_tool_payloads(observation, messages)?;
        anyhow::ensure!(
            call.session_id == session_id,
            "accepted tool call crossed the requested session scope"
        );
        calls.push(call);
    }
    Ok(calls)
}

#[cfg(test)]
pub(crate) async fn load_accepted_tool_arguments(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    tool_doc_id: &str,
) -> Result<Vec<String>> {
    let tool_doc_id = tool_doc_id.to_owned();
    Ok(
        load_accepted_tool_calls(access, node_did, session_id, requester_did, &[tool_doc_id])
            .await?
            .into_iter()
            .map(|call| call.args)
            .collect(),
    )
}

pub async fn load_run_timeline(access: &ConfigAccess, request_id: &str) -> Result<RunTimeline> {
    let mut timeline = build_run_timeline(load_run_timeline_rows(access, request_id).await?);
    if let Some(node_did) = timeline.node_did.as_deref() {
        let diagnostics = match timeline.session_id.as_deref() {
            Some(session_id) => {
                crate::load_background_completion_diagnostics_for_session(
                    access, node_did, session_id,
                )
                .await
            }
            None => crate::load_background_completion_diagnostics(access, node_did).await,
        };
        match diagnostics {
            Ok(diagnostics) => {
                timeline.background_completions = diagnostics
                    .epochs
                    .into_iter()
                    .filter(|epoch| {
                        timeline
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| epoch.session_id == session_id)
                            || epoch.root_request_id == timeline.request_id
                            || epoch.active_request_id == timeline.request_id
                    })
                    .collect();
            }
            Err(error) => {
                timeline.background_completion_diagnostics_error = Some(error.to_string());
            }
        }
        if let Err(error) = name_serving_accounts(access, node_did, &mut timeline.events).await {
            tracing::debug!(%error, "timeline call accounts unavailable");
        }
    }
    Ok(timeline)
}

async fn name_serving_accounts(
    access: &ConfigAccess,
    node_did: &str,
    events: &mut [crate::observe::run_timeline::RunTimelineEvent],
) -> Result<()> {
    let accounts = crate::inference::auth::credential::list_accounts(access, node_did).await?;
    let backends = access
        .transact_readonly("timeline.serving_accounts", |txn| {
            Box::pin(async move {
                crate::store::config_client::list_inference_backends_in_txn(txn, node_did).await
            })
        })
        .await?;
    for event in events {
        let crate::observe::run_timeline::RunTimelineEvent::InferenceCall(call) = event else {
            continue;
        };
        let Some(backend) = backends
            .iter()
            .find(|backend| call.backend_id.as_deref() == Some(backend.backend_id.as_str()))
        else {
            continue;
        };
        let Some(at) = call
            .started_at
            .as_deref()
            .or(call.queued_at.as_deref())
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        else {
            continue;
        };
        let since = crate::inference::auth::credential::backend_account(backend, &accounts)
            .flatten()
            .and_then(|account| account.connected_at);
        if since.is_none_or(|since| at >= since) {
            call.account =
                Some(crate::inference::auth::credential::serving_account(backend, &accounts).label);
        }
    }
    Ok(())
}

/// How long [`load_settled_run_timeline_rows`] waits out a run's incomplete
/// canonical output before failing.
pub const RUN_TIMELINE_SETTLE_BOUND: std::time::Duration = std::time::Duration::from_secs(10);
const RUN_TIMELINE_SETTLE_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// Whether `error` is a canonical read that is incomplete rather than
/// failed, as the canonical output owner classifies it: recoverable once its
/// immutable dependencies arrive.
fn reconstruction_incomplete(error: &anyhow::Error) -> bool {
    crate::session::canonical_read_loading(error)
}

/// [`load_run_timeline_rows`] for a run read just after it ran: a header
/// published before its segments' relationships (or an unresolved closing
/// segment still replicating) is incomplete, not failed, so the read waits
/// that window out for at most `bound`. Any other error, and incompleteness
/// that outlives `bound`, fails.
pub async fn load_settled_run_timeline_rows(
    access: &ConfigAccess,
    request_id: &str,
    bound: std::time::Duration,
) -> Result<RunTimelineRows> {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        match load_run_timeline_rows(access, request_id).await {
            Err(error)
                if reconstruction_incomplete(&error) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(RUN_TIMELINE_SETTLE_RETRY).await;
            }
            result => return result,
        }
    }
}

pub async fn load_run_timeline_rows(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<RunTimelineRows> {
    let request = load_timeline_request_by_id(access, request_id).await?;
    let root_session_id = request.session_id.clone();

    let mut requests = match root_session_id.as_deref() {
        Some(session_id) => {
            load_timeline_requests_for_session(
                access,
                request
                    .node_did
                    .as_deref()
                    .context("timeline root request has no session owner")?,
                session_id,
                request.requester_did.as_deref(),
            )
            .await?
        }
        None => Vec::new(),
    };
    ensure_unique_timeline_request_ids(&requests)?;
    merge_timeline_request(&mut requests, request.clone())?;
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("timeline root AgentRequest has no _docID")?;
    for child in load_timeline_child_requests(access, request_doc_id).await? {
        if child.caused_by_parent_request_doc_id.as_deref() != Some(request_doc_id) {
            continue;
        }
        if child.caused_by_parent_request_id.as_deref() != Some(request.request_id.as_str()) {
            anyhow::bail!(
                "child AgentRequest {} physical parent is {}, but logical parent is {:?}",
                child.request_id,
                request_doc_id,
                child.caused_by_parent_request_id
            );
        }
        merge_timeline_request(&mut requests, child)?;
    }

    let rendered_request_refs = match request.doc_id.as_deref() {
        Some(request_doc_id) => load_timeline_rendered_request_refs(access, request_doc_id).await?,
        None => Vec::new(),
    };

    let session_ids = timeline_session_ids(&requests);
    let mut session_scopes = BTreeSet::new();
    for request in &requests {
        if let Some(session_id) = request.session_id.as_deref() {
            let owner = request
                .node_did
                .as_deref()
                .filter(|owner| !owner.trim().is_empty())
                .context("timeline session request has no owner")?;
            session_scopes.insert((
                owner.to_owned(),
                session_id.to_owned(),
                request.requester_did.clone(),
            ));
        }
    }

    let mut messages = Vec::new();
    let mut tool_calls = Vec::new();
    let mut compactions = Vec::new();
    for (owner, session_id, requester) in &session_scopes {
        let observations = load_timeline_tool_observations_for_session(
            access,
            owner,
            session_id,
            requester.as_deref(),
        )
        .await?;
        let session_messages =
            resolve_timeline_messages_for_session(access, owner, session_id, requester.as_deref())
                .await?;
        tool_calls.extend(
            event_loaders::resolve_timeline_tool_observations(
                access,
                owner,
                session_id,
                requester.as_deref(),
                observations,
                &session_messages,
            )
            .await?,
        );
        messages.extend(session_messages);
        compactions.extend(
            load_timeline_compactions_for_session(access, owner, session_id, requester.as_deref())
                .await?,
        );
    }
    let goal_versions = match root_session_id.as_deref() {
        Some(session_id) => {
            load_timeline_goal_versions_for_session(
                access,
                request
                    .node_did
                    .as_deref()
                    .context("timeline root request has no goal owner")?,
                session_id,
            )
            .await?
        }
        None => Vec::new(),
    };
    let request_doc_ids = timeline_request_doc_ids(&requests)?;
    let mut inference_calls =
        load_timeline_inference_calls_for_requests(access, &request_doc_ids).await?;
    let mut provider_context_reductions =
        load_timeline_provider_context_reductions_for_requests(access, &request_doc_ids).await?;
    let mut rendered_requests = Vec::new();
    for (owner, session_id, requester) in &session_scopes {
        rendered_requests.extend(
            load_timeline_rendered_requests_for_session(
                access,
                owner,
                session_id,
                requester.as_deref(),
            )
            .await?,
        );
    }
    if session_ids.is_empty() || root_session_id.is_none() {
        rendered_requests.extend(
            load_timeline_rendered_requests_for_request(access, &request.request_id).await?,
        );
    }

    let request_bindings = timeline_request_bindings(&requests, &request)?;
    let in_scope_ids = request_bindings
        .values()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let legacy_logical_only_rows = tool_calls
        .iter()
        .filter(|row| {
            nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                && nonempty(row.request_doc_id.as_deref()).is_none()
        })
        .count()
        + inference_calls
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + compactions
            .iter()
            .filter(|row| {
                nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + provider_context_reductions
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(Some(row.request_doc_id.as_str())).is_none()
            })
            .count()
        + rendered_requests
            .iter()
            .filter(|row| {
                nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count();
    if legacy_logical_only_rows > 0 {
        tracing::warn!(
            request_id = %request.request_id,
            legacy_logical_only_rows,
            "timeline omitted rows without physical AgentRequest provenance",
        );
    }
    messages.retain(|row| {
        request_scoped_row_is_in_timeline(&request_bindings, None, row.request_doc_id.as_deref())
    });
    tool_calls.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    inference_calls.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    compactions.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    provider_context_reductions.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            Some(row.request_doc_id.as_str()),
        )
    });
    rendered_requests.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    validate_request_scoped_rows(
        &request_bindings,
        &messages,
        &tool_calls,
        &inference_calls,
        &compactions,
        &provider_context_reductions,
        &rendered_requests,
    )?;
    validate_child_tool_bridges(&request, &requests, &tool_calls)?;
    // `child_request_id` is the single-request projection of a session-message
    // call. A job row's many caused requests come from
    // `session_origin::requests_caused_by_calls`.
    let caused = crate::session::origin::caused_requests(
        access,
        tool_calls.iter().filter_map(|row| row.doc_id.as_deref()),
    )
    .await?;
    for row in &mut tool_calls {
        row.child_request_id = row
            .doc_id
            .as_deref()
            .and_then(|doc_id| caused.get(doc_id))
            .cloned();
    }

    let session = match root_session_id.as_deref() {
        Some(session_id) => {
            let owner = request
                .node_did
                .as_deref()
                .filter(|owner| !owner.trim().is_empty())
                .context("timeline root request has no session owner")?;
            load_timeline_session(access, owner, session_id, request.requester_did.as_deref())
                .await?
        }
        None => None,
    };

    Ok(RunTimelineRows {
        request,
        session,
        requests,
        messages,
        tool_calls,
        goal_versions,
        inference_calls,
        compactions,
        provider_context_reductions,
        rendered_requests,
        rendered_request_refs,
    })
}

mod context_loaders;
mod event_loaders;
mod goal_history;
mod query_helpers;
mod request_loaders;
#[cfg(test)]
mod tool_payload_tests;
mod validation;

use context_loaders::load_timeline_session;
use event_loaders::{
    load_timeline_compactions_for_session, load_timeline_inference_calls_for_requests,
    load_timeline_provider_context_reductions_for_requests, load_timeline_rendered_request_refs,
    load_timeline_rendered_requests_for_request, load_timeline_rendered_requests_for_session,
    load_timeline_tool_observations_for_session,
};
use goal_history::load_timeline_goal_versions_for_session;
use query_helpers::load_rows;
use request_loaders::{
    load_timeline_child_requests, load_timeline_request_by_id, load_timeline_requests_for_session,
};
use validation::{
    ensure_unique_timeline_request_ids, merge_timeline_request, nonempty,
    request_scoped_row_is_in_timeline, timeline_request_bindings, timeline_request_doc_ids,
    timeline_session_ids, validate_child_tool_bridges, validate_request_scoped_rows,
};

#[cfg(test)]
#[path = "run_timeline_fetch/tests.rs"]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_incomplete_reconstruction_is_waited_out() {
        use gents_protocol::output::ReconstructionError;
        let wrapped = |error: ReconstructionError| {
            anyhow::Error::new(error)
                .context("resolving canonical AgentMessage header for timeline")
                .context("timeline.canonical_session_messages")
        };
        assert!(reconstruction_incomplete(&wrapped(
            ReconstructionError::UnresolvedClose {
                close_doc_id: "close".into()
            }
        )));
        assert!(reconstruction_incomplete(&wrapped(
            ReconstructionError::MissingSegment {
                close_doc_id: "close".into(),
                ordinal: 1
            }
        )));
        assert!(!reconstruction_incomplete(&wrapped(
            ReconstructionError::AccessDenied {
                doc_id: "denied".into()
            }
        )));
        assert!(!reconstruction_incomplete(&anyhow::anyhow!(
            "unresolved closing segment"
        )));
    }

    fn created_doc_id(response: &defra_node::QueryResponse, field: &str) -> String {
        let alternate = field
            .strip_prefix("create_")
            .map(|collection| format!("add_{collection}"));
        let created = response
            .data
            .as_ref()
            .and_then(|data| {
                data.get(field)
                    .or_else(|| alternate.as_deref().and_then(|field| data.get(field)))
            })
            .expect("create response field");
        created
            .as_array()
            .and_then(|rows| rows.first())
            .unwrap_or(created)
            .get("_docID")
            .and_then(Value::as_str)
            .expect("created _docID")
            .to_string()
    }

    #[tokio::test]
    async fn job_rows_keep_child_request_id_none() {
        use crate::tools::call::lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmissionOptions,
        };
        use gents_protocol::tool_call::AwaitMode;

        let (fixture, root) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "job-timeline".into(),
            tool_name: Some("workflow".into()),
            await_mode: AwaitMode::Background,
            job_pin: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = &fixture.node;
        let root_id = root.request().request_id.clone();
        let root_doc_id = root.request().doc_id.clone();
        let tool_doc_id = fixture.tool.doc_id().unwrap().to_owned();
        let tool_call_id = fixture.tool.tool_call_id().to_owned();
        for slot in ["alpha", "beta"] {
            let input = serde_json::json!({
                "request_id": format!("job-child-{slot}"),
                "purpose": "normal", "node_did": fixture.node_did,
                "agent_did": root.request().agent_did,
                "session_id": format!("job-child-session-{slot}"),
                "content": "child", "lifecycle_state": "completed", "backend_id": "",
                "created_at": "2026-08-26T00:00:00Z", "retry_count": 0, "request_hop": 1,
                "caused_by_parent_request_id": root_id,
                "caused_by_parent_request_doc_id": root_doc_id,
                "caused_by_parent_tool_call_id": tool_call_id,
                "caused_by_parent_tool_call_doc_id": tool_doc_id,
            });
            fixture.client.transact_write("test.timeline.historical_child", |txn| {
                let input = &input;
                Box::pin(async move {
                    let response = txn.execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input":input}), gents_protocol::schemas::AGENT_REQUEST,
                    ).await?;
                    crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
        }
        let rows = load_run_timeline_rows(&fixture.client, &root_id)
            .await
            .unwrap();

        let job = rows
            .tool_calls
            .iter()
            .find(|row| row.tool_call_id == tool_call_id)
            .expect("the job row");
        assert_eq!(job.child_request_id, None);
        node.shutdown().await.expect("node shutdown");
        tokio::fs::remove_dir_all(fixture.path).await.unwrap();
    }

    #[tokio::test]
    async fn fetches_complete_inference_provenance() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let response = node
            .execute(
                r#"mutation {
                    create_AgentSession(input: {
                        session_id: "session-timeline"
                        node_did: "did:test:agent"
                        agent_did: "general"
                        created_at: "2026-08-14T12:00:00Z"
                    }) { _docID }
                    create_AgentRequest(input: {
                        request_id: "request-timeline"
                        purpose: "normal"
                        node_did: "did:test:agent"
                        agent_did: "general"
                        session_id: "session-timeline"
                        content: "run"
                        lifecycle_state: "completed"
                        created_at: "2026-08-14T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed request: {:?}",
            response.errors
        );
        let request_doc_id = created_doc_id(&response, "create_AgentRequest");

        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_InferenceCall(input: {{
                        call_id: "inference-1"
                        runtime_instance_id: "runtime-a"
                        request_id: "request-timeline"
                        request_doc_id: "{request_doc_id}"
                        call_seq: 1
                        backend_id: "backend-a"
                        agent_did: "general"
                        node_did: "did:test:agent"
                        call_kind: "inference"
                        attempt: 1
                        call_state: "completed"
                        queued_at: "2026-08-14T12:00:01Z"
                        priority: 7
                        queue_depth_at_enqueue: 3
                        controller_generation: 11
                        backend_config_fingerprint: "InferenceBackend synthetic-inline-secret"
                        prompt_tokens: 10
                        completion_tokens: 5
                        cached_input_tokens: 2
                    }}) {{ _docID }}
                }}"#,
            ))
            .await;
        assert!(!response.has_errors(), "seed calls: {:?}", response.errors);
        let access = ConfigAccess::local(node.clone());
        let timeline = load_run_timeline(&access, "request-timeline")
            .await
            .expect("load timeline");
        let inference = timeline.events.iter().find_map(|event| match event {
            crate::observe::run_timeline::RunTimelineEvent::InferenceCall(inference) => {
                Some(inference)
            }
            _ => None,
        });
        let inference = inference.expect("inference event");
        assert_eq!(inference.runtime_instance_id.as_deref(), Some("runtime-a"));
        assert_eq!(inference.priority, Some(7));
        assert_eq!(inference.queue_depth_at_enqueue, Some(3));
        assert_eq!(inference.controller_generation, Some(11));
        assert_eq!(inference.backend_config_fingerprint, None);
        assert!(!serde_json::to_string(&timeline)
            .unwrap()
            .contains("synthetic-inline-secret"));

        node.shutdown().await.expect("node shutdown");
    }

    #[tokio::test]
    async fn call_rows_carry_the_serving_account() {
        use crate::inference::auth::credential::{
            list_accounts, remove_account_in_txn, store_sign_in,
        };
        use crate::tools::call::lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmissionOptions,
        };
        let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "timeline-account-labels".into(),
            start_running: false,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = fixture.node.clone();
        let did = fixture.node_did.as_str();
        let access = fixture.client.clone();
        let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(did).unwrap());
        admin
            .grant_node_permission(
                &identity::Did::new(fixture.requester_identity.did()).unwrap(),
                crate::store::config_client::NodePermission::DocumentDelete,
            )
            .await
            .unwrap();
        let agent_access = ConfigAccess::local_with_agent_signer(
            node.clone(),
            identity::Did::new(&owner.request().agent_did).unwrap(),
            owner.managed_agent_signer().unwrap(),
        )
        .unwrap();
        let request_id = owner.request().request_id.clone();
        let request_doc_id = owner.request().doc_id.clone();
        let session_id = owner.request().session_id.clone();
        let session = access
            .execute(&format!(
                r#"{{AgentSession(filter: {{session_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}}}}"#,
                escape_graphql_string(&session_id)
            ))
            .await
            .unwrap();
        let [session] = session["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("one physical session")
        };
        let session_doc = session["_docID"].as_str().unwrap().to_owned();
        let spec = crate::inference::backends::setup::connection_spec(
            crate::inference::backends::setup::InferenceProviderId::Anthropic,
            crate::inference::backends::setup::InferenceAuthMethod::ClaudeOauth,
            "",
        )
        .unwrap();
        let original = serde_json::from_value(serde_json::json!({
            "node_did": did, "backend_id": "claude", "name": "Claude",
            "provider_kind": spec.provider_kind, "endpoint": spec.endpoint,
            "auth": {"kind": "node_oauth"},
        }))
        .unwrap();
        crate::documents::write_inference_backend_document(&access, &original)
            .await
            .unwrap();
        let sign_in = |who: &str| {
            crate::inference::auth::claude::credential_from_login_tokens(
                did,
                crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
                &crate::inference::auth::claude::ClaudeLoginTokens {
                    access_token: "access-SECRET".into(),
                    refresh_token: format!("refresh-SECRET-{who}"),
                    expires_in: Some(3600),
                    scope: None,
                    account_id: Some("IDENTITY".into()),
                    organization_uuid: Some("org-KEY".into()),
                    account_uuid: Some(format!("account-KEY-{who}")),
                },
                chrono::Utc::now(),
            )
        };
        store_sign_in(&access, sign_in("a"), None).await.unwrap();
        let b = store_sign_in(&access, sign_in("b"), Some("label-b"))
            .await
            .unwrap();
        let b_backend = format!(
            "{}-{}",
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );
        let at =
            |minutes: i64| (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339();
        let call = |call_id: &str, seq: i64, backend_id: &str, kind: &str, started_at: &str| {
            serde_json::json!({
                "call_id": call_id, "request_id": request_id,
                "request_doc_id": request_doc_id, "session_id": session_id,
                "call_seq": seq, "backend_id": backend_id, "node_did": did,
                "agent_did": owner.request().agent_did, "call_kind": kind,
                "attempt": 1, "call_state": "completed", "started_at": started_at,
            })
        };
        let publish_call = |input: serde_json::Value| {
            let access = &agent_access;
            let session_doc = &session_doc;
            async move {
                access.transact_write("test.timeline.account_call", |txn| {
                    let input = &input;
                    Box::pin(async move {
                        let response = txn.execute_with_variables(
                            "mutation($input: InferenceCallMutationInputArg!) {create_InferenceCall(input: $input) {_docID}}",
                            &serde_json::json!({"input": input}),
                        ).await?;
                        let doc = crate::store::graphql::created_doc_id(&response, "InferenceCall")?;
                        txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                }).await.unwrap();
            }
        };
        for input in [
            call("call-a", 1, "claude", "inference", &at(1)),
            call("call-b", 2, &b_backend, "compaction", &at(1)),
            call("call-gone", 3, "deleted-backend", "inference", &at(1)),
        ] {
            publish_call(input).await;
        }
        let accounts = || async {
            let timeline = load_run_timeline(&access, &request_id)
                .await
                .expect("load timeline");
            let text = serde_json::to_string(&timeline).unwrap();
            for secret in ["SECRET", "IDENTITY", "KEY"] {
                assert!(!text.contains(secret), "{secret} in {text}");
            }
            timeline
                .events
                .into_iter()
                .filter_map(|event| match event {
                    crate::observe::run_timeline::RunTimelineEvent::InferenceCall(call) => {
                        Some((call.call_id, call.account))
                    }
                    _ => None,
                })
                .collect::<BTreeMap<_, _>>()
        };
        let label = |label: &str| Some(label.to_string());
        assert_eq!(
            accounts().await,
            BTreeMap::from([
                ("call-a".to_string(), label("Claude")),
                ("call-b".to_string(), label("label-b")),
                ("call-gone".to_string(), None),
            ])
        );

        for account in list_accounts(&access, did).await.unwrap() {
            access
                .transact_write("test.remove", |txn| {
                    let credential_id = account.credential_id.clone();
                    Box::pin(async move { remove_account_in_txn(txn, did, &credential_id).await })
                })
                .await
                .unwrap();
        }
        let reused = store_sign_in(&access, sign_in("new"), Some("label-new"))
            .await
            .unwrap();
        assert_eq!(reused.credential.account_ref, None, "the original slot");
        access.transact_write("test.connected_at", |txn| {
            let doc = escape_graphql_string(&reused.doc_id);
            let connected_at = at(60);
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: OAuthCredentialMutationInputArg!) {{update_OAuthCredential(filter: {{_docID: {{_eq: "{doc}"}}}}, input: $input) {{_docID}}}}"#),
                    &serde_json::json!({"input": {"connected_at": connected_at}}),
                    gents_protocol::schemas::OAUTH_CREDENTIAL,
                ).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
        publish_call(call("call-later", 4, "claude", "inference", &at(120))).await;
        let after = accounts().await;
        assert_eq!(after["call-a"], None, "before the reused slot's sign-in");
        assert_eq!(after["call-later"], label("label-new"));
        drop(owner);
        node.shutdown().await.expect("node shutdown");
        std::fs::remove_dir_all(fixture.path).unwrap();
    }

    #[tokio::test]
    async fn fetches_native_goal_history_without_projecting_usage_only_commits() {
        use crate::store::identity::{KeyIdentity, NodeIdentity};
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        let directory = tempfile::tempdir().unwrap();
        let root = KeyIdentity::load_or_create(directory.path().join("root.key"), None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .with_node_identity_did(root.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        access
            .transact_write("test.timeline_session", |txn| {
                Box::pin(crate::session::ensure_session_in_txn(
                    txn,
                    "session-goal-history",
                    root.did(),
                    agent.did(),
                    Some(agent.did()),
                    None,
                    None,
                    "2026-08-14T12:00:00Z",
                ))
            })
            .await
            .unwrap();
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "request-goal-history",
            root.did(),
            agent.did(),
            agent.did(),
            "session-goal-history",
            "ship the timeline",
            "interactive",
            "2026-08-14T12:00:00Z",
            AgentRequestAdmissionRecord::local_self(agent.did()),
        );
        crate::sign_agent_request_create(agent.as_ref(), &mut request)
            .await
            .unwrap();
        let response = access
            .write(
                "test.timeline_request",
                &request.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let request_doc = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        access.write("test.timeline_terminal", &format!(r#"mutation {{ update_AgentRequest(docID: "{}", input: {{lifecycle_state: "completed"}}) {{_docID}} }}"#, crate::store::graphql::escape_graphql_string(&request_doc))).await.unwrap();
        let active = crate::goal::set_goal_from_access(
            &access,
            root.did(),
            "session-goal-history",
            Some("ship the timeline"),
            Some(crate::goal::GoalStatus::Active),
            None,
            None,
        )
        .await
        .expect("create goal");
        crate::goal::refresh_goal_usage_from_access(&access, &active)
            .await
            .expect("refresh usage");
        crate::goal::set_goal_from_access(
            &access,
            root.did(),
            "session-goal-history",
            None,
            Some(crate::goal::GoalStatus::Paused),
            None,
            None,
        )
        .await
        .expect("pause goal");

        let timeline = load_run_timeline(&access, "request-goal-history")
            .await
            .expect("load timeline");
        let transitions = timeline
            .events
            .iter()
            .filter_map(|event| match event {
                crate::observe::run_timeline::RunTimelineEvent::GoalTransition(event) => {
                    Some(event)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(transitions.len(), 2, "usage-only commit must be omitted");
        assert_eq!(transitions[0].state.status, "active");
        assert_eq!(transitions[1].state.status, "paused");
        assert!(!transitions[0].commit_cid.is_empty());
        assert!(!transitions[1].commit_cid.is_empty());
        assert_eq!(transitions[1].parents.len(), 1);
        assert_eq!(transitions[1].parents[0].state.status, "active");
        assert_ne!(
            transitions[1].parents[0].commit_cid, transitions[0].commit_cid,
            "the semantic event must preserve its immediate native usage-only parent",
        );

        node.shutdown().await.expect("node shutdown");
    }
}
