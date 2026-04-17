pub(crate) fn refreshed_runtime_generation(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    agent_did: &str,
) -> Option<i64> {
    runtime.block_on(core.refresh_store()).ok()?;
    core.store()
        .snapshot()
        .latest_runtime(agent_did)
        .and_then(|row| row.router_generation.or(row.active_generation))
}

pub(crate) fn compact_field(value: Option<&str>) -> String {
    match value {
        Some(value) if value.len() > 96 => format!("{}...", &value[..96]),
        Some(value) => value.to_string(),
        None => "<none>".to_string(),
    }
}

pub(crate) fn describe_live_config_state(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
    switch_backend_id: &str,
    switch_profile_id: &str,
) -> String {
    let refresh = runtime
        .block_on(core.refresh_store())
        .map(|_| "ok".to_string())
        .unwrap_or_else(|error| format!("error={error:#}"));
    let snapshot = core.store().snapshot();
    let behavior = snapshot
        .behaviors
        .iter()
        .find(|row| row.behavior_id == docs.behavior_id)
        .map(|row| {
            format!(
                "behavior(agent={:?}, backend={:?}, model={:?}, tool_selection={:?}, profile={:?}, enabled={:?}, prompt={})",
                row.agent_did,
                row.backend_id,
                row.model_name,
                row.tool_selection_id,
                row.inference_profile_id,
                row.enabled,
                compact_field(row.system_prompt.as_deref())
            )
        })
        .unwrap_or_else(|| "behavior=<missing>".to_string());
    let original_backend = snapshot
        .inference_backends
        .iter()
        .find(|row| row.backend_id == docs.backend_id)
        .map(|row| {
            format!(
                "original_backend(enabled={:?}, probe={:?}, endpoint={}, models={:?})",
                row.enabled,
                row.probe_status.as_deref(),
                compact_field(row.endpoint.as_deref()),
                row.models
            )
        })
        .unwrap_or_else(|| "original_backend=<missing>".to_string());
    let switch_backend = snapshot
        .inference_backends
        .iter()
        .find(|row| row.backend_id == switch_backend_id)
        .map(|row| {
            format!(
                "switch_backend(enabled={:?}, probe={:?}, endpoint={}, models={:?})",
                row.enabled,
                row.probe_status.as_deref(),
                compact_field(row.endpoint.as_deref()),
                row.models
            )
        })
        .unwrap_or_else(|| "switch_backend=<missing>".to_string());
    let tool_selection = snapshot
        .tool_selections
        .iter()
        .find(|row| row.selection_id == docs.tool_selection_id)
        .map(|row| {
            format!(
                "tools(agent={:?}, enable_file={:?}, file_mode={:?}, enable_bash={:?}, bash_mode={:?}, cli={:?}, meta={:?})",
                row.agent_did,
                row.enable_file_tools,
                row.file_tools_mode,
                row.enable_bash,
                row.bash_mode,
                row.cli_tool_names,
                row.enable_meta_tools
            )
        })
        .unwrap_or_else(|| "tools=<missing>".to_string());
    let switch_profile = snapshot
        .inference_profiles
        .iter()
        .find(|row| row.profile_id == switch_profile_id)
        .map(|row| {
            format!(
                "switch_profile(max_output={:?}, max_turns={:?}, temp={:?})",
                row.max_output_tokens, row.max_turns, row.temperature
            )
        })
        .unwrap_or_else(|| "switch_profile=<missing>".to_string());
    let runtime_row = snapshot
        .latest_runtime(agent_did)
        .map(|row| {
            format!(
                "runtime(process={:?}, phase={:?}, active={:?}, router={:?}, default={:?}, runnable={:?}, unavailable={:?}, result={:?}, error={})",
                row.process_state,
                row.reconcile_phase,
                row.active_generation,
                row.router_generation,
                row.default_behavior_id,
                row.runnable_behavior_count,
                row.unavailable_behavior_count,
                row.last_reconcile_result,
                compact_field(row.last_reconcile_error.as_deref())
            )
        })
        .unwrap_or_else(|| "runtime=<missing>".to_string());

    format!(
        "{label}: refresh={refresh}; {runtime_row}; {behavior}; {original_backend}; {switch_backend}; {tool_selection}; {switch_profile}"
    )
}

