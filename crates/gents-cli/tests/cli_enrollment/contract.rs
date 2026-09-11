//! Production enrollment and client-visible conversation contract.
//!
//! Only inference is deterministic. The CLI server, signed enrollment,
//! operator approval, Iroh replication, and the iOS app's ClientCore/store
//! are real. This is not a test of UIKit rendering or iOS suspension; those
//! require the device acceptance run. Each deadline includes all work in its
//! phase, rather than allowing each polling helper a fresh latency budget.

use super::*;
use std::sync::Arc;
use support::mocks::fake_llm::{ChatAction, FakeLlm};
use tokio::time::timeout;

const ENROLL_BUDGET: Duration = Duration::from_secs(30);
const TURN_BUDGET: Duration = Duration::from_secs(20);
const RECONNECT_BUDGET: Duration = Duration::from_secs(20);
const MODEL_DELAY: Duration = Duration::from_secs(2);
const RETURN_TO_OBSERVER_BUDGET: Duration = Duration::from_secs(2);
const REPLY: &str = "PAIRING_CONTRACT_ASSISTANT_REPLY";
const OFFLINE_REPLY: &str = "PAIRING_CONTRACT_OFFLINE_REPLY";
const FOLLOWUP_REPLY: &str = "PAIRING_CONTRACT_CONTINUED_REPLY";
const OFFLINE_PROMPT: &str = "Reply while the app is closed";
#[path = "contract/streaming.rs"]
mod streaming;
const FOLLOWUP_PROMPT: &str = "Continue our existing conversation";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_enrollment_conversation_survives_reopen_with_bounded_latency() -> Result<()> {
    run_contract(0).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_app_pairs_with_aged_runtime_with_bounded_latency() -> Result<()> {
    run_contract(2_500).await
}

async fn run_contract(readiness_revisions: usize) -> Result<()> {
    run_contract_with_streaming(readiness_revisions, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_content_reaches_client_before_provider_completion() -> Result<()> {
    run_contract_with_streaming(0, true).await
}

async fn run_contract_with_streaming(readiness_revisions: usize, streaming: bool) -> Result<()> {
    run_contract_with_streaming_cadence(readiness_revisions, streaming, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn buffered_stream_chunk_reaches_client_during_provider_pause() -> Result<()> {
    run_contract_with_streaming_cadence(0, true, true).await
}

async fn run_contract_with_streaming_cadence(
    readiness_revisions: usize,
    streaming: bool,
    paced: bool,
) -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cli_enrollment=info".into()),
        )
        .with_test_writer()
        .try_init();
    let offline_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let model_gate = Arc::clone(&offline_gate);
    let stream_gate = streaming.then(|| Arc::new(tokio::sync::Semaphore::new(0)));
    let followup_stream_gate = streaming.then(|| Arc::new(tokio::sync::Semaphore::new(0)));
    let model_stream_gate = stream_gate.clone();
    let model_followup_stream_gate = followup_stream_gate.clone();
    let model = FakeLlm::start(
        "pairing-contract",
        None,
        Arc::new(move |request| {
            tracing::info!("deterministic provider received inference request");
            let latest = request
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| {
                    messages
                        .iter()
                        .rev()
                        .find(|message| message["role"] == "user")
                });
            let latest = serde_json::json!({"messages": [latest]});
            if request_contains_role_text(&latest, "user", OFFLINE_PROMPT) {
                ChatAction::WaitThenSse(Arc::clone(&model_gate), completion_text_sse(OFFLINE_REPLY))
            } else {
                let reply = if request_contains_role_text(&latest, "user", FOLLOWUP_PROMPT) {
                    FOLLOWUP_REPLY
                } else {
                    REPLY
                };
                let selected_stream_gate =
                    if request_contains_role_text(&latest, "user", FOLLOWUP_PROMPT) {
                        &model_followup_stream_gate
                    } else {
                        &model_stream_gate
                    };
                if let Some(gate) = selected_stream_gate.as_ref().filter(|_| {
                    request_contains_role_text(&latest, "user", "First conversation turn")
                        || request_contains_role_text(&latest, "user", FOLLOWUP_PROMPT)
                }) {
                    let body = completion_text_sse(reply);
                    let (first, rest) = body.split_once("\n\n").expect("SSE content frame");
                    let chunks = if paced {
                        let (prefix, suffix) = reply.split_at(reply.len() / 2);
                        [prefix, suffix]
                            .into_iter()
                            .enumerate()
                            .map(|(index, text)| {
                                let body = completion_text_sse(text);
                                let frame = body.split_once("\n\n").expect("SSE content frame").0;
                                (
                                    Duration::from_millis(if index == 0 { 0 } else { 20 }),
                                    format!("{frame}\n\n"),
                                )
                            })
                            .collect()
                    } else {
                        vec![(Duration::ZERO, format!("{first}\n\n"))]
                    };
                    ChatAction::GatedSse(chunks, gate.clone(), rest.to_owned())
                } else {
                    ChatAction::DelayThenSse(MODEL_DELAY, completion_text_sse(reply))
                }
            }
        }),
    )?;
    let temp = tempfile::tempdir()?;
    let runtime_home = temp.path().join("runtime");
    let client_home = temp.path().join("app");
    fs::create_dir_all(&runtime_home)?;
    let home = runtime_home.to_str().context("runtime home UTF-8")?;
    let init = run_init_json(
        &runtime_home,
        &[
            "--home",
            home,
            "--agent-name",
            "pairing-contract",
            "--model-name",
            "pairing-contract",
            "--inference-url",
            model.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let behavior = default_behavior_id_for_agent(&agent_did);
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let debug_filter = std::env::var("RUST_LOG").ok();
    let server_env = debug_filter
        .as_deref()
        .map(|filter| ("RUST_LOG", filter))
        .into_iter()
        .collect::<Vec<_>>();
    let (mut server, _) =
        spawn_server_with_ready_json(&runtime_home, port, &["--home", home], &server_env)?;
    wait_for_port(port, &mut server)?;

    let result = server.capturing(async {
        wait_for_runtime_ready(&graphql, &agent_did, ENROLL_BUDGET).await?;
        let readiness_timestamp = seed_readiness_history(&graphql, &agent_did, readiness_revisions).await?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&client_home), ClientCoreOptions::local_only(),
        ).await?;
        core.set_selected_agent_did(Some(agent_did.clone()));
        let enrollment_started = Instant::now();
        let enrollment = timeout(ENROLL_BUDGET, async {
            let (_, offer) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}")).await?;
            let pending = core.request_status_enrollment_with_label(&offer, Some("Contract")).await?;
            anyhow::ensure!(pending.state == "pending_approval", "unexpected initial enrollment: {pending:?}");
            wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
            run_cli_json(&runtime_home, &["p2p", "enrollment", "approve", &pending.request_id, "--home", home])?;
            wait_for_chat_ready_enrollment(&core, &agent_did).await?;
            wait_for_client_behavior_readiness(&core, &agent_did).await?;
            if let Some(expected) = readiness_timestamp.as_deref() {
                wait_for_readiness_revision(&core, &agent_did, expected).await?;
            }
            anyhow::ensure!(query_collection_dids(core.node(), "AgentPrincipal").await?.is_empty(), "runtime principal replicated to app");
            Ok::<_, anyhow::Error>(())
        }).await;
        if !matches!(enrollment, Ok(Ok(()))) {
            let diagnostics = pairing_diagnostics(&core, &graphql).await;
            core.shutdown().await?;
            bail!("enroll/approve/readiness exceeded 30s or failed: {enrollment:?}; {diagnostics}");
        }
        tracing::info!(elapsed_ms = enrollment_started.elapsed().as_millis(), "app enrollment ready");
        assert_observer_did_not_overflow(&core).await?;

        let session = Uuid::new_v4().to_string();
        visible_turn(&core, &graphql, &agent_did, &behavior, &session, "First conversation turn", stream_gate.as_deref()).await?;

        // A request reaches the runtime, then the app closes before inference
        // finishes. Reopen exactly the same home: no re-enrollment, identity
        // reset, injected desired rows, or hand-installed return replicator.
        let records = core.peer_records().await;
        core.submit_request(&session, &agent_did, OFFLINE_PROMPT, Some(&behavior)).await?;
        let (request, _, _) = timeout(TURN_BUDGET, wait_for_runtime_agent_request(
            &graphql, core.node(), &agent_did, OFFLINE_PROMPT,
        )).await.context("second request did not reach runtime in 20s")??;
        core.shutdown().await?;
        drop(core);
        offline_gate.add_permits(1);
        wait_for_complete_agent_response(&graphql, &request, TURN_BUDGET).await?;

        let reconnect_started = Instant::now();
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&client_home), ClientCoreOptions::local_only(),
        ).await?;
        core.set_selected_agent_did(Some(agent_did.clone()));
        let recovered = timeout(RECONNECT_BUDGET.saturating_sub(reconnect_started.elapsed()), async {
            wait_for_chat_ready_enrollment(&core, &agent_did).await?;
            wait_for_client_behavior_readiness(&core, &agent_did).await?;
            wait_for_replicated_reply(&core, &session, &agent_did, &request, OFFLINE_REPLY).await
        }).await;
        if !matches!(recovered, Ok(Ok(()))) {
            let diagnostics = pairing_diagnostics(&core, &graphql).await;
            core.shutdown().await?;
            bail!("app reopen did not recover completed reply within 20s: {recovered:?}; {diagnostics}");
        }
        let reopened = core.peer_records().await;
        anyhow::ensure!(records.len() == reopened.len() && records.iter().zip(&reopened).all(|(old, new)| old.peer_id == new.peer_id && old.enrollment_request_id == new.enrollment_request_id), "reopen changed enrollment identity");
        tracing::info!(elapsed_ms = reconnect_started.elapsed().as_millis(), "app recovered offline reply");
        visible_turn(&core, &graphql, &agent_did, &behavior, &session, FOLLOWUP_PROMPT, followup_stream_gate.as_deref()).await?;
        assert_local_pagination(&core, &session, &agent_did).await?;
        assert_observer_did_not_overflow(&core).await?;
        anyhow::ensure!(query_collection_dids(core.node(), "AgentPrincipal").await?.is_empty(), "reopen replicated runtime principal");
        core.shutdown().await?;
        anyhow::ensure!(model.captured_chat_requests().iter().any(|request| {
            request_contains_role_text(request, "user", FOLLOWUP_PROMPT)
                && request_contains_role_text(request, "assistant", REPLY)
                && request_contains_role_text(request, "assistant", OFFLINE_REPLY)
        }), "continued conversation lost its prior assistant transcript");
        Ok(())
    }).await;
    if debug_filter.is_some() {
        let (stdout, stderr) = server.captured_output()?;
        tracing::debug!(target: "cli_enrollment::server", %stdout, %stderr, "paired runtime trace");
    }
    if result.is_err() {
        let retained = temp.keep();
        tracing::error!(path = %retained.display(), "retained failed pairing fixture for investigation");
    }
    result
}

