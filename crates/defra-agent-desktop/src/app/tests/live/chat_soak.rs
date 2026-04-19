use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::*;

#[test]
#[ignore = "hits the fixed MiniMax live backend and runs a long three-agent soak"]
fn desktop_live_three_agent_multi_turn_soak() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let config = LiveSoakConfig::from_env("desktop-live-three-agent")?;
    let backend = explicit_soak_backend();
    let fixture = build_named_multi_agent_desktop_fixture_with_backend(
        "desktop-live-soak",
        &["alpha", "bravo", "charlie"],
        &backend,
        global_log_store(),
    )?;
    assert_eq!(fixture.deployments.len(), 3);

    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    let diagnostics_dir = config.output_dir.clone();
    let mut diagnostics = LiveSoakDiagnostics::new(&diagnostics_dir)?;
    diagnostics.write_metadata(&fixture.deployments, &backend)?;
    diagnostics.write_log_snapshot(global_log_store().as_ref())?;
    diagnostics.scrape_runtime_metrics(&fixture.runtime_apis)?;

    let tool_roots: BTreeSet<_> = fixture
        .deployments
        .iter()
        .map(|deployment| deployment.running_agent.tool_root.display().to_string())
        .collect();
    assert_eq!(
        tool_roots.len(),
        fixture.deployments.len(),
        "expected one isolated tool root per live deployment"
    );

    for deployment in &fixture.deployments {
        wait_for_stable_runtime_ready(
            fixture.runtime.as_ref(),
            deployment.core.as_ref(),
            &deployment.label,
            &deployment.agent_did,
            Duration::from_secs(2),
            Duration::from_secs(60),
        )?;
        wait_for_stable_runtime_ready(
            fixture.runtime.as_ref(),
            desktop_client.as_ref(),
            &format!("desktop mirror for {}", deployment.label),
            &deployment.agent_did,
            Duration::from_secs(2),
            Duration::from_secs(60),
        )?;
    }
    diagnostics.record_snapshot(
        fixture.runtime.as_ref(),
        &fixture.driver,
        &fixture.deployments,
    )?;
    diagnostics.write_log_snapshot(global_log_store().as_ref())?;
    diagnostics.scrape_runtime_metrics(&fixture.runtime_apis)?;

    let deployments: Vec<_> = fixture
        .deployments
        .iter()
        .map(live_deployment_case)
        .collect();
    let scripted_turns = soak_repo_investigation_turns();
    let mut session_by_peer = BTreeMap::new();
    let mut prompts_by_peer: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (turn_index, prompt_template) in scripted_turns.iter().enumerate() {
        let turn = turn_index + 1;
        for deployment in &deployments {
            let prompt = prompt_template.render(deployment, turn);
            let existing_session_id = session_by_peer.get(&deployment.peer_id).cloned();
            let submission = match submit_soak_prompt_for_deployment(
                fixture.desktop_api.graphql_url(),
                deployment,
                existing_session_id.as_deref(),
                &prompt,
                diagnostics.output_dir(),
            ) {
                Ok(submission) => submission,
                Err(error) => {
                    let _ = diagnostics.record_snapshot(
                        fixture.runtime.as_ref(),
                        &fixture.driver,
                        &fixture.deployments,
                    );
                    let _ = diagnostics.write_log_snapshot(global_log_store().as_ref());
                    let _ = diagnostics.scrape_runtime_metrics(&fixture.runtime_apis);
                    let _ = diagnostics.capture_workspace(fixture._tempdir.path());
                    let recent_logs = soak_recent_problems(0);
                    let _ = diagnostics.record_problem("submit_turn", &error, &recent_logs);
                    return Err(error.context(format!(
                        "soak diagnostics written to {}",
                        diagnostics.output_dir().display()
                    )));
                }
            };

            if let Err(error) = assert_soak_repo_response(
                &deployment.label,
                prompt_template,
                &submission.response,
                turn,
            ) {
                tracing::warn!(
                    deployment = %deployment.label,
                    turn,
                    prompt = prompt_template.name,
                    error = %error,
                    "live soak response quality check failed; continuing because durable turn completion is the primary signal"
                );
            }
            assert_live_submission_rows(
                fixture.runtime.as_ref(),
                desktop_client.as_ref(),
                &format!("desktop {} turn {turn}", deployment.label),
                deployment,
                &submission,
                None,
            )?;
            assert_live_submission_rows(
                fixture.runtime.as_ref(),
                deployment.remote_core,
                &format!("remote {} turn {turn}", deployment.label),
                deployment,
                &submission,
                None,
            )?;

            if let Some(existing_session_id) = session_by_peer.get(&deployment.peer_id) {
                assert_eq!(
                    existing_session_id, &submission.session_id,
                    "expected soak to stay in one conversation per deployment for {}",
                    deployment.label
                );
            } else {
                session_by_peer.insert(deployment.peer_id.clone(), submission.session_id.clone());
            }
            prompts_by_peer
                .entry(deployment.peer_id.clone())
                .or_default()
                .push(prompt.clone());

            diagnostics.record_turn(
                fixture.runtime.as_ref(),
                &fixture.driver,
                &fixture.deployments,
                deployment,
                turn,
                &submission,
            )?;
            diagnostics.write_log_snapshot(global_log_store().as_ref())?;
            diagnostics.scrape_runtime_metrics(&fixture.runtime_apis)?;
        }
    }

    for (index, deployment) in deployments.iter().enumerate() {
        fixture.runtime.block_on(desktop_client.refresh_store())?;
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

    tracing::info!(
        diagnostics_dir = %diagnostics_dir.display(),
        "desktop_live_three_agent_multi_turn_soak completed"
    );
    if let Err(error) = wait_for_post_completion_p2p_quiet(
        fixture.runtime.as_ref(),
        &fixture.driver,
        &fixture.deployments,
        Duration::from_secs(2),
        Duration::from_secs(10),
    ) {
        tracing::warn!(
            diagnostics_dir = %diagnostics_dir.display(),
            error = %error,
            "live soak observed continued P2P activity after completion"
        );
    }
    diagnostics.record_snapshot(
        fixture.runtime.as_ref(),
        &fixture.driver,
        &fixture.deployments,
    )?;
    diagnostics.write_log_snapshot(global_log_store().as_ref())?;
    diagnostics.scrape_runtime_metrics(&fixture.runtime_apis)?;
    if config.keep_workspace {
        diagnostics.capture_workspace(fixture._tempdir.path())?;
    }
    fixture.shutdown()
}

