# Fact Record Durability Implementation Plan

Date: 2026-08-06
Issues: #840 (durable rendered inputs), #523 (verified reconstruction)

## Goal

Make every provider call reproducible from durable data. Persist the exact
rendered request before it is sent, encrypt it to the session participants,
surface it through trace projections, and then build a second time-travel
projection whose output is verified against that captured request.

This plan is the prerequisite for the live multi-turn resolver test in Plan 2.
Plan 2 still owns `tool_call_key`, `read_tool_result`, and executable trimming
stubs.

## Rebase note (2026-08-06)

Rebased onto `origin/main` at `fa2d7a78`, which merged **#988** — ATIF v1.7 is
now a first-class adapter projection (`crates/gents/src/adapter_projection/atif.rs`),
so there are four projections to surface captured requests through, not three.
Also landed: GraphQL identifier and filter-fragment validators at the trust
boundary (#1034), protocol mutation input-key validation, and the compaction
fixes that were stacked on #988. Those compaction fixes matter for
reconstruction: message-window behavior changed under them, so read the current
`drop_compacted_prefix` rather than any pre-#988 description of it.

Housekeeping unblocked by the same merge: issues #1015–#1019, #1031, and #1032
were fixed on the #988 branch and could not auto-close across bases. They are
closeable now.

## Corrections made during review

The previous plan was not implementation-safe. This revision makes the
following decisions explicit:

- A hash-only envelope is not a durable rendered input. `RenderedRequest`
  stores the canonical full `request_json`; hashes and CIDs are provenance.
- Verification compares the complete provider request, not only messages.
  `tools_json`, wire shape, tool choice, sampling, and provider normalization
  all matter.
- "Canonical-JSON exact" is the promised fidelity. Do not say "byte exact"
  unless the serialized HTTP body bytes are captured and hashed too.
- #523's issue text currently says "the exact bytes that were sent." That is a
  stronger contract than this plan's canonical-JSON equality. Before claiming
  #523 complete, either amend the issue/acceptance language to semantic JSON
  fidelity or add transport-body capture after every provider-specific body
  rewrite and before the HTTP send. The latter must persist the exact UTF-8
  body (or bytes) and prove the transport forwarded those same bytes; merely
  serializing `request_json` a second time is not that proof.
- Retry attempts are separate facts. Repair can rewrite assembled input without
  changing the transcript, so attempt 0 and attempt 1 may legitimately differ.
- The record is unique by
  `(agent_did, session_id, request_id, turn_index, attempt)` and idempotent.
  Reusing that key with a different canonical value is an integrity error,
  never an update.
- Capture is fail-closed and occurs before `model.stream`: no provider call is
  allowed without its durable fact record.
- #840's encryption requirement is not weakened. `AgentMessage` is not an
  encryption precedent; its schema is currently plaintext. The new row must be
  document-encrypted and ACP-readable only by the agent owner and non-empty
  requester DID.
- Adding an SDL constant does not register a collection. The migration
  registry, protocol schema lists, branchable lists, and replication profiles
  are in scope.
- The old plan omitted `InferenceProfile` and `InferenceBackend`, both of which
  affect the request. Verified reconstruction cannot begin until all source
  document ids survive runtime resolution and all required collections have a
  real upgrade path to branchability.
- An empty CID map is not a validity signal. Reconstruction reports explicit
  `CapturedOnly`, `Unavailable`, `UnsupportedManifest`, and `ValueMismatch`
  outcomes.
- The first end-to-end test is multi-turn and retry-aware. A one-turn `READY`
  test cannot establish reproducibility.
- Token usage and trigger lineage are useful timeline work but do not prove
  rendered-input durability. They remain separate from this plan unless a
  projection contract requires them.

## Global constraints

- Follow the foundation flow: Lean model, generated conformance cases, Rust.
- Always apply `crate::graphql::escape_graphql_string()` to interpolated
  GraphQL **string-literal** values.
- For anything interpolated as a bare **identifier** — collection names, field
  names — use `validate_collection_identifier` / `validate_graphql_name`, and
  use `validate_graphql_filter_fragment` for filter fragments. These landed on
  main in #1034 (`crates/gents/src/graphql.rs:6-20`, re-exported from
  `gents-protocol`). `escape_graphql_string` cannot defend an identifier
  position; validation is the only defense there. This applies directly to any
  reconstructor query that selects across collections or fields by name.
  `_commits` takes `fieldName` inside a filter, where it is a string literal and
  must be escaped, not treated as an identifier.
- Never emit `[]` in a DefraDB mutation; use `null` for an absent list.
- Treat GraphQL response errors as errors. A missing `data` field is not the
  same as "no rows".
