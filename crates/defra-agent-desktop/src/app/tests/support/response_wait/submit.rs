use super::diagnostics::describe_response_wait_state;
use super::*;

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
    let (request_id, _) = submit_chat_message_and_capture_request(
        driver,
        prompt,
        "focused request id after request-only submission",
        Duration::from_secs(10),
    )?;
    Ok(request_id)
}

pub(crate) fn wait_for_observed_response_for_request(
    driver: &mut AuditDriver,
    request_id: &str,
    prompt: &str,
) -> Result<String> {
    let response_text = wait_for_observed_response_text(
        driver,
        request_id,
        0,
        &format!("observed response content for request {request_id}"),
    )?;
    wait_for_transcript_render(
        driver,
        prompt,
        &response_text,
        &format!("prompt and observed response in transcript for request {request_id}"),
    )?;
    Ok(response_text)
}

pub(crate) fn submit_chat_message_and_wait_for_response_after_request(
    driver: &mut AuditDriver,
    prompt: &str,
    mut after_request: impl FnMut(&mut AuditDriver, &str) -> Result<()>,
) -> Result<(String, String)> {
    let (request_id, prior_response_count) = submit_chat_message_and_capture_request(
        driver,
        prompt,
        "focused request id after submission",
        Duration::from_secs(5),
    )?;
    after_request(driver, &request_id)?;

    let response_text =
        wait_for_response_text_with_refresh(driver, &request_id, prior_response_count)?;
    wait_for_transcript_render(
        driver,
        prompt,
        &response_text,
        "submitted prompt and response in transcript",
    )?;

    Ok((request_id, response_text))
}

pub(crate) fn submit_chat_message_and_wait_for_observed_response_after_request(
    driver: &mut AuditDriver,
    prompt: &str,
    mut after_request: impl FnMut(&mut AuditDriver, &str) -> Result<()>,
) -> Result<(String, String)> {
    let (request_id, prior_response_count) = submit_chat_message_and_capture_request(
        driver,
        prompt,
        "focused request id after observed submission",
        Duration::from_secs(10),
    )?;
    after_request(driver, &request_id)?;

    let response_text = wait_for_observed_response_text(
        driver,
        &request_id,
        prior_response_count,
        "observed response content in client store after submission",
    )?;
    wait_for_transcript_render(
        driver,
        prompt,
        &response_text,
        "submitted prompt and observed response in transcript",
    )?;

    Ok((request_id, response_text))
}

fn submit_chat_message_and_capture_request(
    driver: &mut AuditDriver,
    prompt: &str,
    wait_label: &str,
    timeout: Duration,
) -> Result<(String, usize)> {
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

    let request_id = wait_for_value(wait_label, timeout, || {
        driver.app.client.as_ref().and_then(|client| {
            let snapshot = client.store().snapshot();
            (snapshot.requests.len() > prior_request_count)
                .then(|| client.store().focused_request_id())
                .flatten()
        })
    })?;

    Ok((request_id, prior_response_count))
}

fn wait_for_response_text_with_refresh(
    driver: &mut AuditDriver,
    request_id: &str,
    prior_response_count: usize,
) -> Result<String> {
    let mut next_response_refresh = Instant::now();
    let response_deadline = Instant::now() + Duration::from_secs(180);
    loop {
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
                    "response for request {request_id} reached error status while waiting for content: {}",
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
    }
}

fn wait_for_observed_response_text(
    driver: &mut AuditDriver,
    request_id: &str,
    prior_response_count: usize,
    wait_label: &str,
) -> Result<String> {
    wait_for_value(wait_label, Duration::from_secs(180), || {
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
    })
}

fn wait_for_transcript_render(
    driver: &mut AuditDriver,
    prompt: &str,
    response_text: &str,
    wait_label: &str,
) -> Result<()> {
    let rendered_response_text = response_text.trim();
    wait_for_value(wait_label, Duration::from_secs(30), || {
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
    })
}