async fn visible_turn(
    core: &ClientCore,
    graphql: &str,
    agent: &str,
    behavior: &str,
    session: &str,
    prompt: &str,
    stream_gate: Option<&tokio::sync::Semaphore>,
) -> Result<()> {
    let started = Instant::now();
    let result = timeout(TURN_BUDGET, async {
        core.submit_request(session, agent, prompt, Some(behavior))
            .await?;
        let local_submit_completed = started.elapsed();
        let (request, _, _) =
            wait_for_runtime_agent_request(graphql, core.node(), agent, prompt).await?;
        let request_arrived = started.elapsed();
        let (selection_admitted, runtime_completed, client_visible) = tokio::try_join!(
            async {
                select_session(core, session, agent).await?;
                Ok::<_, anyhow::Error>(started.elapsed())
            },
            async {
                wait_for_complete_agent_response(graphql, &request, TURN_BUDGET).await?;
                Ok::<_, anyhow::Error>(started.elapsed())
            },
            async {
                let expected = if prompt == FOLLOWUP_PROMPT { FOLLOWUP_REPLY } else { REPLY };
                if let Some(gate) = stream_gate {
                    let visibility = streaming::wait_for_visible_content(core, &request, expected).await;
                    gate.add_permits(1);
                    visibility?;
                }
                wait_for_replicated_reply(core, session, agent, &request, expected).await?;
                Ok::<_, anyhow::Error>(started.elapsed())
            },
        )?;
        let return_budget = if stream_gate.is_some() {
            Duration::from_millis(500)
        } else {
            RETURN_TO_OBSERVER_BUDGET
        };
        let observed_return_skew_us = if client_visible >= runtime_completed {
            (client_visible - runtime_completed).as_micros() as i128
        } else {
            -((runtime_completed - client_visible).as_micros() as i128)
        };
        tracing::info!(
            local_submit_ms = local_submit_completed.as_millis(),
            submit_to_runtime_observed_ms = request_arrived.saturating_sub(local_submit_completed).as_millis(),
            request_delivery_ms = request_arrived.as_millis(),
            selection_admission_ms = selection_admitted.as_millis(),
            runtime_completion_ms = runtime_completed.as_millis(),
            client_visible_ms = client_visible.as_millis(),
            observed_return_skew_us,
            runtime_probe_interval_ms = 250,
            client_database_probe_interval_ms = 25,
            "conversation phase timings",
        );
        let lifecycle = graphql_query(graphql, &format!(r#"{{
            AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{created_at claimed_at terminalized_at}}
            AgentResponse(filter: {{request_id: {{_eq: "{}"}}}}) {{created_at completed_at materialized_at}}
        }}"#, escape_graphql_string(&request), escape_graphql_string(&request))).await?;
        tracing::info!(timestamps = ?lifecycle, "runtime lifecycle timing evidence");
        anyhow::ensure!(
            client_visible.saturating_sub(runtime_completed) <= return_budget,
            "completed reply took too long to reach the client projection: runtime={runtime_completed:?}, client={client_visible:?}",
        );
        Ok::<_, anyhow::Error>(())
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        let diagnostics = pairing_diagnostics(core, graphql).await;
        core.shutdown().await?;
        bail!("client-visible turn exceeded 20s or failed: {result:?}; {diagnostics}");
    }
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis(),
        prompt,
        "completed assistant reply visible in app store"
    );
    assert_observer_did_not_overflow(core).await?;
    Ok(())
}