- Run `cargo test -p gents`, not `--lib`.
- Run `cargo check --workspace --all-targets` before handoff.
- Use `tracing`, never `println` outside explicit migration pin-authoring tests.
- New integration-test files must be registered in
  `crates/gents/tests/e2e_runtime.rs`.
- Existing migration baseline SDL and VersionID pins are immutable. New
  collections use `AddCollection`; changes to existing collections use real
  migration steps and authored destination pins.

## Durable contract

For each invocation of the provider boundary, exactly one durable row exists
first:

```text
capture_key = "rendered:v1:" + sha256(canonical_json([
    agent_did, session_id, request_id, turn_index, attempt
]))

assembled request
      |
      v
encrypted durable RenderedRequest(capture_key, request_json)
      |
      v  DefraDB writes a content-addressed field commit for request_json
      v
provider call
```

The persisted JSON is the conformance oracle. The later materialized-view path
must reproduce the same canonical JSON, but capture remains readable even when
a future runtime no longer supports an old provenance-manifest version.

### Integrity comes from the database, not from a column

Do not store a `request_hash`. A stored digest is self-attested: the runtime
computes it and writes it, so a buggy or dishonest writer produces a column
that agrees with whatever bytes it chose to persist. It proves nothing an
auditor could not have been lied to about.

Branchable collections produce collection, composite, **and per-field** commit
blocks. `request_json` therefore already has a content address computed by
DefraDB over the field block that was actually stored:

```graphql
query {
  _commits(
    docID: ["<rendered-request-doc>"]
    filter: { fieldName: { _eq: "request_json" } }
    order: [{ height: DESC }]
    limit: 1
  ) {
    cid
    height
    signature { type identity value }
  }
}
```

That CID is the content-integrity and version witness. It replicates with the
document instead of being a column that can drift from the value it describes,
and it is the artifact forthcoming Merkle-DAG proofs will attest over — a
stored SHA-256 column would be a dead end those proofs cannot reach.

Do **not** assume the commit is signed. In the pinned DefraDB,
`Commit.signature` is nullable and may report ES256K, ES256, EdDSA, or BLS.
Gents' normal embedded-node builders currently leave node block signing
disabled. The runtime's `AgentIdentity` signs other protocol artifacts, but the
current `EmbeddedNode::execute` path does not by itself make every document
commit an agent-signed commit. A signature, when present and verified, adds
authenticity; it is not required for canonical-request equality. If #840 is
intended to require authenticated authorship as well as reproducibility, add an
explicit execution gate to configure principal-bound commit signing and reject
missing or invalid signatures. Do not silently infer that property from a CID.

Verification therefore needs no hash in the loop at all. Reconstruction
canonicalizes its output and compares it to the stored `request_json`
directly. Equality is the reproducibility check; the field CID anchors the
stored version, and a verified optional signature may additionally authenticate
its writer.

## Target schema

The final names may follow existing conventions, but the semantics are fixed:

```graphql
type RenderedRequest @branchable @policy(id: "<pinned-policy-id>", resource: "RenderedRequest") {
    capture_key: String @index(unique: true) @immutable
    request_id: String @index @immutable
    session_id: String @index @immutable
    agent_did: String @index @immutable
    requester_did: String @index @immutable
    behavior_id: String @index @immutable
    turn_index: Int @immutable
    attempt: Int @immutable
    capture_version: Int @immutable
    model_name: String @immutable
    source: String @immutable
    request_json: String @immutable
    prompt_hash: String @index @immutable
    tools_hash: String @index @immutable
    provenance_json: String @immutable
    created_at: DateTime @index @immutable
}
```

`request_json` contains the exact `Value` produced by
`llm::rig_compat::provider_request_json`, serialized canonically. Component
values do not need duplicate columns because they are derivable from
`request_json`.

`prompt_hash` and `tools_hash` are retained **only as query indexes** — finding
every capture sharing a tool surface, dedup, and prefix-stability analysis for
#723. They are explicitly *not* the integrity mechanism; see the section above.
`prompt_hash` covers `messages` for Chat Completions and the complete
`instructions` + `input` prompt surface for Responses; hashing `input` alone
would collapse distinct Codex system prompts.
Anything that treats them as proof of content is a bug. There is deliberately
no `request_hash`: the field commit for `request_json` is the content address,
and duplicating it in a column would create a second source of truth that can
silently disagree with the first.

`provenance_json` is a versioned object, never an overloaded empty-map signal.

---

## Task 1: Fence persist-before-send in Lean and conformance

**Files:**

