use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use defra_agent_protocol::client_protocol::ClientTurnState;

use super::*;

#[test]
#[ignore = "hits the fixed MiniMax live backend and runs a long three-agent soak"]
fn desktop_live_three_agent_multi_turn_soak() -> Result<()> {
    run_named_scripted_graphql_soak(
        "desktop-live-soak",
        &["alpha", "bravo", "charlie"],
        "desktop-live-three-agent",
    )
}

#[test]
#[ignore = "hits the fixed MiniMax live backend and runs a focused single-agent soak"]
fn desktop_live_single_agent_scripted_turns_smoke() -> Result<()> {
    run_named_scripted_graphql_soak(
        "desktop-live-soak-single",
        &["alpha"],
        "desktop-live-single-agent",
    )
}

#[test]
#[ignore = "hits the fixed MiniMax live backend and runs a focused two-agent soak"]
fn desktop_live_two_agent_scripted_turns_smoke() -> Result<()> {
    run_named_scripted_graphql_soak(
        "desktop-live-soak-two",
        &["alpha", "bravo"],
        "desktop-live-two-agent",
    )
}

fn run_named_scripted_graphql_soak(
    fixture_name: &str,
    deployment_names: &[&str],
    artifact_name: &str,
) -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let backend = explicit_soak_backend();
    let fixture = build_named_multi_agent_desktop_fixture_with_backend(
        fixture_name,
        deployment_names,
        &backend,
        global_log_store(),
    )?;
    run_scripted_graphql_soak(fixture, artifact_name)
}

