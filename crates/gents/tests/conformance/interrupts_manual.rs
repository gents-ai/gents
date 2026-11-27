use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[tokio::test]
async fn interrupt_request_follows_the_lean_drain_model() {
    use gents_lean_vocab::families::interrupt_request_drain_drain;
    for case in gents_lean_vocab::families::interrupt_request_drain_drain::cases::<
        String,
        String,
        RequestLifecycleState,
    >() {
        let db = trial_store("interrupt-drain-wakeups").await;
        let actors = support::agent_requests::RequestActors::new(&db).await;
        let session_id = "interrupt-drain-session";
        let foreign = tokio::task::spawn_blocking(|| {
            gents_store::test_support::test_identity("foreign-drain")
        })
        .await
        .unwrap();
        let foreign_node_did = gents_store::identity::NodeIdentity::did(&foreign).to_owned();
        let wakeup_input = format!(
            r#"{{"queue":{{"source":"background_completion","policy":"coalesce","key":"background_completion:{session_id}","queued_after_request_id":null}}}}"#
        );
        let non_wakeup_input = r#"{"queue":{"source":"user","policy":"append","key":null,"queued_after_request_id":null}}"#;
        actors
            .create_session_with(json!({
                "node_did": actors.node_did(),
                "session_id": session_id,
                "agent_did": actors.agent_did(),
                "created_at": "2026-03-23T00:00:00Z",
                "relationships_pending": false,
            }))
            .await;
        let parent_doc = create_pending_queue_row(
            &db,
            &actors,
            "drain-parent",
            session_id,
            actors.node_did(),
            "interactive",
            non_wakeup_input,
        )
        .await;
        actors
            .set_lifecycle_state(&parent_doc, RequestLifecycleState::Processing)
            .await;
        let mut request_ids = Vec::new();
        for (index, row) in case.input.rows.iter().enumerate() {
            let request_id = format!("drain-row-{index}");
            let doc = create_pending_queue_row(
                &db,
                &actors,
                &request_id,
                session_id,
                if row.foreign {
                    foreign_node_did.as_str()
                } else {
                    actors.node_did()
                },
                if row.scheduled {
                    "scheduled"
                } else {
                    "interactive"
                },
                if row.wake {
                    wakeup_input.as_str()
                } else {
                    non_wakeup_input
                },
            )
            .await;
            request_ids.push((request_id, doc));
        }
        let target = if case.input.active {
            "drain-parent"
        } else {
            "bogus-id-that-does-not-exist"
        };
        let outcome = match gents_session::request::interrupt::interrupt_request(
            &db.node,
            actors.agent().as_ref(),
            target,
        )
        .await
        {
            Ok(()) => "latched".to_owned(),
            Err(error) if error.to_string().contains("missing") => "missing".to_owned(),
            Err(error) => panic!("interrupt failed for an unmodeled reason: {error:#}"),
        };
        let latched =
            gents_session::request::interrupt::request_interrupted_at_by_doc_id_with_access(
                &actors.agent_access,
                &parent_doc,
            )
            .await
            .unwrap()
            .is_some();
        for (row, (_, doc)) in case.input.rows.iter().zip(&request_ids) {
            if !row.foreign {
                claim_own_pending(&actors, doc).await;
            }
        }
        let mut rows = Vec::new();
        for (request_id, _) in &request_ids {
            let row = fetch_drain_row(&actors, request_id).await;
            rows.push(interrupt_request_drain_drain::RowObservation {
                state: row.lifecycle_state,
                reason: row.failure_reason.map(|reason| {
                    if reason == gents_session::request::interrupt::INTERRUPT_DRAIN_REASON {
                        "active_interrupted".to_owned()
                    } else {
                        reason
                    }
                }),
            });
        }
        case.check(interrupt_request_drain_drain::Output {
            outcome,
            latched,
            rows,
        });
    }
}

/// The runtime root writes the queued replica for the fixture Agent and grants
/// it as working Agent; only the node scope and execution origin differ.
async fn create_pending_queue_row(
    db: &gents_store::test_support::TestStore,
    actors: &support::agent_requests::RequestActors,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    execution_origin: &str,
    input: &str,
) -> String {
    let input = serde_json::from_str::<Value>(input).expect("request input JSON");
    let value = json!({
        "request_id": request_id,
        "purpose": "normal",
        "node_did": node_did,
        "agent_did": actors.agent_did(),
        "session_id": session_id,
        "retry_root_request": request_id,
        "content": "queued wake-up",
        "input": input,
        "lifecycle_state": "pending",
        "execution_origin": execution_origin,
        "created_at": "2026-03-23T00:00:00Z",
        "retry_count": 0,
        "interrupt_generation": 0,
    });
    let agent = ::identity::Did::new(actors.agent_did()).unwrap();
    gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        ::identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .transact_write("test.interrupt.queue_row", |txn| {
        let (value, agent) = (&value, &agent);
        Box::pin(async move {
            let response = txn
                .execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": value}),
                )
                .await?;
            let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
            txn.add_actor_relationship("AgentRequest", &doc, "working_agent", agent)
                .await?;
            Ok::<_, anyhow::Error>(doc)
        })
    })
    .await
    .expect("create pending queue row as runtime root")
}

/// Runs the production claim of an own pending row as the working Agent: a
/// wake an interrupt covered is drained (`InterruptGeneration.wakeAtClaim`),
/// and with the parent still running every other row stays queued.
async fn claim_own_pending(actors: &support::agent_requests::RequestActors, doc: &str) {
    let request = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &actors.agent_access,
        doc,
    )
    .await
    .expect("load own pending row")
    .expect("own pending row");
    let origin = ExecutionOrigin::from_persisted(request.execution_origin.as_deref())
        .expect("fixture execution origin");
    let mut lifecycle = actors.lifecycle(request, 60, origin, "drain-backend");
    let outcome = lifecycle
        .claim_with_identity()
        .await
        .expect("claim own pending row");
    assert_ne!(
        outcome,
        gents_session::request::lifecycle::ClaimOutcome::Claimed,
        "the parent still runs"
    );
}

#[derive(Debug, Deserialize)]
struct DrainRow {
    lifecycle_state: RequestLifecycleState,
    #[serde(default)]
    failure_reason: Option<String>,
}

async fn fetch_drain_row(
    actors: &support::agent_requests::RequestActors,
    request_id: &str,
) -> DrainRow {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let response = actors
        .agent_access
        .execute(&query)
        .await
        .expect("drain row");
    serde_json::from_value(response["data"]["AgentRequest"][0].clone()).expect("decode drain row")
}
