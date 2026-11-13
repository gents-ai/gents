use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::config_client::{ConfigAccess, IdempotentTransactionRetry};

#[tokio::test]
async fn request_head_uses_exact_scope_and_timestamp_then_logical_id() {
    let node_db = gents_inference::test_support::test_db(
        "request-head-uses-exact-scope-and-timest",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    ConfigAccess::transact_local_idempotent(
        &node, None, IdempotentTransactionRetry::Standard, "test.session.request_head",
        |txn| Box::pin(async move {
            for (id, agent, requester, time) in [
                ("a", "did:test:head", None, "2030-01-01T01:00:00+01:00"),
                ("z", "did:test:head", None, "2030-01-01T00:00:00Z"),
                ("remote", "did:test:head", Some("did:test:requester"), "2030-01-01T00:00:01Z"),
                ("foreign", "did:test:foreign", None, "2040-01-01T00:00:00Z"),
            ] {
                txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"request_id": id, "purpose": "normal", "node_did": agent,
                        "requester_did": requester, "session_id": "head-session", "agent_did": "head-agent",
                        "content": "prompt", "created_at": time, "lifecycle_state": "pending"}}),
                ).await?;
            }
            let local = load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(None)).await?.unwrap();
            assert_eq!(local.observed.request_id, "z");
            let all = load_latest_request_in_txn(txn, "did:test:head", "head-session", None).await?.unwrap();
            assert_eq!(all.observed.request_id, "remote");
            let exact = load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(Some("did:test:requester"))).await?.unwrap();
            assert_eq!(exact, all);
            assert!(load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(Some("did:test:absent"))).await?.is_none());
            Ok::<_, anyhow::Error>(())
        }),
    ).await.unwrap();
}