async fn assert_observer_did_not_overflow(core: &ClientCore) -> Result<()> {
    let metrics = core.observer_metrics().await.context("observer running")?;
    anyhow::ensure!(
        metrics.drop_recoveries == 0,
        "history overflowed the state observer: {metrics:?}"
    );
    tracing::info!(?metrics, "bounded client observation");
    Ok(())
}

/// Session selection is a data-plane action, independent of UI page reads.
async fn select_session(core: &ClientCore, session: &str, agent: &str) -> Result<()> {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationPhase;
    loop {
        core.ensure_session_hydration_started(session, agent)
            .await?;
        match core.session_hydration_progress(session, agent).await?.phase {
            ClientHydrationPhase::Idle => sleep(Duration::from_millis(100)).await,
            ClientHydrationPhase::Failed => bail!("session selection was rejected"),
            _ => return Ok(()),
        }
    }
}

/// No hydration, repair, UI refresh, or remote reads here: success is a
/// completed conversation in the phone-side database AND the observed response
/// projection consumed by the app. A healthy DB with a stuck observer must fail.
async fn wait_for_replicated_reply(
    core: &ClientCore,
    session: &str,
    agent: &str,
    request: &str,
    expected_reply: &str,
) -> Result<()> {
    const DATABASE_PROBE_INTERVAL: Duration = Duration::from_millis(25);

    let filter = format!(
        r#"request_id: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}"#,
        escape_graphql_string(request),
        escape_graphql_string(session),
        escape_graphql_string(agent),
        escape_graphql_string(core.principal().did()),
    );
    let query = format!(
        r#"{{
        AgentRequest(filter: {{{filter}}}) {{_docID lifecycle_state}}
        AgentResponse(filter: {{{filter}}}) {{_docID status content}}
        AgentMessage(filter: {{{filter}}}) {{_docID role content}}
    }}"#
    );
    let visibility_started = Instant::now();
    let database_visibility = async {
        let mut stages_seen = [false; 3];
        let mut response_ready_at = None;
        loop {
            let result = core.node().execute(&query).await;
            anyhow::ensure!(
                !result.has_errors(),
                "replica query failed: {:?}",
                result.errors
            );
            let data = result.data.context("replica query missing data")?;
            let rows = |collection: &str| {
                data.get(collection)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            let requests = rows("AgentRequest");
            let responses = rows("AgentResponse");
            let messages = rows("AgentMessage");
            let completed = requests
                .iter()
                .any(|row| row["lifecycle_state"] == "completed")
                && responses.iter().any(|row| row["status"] == "complete");
            let response_complete = responses.iter().any(|row| row["status"] == "complete");
            if response_complete {
                response_ready_at.get_or_insert_with(Instant::now);
            }
            let body = messages
                .iter()
                .filter(|row| row["role"] == "assistant")
                .filter_map(|row| row["content"].as_str())
                .map(|content| {
                    gents_protocol::transcript::present_persisted_message("assistant", content)
                        .body_markdown
                })
                .collect::<Vec<_>>()
                .join("\n");
            for (index, (stage, ready)) in [
                (
                    "request_completed",
                    requests
                        .iter()
                        .any(|row| row["lifecycle_state"] == "completed"),
                ),
                ("response_complete", response_complete),
                ("transcript_materialized", body.contains(expected_reply)),
            ]
            .into_iter()
            .enumerate()
            {
                if ready && !stages_seen[index] {
                    stages_seen[index] = true;
                    tracing::debug!(request, stage, documents = ?[&requests, &responses, &messages][index].iter().filter_map(|row| row["_docID"].as_str()).collect::<Vec<_>>(), "replica stage document identities");
                    tracing::info!(
                        request,
                        stage,
                        elapsed_ms = visibility_started.elapsed().as_millis(),
                        "client replica delivery stage"
                    );
                }
            }
            anyhow::ensure!(
                !responses.iter().any(|row| row["status"] == "error"),
                "replicated error response: {responses:?}"
            );
            if completed && body.contains(expected_reply) {
                return Ok::<_, anyhow::Error>((
                    Instant::now(),
                    response_ready_at.expect("completed response timestamp"),
                ));
            }
            sleep(DATABASE_PROBE_INTERVAL).await;
        }
    };
    let observer_visibility = async {
        let mut store_updates = core.store_change_updates();
        loop {
            if core
                .store()
                .snapshot()
                .latest_response_for_request(request)
                .is_some_and(|response| response.status.as_deref() == Some("complete"))
            {
                return Ok::<_, anyhow::Error>(Instant::now());
            }
            store_updates
                .changed()
                .await
                .context("app projection update channel closed")?;
        }
    };

    let ((conversation_database_ready, response_database_ready), observer_ready) =
        tokio::try_join!(database_visibility, observer_visibility)?;
    let observer_minus_response_database_us = if observer_ready >= response_database_ready {
        observer_ready
            .duration_since(response_database_ready)
            .as_micros() as i128
    } else {
        -(response_database_ready
            .duration_since(observer_ready)
            .as_micros() as i128)
    };
    tracing::info!(
        request,
        response_database_visible_ms = response_database_ready
            .duration_since(visibility_started)
            .as_millis(),
        conversation_database_visible_ms = conversation_database_ready
            .duration_since(visibility_started)
            .as_millis(),
        observer_visible_ms = observer_ready
            .duration_since(visibility_started)
            .as_millis(),
        observer_minus_response_database_us,
        database_probe_interval_ms = DATABASE_PROBE_INTERVAL.as_millis(),
        "independently observed local database and app projection visibility",
    );
    anyhow::ensure!(
        observer_ready <= response_database_ready + Duration::from_millis(500),
        "local DB had the completed response more than 500ms before the observer projected it"
    );
    Ok(())
}