- Create: `crates/gents/proofs/Proofs/RenderedCapture.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/Contracts.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean`
- Create: `crates/gents/tests/conformance/rendered_capture.rs`
- Modify: `crates/gents/tests/conformance/structure.rs`

- [ ] Model a capture key, an opaque canonical request value, and three stages:
  `Assembled`, `DurablyCaptured`, `Sent`.
- [ ] Make `capture` the only legal predecessor of `send`.
- [ ] Model idempotent recapture of the same `(key, request)` and rejection of
  the same key bound to a different request.
- [ ] Prove `sent_implies_durably_captured` and
  `capture_key_determines_request` with zero `sorry`s.

  State the second theorem over the request **value**, not over a stored
  digest. The model does not need a hash function: the property is that one
  capture key is bound to at most one canonical request, which is what
  idempotency and integrity both rest on. DefraDB supplies the content address
  for the persisted value as a field commit, so introducing a modeled
  hash column would add an unmodeled trust assumption — that the writer
  computed it honestly — to a theorem that does not need one.
- [ ] Emit positive and negative transition cases through the existing contract
  JSON and consume them from Rust.
- [ ] Add the domain to the coverage ledger and structure test.
- [ ] Run:

  ```bash
  cd crates/gents/proofs && lake build
  cargo test -p gents --test conformance rendered_capture
  ```

The implementation is not allowed to choose fail-open capture after this task;
that would violate the modeled transition order.

---

## Task 2: Establish encrypted participant access before adding the collection

**Files:**

- Create: `crates/gents/src/rendered_request/access.rs`
- Create: `crates/gents/tests/e2e_runtime/rendered_request_access.rs`
- Modify: `crates/gents/tests/e2e_runtime.rs`
- Modify the runtime/bootstrap ACP module selected by the investigation.

The pinned DefraDB supports `encrypt: true` / `encryptFields` and gates key
delivery through document ACP. A plaintext collection with identity columns is
not sufficient.

- [ ] Define one stable policy resource for `RenderedRequest` with `read`,
  `update`, and `delete` permissions; only the owner can update/delete, and a
  `reader` relation grants read.
- [ ] Make policy YAML part of the runtime source and assert that its
  content-derived policy id equals the id embedded in the schema. Install or
  validate that policy before the migration engine registers the collection.
- [ ] Use the real embedded-node ACP store used by GraphQL. Do not instantiate
  an unrelated in-memory Zanzibar store as the product implementation.
- [ ] Prove in an embedded-node test that a document created with encryption is
  readable/decryptable by its owner, becomes readable by the requester after a
  `reader` relationship is granted, and remains invisible to an unrelated DID
  and anonymous access.
- [ ] Prove the intended lookup paths still work: authorized equality lookup by
  `capture_key` and authorized session/turn timeline scan. If whole-document
  encryption makes ordinary indexed lookup unusable, leave only routing/index
  metadata plaintext and field-encrypt `request_json` plus `provenance_json`,
  or deliberately add DefraDB searchable-encryption indexes. Do not discover
  this incompatibility after making capture default-on.
- [ ] Prove requester revocation removes both read authorization and decryption
  ability.
- [ ] Define the empty-requester rule: only the agent owner is a participant;
  never create a relationship for an empty DID.
- [ ] If production `EmbeddedNode` does not expose the ACP relationship seam,
  add the smallest upstream-compatible adapter in `defra-node`; do not replace
  this task with application-side filtering.

**Hard gate:** Do not proceed to a default-on sink until this test passes. If
the policy cannot be installed before schema registration on both fresh and
existing nodes, #840 is blocked and must not be marked complete.

---

## Task 3: Add `RenderedRequest` through the real schema/migration surfaces

**Files:**

- Create: `crates/gents-schemas/schemas/agent/rendered_request.graphql`
- Modify: `crates/gents-schemas/src/lib.rs`
- Modify: `crates/gents-protocol/src/schemas.rs`
- Modify: `crates/gents-protocol/src/row.rs`
- Modify: `crates/gents-migration/src/registry.rs`
- Modify: migration tests under `crates/gents-migration/tests/`
- Modify: branchable/replication profiles under
  `crates/gents/src/agent/p2p_reconcile/`
- Modify any desktop collection-name expectation affected by the protocol list.

- [ ] Add the schema from the target contract, including `capture_key`'s unique
  index, immutable facts, `@branchable`, and the pinned ACP directive.
- [ ] Export `RENDERED_REQUEST` and `RENDERED_REQUEST_NAME` through both schema
  crates and add them to `ALL`, `ALL_COLLECTION_NAMES`, and the branchable
  collection list in matching order.
