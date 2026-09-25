//! Production enrollment and client-visible conversation contract.
//!
//! Only inference is deterministic. The CLI server, signed enrollment,
//! operator approval, Iroh replication, and the iOS app's ClientCore/store
//! are real. This is not a test of UIKit rendering or iOS suspension; those
//! require the device acceptance run. Each deadline includes all work in its
//! phase, rather than allowing each polling helper a fresh latency budget.

use super::*;
use gents::{JsonP2pSyncStatusAdapter, P2pSyncStatusAdapter};
use std::sync::Arc;
use support::mocks::fake_llm::{ChatAction, FakeLlm};
use tokio::time::timeout;

const ENROLL_BUDGET: Duration = Duration::from_secs(30);
/// Readiness reaches a client only over the runtime's push replicator; it has
/// no snapshot or bootstrap path. DefraDB acks a pushed block whose DAG is
/// incomplete as soon as the root is registered pending and leaves recovery
/// to the receiver's per-root backoff ladder, and
/// `p2p::PENDING_RECOVERY_WORST_CASE_SECS` is the pinned dependency's bound
/// from that root's first fetch dispatch to its fifth. A fresh peer walking
/// aged readiness history therefore needs that pacing on top of the ordinary
/// enrollment budget before a timeout means anything is wedged.
const AGED_HISTORY_RECOVERY: Duration = Duration::from_secs(p2p::PENDING_RECOVERY_WORST_CASE_SECS);
const TURN_BUDGET: Duration = Duration::from_secs(20);
const RECONNECT_BUDGET: Duration = Duration::from_secs(20);
const MODEL_DELAY: Duration = Duration::from_secs(2);
const RETURN_TO_OBSERVER_BUDGET: Duration = Duration::from_secs(2);
const READINESS_PROBE_INTERVAL: Duration = Duration::from_secs(3);
/// The sampler runs beside enrollment, so a probe never delays it; a probe that
/// cannot answer within this is reported as such, so one stalled query does
/// not silence the samples after it.
const READINESS_PROBE_BUDGET: Duration = Duration::from_millis(750);
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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "cli_enrollment=info,gents_desktop_core::startup=debug,gents_migration=info".into()
            }),
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
        let enroll_budget = if readiness_timestamp.is_some() {
            ENROLL_BUDGET + AGED_HISTORY_RECOVERY
        } else {
            ENROLL_BUDGET
        };
        let probe = ReadinessProbe::of(&core, &graphql, &agent_did);
        let enrollment_started = Instant::now();
        let phases = std::sync::Mutex::new(Vec::<(&'static str, u128)>::new());
        let reached = |phase: &'static str| {
            phases
                .lock()
                .expect("enrollment phase log")
                .push((phase, enrollment_started.elapsed().as_millis()));
        };
        let sampler = tokio::spawn({
            let probe = probe.clone();
            async move {
                let mut ticks = tokio::time::interval(READINESS_PROBE_INTERVAL);
                loop {
                    ticks.tick().await;
                    let progress = probe.sample(READINESS_PROBE_BUDGET).await;
                    tracing::info!(
                        elapsed_ms = enrollment_started.elapsed().as_millis(),
                        progress = %progress,
                        "readiness replication progress",
                    );
                }
            }
        });
        let enrollment = timeout(enroll_budget, async {
            let (status, _) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}")).await?;
            reached("enrollment_token");
            let pending = core.request_status_enrollment_with_label(&status, Some("Contract")).await?;
            anyhow::ensure!(pending.state == "pending_approval", "unexpected initial enrollment: {pending:?}");
            reached("enrollment_offered");
            wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
            reached("runtime_saw_request");
            run_cli_json(&runtime_home, &["p2p", "enrollment", "approve", &pending.request_id, "--home", home])?;
            reached("operator_approved");
            wait_for_chat_ready_enrollment(&core, &agent_did).await?;
            reached("chat_ready_route");
            wait_for_client_behavior_readiness(&core, &agent_did).await?;
            reached("client_readiness_row");
            if let Some(expected) = readiness_timestamp.as_deref() {
                wait_for_readiness_revision(&core, &agent_did, expected).await?;
                reached("client_readiness_revision");
            }
            anyhow::ensure!(query_collection_dids(core.node(), "AgentPrincipal").await?.is_empty(), "runtime principal replicated to app");
            Ok::<_, anyhow::Error>(())
        }).await;
        sampler.abort();
        // A sample still in flight would query the client node while it shuts
        // down.
        let _ = sampler.await;
        if !matches!(enrollment, Ok(Ok(()))) {
            let progress = probe.sample(Duration::from_secs(5)).await;
            let diagnostics = pairing_diagnostics(&core, &graphql).await;
            core.shutdown().await?;
            bail!(
                "enroll/approve/readiness exceeded {enroll_budget:?} or failed: {enrollment:?}; phases_ms={:?}; {progress}; {diagnostics}",
                phases.lock().expect("enrollment phase log"),
            );
        }
        // The probe holds the client's node; the store stays locked against
        // the reopen below until it is released.
        drop(probe);
        tracing::info!(elapsed_ms = enrollment_started.elapsed().as_millis(), "app enrollment ready");
        assert_observer_did_not_overflow(&core).await?;

        let session = Uuid::new_v4().to_string();
        // Paced tiny frames exercise the one-second production cadence timer;
        // the ordinary first-visible contract retains its stricter 500ms bound.
        let stream_visibility_budget = if paced {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(500)
        };
        visible_turn(
            &core,
            &graphql,
            &agent_did,
            &behavior,
            &session,
            "First conversation turn",
            stream_gate.as_deref(),
            stream_visibility_budget,
        )
        .await?;

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
        wait_for_complete_agent_response(&graphql, &request, &session, TURN_BUDGET).await?;

        let reconnect_started = Instant::now();
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&client_home), ClientCoreOptions::local_only(),
        ).await?;
        let client_core_ready = reconnect_started.elapsed();
        core.set_selected_agent_did(Some(agent_did.clone()));
        let recovered = timeout(RECONNECT_BUDGET.saturating_sub(reconnect_started.elapsed()), async {
            wait_for_chat_ready_enrollment(&core, &agent_did).await?;
            let route_ready = reconnect_started.elapsed();
            wait_for_client_behavior_readiness(&core, &agent_did).await?;
            let behavior_ready = reconnect_started.elapsed();
            wait_for_replicated_reply(&core, &session, &agent_did, &request, OFFLINE_REPLY).await?;
            Ok::<_, anyhow::Error>((route_ready, behavior_ready, reconnect_started.elapsed()))
        }).await;
        let (route_ready, behavior_ready, reply_ready) = match recovered {
            Ok(Ok(phases)) => phases,
            failed => {
                let diagnostics = pairing_diagnostics(&core, &graphql).await;
                core.shutdown().await?;
                bail!("app reopen did not recover completed reply within 20s: {failed:?}; {diagnostics}");
            }
        };
        let reopened = core.peer_records().await;
        anyhow::ensure!(records.len() == reopened.len() && records.iter().zip(&reopened).all(|(old, new)| old.peer_id == new.peer_id && old.enrollment_request_id == new.enrollment_request_id), "reopen changed enrollment identity");
        tracing::info!(
            client_core_start_ms = client_core_ready.as_millis(),
            route_after_core_ms = route_ready.saturating_sub(client_core_ready).as_millis(),
            behavior_after_route_ms = behavior_ready.saturating_sub(route_ready).as_millis(),
            reply_after_behavior_ms = reply_ready.saturating_sub(behavior_ready).as_millis(),
            elapsed_ms = reply_ready.as_millis(),
            "app recovered offline reply",
        );
        visible_turn(
            &core,
            &graphql,
            &agent_did,
            &behavior,
            &session,
            FOLLOWUP_PROMPT,
            followup_stream_gate.as_deref(),
            stream_visibility_budget,
        )
        .await?;
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
    stream_visibility_budget: Duration,
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
                wait_for_complete_agent_response(graphql, &request, session, TURN_BUDGET).await?;
                Ok::<_, anyhow::Error>(started.elapsed())
            },
            async {
                let expected = if prompt == FOLLOWUP_PROMPT { FOLLOWUP_REPLY } else { REPLY };
                if let Some(gate) = stream_gate {
                    let visibility = streaming::wait_for_visible_content(
                        core,
                        &request,
                        expected,
                        stream_visibility_budget,
                    )
                    .await;
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
            AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{created_at claimed_at terminalized_at lifecycle_state terminal_output}}
        }}"#, escape_graphql_string(&request))).await?;
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
        AgentRequest(filter: {{{filter}}}) {{
            _docID request_id session_id agent_did requester_did
            lifecycle_state terminal_output execution_generation failure_reason
        }}
    }}"#
    );
    let visibility_started = Instant::now();
    let database_visibility = async {
        let mut stages_seen = [false; 3];
        let mut terminal_ready_at = None;
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
            let request_completed = requests
                .iter()
                .any(|row| row["lifecycle_state"] == "completed");
            // Terminal selection lives on the request. A completed request
            // without it is an incomplete replica state, not a success.
            let terminal_selection = requests.iter().any(|row| !row["terminal_output"].is_null());
            if terminal_selection {
                terminal_ready_at.get_or_insert_with(Instant::now);
            }
            // The exact physical request row: observe the selected terminal
            // content through the canonical request-output owner against the
            // phone-side node. No inline content reads, no header
            // concatenation, no response document.
            let mut terminal_request_row = None;
            for row in &requests {
                let typed: gents_protocol::row::AgentRequestRow =
                    serde_json::from_value(row.clone())
                        .with_context(|| format!("decoding canonical AgentRequest row: {row}"))?;
                anyhow::ensure!(
                    typed.session_id.as_deref() == Some(session)
                        && typed.agent_did.as_deref() == Some(agent)
                        && typed.requester_did.as_deref() == Some(core.principal().did()),
                    "replica AgentRequest row lost tenancy lineage for {request}: {row}"
                );
                if typed.is_terminal()
                    && typed.terminal_output.is_some()
                    && terminal_request_row.is_none()
                {
                    terminal_request_row = Some(typed);
                }
            }
            let mut body = String::new();
            if let Some(row) = terminal_request_row {
                match gents::session::observe_request_output(
                    &gents::ConfigAccess::Local(core.node_arc()),
                    &row,
                )
                .await
                {
                    Ok(gents::session::CanonicalRequestOutput::TerminalMessage {
                        header,
                        presentation,
                        ..
                    }) => {
                        anyhow::ensure!(
                            header.request_doc_id.as_deref() == row.doc_id.as_deref()
                                && header.session_id == session
                                && header.agent_did == agent
                                && header.requester_did.as_deref()
                                    == Some(core.principal().did()),
                            "canonical terminal header lost tenancy lineage for {request}: header={header:?}"
                        );
                        body = presentation.body_markdown.clone();
                    }
                    Ok(_) => {}
                    Err(error) => bail!(
                        "canonical terminal reconstruction failed on the replica for {request}: {error:#}"
                    ),
                }
            }
            for (index, (stage, ready)) in [
                ("request_completed", request_completed),
                ("terminal_selection", terminal_selection),
                ("terminal_reconstruction", body.contains(expected_reply)),
            ]
            .into_iter()
            .enumerate()
            {
                if ready && !stages_seen[index] {
                    stages_seen[index] = true;
                    tracing::debug!(request, stage, documents = ?requests.iter().filter_map(|row| row["_docID"].as_str()).collect::<Vec<_>>(), "replica stage document identities");
                    tracing::info!(
                        request,
                        stage,
                        elapsed_ms = visibility_started.elapsed().as_millis(),
                        "client replica delivery stage"
                    );
                }
            }
            anyhow::ensure!(
                !requests
                    .iter()
                    .any(|row| row["lifecycle_state"] == "failed"),
                "replicated failed request: {requests:?}"
            );
            if request_completed && terminal_selection && body.contains(expected_reply) {
                return Ok::<_, anyhow::Error>((
                    Instant::now(),
                    terminal_ready_at.expect("terminal selection timestamp"),
                ));
            }
            sleep(DATABASE_PROBE_INTERVAL).await;
        }
    };
    let observer_visibility = async {
        let mut store_updates = core.store_change_updates();
        loop {
            if core.store().snapshot().requests.iter().any(|row| {
                row.request_id == request
                    && row
                        .lifecycle_state
                        .is_some_and(|state| state.as_str() == "completed")
            }) {
                return Ok::<_, anyhow::Error>(Instant::now());
            }
            store_updates
                .changed()
                .await
                .context("app projection update channel closed")?;
        }
    };

    let ((conversation_database_ready, terminal_database_ready), observer_ready) =
        tokio::try_join!(database_visibility, observer_visibility)?;
    let observer_minus_terminal_database_us = if observer_ready >= terminal_database_ready {
        observer_ready
            .duration_since(terminal_database_ready)
            .as_micros() as i128
    } else {
        -(terminal_database_ready
            .duration_since(observer_ready)
            .as_micros() as i128)
    };
    tracing::info!(
        request,
        terminal_database_visible_ms = terminal_database_ready
            .duration_since(visibility_started)
            .as_millis(),
        conversation_database_visible_ms = conversation_database_ready
            .duration_since(visibility_started)
            .as_millis(),
        observer_visible_ms = observer_ready
            .duration_since(visibility_started)
            .as_millis(),
        observer_minus_terminal_database_us,
        database_probe_interval_ms = DATABASE_PROBE_INTERVAL.as_millis(),
        "independently observed local database and app projection visibility",
    );
    anyhow::ensure!(
        observer_ready <= terminal_database_ready + Duration::from_millis(500),
        "local DB had the terminal selection more than 500ms before the observer projected it"
    );
    Ok(())
}