async fn pairing_diagnostics(core: &ClientCore, graphql: &str) -> String {
    let query = "{ PeerPairingDesired { peer_id template source } PeerPairingApplied { peer_id } AgentBehaviorReadiness { agent_did updated_at } AgentRequest { _docID request_id agent_did requester_did lifecycle_state } AgentResponse { _docID request_id agent_did requester_did status content } }";
    let client = core.node().execute(query).await;
    let runtime = graphql_query(graphql, query).await;
    let sync = core.sync_state();
    let database_now = timeout(Duration::from_secs(3), core.p2p().sync_status()).await;
    let runtime_database = reqwest::Client::new()
        .get(graphql.replace("/graphql", "/p2p/sync/status"))
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    let runtime_database = match runtime_database {
        Ok(response) => response.json::<serde_json::Value>().await.ok(),
        Err(_) => None,
    };
    let runtime_replicators = reqwest::Client::new()
        .get(graphql.replace("/graphql", "/p2p/replicators"))
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    let runtime_replicators = match runtime_replicators {
        Ok(response) => response.json::<serde_json::Value>().await.ok(),
        Err(_) => None,
    };
    format!(
        "observer={:?}; database_now={database_now:?}; runtime_database={runtime_database:?}; runtime_replicators={runtime_replicators:?}; database={:?}; peers={:?}; client={client:?}; runtime={runtime:?}",
        core.observer_metrics().await, sync.database_sync, sync.peers
    )
}

