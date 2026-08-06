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
- Retry attempts are separate facts. Repair can rewrite assembled input without
  changing the transcript, so attempt 0 and attempt 1 may legitimately differ.
- The record is unique by
  `(agent_did, session_id, request_id, turn_index, attempt)` and idempotent.
  Reusing that key with different bytes is an integrity error, never an update.
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
  `CapturedOnly`, `Unavailable`, `UnsupportedManifest`, and `HashMismatch`
  outcomes.
- The first end-to-end test is multi-turn and retry-aware. A one-turn `READY`
  test cannot establish reproducibility.
- Token usage and trigger lineage are useful timeline work but do not prove
  rendered-input durability. They remain separate from this plan unless a
  projection contract requires them.

## Global constraints

- Follow the foundation flow: Lean model, generated conformance cases, Rust.
- Always apply `crate::graphql::escape_graphql_string()` to interpolated
  GraphQL values.
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
encrypted durable RenderedRequest(capture_key, request_json, request_hash)
      |
      v
provider call
```

The persisted JSON is the conformance oracle. The later materialized-view path
must reproduce the same canonical JSON and hash, but capture remains readable
even when a future runtime no longer supports an old provenance-manifest
version.

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
    request_hash: String @index @immutable
    prompt_hash: String @index @immutable
    tools_hash: String @index @immutable
    provenance_json: String @immutable
    created_at: DateTime @index @immutable
}
```

`request_json` contains the exact `Value` produced by
`llm::rig_compat::provider_request_json`, serialized canonically. Component
values do not need duplicate columns because they are derivable from
`request_json`; keeping `prompt_hash` and `tools_hash` is useful for diagnosis.
`provenance_json` is a versioned object, never an overloaded empty-map signal.

---

## Task 1: Fence persist-before-send in Lean and conformance

**Files:**

- Create: `crates/gents/proofs/Proofs/RenderedCapture.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/Contracts.lean`
- Modify: `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean`
- Create: `crates/gents/tests/conformance/rendered_capture.rs`
- Modify: `crates/gents/tests/conformance/structure.rs`

- [ ] Model a capture key, canonical request hash, and three stages:
  `Assembled`, `DurablyCaptured`, `Sent`.
- [ ] Make `capture` the only legal predecessor of `send`.
- [ ] Model idempotent recapture of the same `(key, hash)` and rejection of the
  same key with a different hash.
- [ ] Prove `sent_implies_durably_captured` and
  `capture_key_determines_request_hash` with zero `sorry`s.
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

- [ ] Add `capture_key`, `requester_did`, `capture_version`, `request_hash`, and
  `provenance_json` to `RenderedCompletionRequest`.
- [ ] Derive `capture_key` from a canonical tuple containing agent, session,
  request, turn, and attempt. Do not concatenate an unescaped delimiter format,
  and do not assume `AgentRequest.request_id` is globally unique (its schema
  index is not unique).
- [ ] Keep `request_json` in the DTO. Do not remove it after calculating the
  hash.
- [ ] Make one canonical JSON encoder the source of both persisted bytes and
  hashes. Expose it as `pub(crate)`; do not maintain a second implementation in
  the sink or reconstructor.
- [ ] Hash the complete canonical `request_json` as `request_hash`; retain
  component hashes for messages/input and tools.
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
- [ ] Encrypt the whole document on create. If DefraDB requires field-level
  encryption for indexed metadata, encrypt at least `request_json` and
  `provenance_json` and document precisely which metadata remains visible.
- [ ] Create/register the ACP object as the agent principal, then grant the
  requester `reader` when present. A partially completed owner-only row is
  recoverable: retry relationship creation before permitting send.
- [ ] Enforce idempotency by `capture_key`:

  - missing row -> create encrypted row and participant relationship;
  - existing row with the same `request_hash` and identical canonical payload
    -> success;
  - existing row with a different hash/payload -> integrity error;
  - never overwrite a prior capture.

- [ ] Reuse the repository's mutation/query retry helpers. Test a write whose
  acknowledgement is lost and then retried.
- [ ] Install this sink by default while retaining the builder override for
  tests and embedders.
- [ ] Keep the callback immediately before `model.stream`. Sink or ACP failure
  must produce a terminal provider/capture error without issuing the HTTP call.
- [ ] Emit structured tracing fields: `capture_key`, `request_id`,
  `turn_index`, `attempt`, `request_hash`, and failure stage. Never log payload
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
  hashes are equal.
- [ ] Repair retry case: the repaired attempt has a different request hash and
  both persisted JSON values equal the two actually observed requests.
- [ ] Compacted-session case: capture still equals the provider request after a
  compaction entry changes the assembled history.
- [ ] Request-context case: the current `<context>` message is captured and old
  per-request context messages are absent exactly as production filtering
  requires.
- [ ] Skill case: selected skill reminders and the skill catalog are present in
  the captured input.
- [ ] ACP/encryption matrix: owner and requester can read/decrypt; stranger and
  anonymous readers receive no row, including exact-CID and `_commits` paths.
- [ ] Restart/idempotence case: re-driving capture does not create a duplicate
  or change the original hash.

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
- [ ] Add rendered requests to OpenAI-Codex, LangGraph, and multi-agent adapter
  shapes where their contracts allow it; otherwise expose a documented
  extension field.
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
  choice, and retry input transform.
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
    Verified { request_json: serde_json::Value },
    CapturedOnly { request_json: serde_json::Value, reason: String },
    HashMismatch { expected: String, actual: String },
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
  8. retry repair transform when applicable;
  9. Rig/provider wire conversion and normalization;
  10. complete canonical request hash.

- [ ] Share production functions or extract pure helpers. Do not copy the
  sanitizer, preamble builder, canonical hasher, or provider converter.
- [ ] Compare `request_hash`, not only `prompt_hash`. Return `Verified` only on
  complete equality.
- [ ] Test: first turn, multi-request session, multi-turn tools, compaction,
  later behavior/profile/backend/tool-selection/skill edits, selected skills,
  request context, transport retry, repair retry, and both provider wire APIs.
- [ ] Tamper each provenance component in turn and require `HashMismatch` or
  `CapturedOnly`, never false `Verified`.
- [ ] Update timeline/projection status to show `Verified`, `CapturedOnly`, or
  the precise failure.

Run: `cargo test -p gents rendered_request_reconstruct`

---

## Final verification

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
