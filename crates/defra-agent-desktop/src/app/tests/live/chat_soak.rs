use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::*;

#[test]
#[ignore = "hits the fixed MiniMax live backend and runs a long three-agent soak"]
fn desktop_live_three_agent_multi_turn_soak() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let backend = explicit_soak_backend();
    let mut fixture = build_named_multi_agent_desktop_fixture_with_backend(
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
    let diagnostics_dir = LiveSoakDiagnostics::persistent_output_dir("desktop-live-three-agent")?;
    let mut diagnostics = LiveSoakDiagnostics::new(&diagnostics_dir)?;
    diagnostics.write_metadata(&fixture.deployments, &backend)?;

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

    let deployments: Vec<_> = fixture
        .deployments
        .iter()
        .map(live_deployment_case)
        .collect();
    let mut session_by_peer = BTreeMap::new();
    let mut prompts_by_peer: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for turn in 1..=10 {
        for (index, deployment) in deployments.iter().enumerate() {
            let token = format!("SOAK_AGENT_{}_TURN_{turn}", index + 1);
            let prompt = format!(
                "Desktop soak turn {turn} for deployment {}. Reply with exactly {token} and nothing else.",
                deployment.label
            );
            let submission = match submit_soak_prompt_for_deployment(
                &mut fixture.driver,
                deployment,
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
                    let recent_logs = soak_recent_problems(0);
                    let _ = diagnostics.record_problem("submit_turn", &error, &recent_logs);
                    return Err(error.context(format!(
                        "soak diagnostics written to {}",
                        diagnostics.output_dir().display()
                    )));
                }
            };

            assert_eq!(
                submission.response.trim(),
                token,
                "unexpected soak response for {} turn {}",
                deployment.label,
                turn
            );
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
        }
    }

    for (index, deployment) in deployments.iter().enumerate() {
        let conversation_target = audit::targets::chat_conversation(
            session_by_peer
                .get(&deployment.peer_id)
                .ok_or_else(|| anyhow!("missing soak session for {}", deployment.label))?,
        );
        fixture
            .driver
            .click_target(&audit::targets::chat_deployment(&deployment.peer_id));
        fixture.driver.click_target(&conversation_target);
        let texts = fixture.driver.render();

        let own_prompts = prompts_by_peer
            .get(&deployment.peer_id)
            .ok_or_else(|| anyhow!("missing prompts for {}", deployment.label))?;
        assert!(
            texts.iter().any(|text| text.contains(&own_prompts[0])),
            "expected first soak prompt to remain visible for {}",
            deployment.label
        );
        assert!(
            texts.iter().any(|text| text.contains(
                own_prompts
                    .last()
                    .expect("deployment soak prompts are non-empty")
            )),
            "expected last soak prompt to remain visible for {}",
            deployment.label
        );

        let other = &deployments[(index + 1) % deployments.len()];
        let other_prompt = prompts_by_peer
            .get(&other.peer_id)
            .and_then(|prompts| prompts.first())
            .ok_or_else(|| anyhow!("missing comparison prompt for {}", other.label))?;
        assert!(
            !texts.iter().any(|text| text.contains(other_prompt)),
            "transcript for {} leaked prompt from {}",
            deployment.label,
            other.label
        );
    }

    tracing::info!(
        diagnostics_dir = %diagnostics_dir.display(),
        "desktop_live_three_agent_multi_turn_soak completed"
    );
    fixture.shutdown()
}

fn submit_soak_prompt_for_deployment(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    prompt: &str,
    diagnostics_dir: &Path,
) -> Result<LiveSubmissionCase> {
    driver.open_activity(Activity::Chat);
    let deployment_target = audit::targets::chat_deployment(&deployment.peer_id);
    driver.wait_for_target(
        &format!("chat deployment row for {}", deployment.label),
        Duration::from_secs(15),
        &deployment_target,
    )?;
    driver.click_target(&deployment_target);
    assert_chat_context(driver, deployment, None);

    let _ = ensure_chat_session_selected(
        driver,
        &format!("chat session ready for {}", deployment.label),
        Duration::from_secs(15),
    )?;

    let prior_request_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().requests.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    let prior_response_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().responses.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    let log_baseline = global_log_store()
        .snapshot()
        .entries
        .last()
        .map(|entry| entry.id)
        .unwrap_or_default();

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text(prompt);
    driver.click_target(audit::targets::CHAT_SEND);

    let request_id = wait_for_value(
        &format!("soak request id for {}", deployment.label),
        Duration::from_secs(15),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                (snapshot.requests.len() > prior_request_count)
                    .then(|| client.store().focused_request_id())
                    .flatten()
            })
        },
    )?;

    let response_text = wait_for_soak_response_text(
        driver,
        deployment,
        &request_id,
        prior_response_count,
        log_baseline,
        diagnostics_dir,
    )?;
    wait_for_value(
        &format!("soak transcript render for {}", deployment.label),
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            (texts.iter().any(|text| text.contains(prompt))
                && texts.iter().any(|text| text.contains(response_text.trim())))
            .then_some(())
        },
    )?;
    let session_id = driver
        .app
        .state
        .chat
        .shell
        .selected_session_id
        .clone()
        .ok_or_else(|| anyhow!("missing soak session id for {}", deployment.label))?;

    Ok(LiveSubmissionCase {
        prompt: prompt.to_string(),
        request_id,
        response: response_text,
        session_id,
    })
}

fn wait_for_soak_response_text(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    request_id: &str,
    prior_response_count: usize,
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

        let client = driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing while waiting for soak response"))?;
        let snapshot = client.store().snapshot();
        let request = snapshot
            .requests
            .iter()
            .find(|row| row.request_id == request_id);
        let response = snapshot.latest_response_for_request(request_id);

        if let Some(response) = response {
            if matches!(response.status.as_deref(), Some("complete" | "completed")) {
                if let Some(content) = response.content.as_deref() {
                    if !content.trim().is_empty() {
                        return Ok(content.to_string());
                    }
                }
            }

            if matches!(
                response.status.as_deref(),
                Some("error" | "failed" | "failure")
            ) {
                anyhow::bail!(
                    "soak response for {} request {} reached error status: {} (diagnostics: {})",
                    deployment.label,
                    request_id,
                    describe_response_wait_state(
                        request,
                        Some(response),
                        prior_response_count,
                        snapshot.responses.len()
                    ),
                    diagnostics_dir.display()
                );
            }
        }

        if let Some(request) = request {
            if matches!(
                request.lifecycle_state.as_deref(),
                Some("failed" | "dead" | "superseded")
            ) {
                anyhow::bail!(
                    "soak request {} for {} reached terminal lifecycle: {} (diagnostics: {})",
                    request_id,
                    deployment.label,
                    describe_response_wait_state(
                        Some(request),
                        response,
                        prior_response_count,
                        snapshot.responses.len()
                    ),
                    diagnostics_dir.display()
                );
            }
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for soak response for {} request {}: {} (diagnostics: {})",
                deployment.label,
                request_id,
                describe_response_wait_state(
                    request,
                    response,
                    prior_response_count,
                    snapshot.responses.len()
                ),
                diagnostics_dir.display()
            );
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn soak_p2p_problem_since(log_baseline: u64) -> Option<String> {
    soak_recent_problems(log_baseline).into_iter().next()
}

fn soak_recent_problems(log_baseline: u64) -> Vec<String> {
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
            interesting.then(|| format!("{} {}: {}", entry.level, entry.target, entry.message))
        })
        .take(32)
        .collect()
}