/// DefraDB makes a replicated document queryable only once its whole commit
/// ancestry has transferred and merged, and a fresh peer has no owner index to
/// short-circuit that walk. The readiness row can therefore stay absent on the
/// client while every block is already in flight, so the merged head height
/// against the runtime's — not row presence — separates an undelivered head
/// from historical DAG transfer that has not finished.
///
/// It owns the client handles it reads, so the sampler runs beside enrollment
/// rather than inside it.
#[derive(Clone)]
struct ReadinessProbe {
    node: Arc<gents::defra_node::EmbeddedNode>,
    p2p: Arc<dyn defra_p2p_adapter::P2POperations>,
    store: Arc<gents_desktop_core::client::ObservedStore>,
    graphql: String,
    agent: String,
}

impl ReadinessProbe {
    fn of(core: &ClientCore, graphql: &str, agent: &str) -> Self {
        Self {
            node: core.node_arc(),
            p2p: core.p2p().clone(),
            store: core.store().clone(),
            graphql: graphql.to_owned(),
            agent: agent.to_owned(),
        }
    }

    async fn sample(&self, budget: Duration) -> String {
        let Self {
            node,
            p2p,
            store,
            graphql,
            agent,
        } = self;
        let probe = async {
            let escaped = escape_graphql_string(agent);
            let runtime_row = graphql_query(
                graphql,
                &format!(
                    r#"{{ AgentBehaviorReadiness(filter: {{agent_did: {{_eq: "{escaped}"}}}}) {{_docID updated_at}} }}"#
                ),
            )
            .await;
            let Some(doc_id) = runtime_row
                .as_ref()
                .ok()
                .and_then(|row| row.pointer("/data/AgentBehaviorReadiness/0/_docID"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
            else {
                return format!("readiness_progress=runtime_row_absent({runtime_row:?})");
            };
            let heads = format!(
                r#"{{ _commits(docID: "{}", depth: 1) {{height fieldName}} }}"#,
                escape_graphql_string(&doc_id)
            );
            let runtime_height = graphql_query(graphql, &heads)
                .await
                .ok()
                .and_then(|response| highest_commit(response.pointer("/data/_commits")));
            let client_heads = node.execute(&heads).await;
            let client_height = client_heads
                .data
                .as_ref()
                .and_then(|data| highest_commit(data.get("_commits")));
            let client_rows = store
                .snapshot()
                .behavior_readiness
                .iter()
                .filter(|row| &row.agent_did == agent)
                .count();
            let sync = match p2p.sync_status().await {
                Ok(status) => match JsonP2pSyncStatusAdapter.adapt(&status) {
                    Ok(status) => format!(
                        "pending_dags={} persisted_pending_dags={} quarantined_dags={} fetch_exhausted={} fetch_deferred_unavailable={} provider_rotations={} missing_link_retries={} car_requested_cids={} car_present_cids={} next_retry_ms={:?}",
                        status.pending_dags,
                        status.persisted_pending_dags,
                        status.quarantined_pending_dags,
                        status.pending_dag_fetch_exhausted,
                        status.pending_dag_fetch_deferred_unavailable,
                        status.provider_rotations,
                        status.missing_link_retries,
                        status.car_requested_cids,
                        status.car_present_cids,
                        status.next_pending_retry_in_ms,
                    ),
                    Err(error) => format!("undecodable({error})"),
                },
                Err(error) => format!("unavailable({error})"),
            };
            format!(
                "readiness_progress=doc_id={doc_id} runtime_head_height={runtime_height:?} client_head_height={client_height:?} client_projected_rows={client_rows} client_commits_errors={:?}; client_sync={sync}",
                client_heads.errors,
            )
        };
        match timeout(budget, probe).await {
            Ok(progress) => progress,
            Err(_) => format!("readiness_progress=probe exceeded {budget:?}"),
        }
    }
}

fn highest_commit(commits: Option<&Value>) -> Option<i64> {
    commits?
        .as_array()?
        .iter()
        .filter_map(|commit| commit.get("height").and_then(Value::as_i64))
        .max()
}

async fn pairing_diagnostics(core: &ClientCore, graphql: &str) -> String {
    let query = "{ PeerPairingDesired { peer_id template source } PeerPairingApplied { peer_id } AgentBehaviorReadiness { agent_did updated_at } AgentRequest { _docID request_id agent_did requester_did lifecycle_state terminal_output } }";
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
            page.store.transcript_messages.len() <= 1,
            "local page exceeded requested message budget"
        );
        for message in &page.store.transcript_messages {
            let sequence = i64::from(message.message.sequence);
            let key = message.message.message_key.clone();
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
            !page.store.transcript_messages.is_empty(),
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
/// client enrolled. Seed real signed runtime writes before creating the app.
///
/// A fresh peer cannot read the current revision before the whole ancestry
/// transfers and merges: DefraDB resolves a replicated composite's document
/// identity by walking parents to genesis, and it materializes nothing until
/// that closure is complete. The enrollment budget therefore covers the whole
/// history, and the revision count must stay below DefraDB's merge-depth
/// limit, past which the walk fails terminally and the root is quarantined
/// rather than retried.
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