struct SoakPromptTemplate {
    name: &'static str,
    body: &'static str,
}

impl SoakPromptTemplate {
    fn render(&self, deployment: &LiveDeploymentCase<'_>, turn: usize) -> String {
        format!(
            "You are running a desktop live soak investigation for {deployment_label}.\n\
Use the repo copy under ./workspace and cite the file paths you inspected.\n\
Do not answer from memory; read files before answering.\n\
Turn {turn}: {body}",
            deployment_label = deployment.label,
            body = self.body,
        )
    }
}

fn soak_repo_investigation_turns() -> &'static [SoakPromptTemplate] {
    &[
        SoakPromptTemplate {
            name: "p2p-summary",
            body: "Please summarize how the desktop app and defra-agent communicate over P2P in this repository. Cite the files you used and keep the answer focused on the actual code paths.",
        },
        SoakPromptTemplate {
            name: "identities",
            body: "Now explain which identities are involved in that exchange and how they affect authorization, routing, or trust. Build on your previous answer and cite the files you used.",
        },
        SoakPromptTemplate {
            name: "failure-points",
            body: "Now identify the most likely failure points in that desktop-to-agent P2P flow and where you would instrument it for debugging. Cite the files you used.",
        },
    ]
}

fn assert_soak_repo_response(
    deployment_label: &str,
    prompt: &SoakPromptTemplate,
    response: &str,
    turn: usize,
) -> Result<()> {
    let trimmed = response.trim();
    if trimmed.len() < 200 {
        anyhow::bail!(
            "expected a substantive repo-backed response for {} turn {} ({}), got too little text: {}",
            deployment_label,
            turn,
            prompt.name,
            trimmed
        );
    }

    let expected_markers = [
        "workspace/",
        "crates/",
        "docs/",
        ".rs",
        "Cargo.toml",
        "README.md",
    ];
    if !expected_markers
        .iter()
        .any(|marker| trimmed.contains(marker))
    {
        anyhow::bail!(
            "expected repo/file references in soak response for {} turn {} ({}), got: {}",
            deployment_label,
            turn,
            prompt.name,
            trimmed
        );
    }

    Ok(())
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

    let response_text = wait_for_soak_response_text(
        desktop_graphql_url,
        deployment,
        &submitted.request_id,
        log_baseline,
        diagnostics_dir,
    )?;

    Ok(LiveSubmissionCase {
        prompt: prompt.to_string(),
        request_id: submitted.request_id,
        response: response_text,
        session_id: submitted.session_id,
    })
}

fn wait_for_soak_response_text(
    desktop_graphql_url: &str,
    deployment: &LiveDeploymentCase<'_>,
    request_id: &str,
    log_baseline: u64,
    diagnostics_dir: &Path,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(180);
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

        let state = match wait_for_derived_completed_turn(
            desktop_graphql_url,
            request_id,
            Duration::from_millis(250),
            Duration::from_millis(50),
        ) {
            Ok(state) => state,
            Err(error) if Instant::now() < deadline => {
                if error
                    .to_string()
                    .contains("timed out waiting for derived completed turn")
                {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                return Err(error.context(format!(
                    "waiting for soak response for {} request {} (diagnostics: {})",
                    deployment.label,
                    request_id,
                    diagnostics_dir.display()
                )));
            }
            Err(error) => {
                return Err(error.context(format!(
                    "waiting for soak response for {} request {} (diagnostics: {})",
                    deployment.label,
                    request_id,
                    diagnostics_dir.display()
                )));
            }
        };

        let response = state.response.as_ref().ok_or_else(|| {
            anyhow!(
                "soak request {} for {} derived Completed without AgentResponse row",
                request_id,
                deployment.label
            )
        })?;
        let content = response.content.as_deref().unwrap_or_default().trim();
        if content.is_empty() {
            anyhow::bail!(
                "soak request {} for {} reached Completed with empty response content: request={:?} response={:?} (diagnostics: {})",
                request_id,
                deployment.label,
                state.request,
                state.response,
                diagnostics_dir.display()
            );
        }
        return Ok(content.to_string());
    }
}

fn soak_p2p_problem_since(log_baseline: u64) -> Option<String> {
    soak_recent_problems(log_baseline)
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
