# Agent memory on DefraDB DiskANN

Status: in progress. Plan slug: `agent-memory`. Branch: `vclq/memory`.

## Problem

Agents persist every `AgentMessage`, but cross-session recall is literal substring
search over message text. There is no semantic store, no embedding, and no
retrieval step, so long-horizon memory benchmarks (AMB, DolphinBench, LoCoMo,
LongMemEval) score at the no-memory baseline. The runtime owns the primitives
(DefraDB vector indexes, BM25, ACP, transactions) but not the pipeline.

## Delivered behavior

1. `help indexes vector diskann` documents the DiskANN engine.
2. A pure-Rust in-process embedder ships in the binary and generates vectors with
   no network and no external service.
3. An application collection `AgentMemory` stores text, metadata and a DiskANN
   vector index, scoped by node, agent and ACP.
4. A `memory` tool ingests and retrieves memory (remember, search, forget, list,
   ingest).
5. Retrieval is hybrid: BM25 and DiskANN, fused by reciprocal rank, deduped and
   bounded.
6. A bounded memory section is injected into the provider context before each
   turn, behind config, with the model id and ranking recorded.
7. Measurements: embed latency and throughput, search latency, DiskANN recall
   against FLAT, hybrid vs vector-only recall.

## Decisions

- Embedder: `minishlab/blink-multilingual-64M-int8` (MIT, 256d, 100+ languages,
  static, MMTEB mean 51.60, retrieval 47.52). Rejected: potion-retrieval-32M
  (English only), potion-multilingual-128M (lower), bge-small (English), Qwen3-0.6B
  (too large to bundle), EmbeddingGemma (gated license).
- Pure Rust: `model2vec-rs` with `fancy-regex` and `local-only`, weights in the
  binary via `include_bytes!`.
- DiskANN (FreshVamana) is the vector engine. No VM25 exists in the pinned
  defradb.rs rev or in the Go defradb repo.
- Application collection installed by gents with the household `children`
  policy; no new core document type and no new Lean ACP model.
- Hybrid retrieval, no cross-encoder reranker in v1. A reranker is deferred until
  recall measurement shows a gap.
- Tool plus automatic injection. Injection happens in `request_frontier`, before
  the first request entry, bounded by tokens.

## Phases

| # | Phase | Status |
|---|---|---|
| 0 | Worktree, plan, decision log, WIP PR | in progress |
| 1 | Expose DiskANN in schema help and discovery | pending |
| 2 | `gents-memory-embed`: pure-Rust embedder + bundled int8 model | pending |
| 3 | `AgentMemory` collection, install, store, ACP | pending |
| 4 | `memory` tool: remember, search, forget, list, help | pending |
| 5 | Ingestion: explicit and bounded session ingest | pending |
| 6 | Hybrid retrieval: BM25 + DiskANN RRF | pending |
| 7 | Automatic injection in the provider context | pending |
| 8 | Measurement and recall gate | pending |
| 9 | Gate, verify, document results | pending |

## Phase detail

### 0. Setup

`make worktree BRANCH=vclq/memory BASE=origin/main`, decision log, draft PR.
Commit and push at each phase boundary.

### 1. DiskANN exposure

Add `indexes vector diskann` to `help::PATHS`, a page in
`crates/gents-tools/src/builtin/schema_help/help/indexes.rs`, extend the
`indexes vector` discovery line, and cover the page in the existing parse tests.

### 2. Embedder

New leaf crate `gents-memory-embed`:
- `Embedder` trait: `model_id`, `dimensions`, `embed(&[String]) -> Vec<Vec<f32>>`.
- `StaticEmbedder` over `model2vec_rs::model::StaticModel` loaded from bytes.
- Bundled asset: `blink-multilingual-64M-int8` config, tokenizer and safetensors,
  with the model license.
- `spawn_blocking` at call sites; no second runtime.
- Tests: deterministic vectors for a fixed sentence, dimension and normalization.

### 3. Memory collection

`AgentMemory` SDL with:
- `memory_key` unique, `node_did`, `agent_did`, `session_id`, `source`, `kind`,
  `text`, `embedding: [Float32!] @index(vector: {dimensions: 256, diskann: {...}})`,
  `content_hash`, `model_id`, `created_at`, `last_used_at`, `metadata`.
- composite unique index on `agent_did` and `content_hash`.
- `@fulltext` on `text` for BM25.
- household `children` policy rendered with `HomePolicyIds`.
- install owner: preview and apply through `ConfigAccess`; idempotent.

Tests: install, write, read back, nearest neighbor (exact below training
threshold), agent isolation.

### 4. Tool

`gents-tools/src/memory/` with `help`, `remember`, `search`, `forget`, `list`.
`search` embeds the query, runs hybrid retrieval, returns bounded snippets with
scores and ranking mode. Guided failures; no unbounded scans.

### 5. Ingestion

`remember` writes one record. `ingest` pages `AgentMessage` for a session,
chunks, embeds in batches, dedupes by `content_hash`, checkpoints a cursor and
reports counts. Bounded page size and batch size.

### 6. Hybrid retrieval

BM25 and DiskANN candidates in parallel, reciprocal rank fusion (k = 60),
dedupe by `memory_key`, bounded candidate and result counts. Shared query builder
with `defra_query`; no second GraphQL interpreter.

### 7. Automatic injection

In `request_frontier`, after the authored prompt is known, retrieve a bounded
memory section and append it to the context message. Config: enabled, top_k,
char and token budget. Timeout bounded by the request deadline. Record model id
and ranking mode in the injected section, never memory text in logs.

### 8. Measurement

- Embed latency p50/p95 and texts/s for the bundled model.
- Search latency p50/p95 for BM25, DiskANN, hybrid.
- Recall@10 of DiskANN vs FLAT on a fixed corpus.
- Recall@10 of hybrid vs vector-only.
- A recall gate test; a reranker decision recorded from the numbers.

### 9. Gate

`make gate-plan`, the selected jobs, `fmt`, `lint`, `contract` for the new crate
and docs. Report exact commands and results.

## Deferred

- Cross-encoder reranker: only if measured recall demands it.
- P2P memory replication: inherited from DefraDB, not designed here.
- LLM summarization/extraction on ingest: not in v1.

## Risks

- DiskANN is approximate after training and cosine only; recall gate required.
- Static embeddings trade quality for speed; hybrid BM25 and measurements hold it.
- 68 MB asset grows the binary; measured and documented.
- Application-collection policy must be proven by an ACP isolation test.
- Memory text is prompt material; same access boundary as the session, no PII in
  logs.