pub(crate) fn wait_for_stable_runtime_ready(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
    stable_for: Duration,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut stable_since = None;
    let mut stable_generation = None;
    let mut last_state = "runtime=<missing>".to_string();

    loop {
        runtime.block_on(core.refresh_store())?;
        let snapshot = core.store().snapshot();
        let runtime_row = snapshot.latest_runtime(agent_did);
        let ready = runtime_row.is_some_and(|row| {
            let generation = row.router_generation.or(row.active_generation);
            last_state = format!(
                "generation={generation:?} runnable={:?} unavailable={:?} result={:?} error={}",
                row.runnable_behavior_count,
                row.unavailable_behavior_count,
                row.last_reconcile_result,
                compact_field(row.last_reconcile_error.as_deref())
            );
            generation.is_some()
                && row.runnable_behavior_count == Some(1)
                && row.unavailable_behavior_count == Some(0)
                && row
                    .last_reconcile_error
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
        });
        let generation =
            runtime_row.and_then(|row| row.router_generation.or(row.active_generation));

        if ready {
            match (stable_generation, generation) {
                (Some(stable), Some(current)) if stable == current => {}
                (_, Some(current)) => {
                    stable_generation = Some(current);
                    stable_since = Some(Instant::now());
                }
                _ => {
                    stable_generation = None;
                    stable_since = None;
                }
            }
            if stable_since.is_some_and(|since| since.elapsed() >= stable_for) {
                return Ok(());
            }
        } else {
            stable_generation = None;
            stable_since = None;
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for stable runtime ready for {label}; last={last_state}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(crate) fn tool_loop_prompt(label: &str, directory: &str, file_paths: &[String]) -> String {
    format!(
        "This is live desktop tool-loop request {label} for {directory}. Call read_file separately for each of these files, in this exact order: {}. Reply with only the file tokens in that same order, separated by a single space. Do not guess or reuse tokens from another conversation.",
        file_paths.join(", ")
    )
}

pub(crate) fn assert_response_contains_tokens(
    label: &str,
    response: &str,
    expected_tokens: &[String],
) -> Result<()> {
    let mut search_from = 0;
    for token in expected_tokens {
        let offset = response[search_from..].find(token.as_str()).ok_or_else(|| {
            anyhow!(
                "{label} response missing expected token {token} after offset {search_from}: {response}"
            )
        })?;
        search_from += offset + token.len();
    }
    Ok(())
}

pub(crate) fn wait_for_session_tool_activity(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    session_id: &str,
    expected_list_files: usize,
    expected_read_files: usize,
    expected_tokens: &[String],
) -> Result<String> {
    wait_for_value(label, Duration::from_secs(90), || {
        runtime.block_on(core.refresh_store()).ok()?;
        let snapshot = core.store().snapshot();
        let transcript = snapshot.transcript(session_id);

        let list_calls = transcript
            .tool_calls
            .iter()
            .filter(|row| {
                row.tool_name.as_deref() == Some("list_files")
                    && row.status.as_deref() == Some("completed")
            })
            .count();
        let read_calls = transcript
            .tool_calls
            .iter()
            .filter(|row| {
                row.tool_name.as_deref() == Some("read_file")
                    && row.status.as_deref() == Some("completed")
            })
            .collect::<Vec<_>>();
        if list_calls < expected_list_files || read_calls.len() < expected_read_files {
            return None;
        }

        let read_results = transcript
            .tool_results
            .iter()
            .filter(|row| row.tool_name.as_deref() == Some("read_file"))
            .filter_map(|row| row.output_text.clone())
            .collect::<Vec<_>>();
        if !expected_tokens.iter().all(|token| {
            read_results
                .iter()
                .any(|result| result.contains(token.as_str()))
        }) {
            return None;
        }

        read_calls.last().map(|tool_call| {
            tool_call
                .tool_call_id
                .clone()
                .unwrap_or_else(|| tool_call.tool_call_key.clone())
        })
    })
}

pub(crate) fn wait_for_two_requests_in_flight(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    first_request_id: &str,
    second_request_id: &str,
) -> Result<()> {
    wait_for_value(
        "two live requests accepted before either response completed",
        Duration::from_secs(20),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let first_request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == first_request_id)?;
            let second_request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == second_request_id)?;
            let first_complete = snapshot
                .latest_response_for_request(first_request_id)
                .is_some_and(|row| matches!(row.status.as_deref(), Some("complete" | "completed")));
            let second_complete = snapshot
                .latest_response_for_request(second_request_id)
                .is_some_and(|row| matches!(row.status.as_deref(), Some("complete" | "completed")));

            (!first_complete
                && !second_complete
                && !matches!(
                    first_request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                )
                && !matches!(
                    second_request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                ))
            .then_some(())
        },
    )
}

pub(crate) fn assert_live_submission_rows(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    submission: &LiveSubmissionCase,
    expected_backend_id: Option<&str>,
) -> Result<()> {
    wait_for_value(
        &format!("{label} submission rows for {}", deployment.label),
        Duration::from_secs(30),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == submission.request_id)?;
            let request_ok = request.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && request.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && request.session_id.as_deref() == Some(submission.session_id.as_str())
                && expected_backend_id
                    .is_none_or(|backend_id| request.backend_id.as_deref() == Some(backend_id))
                && request.content.as_deref() == Some(submission.prompt.as_str())
                && request
                    .failure_reason
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && !matches!(
                    request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                );

            let response = snapshot.latest_response_for_request(&submission.request_id)?;
            let response_ok = response.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && response.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && response.session_id.as_deref() == Some(submission.session_id.as_str())
                && matches!(response.status.as_deref(), Some("complete" | "completed"))
                && response
                    .error_message
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && response
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains(submission.response.trim()));

            let conversation_ok = snapshot
                .conversations
                .iter()
                .find(|row| row.session_id == submission.session_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                        && row.latest_request_id.as_deref() == Some(submission.request_id.as_str())
                });
            let session_ok = snapshot
                .sessions
                .iter()
                .find(|row| row.session_id == submission.session_id)
                .is_some_and(|row| {
                    row.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                });

            (request_ok && response_ok && conversation_ok && session_ok).then_some(())
        },
    )
}

pub(crate) fn assert_live_deployment_default_config(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    expected_model_name: &str,
) -> Result<()> {
    wait_for_value(
        &format!("{label} default config remains isolated"),
        Duration::from_secs(30),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let behavior_ok = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.backend_id.as_deref() == Some(deployment.docs.backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(deployment.docs.inference_profile_id.as_str())
                        && row.tool_selection_id.as_deref()
                            == Some(deployment.docs.tool_selection_id.as_str())
                        && row.model_name.as_deref() == Some(expected_model_name)
                        && row.enabled == Some(true)
                });
            let tools_ok = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.enable_file_tools == Some(false)
                        && row.enable_bash == Some(false)
                        && row.cli_tool_names.is_empty()
                        && row.delegate_to.is_empty()
                });
            let profile_ok = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == deployment.docs.inference_profile_id)
                .is_some_and(|row| {
                    row.max_output_tokens == Some(1024)
                        && row.max_turns == Some(12)
                        && row.temperature == Some(0.0)
                });
            (behavior_ok && tools_ok && profile_ok).then_some(())
        },
    )
}