- [ ] Add a protocol row type with strict required fields. Only legacy/epoch
  fields may deserialize as `Option`; do not turn core facts into defaults that
  hide malformed rows.
- [ ] Add an `AddCollection` migration step after the frozen baseline. Author
  and pin its VersionID using the migration crate's chain-replay workflow.
- [ ] Test fresh install, upgrade from the current frozen baseline,
  crash/resume, and idempotent second ensure.
- [ ] Add the collection to the same replication profiles that carry
  `AgentMessage`/`AgentRequest`. Add assertions to the existing profile tests;
  do not update string lists without coverage.
- [ ] Run:

  ```bash
  cargo test -p gents-schemas
  cargo test -p gents-protocol
  cargo test -p gents-migration
  cargo test -p gents agent::p2p_reconcile
  ```

Changing the three config SDL files in place is deliberately not part of this
task: doing so would invalidate the migration baseline pins.

---

## Task 4: Make the capture DTO complete and canonical

**Files:**

- Modify: `crates/gents/src/rendered_request.rs`
- Modify: `crates/gents/src/llm/rig_compat.rs`
- Add focused unit tests in those modules.

- [ ] Add `capture_key`, `requester_did`, `capture_version`, and
  `provenance_json` to `RenderedCompletionRequest`. No `request_hash` — see
  "Integrity comes from the database, not from a column".
- [ ] Derive `capture_key` from a canonical tuple containing agent, session,
  request, turn, and attempt. Do not concatenate an unescaped delimiter format,
  and do not assume `AgentRequest.request_id` is globally unique (its schema
  index is not unique).
- [ ] Keep `request_json` in the DTO. Do not remove it after calculating the
  component query hashes.
- [ ] Make one canonical JSON encoder the source of both persisted bytes and
  hashes. Expose it as `pub(crate)`; do not maintain a second implementation in
  the sink or reconstructor.
- [ ] Retain component hashes for messages/input and tools as query indexes
  only. Do not compute a whole-request digest: the field commit for
  `request_json` is the content address, and a second one would create a
  source of truth that can disagree with it.
- [ ] Extend the capture seam to carry an `AssemblyTrace` (or equivalent typed
  data) alongside the final `CompletionRequest`. After #988,
  `build_budgeted_request` may invoke the model-backed `turn_compactor` and
  inject a request-local continuation checkpoint. That checkpoint is persisted
  as `ProviderContextReduction`, not `AgentCompactionEntry`, and its key is
  carried by `AssemblyTrace` for #523 provenance.
  Record whether dynamic output clamping and per-turn compaction occurred, the
  effective compacted provider messages/checkpoint, and the pre/post budget
  estimates. Never re-run the summarizer during reconstruction and expect the
  same words.
- [ ] Include `requester_did` in `RenderedRequestContext::for_request`.
- [ ] Define `capture_version = 1` and a typed, versioned provenance manifest.
  Version 1 may begin as `CapturedOnly`; it must not serialize missing fields as
  evidence of reconstructibility.
- [ ] Test both wire sources (`openai_responses`,
  `openai_chat_completions`), responses normalization, empty tools/tool choice,
  and stable object-key ordering.
- [ ] Add a round-trip test asserting that parsing persisted `request_json`
  gives exactly the `Value` passed to the mock provider converter.

Run: `cargo test -p gents rendered_request`

---

## Task 5: Implement an encrypted, idempotent, fail-closed sink

**Files:**

- Create: `crates/gents/src/rendered_request/sink.rs`
- Modify: `crates/gents/src/rendered_request.rs`
- Modify: `crates/gents/src/agent.rs`
- Modify: `crates/gents/src/agent/daemon/inference.rs`
- Add sink unit tests and fault-injection integration tests.

- [ ] Build GraphQL with the shared escape helper for every string.
- [ ] Apply the encryption mode proven in Task 2. Encrypt at least
  `request_json` and `provenance_json`; encrypt the whole document only if the
  required idempotency and timeline lookups were proven to work. Document
  precisely which routing/index metadata remains visible.
- [ ] Create/register the ACP object as the agent principal, then grant the
  requester `reader` when present. A partially completed owner-only row is
  recoverable: retry relationship creation before permitting send.
- [ ] Enforce idempotency by `capture_key`:

  - missing row -> create encrypted row and participant relationship;
  - existing row with an identical canonical `request_json` -> success;
  - existing row with a different canonical `request_json` -> integrity error;
  - never overwrite a prior capture.

- [ ] Reuse the repository's mutation/query retry helpers. Test a write whose
  acknowledgement is lost and then retried.
- [ ] Install this sink by default while retaining the builder override for
  tests and embedders.
