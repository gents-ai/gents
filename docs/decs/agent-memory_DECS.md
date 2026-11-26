# Decisions: agent-memory

Newest first. Each entry: decision, options, why, effect, reversible.

## D10: No tests until the plan is approved
- Decision: hold all test runs and code commits until the operator accepts the
  corrected plan as complete and factual.
- Why: the first plan contained an unverified claim about VM25 and did not yet
  account for DefraDB's embedding boundary or the ACP policy requirement.
- Effect: the branch holds the plan and this log only. Reversible: yes.

## D9: Prose plan and decision log in this repo
- Decision: keep `docs/plans/agent-memory.md` and this log for the loop run.
- Options: keep decisions in chat only; write them under `docs/`.
- Why: the operator requested `vertexia-loop`, whose accountability is the log;
  that explicit instruction supersedes the repo's general "no prose documents"
  rule for this run.
- Effect: two markdown files in the PR. Reversible: yes, delete them.

## D8: Automatic injection point
- Decision: inject a bounded memory section into `loop_config.context_message` in
  `request_frontier`, after the authored prompt is known.
- Options: steering inputs; a new provider-input hook; the session hook.
- Why: the hook cannot change the prompt; steering inputs are user messages; the
  context message is the authored context boundary and is assembled into the
  first provider input.
- Effect: memory is visible on the first turn, bounded, recorded. Reversible:
  yes.

## D7: Hybrid retrieval in gents, not `hybrid_search_dense` in v1
- Decision: run DefraDB BM25 and DiskANN queries, fuse with RRF `k = 60` in
  gents.
- Options: use `db::hybrid_search_dense`; change defradb.rs to accept a
  precomputed query vector; fuse in gents.
- Why: `hybrid_search_dense` embeds the query through an OpenAI/Ollama HTTP
  endpoint (`EmbeddingClientConfig` + `embed_query`). Gents must use the
  in-process embedder, so the helper cannot serve it unchanged. Fusing in gents
  keeps the branch self-contained; `k = 60` matches DefraDB's
  `DEFAULT_RRF_K`.
- Effect: two bounded reads in parallel and one small fusion function. Follow-up:
  add an optional query vector to `DenseHybridSearchRequest` in defradb.rs and
  switch to the single owner. Reversible: yes.

## D6: Memory collection ownership and ACP
- Decision: install `AgentMemory` with
  `@policy(id: "$household", resource: "agent_children")` and an explicit
  `parent` relationship to the `Agent` document.
- Options: an ungoverned application collection; a new Lean policy resource;
  `agent_children` with a parent relationship; the session-scoped `children`
  resource.
- Why: an ungoverned collection is public (ACP is not enforced), which is not
  acceptable for prompt material. `agent_children` already exists with the right
  parent (agents) and permissions; it declares no `create` rule, so it takes no
  `parentField` and the parent is granted separately. This avoids a new Lean
  policy resource while keeping ACP.
- Effect: one collection install plus one relationship grant per document. An
  agent-isolation test is a release gate. Reversible: yes.

## D5: Bundle the model in the binary
- Decision: ship `blink-multilingual-64M-int8` assets in a new
  `gents-memory-embed` crate with `include_bytes!`.
- Options: download at first use; bundle a smaller fallback; bundle the model.
- Why: the operator requires it in the binary and requires offline, local-first
  behavior. The asset is about 68 MB (61 MB int8 weights plus a 7.4 MB
  tokenizer).
- Effect: larger binary and repository; a hakari update. Reversible: yes.

## D4: Embedder
- Decision: `minishlab/blink-multilingual-64M-int8`, 256 dimensions, MIT, 100+
  languages, static, MMTEB Multilingual v2 mean 51.60 and retrieval 47.52.
- Options: potion-retrieval-32M (English only); potion-multilingual-128M (lower);
  bge-small-en-v1.5 (English); Qwen3-Embedding-0.6B (better quality, too large to
  bundle); EmbeddingGemma-300m (better quality, gated license, too large).
- Why: it is the only surveyed option that is multilingual, strong for its size,
  small enough to bundle, and fast enough to run in-process.
- Effect: 256-dimensional DiskANN index; a model change requires a new
  collection. Reversible: yes.

## D3: Vector engine and parameters
- Decision: the disk-based DiskANN engine with cosine, `R 64`, `lBuild 100`,
  `lSearch 100`, `alphaPercent 120`, `m` derived, `sampleBytes 128 MiB`,
  `residentCodes` on. Final values are measured in the measurement phase.
- Options: HNSW, FLAT, IVF_PQ, IVF_FLAT, SSG, DiskANN.
- Why: DiskANN bounds resident memory for a large corpus and is the disk-based
  engine in defradb.rs at the pinned rev.
- Effect: one vector index declaration. Reversible: yes, by rebuilding.

## D2: Use defradb.rs, not Go
- Decision: all storage, vector, BM25 and ACP behavior comes from the pinned
  defradb.rs rev.
- Why: that is the dependency gents builds against.
- Effect: the layout and APIs cited in the plan are `crates/database/...` and
  `crates/foundation/...` at `2e6f673bf5...`. Reversible: no; it is the runtime.

## D1: VM25 reference
- Decision: plan on DiskANN, the disk-based vector index present at the pinned
  rev, and ask for the exact VM25 reference before incorporating a separate
  engine.
- Why: the operator named VM25; the pinned tree and the Go repo do not contain
  that identifier, so a separate engine cannot be wired without its reference.
  DiskANN is the disk-based engine that is present.
- Effect: if VM25 is a different engine, it needs its own decision and likely a
  defradb.rs pin bump. Reversible: yes.
