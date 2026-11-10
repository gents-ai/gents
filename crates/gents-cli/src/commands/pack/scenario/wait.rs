use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde_json::Value;

pub(super) async fn wait_for_event_source(
    log: &Path,
    collection: &str,
    deadline: Duration,
) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if event_source_ready(&text, collection) {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!(
        "timed out after {}s waiting for the event source to observe {collection} \
         and open its global Update subscription. \
         The pack's backend may still be unprobed — check {} for \
         'agent unavailable after runtime reconcile'.",
        deadline.as_secs(),
        log.display()
    )
}

pub(super) async fn wait_for_server_ready_marker(
    log: &Path,
    server: &mut tokio::process::Child,
    deadline: Duration,
) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if server_ready_marker(&text) {
                return Ok(());
            }
        }
        if let Ok(Some(status)) = server.try_wait() {
            bail!(
                "pack server exited before publishing post-apply readiness ({status}); check {}",
                log.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!(
        "timed out after {}s waiting for the server's post-apply readiness marker; check {}",
        deadline.as_secs(),
        log.display()
    )
}

pub(super) fn server_ready_marker(log: &str) -> bool {
    log.lines().any(|line| {
        let line = strip_ansi(line);
        line.contains("gents server is running ") && line.contains("Press Ctrl-C to stop.")
    })
}

pub(super) fn event_source_ready(log: &str, collection: &str) -> bool {
    let mut observes_target = false;
    let mut subscription_open = false;
    for line in log.lines() {
        let line = strip_ansi(line);
        observes_target |= observes_collection(&line, collection);
        subscription_open |= line.contains("event source opened global Update subscription");
    }
    observes_target && subscription_open
}

pub(super) fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
        }
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
    out
}

pub(super) fn observes_collection(line: &str, collection: &str) -> bool {
    if !line.contains("event source now observing") {
        return false;
    }
    let needle = format!("source_collection={collection}");
    let Some(idx) = line.find(&needle) else {
        return false;
    };
    line[idx + needle.len()..]
        .chars()
        .next()
        .is_none_or(|next| !next.is_alphanumeric() && next != '_')
}

pub(super) fn event_backed_trigger_ready(response: &Value, collection: &str) -> bool {
    let sources = response
        .pointer("/data/EventSource")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            (row.get("source_collection").and_then(Value::as_str) == Some(collection))
                .then(|| row.get("event_source_id").and_then(Value::as_str))
                .flatten()
        })
        .collect::<BTreeSet<_>>();
    response
        .pointer("/data/Trigger")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| row.get("enabled").and_then(Value::as_bool) == Some(true))
        .filter_map(|row| row.get("source"))
        .filter_map(|source| match source {
            Value::String(source) => serde_json::from_str(source).ok(),
            source => Some(source.clone()),
        })
        .filter_map(|source| {
            source
                .get("event_source_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .any(|source_id| sources.contains(source_id.as_str()))
}

pub(super) fn has_correlated_request(response: &Value) -> bool {
    response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
}

async fn wait_until_value<T, F, Fut, P>(
    label: &str,
    deadline: Duration,
    mut probe: F,
    ready: P,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    P: Fn(&T) -> bool,
{
    let started = Instant::now();
    let mut last = None::<String>;
    while started.elapsed() < deadline {
        match probe().await {
            Ok(value) if ready(&value) => return Ok(value),
            Ok(_) => {}
            Err(error) => last = Some(error.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    match last {
        Some(error) => bail!("{label} timed out after {}s: {error}", deadline.as_secs()),
        None => bail!("{label} timed out after {}s", deadline.as_secs()),
    }
}

pub(super) async fn wait_until<F, Fut>(label: &str, deadline: Duration, probe: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    wait_until_value(label, deadline, probe, |ready| *ready)
        .await
        .map(|_| ())
}

pub(super) async fn wait_http_ok(url: &str, deadline: Duration) -> Result<()> {
    let client = reqwest::Client::new();
    wait_until(url, deadline, || {
        let client = client.clone();
        async move {
            Ok(client
                .get(url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .map(|response| response.status().is_success())
                .unwrap_or(false))
        }
    })
    .await
}