- [ ] Keep the callback immediately before `model.stream`. Sink or ACP failure
  must produce a terminal provider/capture error without issuing the HTTP call.
- [ ] Emit structured tracing fields: `capture_key`, `request_id`,
  `turn_index`, `attempt`, `prompt_hash`, and failure stage. Never log payload
  contents.
- [ ] Add a fault-injection test whose sink fails and assert the mock backend
  observed zero requests.

Run: `cargo test -p gents rendered_request_capture`

---

## Task 6: Prove exact capture with multi-turn and retry-aware E2E tests

**Files:**

- Create: `crates/gents/tests/e2e_runtime/rendered_request_capture.rs`
- Modify: `crates/gents/tests/e2e_runtime.rs`
- Reuse: `crates/gents/tests/support/streaming_backend.rs`
- Reuse completion retry fixtures where practical.

Use the real test database, runtime bootstrap, and `MockStreamingBackend`.
There is no `runtime_harness()` helper with the API shown in the old plan; use
the established helpers in neighboring E2E tests.

- [ ] One-turn default-on case: no explicit capture factory, exactly one row,
  and the persisted parsed `request_json` equals the body observed by the mock
  backend.
- [ ] Multi-turn tool case: force at least one tool call and continuation;
  assert ordered `(turn_index, attempt)` rows and equality with every observed
  provider request.
- [ ] Transport retry case: two attempt rows exist even when their request
  JSON values are equal.
- [ ] Repair retry case: the repaired attempt has a different request JSON and
  both persisted JSON values equal the two actually observed requests.
- [ ] Compacted-session cases: capture still equals the provider request after
  a durable compaction entry changes assembled history **and** after #988's
  per-turn budget guard performs ephemeral model-backed compaction. Assert the
  latter's effective checkpoint/messages are present in the provenance trace.
- [ ] Request-context case: the current `<context>` message is captured and old
  per-request context messages are absent exactly as production filtering
  requires.
- [ ] Skill case: selected skill reminders and the skill catalog are present in
  the captured input.
- [ ] ACP/encryption matrix: owner and requester can read/decrypt; stranger and
  anonymous readers receive no row, including exact-CID and `_commits` paths.
- [ ] Restart/idempotence case: re-driving capture does not create a duplicate
  or change the original canonical value.

Run the full package suite after this task: `cargo test -p gents`.

At this point #840's durable-input core is complete even if the materialized
view in Tasks 8-9 is not.

---

## Task 7: Surface captured requests in timelines and adapter projections

**Files:**

- Modify: `crates/gents/src/run_timeline.rs`
- Modify: `crates/gents/src/run_timeline_fetch.rs`
- Modify: `crates/gents/src/adapter_projection.rs`
- Modify: `crates/gents-cli/src/commands/trace.rs`
- Extend projection fixture and CLI trace tests.

- [ ] Add `TimelineRenderedRequestRow` and load rows for every request in the
  timeline, ordered by `turn_index`, then `attempt`.
- [ ] Represent them in `RunTimelineRows` and in timeline events or a clearly
  documented rendered-request section. Do not pretend `InferenceCall.attempt`
  is the owned loop's completion attempt; they are different counters.
- [ ] Include capture key, hashes, source/model, capture version, and provenance
  status in metadata-safe projections.
- [ ] Include decrypted `request_json` only in an explicitly authorized full
  projection. Apply redaction after the authorized read; never persist a
  redacted replacement over the canonical row.
- [ ] Add rendered requests to OpenAI-Codex, LangGraph, ATIF, and multi-agent
  adapter shapes where their contracts allow it; otherwise expose a documented
  extension field. ATIF v1.7 has explicit `extra` maps: use those rather than
  inventing a non-ATIF top-level field, and keep decrypted payloads out of the
  default Harbor/native export unless the caller requested an authorized full
  projection.
- [ ] Test unauthorized CLI/adapter reads fail closed and do not leak whether a
  capture key exists.
- [ ] Test an older database with no `RenderedRequest` collection reports
  `Unavailable` rather than failing the entire legacy timeline, if mixed
  versions are an explicitly supported read path.

Run:

```bash
cargo test -p gents adapter_projection
cargo test -p gents-cli trace
```

