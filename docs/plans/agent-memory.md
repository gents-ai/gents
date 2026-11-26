# Agent memory on DefraDB (DiskANN + BM25)

Status: plan under review. Plan slug: `agent-memory`. Branch: `vclq/memory`.
No implementation tests will run until this plan is approved as complete.

## Goal

Give an agent cross-session memory: an in-process multilingual embedder, a
DefraDB collection with a DiskANN (disk-based) vector index and a BM25 full-text
index, an ingest and retrieval path, a `memory` tool, and bounded automatic
context injection. Target long-horizon memory tasks (AMB, DolphinBench, LoCoMo,
LongMemEval) without a remote embedding service.

## Verified facts (pinned DefraDB rev `2e6f673bf5...`, 2026-11-25)

- gents pins defradb.rs at `2e6f673bf5229b3fbb98a5ee2afae8ecdd8e6606`
  (`Cargo.toml`, `Cargo.lock`). This is the Rust runtime. Go `defradb` is not used.
- DiskANN is the disk-based vector engine. Code:
  `crates/database/db/src/index/vector/engine/diskann/` (mod, build, codec, graph,
  merge, pending_cache, params). `EngineKind::DiskAnn` is in
  `crates/database/db/src/index/vector/engine/ann.rs:144`.
- DiskANN SDL is `@index(vector: {dimensions: N, diskann: {...}})`. Accepted keys:
  `R`, `lBuild`, `lSearch`, `alphaPercent`, `m`, `sampleBytes`, `residentCodes`
  (`crates/engine/query/tests/vector_index_sdl.rs:547`). `alg: DISKANN` selects
  defaults. Defaults are `R 64`, `lBuild 100`, `lSearch 100`, `alphaPercent 120`,
  `m 0` (derived), `sampleBytes 128 MiB`, `residentCodes false`
  (`crates/foundation/schema/src/index.rs`, `DiskAnnParams`).
- DiskANN accepts `COSINE` only; `EUCLIDEAN` and `DOT` are refused at definition
  time (`supports_metric`, same file). Below `TRAIN_THRESHOLD = 1024`
  (`diskann/params.rs:7`) the index answers by exhaustive scan; the first write
  at the threshold trains a product quantizer and builds a FreshVamana graph.
  After training, inserts go to a durable pending set with an in-memory cache,
  and searches walk the graph, scan pending codes, and re-rank.
- BM25 is a first-class DefraDB index: `@fulltext(language, k1, b)`
  (`FullTextIndexDescription` in `crates/foundation/schema/src/index.rs`), and
  queries rank with `BM25(query: "...", fields: [...])`.
- DefraDB also has a Rust hybrid helper:
  `DenseHybridSearchRequest` and `hybrid_search_dense` in
  `crates/database/db/src/search/dense.rs`, exported from `db/src/lib.rs`. It
  issues a BM25 query and a dense query and fuses them with reciprocal rank
  fusion, `DEFAULT_RRF_K = 60.0`.
- That helper embeds the query itself through `EmbeddingClientConfig` and
  `embed_query`, which call an OpenAI-compatible or Ollama HTTP endpoint
  (`crates/database/db/src/search/dense.rs`, `search/config.rs`,
  `search/embedding.rs`). It has no input for a precomputed query vector, so an
  in-process embedder cannot use it unchanged.
- A collection with no `@policy` is public: ACP is not enforced
  (`crates/database/db/src/collection/acp.rs:59-77`, `116-127`). Memory must carry
  a policy.
- `parentField` is required exactly when a policy resource declares a `create`
  rule (`crates/database/db/src/access/dac/mod.rs:231`). The household
  `agent_children` resource (parent agents; read `parent->session_read`, update
  and delete `parent->session_admin`) has no `create` rule, so its documents take
  no `parentField`; the `parent` relationship is granted separately
  (`crates/gents-schemas/schemas/agent/agent_ownership_policy_v2.yaml:314`).
- Handshake points in gents: `ConfigAccess::add_collection`
  (`crates/gents-store/src/config_client/mod.rs:1092`), reads through
  `graphql_with_transaction_retry` (`crates/gents-store/src/graphql.rs:172`),
  policy placeholder rendering through `HomePolicyIds::render`
  (`crates/gents-schemas/src/policy_ids.rs`), and the pre-turn context message at
  `AgentDaemon::request_frontier`
  (`crates/gents/src/runtime/agent/daemon/inference/prepare.rs:427`).

## Embedder

