# Decisions: agent-memory

Newest first. Each entry: decision, options, why, effect, reversible.

## D15: Measurement deferred to the benchmark harness
- Decision: phase 7 is deferred; the branch ships the retrieval path and the
  unit tests. DefraDB's own `vector_recall_gate` covers engine recall.
- Options: build a synthetic recall corpus in this branch; defer to the harness.
- Why: a meaningful hybrid-vs-vector and DiskANN-vs-FLAT lift needs the AMB or
  DolphinBench corpus, which is the harness phase. A synthetic micro-corpus would
  not support a benchmark claim.
- Effect: no benchmark number is claimed here. The plan records the deferral.
  Reversible: yes.

## D14: Tool always advertised; injection once per request
- Decision: the memory tool is added for every managed agent, and the bounded
  memory section is injected in `request_frontier` once per request.
- Options: a new config field; gate behind session history; inject per turn.
- Why: no config document churn in v1; `request_frontier` is the single place
  that builds the provider context, and its context message persists across the
  request's turns.
- Effect: one extra tool and one retrieval per request. Reversible: yes.

## D13: `children` policy and session parent, not `agent_children`
- Decision: the collection uses `@policy(resource: "children")` and publishes its
  `parent` relationship to the session document.
- Options: `agent_children` with an agent parent; an ungoverned collection.
- Why: `agent_children` has no wired parent publication for a new collection in
  this tree; the session `children` path is already published and gives the agent
  the same ACP boundary as its messages. An ungoverned collection is public.
- Effect: memory documents are session children, filtered by `agent_did`; the
  agent reads across its sessions through the session read permission. An
  isolation test is the remaining release gate. Reversible: yes.

## D12: Escaped GraphQL strings for memory queries
- Decision: memory reads use `transact_local_readonly` with escaped GraphQL
  strings, marked `vertexia:` in the code.
- Options: `gents_protocol::document_query` typed fields.
- Why: it follows the existing `session_history` owner and avoids a parallel typed
  collection definition before the schema stabilizes.
- Effect: one `vertexia:` shortcut to move to `document_query` later. Reversible.

## D11: Session ingest reads `AgentMessage.blocks` directly
- Decision: `ingest` extracts text from the stored `blocks` JSON, marked
  `vertexia:`.
- Options: the canonical transcript renderer.
- Why: the transcript renderer is bounded to six messages and 8000 characters
  per chunk, which is wrong for bulk ingest; direct extraction is bounded by the
  ingest page and chunk sizes.
- Effect: a shortcut to switch when a bounded plain-text projection exists.
  Reversible.

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