#[tokio::test]
async fn batched_request_heads_preserve_scope_order_and_bound_query_count() {
    let node_db = gents_inference::test_support::test_db(
        "batched-request-heads-preserve-scope-ord",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let session_ids = (0..REQUEST_FACT_SESSION_BATCH_SIZE + 1)
        .map(|index| format!("batch-session-{index:02}"))
        .collect::<Vec<_>>();

    ConfigAccess::transact_local_idempotent(
        &node,
        None,
        IdempotentTransactionRetry::Standard,
        "test.session.batched_request_heads",
        |txn| {
            let session_ids = session_ids.clone();
            Box::pin(async move {
                for (index, session_id) in session_ids.iter().enumerate() {
                    let request_id = format!("request-{index:02}");
                    txn.execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": {
                            "request_id": request_id,
                            "purpose": "normal",
                            "node_did": "did:test:batch-heads",
                            "session_id": session_id,
                            "agent_did": "batch-agent",
                            "content": "prompt",
                            "created_at": "2030-01-01T00:00:00Z",
                            "lifecycle_state": "pending"
                        }}),
                    ).await?;
                }
                for (request_id, purpose, node_did, requester_did, created_at) in [
                    ("a-tie", "normal", "did:test:batch-heads", None, "2030-01-01T01:00:00+01:00"),
                    ("z-tie", "normal", "did:test:batch-heads", None, "2030-01-01T00:00:00Z"),
                    ("remote-head", "normal", "did:test:batch-heads", Some("did:test:requester"), "2030-01-01T00:00:01Z"),
                    ("foreign-node", "normal", "did:test:other-node", None, "2040-01-01T00:00:00Z"),
                    ("title-audit", "title-audit", "did:test:batch-heads", None, "2050-01-01T00:00:00Z"),
                ] {
                    txn.execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": {
                            "request_id": request_id,
                            "purpose": purpose,
                            "node_did": node_did,
                            "requester_did": requester_did,
                            "session_id": session_ids[0],
                            "agent_did": "batch-agent",
                            "content": "prompt",
                            "created_at": created_at,
                            "lifecycle_state": "pending"
                        }}),
                    ).await?;
                }

                let (heads, batch_queries) = count_request_fact_batch_queries(
                    load_latest_requests_in_txn(
                        txn,
                        "did:test:batch-heads",
                        &session_ids,
                        None,
                    ),
                )
                .await;
                let heads = heads?;
                assert_eq!(batch_queries, 2);
                assert_eq!(heads.len(), session_ids.len());
                assert_eq!(heads[&session_ids[0]].observed.request_id, "remote-head");
                assert_eq!(heads[&session_ids[1]].observed.request_id, "request-01");

                let absent_requester = load_latest_requests_in_txn(
                    txn,
                    "did:test:batch-heads",
                    &session_ids[..1],
                    Some(None),
                )
                .await?;
                assert_eq!(absent_requester[&session_ids[0]].observed.request_id, "z-tie");
                let exact_requester = load_latest_requests_in_txn(
                    txn,
                    "did:test:batch-heads",
                    &session_ids[..1],
                    Some(Some("did:test:requester")),
                )
                .await?;
                assert_eq!(exact_requester[&session_ids[0]].observed.request_id, "remote-head");
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn session_observation_traces_match_the_lean_model() {
    let base = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z").unwrap();
    let at_ms = move |ms: u64| (base + chrono::Duration::milliseconds(ms as i64)).to_rfc3339();
    let at_s = move |s: u64| (base + chrono::Duration::seconds(s as i64)).to_rfc3339();
    let since = move |value: &str| {
        (chrono::DateTime::parse_from_rfc3339(value).unwrap() - base).num_milliseconds() as u64
    };
    let node_did = |n: u64| format!("did:test:node-{n}");
    let session_id = |n: u64| format!("session-{n}");
    let requester_did = |n: u64| format!("did:test:requester-{n}");
    let agent_did = |n: u64| format!("agent-{n}");
    let request_id = |n: u64| format!("{n:08}");
    for case in gents_lean_vocab::families::session_observation_trace::cases::<RequestLifecycleState, String>() {
        let node_db = gents_inference::test_support::test_db(
            "session-observation-traces-match-the-lea",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();
        let trace = case.input.clone();
        let observed = ConfigAccess::transact_local_idempotent(
            &node, None, IdempotentTransactionRetry::Standard, "test.session.observation_trace",
            |txn| {
                let trace = trace.clone();
                Box::pin(async move {
                    let owner = &trace.session;
                    let (node, session) = (node_did(owner.node), session_id(owner.session));
                    let requester = owner.requester.map(requester_did);
                    let mut docs = Vec::<String>::new();
                    let mut observed = Vec::new();
                    for step in &trace.steps {
                        let mut applied = None;
                        let mut latest = None;
                        match step.kind.as_str() {
                            "ensure" => {
                                crate::session::ensure_session_in_txn(
                                    txn, &session, &node, &agent_did(owner.agent),
                                    requester.as_deref(), None, None, &at_ms(step.now),
                                ).await?;
                            }
                            "create" => {
                                let row = step.row.as_ref().context("create without a row")?;
                                let response = txn.execute_with_variables(
                                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                                    &serde_json::json!({"input": {
                                        "request_id": request_id(row.request_id), "purpose": "normal",
                                        "node_did": node_did(row.node), "session_id": session_id(row.session),
                                        "requester_did": row.requester.map(requester_did),
                                        "agent_did": agent_did(row.agent), "content": "prompt",
                                        "created_at": at_s(row.created_at), "lifecycle_state": row.state}}),
                                ).await?;
                                docs.push(
                                    gents_store::graphql::created_doc_id(&response, "AgentRequest")?,
                                );
                            }
                            "finish" => {
                                let doc = escape_graphql_string(&docs[step.target as usize]);
                                let terminalized = step
                                    .terminalized_at
                                    .map(|ms| format!(r#", terminalized_at: "{}""#, at_ms(ms)))
                                    .unwrap_or_default();
                                txn.execute_local_response(&format!(r#"mutation {{
                                    update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }},
                                        input: {{ lifecycle_state: "{}"{terminalized} }}) {{ _docID }}
                                }}"#, step.state.as_str())).await?;
                            }
                            "advance" => {
                                let incoming = trace_fact(txn, docs.get(step.target as usize)).await?;
                                applied = Some(
                                    advance_session_request_observation_in_txn(
                                        txn, &incoming, &step.preview, &at_ms(step.now),
                                    ).await?,
                                );
                            }
                            "refresh" => {
                                let incoming = trace_fact(txn, docs.get(step.target as usize)).await?;
                                applied = Some(
                                    refresh_session_request_observation_in_txn(
                                        txn, &node, requester.as_deref(), &session,
                                        SessionRequestRefresh {
                                            request_doc_id: &incoming.observed.request_doc_id,
                                            request_id: &incoming.observed.request_id,
                                            reply_preview: step.terminal.then_some(step.reply.as_deref()),
                                        },
                                        &at_ms(step.now),
                                    ).await?,
                                );
                            }
                            "close" => {
                                let row = load_agent_session_row_in_txn(
                                    txn, &node, &session, requester.as_deref(),
                                ).await?.context("trace closes a missing session")?;
                                patch_session_in_txn(txn, &row.doc_id, serde_json::json!({
                                    "closed_at": at_ms(step.now), "tags": step.tags,
                                })).await?;
                            }
                            "reopen" => {
                                applied = Some(
                                    crate::session::reopen_session_in_txn(
                                        txn, &session, &node, requester.as_deref(), &at_ms(step.now),
                                    ).await?,
                                );
                            }
                            "stale" => {
                                let incoming = trace_fact(txn, docs.get(step.target as usize)).await?;
                                let row = load_agent_session_row_in_txn(
                                    txn, &node, &session, requester.as_deref(),
                                ).await?.context("trace points a missing session")?;
                                let mut observation =
                                    row.session.observation.clone().context("stale point without observation")?;
                                observation.latest_request = Some(
                                    gents_protocol::session::SessionRequestObservation {
                                        lifecycle_state: step.state,
                                        ..incoming.observed.clone()
                                    },
                                );
                                patch_session_in_txn(
                                    txn, &row.doc_id, serde_json::json!({ "observation": observation }),
                                ).await?;
                            }
                            "latest" => {
                                let scope = (!step.all_requesters)
                                    .then(|| step.requester.map(requester_did));
                                latest = load_latest_request_in_txn(
                                    txn, &node, &session, scope.as_ref().map(Option::as_deref),
                                )
                                .await?
                                .map(|fact| {
                                    docs.iter().position(|doc| *doc == fact.observed.request_doc_id)
                                        .expect("selected an unknown request") as u64
                                });
                            }
                            other => anyhow::bail!("unmodeled step {other}"),
                        }
                        let current = load_agent_session_row_in_txn(
                            txn, &node, &session, requester.as_deref(),
                        ).await?.map(|row| row.session);
                        let observation = current.as_ref().and_then(|s| s.observation.clone());
                        observed.push(gents_lean_vocab::families::session_observation_trace::Observed {
                            applied,
                            latest,
                            head: observation.as_ref().and_then(|o| o.latest_request.as_ref()).map(|head| {
                                gents_lean_vocab::families::session_observation_trace::HeadWire {
                                    request: docs.iter().position(|doc| *doc == head.request_doc_id)
                                        .expect("head names an unknown request") as u64,
                                    request_id: head.request_id.parse().expect("numeric request id"),
                                    state: head.lifecycle_state,
                                }
                            }),
                            reply_preview: observation.and_then(|o| o.reply_preview),
                            closed: current.as_ref().is_some_and(|s| s.closed_at.is_some()),
                            created_at: current.as_ref().map(|s| since(&s.created_at)),
                            tags: current.map(|s| s.tags).unwrap_or_default(),
                        });
                    }
                    Ok::<_, anyhow::Error>(observed)
                })
            },
        ).await.unwrap();
        case.check(observed);
    }
}

