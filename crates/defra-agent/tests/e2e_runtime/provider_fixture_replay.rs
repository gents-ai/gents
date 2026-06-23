use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

const PROVIDER_FIXTURE_ROOT: &str = "tests/fixtures/providers";

#[derive(Debug, Deserialize)]
struct ProviderFixtureCorpus {
    provider: String,
    scenario: String,
    exchanges: Vec<ProviderFixtureExchange>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderFixtureExchange {
    request_key: String,
    ordinal: u32,
    request: ProviderFixtureRequest,
    response: ProviderFixtureResponse,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderFixtureRequest {
    method: String,
    path: String,
    body: Value,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderFixtureResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

struct ProviderFixtureReplayer {
    provider: String,
    scenario: String,
    pending: BTreeMap<String, VecDeque<ProviderFixtureExchange>>,
}

impl ProviderFixtureReplayer {
    fn from_file(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading provider fixture {}", path.display()))?;
        let corpus: ProviderFixtureCorpus = serde_json::from_str(&raw)
            .with_context(|| format!("parsing provider fixture {}", path.display()))?;
        Self::from_corpus(corpus)
    }

    fn from_corpus(corpus: ProviderFixtureCorpus) -> Result<Self> {
        let mut seen = BTreeSet::new();
        let mut pending: BTreeMap<String, Vec<_>> = BTreeMap::new();
        for exchange in corpus.exchanges {
            anyhow::ensure!(
                !exchange.request_key.trim().is_empty(),
                "provider fixture {}:{} has an empty request key",
                corpus.provider,
                corpus.scenario
            );
            anyhow::ensure!(
                seen.insert((exchange.request_key.clone(), exchange.ordinal)),
                "provider fixture {}:{} repeats key {} ordinal {}",
                corpus.provider,
                corpus.scenario,
                exchange.request_key,
                exchange.ordinal
            );
            pending
                .entry(exchange.request_key.clone())
                .or_default()
                .push(exchange);
        }

        Ok(Self {
            provider: corpus.provider,
            scenario: corpus.scenario,
            pending: pending
                .into_iter()
                .map(|(key, mut exchanges)| {
                    exchanges.sort_by_key(|exchange| exchange.ordinal);
                    (key, VecDeque::from(exchanges))
                })
                .collect(),
        })
    }

    fn next(&mut self, request_key: &str) -> Result<ProviderFixtureExchange> {
        let Some(exchanges) = self.pending.get_mut(request_key) else {
            anyhow::bail!(
                "no provider fixture for {}:{} request key {}",
                self.provider,
                self.scenario,
                request_key
            );
        };
        let Some(exchange) = exchanges.pop_front() else {
            anyhow::bail!(
                "provider fixture {}:{} exhausted request key {}",
                self.provider,
                self.scenario,
                request_key
            );
        };
        Ok(exchange)
    }

    fn assert_fully_consumed(&self) -> Result<()> {
        let leftovers = self
            .pending
            .iter()
            .filter_map(|(key, exchanges)| (!exchanges.is_empty()).then_some((key, exchanges)))
            .map(|(key, exchanges)| format!("{key}:{} left", exchanges.len()))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            leftovers.is_empty(),
            "provider fixture {}:{} had unconsumed exchanges: {}",
            self.provider,
            self.scenario,
            leftovers.join(", ")
        );
        Ok(())
    }
}

pub fn provider_wire_fixture_replay_consumes_every_recorded_exchange_once() -> Result<()> {
    let mut replay = ProviderFixtureReplayer::from_file(&fixture_path("synthetic/basic.json"))?;

    let first = replay.next("chat-turn")?;
    assert_eq!(first.ordinal, 0);
    assert_eq!(first.request.method, "POST");
    assert_eq!(first.request.path, "/v1/responses");
    assert_eq!(first.request.body["model"], "fixture-model");
    assert_eq!(first.response.status, 200);
    assert_eq!(
        first
            .response
            .headers
            .get("content-type")
            .map(String::as_str),
        Some("text/event-stream")
    );
    assert!(first.response.body.contains("first"));

    let second = replay.next("chat-turn")?;
    assert_eq!(second.ordinal, 1);
    assert!(second.response.body.contains("second"));

    replay.assert_fully_consumed()
}

pub fn provider_wire_fixture_replay_rejects_unmatched_and_leftover_requests() -> Result<()> {
    let mut replay = ProviderFixtureReplayer::from_file(&fixture_path("synthetic/basic.json"))?;

    let missing = replay
        .next("missing-key")
        .expect_err("unmatched request keys should fail replay");
    assert!(missing.to_string().contains("missing-key"));

    let _ = replay.next("chat-turn")?;
    let leftovers = replay
        .assert_fully_consumed()
        .expect_err("leftover fixture entries should fail replay");
    assert!(leftovers.to_string().contains("chat-turn:1 left"));
    Ok(())
}

fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(PROVIDER_FIXTURE_ROOT)
        .join(relative)
}