fn run_scripted_graphql_soak(
    fixture: MultiAgentLiveDesktopFixture,
    artifact_name: &str,
) -> Result<()> {
    let config = LiveSoakConfig::from_env(artifact_name)?;
    let mut fixture = Some(fixture);
    let fixture_ref = fixture
        .as_ref()
        .expect("fixture should be present while soak is running");
    assert!(
        !fixture_ref.deployments.is_empty(),
        "expected at least one live deployment in scripted soak"
    );

    let desktop_client = Arc::clone(
        fixture_ref
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    let diagnostics_dir = config.output_dir.clone();
    let mut diagnostics = LiveSoakDiagnostics::new(&diagnostics_dir)?;
    diagnostics.write_metadata(&fixture_ref.deployments, &fixture_ref.backend)?;
    diagnostics.write_log_snapshot(global_log_store().as_ref())?;
    diagnostics.scrape_runtime_metrics(&fixture_ref.runtime_apis)?;

    let tool_roots: BTreeSet<_> = fixture
        .as_ref()
        .expect("fixture present during soak")
        .deployments
        .iter()
        .map(|deployment| deployment.running_agent.tool_root.display().to_string())
        .collect();
    assert_eq!(
        tool_roots.len(),
        fixture_ref.deployments.len(),
        "expected one isolated tool root per live deployment"
    );

    for deployment in &fixture_ref.deployments {
        wait_for_stable_runtime_ready(
            fixture_ref.runtime.as_ref(),
            deployment.core.as_ref(),
            &deployment.label,
            &deployment.agent_did,
            Duration::from_secs(2),
            Duration::from_secs(60),
        )?;
        wait_for_stable_runtime_ready(
            fixture_ref.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("desktop mirror for {}", deployment.label),
            &deployment.agent_did,
            Duration::from_secs(2),
            Duration::from_secs(60),
        )?;
    }
    diagnostics.record_snapshot(
        fixture_ref.runtime.as_ref(),
        &fixture_ref.driver,
        &fixture_ref.deployments,
    )?;
    diagnostics.write_log_snapshot(global_log_store().as_ref())?;
    diagnostics.scrape_runtime_metrics(&fixture_ref.runtime_apis)?;

    let deployments: Vec<_> = fixture
        .as_ref()
        .expect("fixture present during soak")
        .deployments
        .iter()
        .map(ParallelDeploymentCase::from_live_deployment)
        .collect();
    let scripted_turns = soak_repo_investigation_turns();
    let run_results = match std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for deployment in &deployments {
            let deployment = deployment.clone();
            let runtime = Arc::clone(&fixture_ref.runtime);
            let desktop_client = Arc::clone(&desktop_client);
            let desktop_graphql_url = fixture_ref.desktop_api.graphql_url().to_string();
            let diagnostics_dir = diagnostics.output_dir().to_path_buf();
            workers.push(scope.spawn(move || {
                run_parallel_deployment_soak(
                    runtime,
                    desktop_client,
                    &desktop_graphql_url,
                    &diagnostics_dir,
                    deployment,
                    scripted_turns,
                )
            }));
        }

        workers
            .into_iter()
            .map(|worker| worker.join().expect("parallel soak worker panicked"))
            .collect::<Result<Vec<_>>>()
    }) {
        Ok(results) => results,
        Err(error) => {
            let _ = diagnostics.record_snapshot(
                fixture_ref.runtime.as_ref(),
                &fixture_ref.driver,
                &fixture_ref.deployments,
            );
            let _ = diagnostics.write_log_snapshot(global_log_store().as_ref());
            let _ = diagnostics.scrape_runtime_metrics(&fixture_ref.runtime_apis);
            let _ = diagnostics.capture_workspace(fixture_ref._tempdir.path());
            let recent_logs = soak_recent_problems(0);
            let _ = diagnostics.record_problem("parallel_turn", &error, &recent_logs);
            let shutdown_result = fixture
                .take()
                .expect("fixture should still be present on soak error")
                .shutdown();
            let error = error.context(format!(
                "soak diagnostics written to {}",
                diagnostics.output_dir().display()
            ));
            return match shutdown_result {
                Ok(()) => Err(error),
                Err(shutdown_error) => Err(error.context(format!(
                    "fixture shutdown after soak error also failed: {shutdown_error:#}"
                ))),
            };
        }
    };

    let mut session_by_peer = BTreeMap::new();
    let mut prompts_by_peer: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut completed_turns = Vec::new();
    for result in &run_results {
        session_by_peer.insert(result.deployment.peer_id.clone(), result.session_id.clone());
        prompts_by_peer.insert(
            result.deployment.peer_id.clone(),
            result
                .turns
                .iter()
                .map(|turn| turn.prompt.clone())
                .collect::<Vec<_>>(),
        );
        for turn in &result.turns {
            completed_turns.push((
                turn.turn,
                result.deployment.label.clone(),
                &result.deployment,
                &turn.submission,
            ));
        }
    }
    completed_turns.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (turn, _, deployment, submission) in completed_turns {
        let deployment_case = deployment.as_case();
        diagnostics.record_turn(
            fixture_ref.runtime.as_ref(),
            &fixture_ref.driver,
            &fixture_ref.deployments,
            &deployment_case,
            turn,
            submission,
        )?;
        diagnostics.write_log_snapshot(global_log_store().as_ref())?;
        diagnostics.scrape_runtime_metrics(&fixture_ref.runtime_apis)?;
    }

    for (index, deployment) in deployments.iter().enumerate() {
        wait_for_session_tool_activity(
            fixture_ref.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("desktop {} session tool activity", deployment.label),
            session_by_peer
                .get(&deployment.peer_id)
                .ok_or_else(|| anyhow!("missing soak session for {}", deployment.label))?,
            0,
            1,
            &[],
        )?;
        wait_for_session_tool_activity(
            fixture_ref.runtime.as_ref(),
            deployment.remote_core.as_ref(),
            &format!("remote {} session tool activity", deployment.label),
            session_by_peer
                .get(&deployment.peer_id)
                .ok_or_else(|| anyhow!("missing soak session for {}", deployment.label))?,
            0,
            1,
            &[],
        )?;
        fixture_ref.runtime.block_on(desktop_client.refresh_store())?;
        let desktop_snapshot = desktop_client.store().snapshot();
        let session_id = session_by_peer
            .get(&deployment.peer_id)
            .ok_or_else(|| anyhow!("missing soak session for {}", deployment.label))?;

        let own_prompts = prompts_by_peer
            .get(&deployment.peer_id)
            .ok_or_else(|| anyhow!("missing prompts for {}", deployment.label))?;
        let session_requests = desktop_snapshot.requests_for_session(session_id);
        let session_responses: Vec<_> = desktop_snapshot
            .responses
            .iter()
            .filter(|row| row.session_id.as_deref() == Some(session_id.as_str()))
            .collect();
        let session_request_contents: Vec<_> = session_requests
            .iter()
            .filter_map(|row| row.content.as_deref())
            .collect();

        if session_requests.len() != own_prompts.len() {
            anyhow::bail!(
                "expected {} soak requests persisted for {}, found {} in session {}. request_contents={:?}",
                own_prompts.len(),
                deployment.label,
                session_requests.len(),
                session_id,
                session_request_contents
            );
        }
        if session_responses.len() < own_prompts.len() {
            anyhow::bail!(
                "expected at least {} soak responses persisted for {}, found {} in session {}",
                own_prompts.len(),
                deployment.label,
                session_responses.len(),
                session_id
            );
        }
        if !session_request_contents
            .iter()
            .any(|content| content.contains(&own_prompts[0]))
        {
            anyhow::bail!(
                "expected first soak prompt to persist in session {} for {}. request_contents={:?}",
                session_id,
                deployment.label,
                session_request_contents
            );
        }
        let last_prompt = own_prompts
            .last()
            .expect("deployment soak prompts are non-empty");
        if !session_request_contents
            .iter()
            .any(|content| content.contains(last_prompt))
        {
            anyhow::bail!(
                "expected last soak prompt to persist in session {} for {}. request_contents={:?}",
                session_id,
                deployment.label,
                session_request_contents
            );
        }
        if deployments.len() > 1 {
            let other = &deployments[(index + 1) % deployments.len()];
            let other_prompt = prompts_by_peer
                .get(&other.peer_id)
                .and_then(|prompts| prompts.first())
                .ok_or_else(|| anyhow!("missing comparison prompt for {}", other.label))?;
            if session_request_contents
                .iter()
                .any(|content| content.contains(other_prompt))
            {
                anyhow::bail!(
                    "persisted transcript for {} leaked prompt from {}. session={} request_contents={:?}",
                    deployment.label,
                    other.label,
                    session_id,
                    session_request_contents
                );
            }
        }
    }

    tracing::info!(
        artifact_name,
        diagnostics_dir = %diagnostics_dir.display(),
        "desktop live scripted soak completed"
    );
    if fixture_ref.deployments.len() > 1 {
        if let Err(error) = wait_for_post_completion_p2p_quiet(
            fixture_ref.runtime.as_ref(),
            &fixture_ref.driver,
            &fixture_ref.deployments,
            Duration::from_secs(2),
            Duration::from_secs(10),
        ) {
            tracing::warn!(
                diagnostics_dir = %diagnostics_dir.display(),
                error = %error,
                "live soak observed continued P2P activity after completion"
            );
        }
    }
    diagnostics.record_snapshot(
        fixture_ref.runtime.as_ref(),
        &fixture_ref.driver,
        &fixture_ref.deployments,
    )?;
    diagnostics.write_log_snapshot(global_log_store().as_ref())?;
    diagnostics.scrape_runtime_metrics(&fixture_ref.runtime_apis)?;
    if config.keep_workspace {
        diagnostics.capture_workspace(fixture_ref._tempdir.path())?;
    }
    fixture
        .take()
        .expect("fixture should be present on soak success")
        .shutdown()
}

