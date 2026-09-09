use super::*;

#[tokio::test]
async fn offline_barrier_stays_open_for_later_provider_attempts() -> Result<()> {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let model_gate = gate.clone();
    let model = FakeLlm::start(
        "barrier-contract",
        None,
        Arc::new(move |_| {
            ChatAction::WaitThenSse(model_gate.clone(), completion_text_sse("visible"))
        }),
    )?;
    gate.add_permits(1);
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let body = timeout(Duration::from_secs(2), async {
            client
                .post(format!("{}/chat/completions", model.endpoint()))
                .json(&serde_json::json!({"messages": []}))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        })
        .await
        .context("an opened offline barrier blocked a later attempt")??;
        anyhow::ensure!(body.contains("visible"), "missing fixture content");
    }
    Ok(())
}

pub(super) async fn wait_for_visible_content(
    core: &ClientCore,
    request: &str,
    expected: &str,
) -> Result<()> {
    let started = Instant::now();
    timeout(Duration::from_millis(500), async {
        loop {
            let visible = core
                .store()
                .snapshot()
                .latest_response_for_request(request)
                .is_some_and(|response| {
                    response.status.as_deref() == Some("streaming")
                        && response
                            .content
                            .as_deref()
                            .is_some_and(|text| text.contains(expected))
                });
            if visible {
                tracing::info!(
                    request,
                    elapsed_ms = started.elapsed().as_millis(),
                    "streaming content observed while provider completion remains gated"
                );
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("streaming content did not reach client projection before provider completion")?;
    Ok(())
}