`minishlab/blink-multilingual-64M-int8` (MIT): 64M parameters, 256 dimensions,
100+ languages, normalized, static Model2Vec. Published MMTEB Multilingual v2
mean 51.60, retrieval 47.52, MNanoBEIR 41.18. Asset size: 64,000,088 bytes
int8 safetensors, 7,417,320 bytes tokenizer, 84 bytes config (about 68 MB).
Run through `model2vec-rs` 0.5.1 with `fancy-regex` and `local-only` for a pure
Rust, offline build; weights embedded with `include_bytes!`.

This is a static model: faster than a transformer, with lower retrieval quality.
Hybrid BM25 and the measurement phase are how the plan holds that trade-off
honestly. Rejected: `potion-retrieval-32M` and `bge-small-en-v1.5` (English
only), `potion-multilingual-128M` (lower quality), `Qwen3-Embedding-0.6B` and
`EmbeddingGemma-300m` (too large to embed in the binary; Gemma also has a gated
license).

## Design

- New leaf crate `gents-memory-embed`: the bundled model and an `Embedder` trait
  (`model_id`, `dimensions`, `embed(batch)`); calls run on `spawn_blocking`, no
  second runtime.
- Collection `AgentMemory`, installed by gents at first use as an application
  collection with `@policy(id: "$household", resource: "agent_children")` and an
  explicit `parent` relationship to the `Agent` document. It is not a new Lean
  policy resource. Fields: `memory_key`, `node_did`, `agent_did`, `session_id`,
  `source`, `kind`, `text` (`@fulltext`), `embedding: [Float32!]` with
  `@index(vector: {dimensions: 256, diskann: {...}})`, `content_hash`,
  `model_id`, `created_at`, `last_used_at`, `metadata`, plus a composite unique
  index on `agent_did` and `content_hash`.
- Retrieval: BM25 and DiskANN candidates, fused in gents with RRF `k = 60`
  (matching DefraDB's constant), deduped and bounded. Two reads in parallel. We
  do not use `hybrid_search_dense` in v1 because it cannot take the in-process
  query vector; switching to it needs a defradb.rs input for a precomputed
  vector, which is out of scope for this branch and recorded as a follow-up.
- Tool `memory`: `help`, `remember`, `search`, `forget`, `list`, `ingest`.
  Ingestion pages `AgentMessage`, chunks within a token budget, embeds in
  batches, dedupes by `content_hash`, and checkpoints a cursor. Bounded.
- Automatic injection: in `request_frontier`, after the authored prompt is
  known, retrieve a bounded memory section and append it to the context message.
  Config: enabled, top_k, char and token budget. No memory text in logs.
- Observability: tracing counters and histograms for embed latency, search
  latency, candidate counts, and fusion; model id and ranking mode recorded.

## Phases and status

| # | Phase | Status |
|---|---|---|
| 0 | Worktree, factual plan, decision log, WIP PR | done |
| 1 | Expose DiskANN in schema help and discovery | pending |
| 2 | `gents-memory-embed` crate, bundled int8 model, hakari update | pending |
| 3 | `AgentMemory` collection, policy parent, install, ACP isolation | pending |
| 4 | `memory` tool and bounded ingestion | pending |
| 5 | Hybrid BM25 + DiskANN retrieval with RRF k=60 | pending |
| 6 | Bounded automatic context injection | pending |
| 7 | Measurement: embed/search latency, DiskANN recall vs FLAT, hybrid lift | pending |
| 8 | Gate, verification, PR description with results | pending |

Tests are written with each phase and run only after this plan is approved as
complete. Each phase ends with a commit and push to `vclq/memory`.

## Open points already decided

- Use defradb.rs, not Go.
- Use the disk-based DiskANN engine and DefraDB BM25.
- Keep the branch self-contained: gents-side RRF instead of a defradb.rs API
  change for a precomputed query vector.
- `agent_children` policy with an explicit parent relationship; prove agent
  isolation.
- One bundled multilingual static model; no optional quality model in v1.

## Deferred

- `hybrid_search_dense` with a precomputed query vector (needs a defradb.rs
  change and a pin bump).
- Cross-encoder reranker; only if measured recall demands it.
- LLM summarization or extraction on ingest.
- P2P memory replication beyond whatever DefraDB already provides.

## Risks

- Static embeddings are weaker than transformers; hybrid retrieval and the
  measurement phase bound the risk.
- DiskANN is approximate after 1024 vectors and cosine only; a recall gate
  against FLAT is required.
- 68 MB asset increases binary and repository size.
- ACP correctness depends on the `agent_children` parent relationship; an
  isolation test is a release gate.
- If `VM25` names a different disk-based engine than DiskANN, it is not in the
  pinned tree; the exact reference is needed before it can be used.
