# Decisions: agent-memory

Newest first. Each entry: decision, options, why, effect, reversible.

## D8: Prose plan and decision log in this repo
- Decision: write `docs/plans/agent-memory.md` and this log for the loop run.
- Options: keep decisions in chat only; write them under `docs/`.
- Why: the operator requested `vertexia-loop`, whose accountability is the log;
  the repo's "no prose documents" rule is superseded for this run by that
  explicit instruction.
- Effect: two markdown files in the PR. Reversible: yes, delete them.

## D7: Automatic injection point
- Decision: inject a bounded memory section into `loop_config.context_message` in
  `request_frontier`, after the authored prompt is known.
- Options: steering inputs; a new provider-input hook; the session hook.
- Why: the hook cannot change the prompt; steering inputs are user messages; the
  context message is already the authored context boundary and is assembled into
  the first provider input.
- Effect: memory is visible on the first turn, bounded, and recorded. Reversible:
  yes.

## D6: Hybrid retrieval, no reranker in v1
- Decision: BM25 plus DiskANN, reciprocal rank fusion, top-k. Add a reranker only
  if the recall measurement shows a gap.
- Options: vector only; hybrid plus rerank.
- Why: the benchmark shape mixes exact names with semantics; hybrid covers both,
  and a reranker adds a second model and unbounded cost before measurement.
- Effect: two candidate queries and a fusion step. Reversible: yes.

## D5: Memory collection ownership
- Decision: install `AgentMemory` as an application collection with the household
  `children` policy, scoped by `agent_did` and proven by an ACP test.
- Options: a core runtime schema with a new Lean ACP model; a pack-carried
  collection; an unauthenticated application collection.
- Why: reuses the existing policy resource and the application-collection install
  path, without adding a core document type or a second authorization layer.
- Effect: a collection installed by gents at first use. Reversible: yes.

## D4: Bundle the model in the binary
- Decision: ship `blink-multilingual-64M-int8` assets inside a new
  `gents-memory-embed` crate with `include_bytes!`.
- Options: download at first use; bundle a smaller fallback; bundle the model.
- Why: the operator requires it in the binary and the operator requires offline,
  local-first behavior. Total asset is about 68 MB (61 MB int8 weights, 7.4 MB
  tokenizer).
- Effect: larger binary and repository. Reversible: yes, move to a pack.

## D3: Embedder
- Decision: `minishlab/blink-multilingual-64M-int8`, 256 dimensions, MIT, 100+
  languages, static, MMTEB mean 51.60 and retrieval 47.52.
- Options: potion-retrieval-32M (35.06 retrieval, English only);
  potion-multilingual-128M (47.67 mean, retrieval 41.22); bge-small-en-v1.5
  (English, 51.68); Qwen3-Embedding-0.6B (best quality, too large to bundle);
  EmbeddingGemma-300m (gated license).
- Why: it is the only option that is multilingual, high quality for its size,
  small enough to bundle, and fast enough to run in-process.
- Effect: 256-dimensional DiskANN index; a model change requires a new collection.
  Reversible: yes, via a new collection and a new version.

## D2: Vector engine
- Decision: DiskANN (FreshVamana) with cosine, R 64, LBuild 100, LSearch 100,
  alphaPercent 120, M derived, sampleBytes 128 MiB, residentCodes on.
- Options: HNSW, FLAT, IVF_PQ, IVF_FLAT, SSG, DiskANN.
- Why: DiskANN bounds resident memory for a large corpus and is the engine the
  operator named. Parameters are measured in phase 8, not hand-final.
- Effect: vector index declaration. Reversible: yes, by rebuilding the index.

## D1: VM25 does not exist in the available DefraDB
- Decision: proceed on DiskANN and record the discrepancy.
- Evidence: the pinned rev `2e6f673bf5...` has `crates/database/db/src/index/
  vector/engine/diskann/` and `EngineKind` lists HNSW, FLAT, IVF_PQ, IVF_FLAT,
  SSG, DiskANN. A shallow clone of the Go `sourcenetwork/defradb` repo has no
  `vm25` or `vamana` identifier either.
- Options: wait for a VM25 reference; proceed on DiskANN.
- Why: DiskANN is FreshVamana and is present at the pinned rev.
- Effect: if VM25 is a specific branch or a Go-only feature, it needs a rev bump
  and a new decision. Reversible: yes.