struct SoakPromptTemplate {
    body: &'static str,
    starting_paths: &'static [&'static str],
}

impl SoakPromptTemplate {
    fn render(&self, deployment: &LiveDeploymentCase<'_>, turn: usize) -> String {
        let starting_paths = self
            .starting_paths
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "You are running a desktop live soak investigation for {deployment_label}.\n\
Use the repo copy under ./workspace and cite the file paths you inspected.\n\
Do not answer from memory; read files before answering.\n\
Use the file tools (`list_files` and `read_file`) to explore the workspace.\n\
Do not use the bash tool for repository exploration, directory listing, or file reading.\n\
When using `read_file`, call it with exactly one `path` at a time, not an array of files.\n\
Keep the final answer very concise: at most 4 short bullets and under 120 words total.\n\
Stop immediately after the final bullet. Do not add an intro, conclusion, or extra explanation.\n\
Do not quote large code blocks or paste long file excerpts.\n\
Summarize findings in your own words and cite the files you inspected.\n\
Start from these known paths in the seeded workspace:\n\
{starting_paths}\n\
Prefer those exact files/directories before exploring anything else.\n\
Turn {turn}: {body}",
            deployment_label = deployment.label,
            starting_paths = starting_paths,
            body = self.body,
        )
    }
}

fn soak_repo_investigation_turns() -> &'static [SoakPromptTemplate] {
    &[
        SoakPromptTemplate {
            body: "Please summarize how the desktop app and defra-agent communicate over P2P in this repository. Cite the files you used and keep the answer focused on the actual code paths.",
            starting_paths: &[
                "./workspace/crates/defra-agent-desktop/src/app/tests/live/chat_soak.rs",
                "./workspace/crates/defra-agent-desktop/src/app/tests/support/live_fixture/builders.rs",
                "./workspace/crates/defra-agent/src/watcher.rs",
                "./workspace/crates/defra-agent/src/watcher/query.rs",
                "./workspace/crates/defra-agent-protocol/src/client_protocol.rs",
            ],
        },
        SoakPromptTemplate {
            body: "Now explain which identities are involved in that exchange and how they affect authorization, routing, or trust. Build on your previous answer and cite the files you used.",
            starting_paths: &[
                "./workspace/crates/defra-agent/src/identity.rs",
                "./workspace/crates/defra-agent/src/document_config/principal.rs",
                "./workspace/crates/defra-agent/src/lifecycle/claim.rs",
                "./workspace/crates/defra-agent/src/agent/runtime/context.rs",
                "./workspace/crates/defra-agent/src/agent/runtime/router.rs",
            ],
        },
        SoakPromptTemplate {
            body: "Now identify the most likely failure points in that desktop-to-agent P2P flow and where you would instrument it for debugging. Cite the files you used.",
            starting_paths: &[
                "./workspace/crates/defra-agent/src/watcher.rs",
                "./workspace/crates/defra-agent/src/watcher/query.rs",
                "./workspace/crates/defra-agent/src/lifecycle/claim.rs",
                "./workspace/crates/defra-agent-desktop/src/app/tests/live/common/submissions.rs",
                "./workspace/docs/protocols/client-state-machine.md",
            ],
        },
    ]
}