The existing `training_safe` all-string masking defect (#842) remains a
separate issue, but it does not authorize omitting the new collection from the
projection model.

---

## Task 8: Make reconstruction inputs genuinely versioned

This task starts #523. It must not mutate frozen schema roots in place.

**Files:**

- Modify the relevant config SDLs and protocol SDLs through migration targets.
- Modify: `crates/gents-migration/src/registry.rs`
- Modify: `crates/gents/src/config.rs`
- Modify: `crates/gents/src/agent/document_view/snapshot.rs`
- Modify document runtime record types as needed.
- Create: `crates/gents/src/rendered_request/cids.rs`
- Add migration and CID regression tests.

- [ ] First prove whether DefraDB can migrate an existing non-branchable
  collection to branchable through `patch_collection`. Add an executable
  migration test. If it cannot, write the successor-collection/backfill plan;
  do not edit baseline SDL and hope existing databases converge.
- [ ] Give `AgentBehavior`, `ToolSelection`, `Skill`, `InferenceProfile`, and
  `InferenceBackend` a real branchable upgrade path with pinned destination
  VersionIDs.
- [ ] Preserve source document ids in the resolved runtime. Add a typed
  `BehaviorSourceRefs` (behavior, backend, profile, optional tool selection,
  effective skills) rather than trying to rediscover ids at capture time.
- [ ] Preserve the resolved-runtime generation/fingerprint used by the daemon.
  A concurrent config edit may trigger reconcile but must not change the source
  refs of the generation currently serving a request.
- [ ] Implement `composite_head_cid` using:

  ```graphql
  _commits(
    docID: ["..."]
    filter: { fieldName: { _eq: "_C" } }
    order: { height: DESC }
    limit: 1
  ) { cid height fieldName }
  ```

  Check GraphQL errors. Do not take the maximum height across field commits.
- [ ] Stamp every required config CID plus explicit document id into provenance.
  Required means required: a skill-bearing request with no skill CID is
  `CapturedOnly`, not `Verified`.
- [ ] Define a transcript snapshot contract. Choose and prove one:

  1. pin the composite CID of every contributing `AgentMessage` and
     `CompactionEntry`; or
  2. migrate semantic transcript fields to immutable and capture sequence
     high-water marks, with a conformance proof that runtime writes are
     append-only.

  A bare `request_id` filter is invalid because provider history is
  session-scoped and includes prior requests.
- [ ] Capture the runtime-only leak set needed by projection: resolved tool
  definitions (including prompt-sensitive/MCP schemas), active subagent target
  descriptions, effective skill/tool-ceiling manifest, provider wire/normalizer
  choice, retry input transform, effective output-token clamp, and #988's
  per-turn compaction result. The per-turn continuation checkpoint is
  model-generated and currently ephemeral; either persist it as a first-class
  fact before send or carry the exact effective result in the versioned
  provenance manifest.
- [ ] Version the provenance manifest and test stable serialization.

Run migration tests, the DefraDB time-travel E2E, `cargo test -p gents`, and
`lake build` before proceeding.

---

## Task 9: Reconstruct the complete request and verify it

**Files:**

- Create: `crates/gents/src/rendered_request/reconstruct.rs`
- Create: `crates/gents/tests/e2e_runtime/rendered_request_reconstruct.rs`
- Modify: `crates/gents/tests/e2e_runtime.rs`
- Extend the PromptAssembly Lean model and conformance output.

Use explicit outcomes:

```rust
pub enum ReconstructionOutcome {
    Verified {
        request_json: serde_json::Value,
        request_json_commit_cid: String,
        commit_signature: Option<CommitSignature>,
    },
    CapturedOnly { request_json: serde_json::Value, reason: String },
    ValueMismatch { differing_paths: Vec<String> },
    UnsupportedManifest { version: u32 },
    Unavailable { reason: String },
}
```

- [ ] Extend the Lean model from persist-before-send to projection fidelity:
  when all versioned inputs and leak-set values agree, reconstructed render
  equals captured render. Export conformance cases.
- [ ] Load the envelope by all three key components, rejecting duplicates as
  corruption.
- [ ] Time-travel every config and transcript input at its pinned composite CID
  or use the proven immutable high-water contract selected in Task 8.
- [ ] Reproduce the production path, not a shortcut:

  1. session history load;
  2. `provider_view` / tool-result stripping;
  3. compaction-prefix drop using only entries present at capture;
  4. summary reminders;
  5. selected skill reminders;
  6. old request-context filtering and current context placement;
  7. preamble construction with effective tool/target manifest;
  8. #988 budget enforcement, including the captured effective per-turn
     compaction result and output-token clamp (never a fresh model summary);
  9. retry repair transform when applicable — matching the current production
     repair path, which rebuilds with `build_request` rather than re-entering
     `build_budgeted_request`;
  10. Rig/provider wire conversion and normalization;
  11. canonical serialization of the complete request.

- [ ] Share production functions or extract pure helpers. Do not copy the
  sanitizer, preamble builder, canonical encoder, or provider converter.
- [ ] Compare the reconstructed canonical `request_json` against the stored
  one directly, as values — not `prompt_hash`, and not a recomputed digest.
  Return `Verified` only on complete equality. Report the stored value's
  field-commit CID and optional signature alongside the verdict so a caller can
  see which durable version was compared against. Never label an absent or
  unverified signature as authenticated. Bound mismatch diagnostics to JSON
  paths; do not duplicate or log the encrypted request contents in an error.
- [ ] Test: first turn, multi-request session, multi-turn tools, compaction,
  later behavior/profile/backend/tool-selection/skill edits, selected skills,
  request context, transport retry, repair retry, and both provider wire APIs.
- [ ] Tamper each provenance component in turn and require `ValueMismatch` or
  `CapturedOnly`, never false `Verified`.
- [ ] Update timeline/projection status to show `Verified`, `CapturedOnly`, or
  the precise failure.

Run: `cargo test -p gents rendered_request_reconstruct`

---

## Final verification

- [ ] Resolve #523's fidelity contract. Canonical-JSON replay and literal
  HTTP-body byte equality are distinct deliverables; the implementation and
  issue acceptance criteria name one consistently. If literal bytes remain
  required, transport interception proves the captured bytes are exactly those
  forwarded for Responses normalization, Chat Completions, ChatGPT Codex, and
  xAI.
- [ ] `cd crates/gents/proofs && lake build`
- [ ] `cargo test -p gents-schemas`
- [ ] `cargo test -p gents-protocol`
- [ ] `cargo test -p gents-migration`
- [ ] `cargo test -p gents`
- [ ] `cargo test -p gents-cli`
- [ ] `cargo check --workspace --all-targets`
- [ ] Confirm no `sorry` was added.
- [ ] Confirm the migration works from the frozen baseline and on a fresh node.
- [ ] Confirm the default path cannot contact the mock provider when capture
  persistence or participant authorization fails.
- [ ] Confirm every observed provider request in the multi-turn/retry fixture
  has one and only one equal decrypted capture row.
- [ ] Confirm unauthorized, anonymous, exact-CID, and `_commits` reads reveal no
  rendered row.

## Plan 2 handoff

After Task 6 is green, write/execute Plan 2 for #722:

- structured `AgentToolResult.tool_call_key` migration and fork/projection
  round-trip;
- `sessions(action = "read_tool_result")` with bounded paging and negative ACP
  tests;
- executable, idempotent trimming stubs;
- honest failed-spill state;
- live multi-turn test that trims a result, resolves it, continues, and asserts
  every provider request against the durable captures introduced here.

---

# Post-workflow-1 corrections and locked scope (2026-08-06)

Task 1 is committed (`8180bf67`). Recon plus adversarial verification produced
the following binding decisions. Where this section conflicts with anything
above, this section wins.

## Scope DROPPED

- **ACP / `@policy`.** Blocked on
  [defradb.rs#1318](https://github.com/sourcenetwork/defradb.rs/issues/1318):
  `EmbeddedNode` exposes no policy or relationship API, `create_document_acp`
  is `pub(crate)`, and the constructed `LocalDocumentACP` is never retained on
  the node. Worse, a `@policy` id that is never installed deploys cleanly and
  leaves rows anonymously readable — unregistered documents are public. **Do
  not put `@policy` in the shipped SDL.** #840's participant-boundary criterion
  is explicitly NOT met by this work; say so in the PR.
- **At-rest encryption.** `encrypt: true` / `encryptFields` protect the
  replicated CRDT block deltas only. The local node writes plaintext into the
  datastore and builds indexes from it, so on a single-node install the payload
  is readable by anyone who can query the node. The only local mechanism is
  `NodeBuilder::with_at_rest_encryption_key`, which gents does not use. Do not
  ship a green `encrypt: true` test as evidence of confidentiality.
- **Branchability workstream (former Task 8) — DELETE ENTIRELY.** Two
  independent reasons. It is unnecessary: per-field and composite commit blocks
  are written unconditionally (`db-blocks/src/write.rs:59`, composite at `:268`),
  `is_branchable` gates only the collection-level block
  (`db/src/doc_mutator.rs:259`), and it appears exactly once in `crates/query/`
  — as an ACP-scope argument at `runner/commits.rs:195`. And it is impossible:
  `validate_branchable_not_mutated` rejects every patch shape, and populated
  collections cannot be deleted and recreated.
- **The encryption/index compatibility fork.** A non-problem. Indexes are built
  from the plaintext document after the block write, so encrypted fields remain
  indexable and filterable; upstream proves it end-to-end by filtering an
  `@index`ed `encryptFields` column on a replica.

## Scope LOCKED IN

- **Capture at the transport seam**, not pre-transport. The ChatGPT-Codex
  transport rewrites the body after rig serializes it — hoists system text into
  `instructions` and strips it from `input`, sets `store:false`/`stream:true`,
  deletes `max_output_tokens`/`temperature`/`top_p`, and forces `strict:false`
  on every tool (`chatgpt_codex.rs:358-401`). Grok injects `store:false`
  (`xai_grok_oauth.rs:312`). Capturing pre-transport makes the equality claim
  false for two of four provider kinds.
- **Widen the sink to every provider call site.** `completion_factory::loop_config`
  hardcodes `on_rendered_request: None` (`completion_factory.rs:56`), leaving
  the compaction summarizer, title generation, and the one-shot runner
  uncaptured. The compaction summarizer is the one that produces the ephemeral
  checkpoint this design exists to explain.
- **`RenderedRequest` registers as a `DEFAULT_BASELINE` entry**, not via
  `AddCollection`. The two are mutually exclusive under `baseline_ensure.rs:83-86`
  (set equality) and `:30-40` (ordered pairwise equality); no pin-authoring
  workflow exists for steps (`phase_b_steps.rs:304-341` iterates baseline only);
  and `Registry::managed_names()` excludes AddCollection collections from eager
  materialization.
- **AssemblyTrace carries the four genuinely unrecoverable leaks:**
  1. the provider assistant `message_id` — stamped into in-memory history
     (`loop_stream.rs:802-806`), persisted as `id: None`
     (`stream_processor.rs:305`);
  2. the exact threaded tool-result text per call id — the loop threads
     `truncate_text(model_facing_text, tool_result_truncation_mode(name), …)`
     while persistence re-derives from `AgentToolCall.result` with
     `TruncationMode::Head`, different limits, and
     `model_observation_for_tool_result`;
  3. the effective native message list whenever it contains runtime-only
     content — rendered request context can read `now` and live collection
     state, and per-turn compaction is a STICKY mutation
     (`*history = compacted; *new_messages = vec![…]`), so one turn's summary
     governs every later turn. Ordinary reconstructible turns omit this second
     full copy and retain only the message count plus positional overlays;
  4. the `build_path` discriminator — repair calls `build_request` directly
     (`loop_stream.rs:353,447`) and never applies the output clamp, so a
     repaired attempt carries raw `max_tokens` while the original carries the
     clamped one.

  The clamp VALUE is not needed: it is a pure function of the assembled request
  plus durable config, and `completion_request_input_estimate` does not read
  `max_tokens`, so one pass reproduces it exactly.

## Required test the current fence lacks

`attempt_distinguishes_facts` is proven in Lean with no Rust fence behind it. A
mutation probe during verification replaced the loop's `attempt` with a literal
`0` at `loop_stream.rs:298`; `cargo check` passed and **no test failed**.
`loop_stream/tests.rs:638` binds `|_turn_index, _attempt, _request|` and rebuilds
the key from the Lean case; `tests.rs:534` only ever observes attempt 0.

Add a multi-attempt test asserting the sink receives DISTINCT `attempt` values
across a retry, sourced from the loop's own arguments and not reconstructed.

## Citation fixes

- The bare-word `truncated` false positive was fixed by **#998** (`061a3a34`,
  2026-08-02), not #988.
- `tool_call_key` WRITE sites are `tool_call_lifecycle/transition/native.rs:47,
  319, 457, 519` and `session/fork.rs:581` (fork re-keys to the child session).
  `hook/persistence/helpers.rs:160-166` is a read path.
- `tool_call_id` is rig's locally minted nanoid or a uuid v4 — never model- or
  provider-supplied. The realizable collision direction is via `session_id`,
  which is caller-controlled and unvalidated (`ChatArgs::session_id` has no
  `value_parser`).
- `_commits` accepts exactly ONE docID; two or more is a parse error. Its
  `fieldName` filter is evaluated in memory with
  `filter.matches(…).unwrap_or(true)`, so a malformed filter silently degrades
  to no filter — assert the returned `fieldName` in Rust. Treat `[]` as an
  explicit `Unavailable`.
- `SessionHistoryTool` carries only `{ node, agent_did }`; there is no requester
  filter, and three of its four sub-queries have no `agent_did` filter at all.
- Trace redaction defaults to `Full` (none); ACP filtering is skipped unless a
  `ProjectionAcpBinding` row exists; Harbor invokes `trace project` with neither
  `--redaction` nor `--actor-did`; and ATIF `extra` maps bypass redaction
  entirely. Excluding captured inputs from default exports must therefore be a
  positive default, not an opt-out.