async fn trace_fact(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    doc: Option<&String>,
) -> anyhow::Result<SessionRequestFact> {
    let doc = doc.context("trace names an uncreated request")?;
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ node_did session_id requester_did }} }}"#,
            escape_graphql_string(doc)
        ))
        .await?;
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .context("request row missing")?;
    let text = |field: &str| row.get(field).and_then(serde_json::Value::as_str).map(str::to_owned);
    let requester = text("requester_did");
    load_request_facts_in_txn(
        txn,
        &text("node_did").context("request omitted node_did")?,
        &text("session_id").context("request omitted session_id")?,
        Some(requester.as_deref()),
    )
    .await?
    .into_iter()
    .find(|fact| fact.observed.request_doc_id == *doc)
    .context("request fact missing")
}

/// Lean `AgentSession.latest` selection cases, run through the native fact
/// query and head selection over DefraDB.
#[tokio::test]
async fn generated_selection_cases_drive_the_native_request_head() {
    let base = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z").unwrap();
    let seconds =
        |value: u64| (base + chrono::Duration::seconds(i64::try_from(value).unwrap())).to_rfc3339();
    let millis = |value: Option<u64>| {
        value.map(|ms| {
            (base + chrono::Duration::milliseconds(i64::try_from(ms).unwrap())).to_rfc3339()
        })
    };
    let did = |kind: &str, value: Option<u64>| value.map(|n| format!("did:test:{kind}-{n}"));
    let request_id = |value: u64| format!("{value:08}");
    for case in gents_lean_vocab::families::contracts_session_selection::cases::<String>() {
        let node_db = gents_inference::test_support::test_db(
            "generated-selection-cases-drive-the-nati",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();
        let input = case.input.clone();
        let selected = ConfigAccess::transact_local_idempotent(
            &node, None, IdempotentTransactionRetry::Standard, "test.session.generated_selection",
            |txn| {
                let input = input.clone();
                Box::pin(async move {
                    for row in &input.requests {
                        txn.execute_with_variables(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({"input": {
                                "request_id": request_id(row.request_id),
                                "purpose": row.purpose, "node_did": did("node", Some(row.scope.node)),
                                "session_id": format!("session-{}", row.scope.session),
                                "requester_did": did("requester", row.scope.requester),
                                "agent_did": format!("agent-{}", row.agent), "content": "prompt",
                                "created_at": seconds(row.created_at),
                                "terminalized_at": millis(row.terminalized_at),
                                "lifecycle_state": row.state}}),
                        ).await?;
                    }
                    let scope = input.requester_scoped
                        .then(|| did("requester", input.requester));
                    let latest = load_latest_request_in_txn(
                        txn, &did("node", Some(input.node)).unwrap(),
                        &format!("session-{}", input.session), scope.as_ref().map(Option::as_deref),
                    ).await?;
                    Ok::<_, anyhow::Error>(latest.map(|fact| fact.observed.request_id))
                })
            },
        ).await.unwrap();
        let selected = selected.map(|id| {
            case.input
                .requests
                .iter()
                .find(|row| request_id(row.request_id) == id)
                .unwrap()
                .clone()
        });
        case.check(
            gents_lean_vocab::families::contracts_session_selection::SelectionOutput { selected },
        );
        node.shutdown().await.expect("shut down test node");
    }
}