fn submit_soak_prompt_for_deployment(
    desktop_graphql_url: &str,
    deployment: &LiveDeploymentCase<'_>,
    expected_session_id: Option<&str>,
    prompt: &str,
    diagnostics_dir: &Path,
) -> Result<LiveSubmissionCase> {
    let log_baseline = global_log_store()
        .snapshot()
        .entries
        .last()
        .map(|entry| entry.id)
        .unwrap_or_default();
    let submitted = create_live_agent_request_via_graphql(
        desktop_graphql_url,
        &deployment.agent_did,
        prompt,
        expected_session_id,
        Some(&deployment.docs.behavior_id),
    )
    .with_context(|| format!("submit_request failed for {}", deployment.label))?;

    let (effective_request_id, response_text) = wait_for_soak_response_text(
        desktop_graphql_url,
        deployment,
        &submitted.request_id,
        &submitted.session_id,
        log_baseline,
        diagnostics_dir,
    )?;

    Ok(LiveSubmissionCase {
        prompt: prompt.to_string(),
        request_id: submitted.request_id,
        effective_request_id,
        response: response_text,
        session_id: submitted.session_id,
    })
}

#[derive(Clone)]
struct ParallelDeploymentCase {
    label: String,
    peer_id: String,
    agent_did: String,
    docs: LiveAgentDocs,
    remote_core: Arc<ClientCore>,
}