async fn assert_local_pagination(core: &ClientCore, session: &str, agent: &str) -> Result<()> {
    timeout(TURN_BUDGET, async {
        use gents::agent::p2p_reconcile::session_hydration::ClientHydrationPhase;
        loop {
            match core.session_hydration_progress(session, agent).await?.phase {
                ClientHydrationPhase::Complete => return Ok::<_, anyhow::Error>(()),
                ClientHydrationPhase::Failed => bail!("session hydration failed before pagination"),
                _ => sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .context("session hydration did not settle before pagination")??;
    let intent_query = "{ SessionHydrationRequest { _docID request_key status status_detail served_doc_count served_manifest_json processed_at outcome_signer_did outcome_signature } PeerPairingDesired { _docID peer_id collections profiles template source enrollment_request_digest enrollment_authorization_sequence enrollment_authorization_expires_at updated_at } }";
    let before = core.node().execute(intent_query).await;
    anyhow::ensure!(!before.has_errors(), "local intent query failed");
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut last_sequence = i64::MAX;
    loop {
        let page = gents_desktop_core::client::load_session_transcript_page(
            core.node(),
            session,
            Some(agent),
            Some(core.principal().did()),
            cursor.as_deref(),
            Some(1),
        )
        .await?;
        anyhow::ensure!(
            page.store.messages.len() <= 1,
            "local page exceeded requested message budget"
        );
        for message in &page.store.messages {
            let sequence = message.sequence.context("message sequence")?;
            let key = message.message_key.clone();
            anyhow::ensure!(
                sequence < last_sequence && seen.insert(key.clone()),
                "local pagination repeated or reordered a message"
            );
            last_sequence = sequence;
            cursor = Some(key);
        }
        if page.source_exhausted {
            break;
        }
        anyhow::ensure!(
            !page.store.messages.is_empty(),
            "local pagination made no progress"
        );
    }
    anyhow::ensure!(
        seen.len() >= 6,
        "three conversation turns must survive local paging: {seen:?}"
    );
    let after = core.node().execute(intent_query).await;
    anyhow::ensure!(
        !after.has_errors() && before.data == after.data,
        "scrolling changed pairing or hydration intent"
    );
    Ok(())
}

/// Production Amy had about 2,500 readiness revisions when a clean mobile
/// client enrolled. Seed real signed runtime writes before creating the app;
/// this must not become a dependency on old history for current readiness.
async fn seed_readiness_history(
    graphql: &str,
    agent: &str,
    revisions: usize,
) -> Result<Option<String>> {
    if revisions == 0 {
        return Ok(None);
    }
    let agent = escape_graphql_string(agent);
    let readiness = graphql_query(graphql, &format!(
        r#"{{ AgentBehaviorReadiness(filter: {{agent_did: {{_eq: "{agent}"}}}}) {{snapshot_json}} }}"#,
    )).await?;
    let snapshot = readiness
        .pointer("/data/AgentBehaviorReadiness/0/snapshot_json")
        .and_then(Value::as_str)
        .context("aged fixture requires runtime readiness")?;
    let snapshot = escape_graphql_string(snapshot);
    let started = Instant::now();
    for batch in (0..revisions).step_by(50) {
        let mut fields = String::new();
        for revision in batch..(batch + 50).min(revisions) {
            let timestamp = (Utc::now() - chrono::Duration::seconds((revisions - revision) as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
            fields.push_str(&format!(
                // Match the removed publisher: even an unchanged snapshot was
                // rewritten alongside its timestamp, adding another field DAG.
                r#"r{revision}: update_AgentBehaviorReadiness(filter: {{agent_did: {{_eq: "{agent}"}}}}, input: {{snapshot_json: "{snapshot}", updated_at: "{}"}}) {{_docID}} "#,
                escape_graphql_string(&timestamp),
            ));
        }
        graphql_query(graphql, &format!("mutation {{ {fields} }}")).await?;
    }
    let final_timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    if revisions > 0 {
        graphql_query(graphql, &format!(
            r#"mutation {{ update_AgentBehaviorReadiness(filter: {{agent_did: {{_eq: "{agent}"}}}}, input: {{updated_at: "{}"}}) {{_docID}} }}"#,
            escape_graphql_string(&final_timestamp),
        )).await?;
        tracing::info!(
            revisions,
            elapsed_ms = started.elapsed().as_millis(),
            "aged runtime readiness fixture ready"
        );
    }
    Ok(Some(final_timestamp))
}

async fn wait_for_readiness_revision(core: &ClientCore, agent: &str, expected: &str) -> Result<()> {
    let query = format!(
        r#"{{ AgentBehaviorReadiness(filter: {{agent_did: {{_eq: "{}"}}}}) {{agent_did snapshot_json updated_at}} }}"#,
        escape_graphql_string(agent)
    );
    loop {
        let result = core.node().execute(&query).await;
        anyhow::ensure!(
            !result.has_errors(),
            "readiness convergence query failed: {:?}",
            result.errors
        );
        if let Some(row) = result
            .data
            .as_ref()
            .and_then(|data| data["AgentBehaviorReadiness"].as_array())
            .and_then(|rows| rows.first())
        {
            let row: gents_protocol::row::AgentBehaviorReadinessRow =
                serde_json::from_value(row.clone())?;
            let actual = chrono::DateTime::parse_from_rfc3339(&row.updated_at)?;
            if actual >= chrono::DateTime::parse_from_rfc3339(expected)? {
                anyhow::ensure!(
                    matches!(
                        gents_protocol::row::project_behavior_readiness_summary(
                            Some(&row),
                            agent,
                            Utc::now()
                        ),
                        gents_protocol::row::ProjectedBehaviorReadinessSummary::Observed(_)
                    ),
                    "latest readiness revision is not a usable semantic snapshot"
                );
                return Ok(());
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
}
