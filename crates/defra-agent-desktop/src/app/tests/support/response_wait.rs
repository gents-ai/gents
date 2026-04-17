pub(crate) fn submit_chat_message_and_wait_for_response(
    driver: &mut AuditDriver,
    prompt: &str,
) -> Result<(String, String)> {
    submit_chat_message_and_wait_for_response_after_request(driver, prompt, |_, _| Ok(()))
}

pub(crate) fn submit_chat_message_and_wait_for_observed_response(
    driver: &mut AuditDriver,
    prompt: &str,
) -> Result<(String, String)> {
    submit_chat_message_and_wait_for_observed_response_after_request(driver, prompt, |_, _| Ok(()))
}

pub(crate) fn submit_chat_message_and_wait_for_request_observed(
    driver: &mut AuditDriver,
    prompt: &str,
) -> Result<String> {
    let prior_request_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().requests.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text(prompt);
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    assert!(driver.app.state.chat.editor.composer_text.is_empty());

    wait_for_value(
        "focused request id after request-only submission",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                (snapshot.requests.len() > prior_request_count)
                    .then(|| client.store().focused_request_id())
                    .flatten()
            })
        },
    )
}

pub(crate) fn wait_for_observed_response_for_request(
    driver: &mut AuditDriver,
    request_id: &str,
    prompt: &str,
) -> Result<String> {
    let response_text = wait_for_value(
        &format!("observed response content for request {request_id}"),
        Duration::from_secs(180),
        || {
            let client = driver.app.client.as_ref()?;
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
                            return Some(content.to_string());
                        }
                    }
                }

                if matches!(
                    response.status.as_deref(),
                    Some("error" | "failed" | "failure")
                ) {
                    panic!(
                        "response for request {request_id} reached error status while waiting for observed content: {}",
                        describe_response_wait_state(
                            request,
                            Some(response),
                            0,
                            snapshot.responses.len()
                        )
                    );
                }
            }

            if let Some(request) = request {
                if matches!(
                    request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                ) {
                    panic!(
                        "request {request_id} reached terminal lifecycle while waiting for observed response: {}",
                        describe_response_wait_state(
                            Some(request),
                            response,
                            0,
                            snapshot.responses.len()
                        )
                    );
                }
            }

            None
        },
    )?;

    let rendered_response_text = response_text.trim();
    wait_for_value(
        &format!("prompt and observed response in transcript for request {request_id}"),
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(prompt))
                .then_some(())
                .and_then(|_| {
                    texts
                        .iter()
                        .any(|text| text.contains(rendered_response_text))
                        .then_some(())
                })
        },
    )?;

    Ok(response_text)
}

pub(crate) fn submit_chat_message_and_wait_for_response_after_request(
    driver: &mut AuditDriver,
    prompt: &str,
    mut after_request: impl FnMut(&mut AuditDriver, &str) -> Result<()>,
) -> Result<(String, String)> {
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

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text(prompt);
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    assert!(driver.app.state.chat.editor.composer_text.is_empty());

    let request_id = wait_for_value(
        "focused request id after submission",
        Duration::from_secs(5),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                (snapshot.requests.len() > prior_request_count)
                    .then(|| client.store().focused_request_id())
                    .flatten()
            })
        },
    )?;
    after_request(driver, &request_id)?;

    let mut next_response_refresh = Instant::now();
    let response_deadline = Instant::now() + Duration::from_secs(180);
    let response_text = loop {
        let client = Arc::clone(
            driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing while waiting for response"))?,
        );
        if Instant::now() >= next_response_refresh {
            driver.app.block_on_runtime(client.refresh_store())?;
            next_response_refresh = Instant::now() + Duration::from_secs(1);
        }

        let snapshot = client.store().snapshot();
        let request = snapshot
            .requests
            .iter()
            .find(|row| row.request_id == request_id);
        let response = snapshot.latest_response_for_request(&request_id);
        if let Some(response) = response {
            if matches!(response.status.as_deref(), Some("complete" | "completed")) {
                if let Some(content) = response.content.as_deref() {
                    if !content.trim().is_empty() {
                        break content.to_string();
                    }
                }
            }

            if matches!(
                response.status.as_deref(),
                Some("error" | "failed" | "failure")
            ) {
                anyhow::bail!(
                    "response for request {request_id} reached error status while waiting for content: {}",
                    describe_response_wait_state(request, Some(response), prior_response_count, snapshot.responses.len())
                );
            }
        }

        if let Some(request) = request {
            if matches!(
                request.lifecycle_state.as_deref(),
                Some("failed" | "dead" | "superseded")
            ) {
                anyhow::bail!(
                    "request {request_id} reached terminal lifecycle before response content: {}",
                    describe_response_wait_state(
                        Some(request),
                        response,
                        prior_response_count,
                        snapshot.responses.len()
                    )
                );
            }
        }

        if Instant::now() >= response_deadline {
            anyhow::bail!(
                "timed out waiting for response content in client store after submission: {}",
                describe_response_wait_state(
                    request,
                    response,
                    prior_response_count,
                    snapshot.responses.len()
                )
            );
        }

        std::thread::sleep(Duration::from_millis(50));
    };

    let rendered_response_text = response_text.trim();
    wait_for_value(
        "submitted prompt and response in transcript",
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(prompt))
                .then_some(())
                .and_then(|_| {
                    texts
                        .iter()
                        .any(|text| text.contains(rendered_response_text))
                        .then_some(())
                })
        },
    )?;

    Ok((request_id, response_text))
}