impl ParallelDeploymentCase {
    fn from_live_deployment(deployment: &LiveRemoteDeployment) -> Self {
        Self {
            label: deployment.label.clone(),
            peer_id: deployment.peer_id.clone(),
            agent_did: deployment.agent_did.clone(),
            docs: LiveAgentDocs {
                behavior_id: deployment.docs.behavior_id.clone(),
                backend_id: deployment.docs.backend_id.clone(),
                tool_selection_id: deployment.docs.tool_selection_id.clone(),
                inference_profile_id: deployment.docs.inference_profile_id.clone(),
                scheduled_task_id: deployment.docs.scheduled_task_id.clone(),
            },
            remote_core: Arc::clone(&deployment.core),
        }
    }

    fn as_case(&self) -> LiveDeploymentCase<'_> {
        LiveDeploymentCase {
            label: self.label.clone(),
            peer_id: self.peer_id.clone(),
            agent_did: self.agent_did.clone(),
            docs: LiveAgentDocs {
                behavior_id: self.docs.behavior_id.clone(),
                backend_id: self.docs.backend_id.clone(),
                tool_selection_id: self.docs.tool_selection_id.clone(),
                inference_profile_id: self.docs.inference_profile_id.clone(),
                scheduled_task_id: self.docs.scheduled_task_id.clone(),
            },
            remote_core: self.remote_core.as_ref(),
        }
    }
}

struct ParallelDeploymentTurn {
    turn: usize,
    prompt: String,
    submission: LiveSubmissionCase,
}

struct ParallelDeploymentRun {
    deployment: ParallelDeploymentCase,
    session_id: String,
    turns: Vec<ParallelDeploymentTurn>,
}

fn run_parallel_deployment_soak(
    runtime: Arc<Runtime>,
    desktop_client: Arc<ClientCore>,
    desktop_graphql_url: &str,
    diagnostics_dir: &Path,
    deployment: ParallelDeploymentCase,
    scripted_turns: &[SoakPromptTemplate],
) -> Result<ParallelDeploymentRun> {
    let deployment_case = deployment.as_case();
    let mut session_id: Option<String> = None;
    let mut turns = Vec::new();

    for (turn_index, prompt_template) in scripted_turns.iter().enumerate() {
        let turn = turn_index + 1;
        let prompt = prompt_template.render(&deployment_case, turn);
        let submission = submit_soak_prompt_for_deployment(
            desktop_graphql_url,
            &deployment_case,
            session_id.as_deref(),
            &prompt,
            diagnostics_dir,
        )
        .with_context(|| {
            format!(
                "parallel soak submit failed for {} turn {}",
                deployment.label, turn
            )
        })?;

        assert_live_submission_rows_with_options(
            runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("desktop {} turn {turn}", deployment.label),
            &deployment_case,
            &submission,
            None,
            SubmissionRowAssertOptions {
                timeout: Duration::from_secs(45),
                require_response_content_match: true,
            },
        )?;
        assert_live_submission_rows_with_options(
            runtime.as_ref(),
            deployment.remote_core.as_ref(),
            &format!("remote {} turn {turn}", deployment.label),
            &deployment_case,
            &submission,
            None,
            SubmissionRowAssertOptions {
                timeout: Duration::from_secs(75),
                require_response_content_match: false,
            },
        )?;
        wait_for_session_settled(
            runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("desktop {} turn {turn} settled", deployment.label),
            &submission.session_id,
            &submission.effective_request_id,
        )?;
        wait_for_session_settled(
            runtime.as_ref(),
            deployment.remote_core.as_ref(),
            &format!("remote {} turn {turn} settled", deployment.label),
            &submission.session_id,
            &submission.effective_request_id,
        )?;

        if let Some(existing_session_id) = &session_id {
            assert_eq!(
                existing_session_id, &submission.session_id,
                "expected soak to stay in one conversation per deployment for {}",
                deployment.label
            );
        } else {
            session_id = Some(submission.session_id.clone());
        }

        turns.push(ParallelDeploymentTurn {
            turn,
            prompt,
            submission,
        });
    }

    Ok(ParallelDeploymentRun {
        deployment,
        session_id: session_id.expect("parallel soak should create a session"),
        turns,
    })
}