pub(crate) fn submit_chat_message_and_wait_for_observed_response_after_request(
    driver: &mut AuditDriver,
    prompt: &str,
    mut after_request: impl FnMut(&mut AuditDriver, &str) -> Result<()>,
) -> Result<(String, String)> {
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

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text(prompt);
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    assert!(driver.app.state.chat.editor.composer_text.is_empty());

    let request_id = wait_for_value(
        "focused request id after observed submission",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                (snapshot.requests.len() > prior_request_count)
                    .then(|| client.store().focused_request_id())
                    .flatten()
            })
        },
    )?;
    after_request(driver, &request_id)?;

    let response_text = wait_for_value(
        "observed response content in client store after submission",
        Duration::from_secs(180),
        || {
            let client = driver.app.client.as_ref()?;
            let snapshot = client.store().snapshot();
            let request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == request_id);
            let response = snapshot.latest_response_for_request(&request_id);

            if let Some(response) = response {
                if matches!(response.status.as_deref(), Some("complete" | "completed")) {
                    if let Some(content) = response.content.as_deref() {
                        if !content.trim().is_empty() {
                            return Some(content.to_string());
                        }
                    }
                }

                if matches!(
                    response.status.as_deref(),
                    Some("error" | "failed" | "failure")
                ) {
                    panic!(
                        "response for request {request_id} reached error status while waiting for observed content: {}",
                        describe_response_wait_state(
                            request,
                            Some(response),
                            prior_response_count,
                            snapshot.responses.len()
                        )
                    );
                }
            }

            if let Some(request) = request {
                if matches!(
                    request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                ) {
                    panic!(
                        "request {request_id} reached terminal lifecycle while waiting for observed response: {}",
                        describe_response_wait_state(
                            Some(request),
                            response,
                            prior_response_count,
                            snapshot.responses.len()
                        )
                    );
                }
            }

            None
        },
    )?;

    let rendered_response_text = response_text.trim();
    wait_for_value(
        "submitted prompt and observed response in transcript",
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(prompt))
                .then_some(())
                .and_then(|_| {
                    texts
                        .iter()
                        .any(|text| text.contains(rendered_response_text))
                        .then_some(())
                })
        },
    )?;

    Ok((request_id, response_text))
}

pub(crate) fn describe_response_wait_state(
    request: Option<&defra_agent_protocol::row::AgentRequestRow>,
    response: Option<&defra_agent_protocol::row::AgentResponseRow>,
    prior_response_count: usize,
    current_response_count: usize,
) -> String {
    let request_summary = request.map_or_else(
        || "request=<missing>".to_string(),
        |row| {
            format!(
                "request={{status={}, lifecycle_state={}, agent_did={}, behavior_id={}, backend_id={}, execution_origin={}, failure_reason={}, claimed_at={}, deadline={}}}",
                optional_str(row.status.as_deref()),
                optional_str(row.lifecycle_state.as_deref()),
                optional_str(row.agent_did.as_deref()),
                optional_str(row.behavior_id.as_deref()),
                optional_str(row.backend_id.as_deref()),
                optional_str(row.execution_origin.as_deref()),
                optional_str(row.failure_reason.as_deref()),
                optional_str(row.claimed_at.as_deref()),
                optional_str(row.deadline.as_deref()),
            )
        },
    );
    let response_summary = response.map_or_else(
        || "response=<missing>".to_string(),
        |row| {
            format!(
                "response={{key={}, status={}, agent_did={}, behavior_id={}, error_message={}, content_len={}, progress_seq={}, completed_at={}}}",
                row.response_key,
                optional_str(row.status.as_deref()),
                optional_str(row.agent_did.as_deref()),
                optional_str(row.behavior_id.as_deref()),
                optional_str(row.error_message.as_deref()),
                row.content.as_deref().map(str::len).unwrap_or_default(),
                row.progress_seq.unwrap_or_default(),
                optional_str(row.completed_at.as_deref()),
            )
        },
    );
    format!(
        "{request_summary}; {response_summary}; responses_before_submit={prior_response_count}; responses_now={current_response_count}"
    )
}

pub(crate) fn optional_str(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("<empty>")
}