fn wait_for_soak_response_text(
    desktop_graphql_url: &str,
    deployment: &LiveDeploymentCase<'_>,
    request_id: &str,
    session_id: &str,
    log_baseline: u64,
    diagnostics_dir: &Path,
) -> Result<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let started = Instant::now();
    let mut current_request_id = request_id.to_string();
    let mut visited = std::collections::BTreeSet::from([current_request_id.clone()]);
    let mut polls = 0usize;
    let mut last_logged_signature: Option<String> = None;
    loop {
        if let Some(problem) = soak_p2p_problem_since(log_baseline) {
            anyhow::bail!(
                "observed P2P failure while waiting for soak response for {} request {}: {} (diagnostics: {})",
                deployment.label,
                request_id,
                problem,
                diagnostics_dir.display()
            );
        }

        let state = fetch_graphql_turn_state(desktop_graphql_url, &current_request_id)
            .with_context(|| {
                format!(
                    "fetching soak turn state for {} request {}",
                    deployment.label, current_request_id
                )
            })?;
        polls += 1;
        let log_signature = format!(
            "{}|{}|{}|{}",
            current_request_id,
            state
                .request
                .as_ref()
                .and_then(|row| row.lifecycle_state.as_deref())
                .unwrap_or_default(),
            state
                .response
                .as_ref()
                .and_then(|row| row.status.as_deref())
                .unwrap_or_default(),
            state
                .derived_turn_state()
                .map(|turn_state| format!("{turn_state:?}"))
                .unwrap_or_default()
        );
        let should_log = polls == 1
            || polls % 100 == 0
            || last_logged_signature.as_deref() != Some(log_signature.as_str());
        if should_log {
            last_logged_signature = Some(log_signature);
            persist_turn_wait_snapshot(
                diagnostics_dir,
                deployment,
                request_id,
                &current_request_id,
                polls,
                &state,
            )?;
            tracing::info!(
                request_id = %request_id,
                current_request_id = %current_request_id,
                polls,
                elapsed_ms = started.elapsed().as_millis() as u64,
                turn_state = ?state.derived_turn_state(),
                lifecycle_state = state.request.as_ref().and_then(|row| row.lifecycle_state.as_deref()).unwrap_or_default(),
                response_status = state.response.as_ref().and_then(|row| row.status.as_deref()).unwrap_or_default(),
                response_preview = state
                    .response
                    .as_ref()
                    .and_then(|row| row.content.as_deref())
                    .map(compact_response_preview)
                    .unwrap_or_default(),
                "waiting for soak response"
            );
        }

        match state.derived_turn_state() {
            Some(ClientTurnState::Completed) => {
                if !state.response_is_durably_complete() {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                let response = state.response.as_ref().ok_or_else(|| {
                    anyhow!(
                        "soak request {} for {} derived Completed without AgentResponse row",
                        current_request_id,
                        deployment.label
                    )
                })?;
                let content = response.content.as_deref().unwrap_or_default().trim();
                if !content.is_empty() && !response_is_tool_call_only(content) {
                    persist_session_shape_snapshot(
                        desktop_graphql_url,
                        diagnostics_dir,
                        deployment,
                        session_id,
                        request_id,
                        &current_request_id,
                        "completed",
                    )?;
                    return Ok((current_request_id.clone(), content.to_string()));
                }
                if let Some(next_request_id) = state.successor_request_id() {
                    if visited.insert(next_request_id.clone()) {
                        tracing::info!(
                            request_id = %request_id,
                            current_request_id = %current_request_id,
                            next_request_id = %next_request_id,
                            "following completed request chain to successor"
                        );
                        current_request_id = next_request_id;
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                }
            }
            Some(ClientTurnState::Superseded) => {
                if let Some(next_request_id) = state.successor_request_id() {
                    if visited.insert(next_request_id.clone()) {
                        tracing::info!(
                            request_id = %request_id,
                            current_request_id = %current_request_id,
                            next_request_id = %next_request_id,
                            "following superseded request chain to successor"
                        );
                        current_request_id = next_request_id;
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                }
                anyhow::bail!(
                    "request {} for {} reached superseded turn state without a successor: request={:?} response={:?} (diagnostics: {})",
                    current_request_id,
                    deployment.label,
                    state.request,
                    state.response,
                    diagnostics_dir.display()
                );
            }
            Some(ClientTurnState::Failed) => {
                anyhow::bail!(
                    "soak request {} for {} reached failed turn state: request={:?} response={:?} (diagnostics: {})",
                    current_request_id,
                    deployment.label,
                    state.request,
                    state.response,
                    diagnostics_dir.display()
                );
            }
            Some(ClientTurnState::WaitingForClaim | ClientTurnState::Streaming) | None => {}
        }

        if Instant::now() >= deadline {
            let _ = persist_session_shape_snapshot(
                desktop_graphql_url,
                diagnostics_dir,
                deployment,
                session_id,
                request_id,
                &current_request_id,
                "timeout",
            );
            anyhow::bail!(
                "timed out waiting for soak response for {} request {}: current_request_id={} turn_state={:?} request={:?} response={:?} (diagnostics: {})",
                deployment.label,
                request_id,
                current_request_id,
                state.derived_turn_state(),
                state.request,
                state.response,
                diagnostics_dir.display()
            );
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn response_is_tool_call_only(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("[TOOL_CALL]") && trimmed.ends_with("[/TOOL_CALL]")
}

fn compact_response_preview(content: &str) -> String {
    const MAX_LEN: usize = 160;
    let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_LEN {
        single_line
    } else {
        let truncated = single_line.chars().take(MAX_LEN).collect::<String>();
        format!("{truncated}...")
    }
}

fn persist_turn_wait_snapshot(
    diagnostics_dir: &Path,
    deployment: &LiveDeploymentCase<'_>,
    root_request_id: &str,
    current_request_id: &str,
    polls: usize,
    state: &GraphqlTurnState,
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct WaitSnapshot<'a> {
        timestamp: String,
        deployment: &'a str,
        root_request_id: &'a str,
        current_request_id: &'a str,
        polls: usize,
        turn_state: Option<String>,
        state: &'a GraphqlTurnState,
    }

    let snapshots_dir = diagnostics_dir.join("store-snapshots");
    std::fs::create_dir_all(&snapshots_dir)
        .with_context(|| format!("creating {}", snapshots_dir.display()))?;
    let path = snapshots_dir.join(format!(
        "wait-{}-{}-{:04}.json",
        soak_filename_component(&deployment.label),
        soak_filename_component(root_request_id),
        polls
    ));
    let snapshot = WaitSnapshot {
        timestamp: chrono::Utc::now().to_rfc3339(),
        deployment: &deployment.label,
        root_request_id,
        current_request_id,
        polls,
        turn_state: state.derived_turn_state().map(|state| format!("{state:?}")),
        state,
    };
    let bytes = serde_json::to_vec_pretty(&snapshot)?;
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn persist_session_shape_snapshot(
    graphql_url: &str,
    diagnostics_dir: &Path,
    deployment: &LiveDeploymentCase<'_>,
    session_id: &str,
    root_request_id: &str,
    current_request_id: &str,
    phase: &str,
) -> Result<()> {
    let snapshots_dir = diagnostics_dir.join("store-snapshots");
    std::fs::create_dir_all(&snapshots_dir)
        .with_context(|| format!("creating {}", snapshots_dir.display()))?;
    let path = snapshots_dir.join(format!(
        "session-{}-{}-{}-{}.json",
        soak_filename_component(&deployment.label),
        soak_filename_component(root_request_id),
        soak_filename_component(current_request_id),
        soak_filename_component(phase)
    ));
    let snapshot = fetch_graphql_session_shape(graphql_url, session_id, current_request_id)?;
    let bytes = serde_json::to_vec_pretty(&snapshot)?;
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn soak_filename_component(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect()
}

fn soak_p2p_problem_since(log_baseline: u64) -> Option<String> {
    soak_recent_fatal_problems(log_baseline)
        .into_iter()
        .next()
        .map(|problem| {
            let diagnostics = summarize_p2p_logs(std::slice::from_ref(&problem));
            let mut details = Vec::new();
            if !diagnostics.peer_ids.is_empty() {
                details.push(format!("peer_ids={}", diagnostics.peer_ids.join(",")));
            }
            if !diagnostics.collection_ids.is_empty() {
                details.push(format!(
                    "collection_ids={}",
                    diagnostics.collection_ids.join(",")
                ));
            }
            if details.is_empty() {
                problem.summary_line()
            } else {
                format!("{} [{}]", problem.summary_line(), details.join(" "))
            }
        })
}

fn soak_recent_fatal_problems(log_baseline: u64) -> Vec<SoakLogRecord> {
    global_log_store()
        .snapshot()
        .entries
        .into_iter()
        .filter(|entry| entry.id > log_baseline)
        .filter_map(|entry| {
            let message = entry.message.to_lowercase();
            let interesting = message.contains("not a replicator")
                || message.contains("access denied")
                || message.contains("rate limited")
                || message.contains("bitswap fetch failed")
                || message.contains("pushlog to replicator was rejected");
            interesting.then(|| soak_log_record(entry))
        })
        .take(32)
        .collect()
}

fn soak_recent_problems(log_baseline: u64) -> Vec<SoakLogRecord> {
    global_log_store()
        .snapshot()
        .entries
        .into_iter()
        .filter(|entry| entry.id > log_baseline)
        .filter_map(|entry| {
            let message = entry.message.to_lowercase();
            let target = entry.target.to_lowercase();
            let interesting = message.contains("not a replicator")
                || message.contains("access denied")
                || message.contains("rate limited")
                || message.contains("bitswap fetch failed")
                || message.contains("push to replicator failed")
                || message.contains("pushlog to replicator was rejected")
                || message.contains("endpoint dropped without calling")
                || message.contains("failed to send two-stream response")
                || target.contains("p2p");
            interesting.then(|| soak_log_record(entry))
        })
        .take(32)
        .collect()
}

fn wait_for_post_completion_p2p_quiet(
    runtime: &Runtime,
    driver: &AuditDriver,
    deployments: &[LiveRemoteDeployment],
    quiet_for: Duration,
    timeout: Duration,
) -> Result<()> {
    let started = Instant::now();
    let mut quiet_started = Instant::now();
    let mut cursor = global_log_store()
        .snapshot()
        .entries
        .last()
        .map(|entry| entry.id)
        .unwrap_or_default();

    loop {
        if started.elapsed() >= timeout {
            let recent = soak_recent_problems(cursor.saturating_sub(512));
            anyhow::bail!(
                "timed out waiting for post-completion P2P quiet; recent={:?}",
                recent
                    .iter()
                    .map(SoakLogRecord::summary_line)
                    .collect::<Vec<_>>()
            );
        }

        if let Some(client) = driver.app.client.as_ref() {
            runtime.block_on(client.refresh_store())?;
        }
        for deployment in deployments {
            runtime.block_on(deployment.core.refresh_store())?;
        }

        let new_problems = soak_recent_problems(cursor);
        if let Some(last) = new_problems.last() {
            cursor = last.id;
            quiet_started = Instant::now();
        } else if quiet_started.elapsed() >= quiet_for {
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_session_settled(
    runtime: &Runtime,
    core: &ClientCore,
    label: &str,
    session_id: &str,
    effective_request_id: &str,
) -> Result<()> {
    wait_for_value(label, Duration::from_secs(15), || {
        runtime.block_on(core.refresh_store()).ok()?;
        let snapshot = core.store().snapshot();
        let latest_request_id = snapshot.latest_request_id_for_session(session_id)?;
        let turn_state = snapshot.derive_turn(session_id)?;
        let active_status_requests = snapshot
            .requests_for_session(session_id)
            .into_iter()
            .filter(|row| matches!(row.status.as_deref(), Some("pending" | "processing")))
            .count();
        (latest_request_id == effective_request_id
            && matches!(
                turn_state,
                ClientTurnState::Completed | ClientTurnState::Failed | ClientTurnState::Superseded
            )
            && active_status_requests == 0)
            .then_some(())
    })
}
