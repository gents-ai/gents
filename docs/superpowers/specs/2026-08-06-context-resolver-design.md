# Durable context resolution: recoverable tool results and reconstructible provider inputs

Date: 2026-08-06
Issues: #722 (primary), #840 (folded in), #523 (resolved by this design)
Cluster: `context-memory`

## Problem

Two reference markers already exist in the transcript, and nothing can follow
either of them.

1. Every tool result in the provider view is unconditionally replaced with a
   stub (`compaction::history::strip_tool_result`):

   ```
   [tool: read, call_id: call-1, 5000 bytes — see DefraDB AgentToolCall for full output]
   ```

2. Oversized output spills to a document and appends
   `[Full output: DefraDB doc {id}]` (`crates/gents/src/truncation/spill.rs:119`).

Neither is executable. `AgentToolResult.output_text` is copied by session
forking but is not exposed through a model tool; `conversation_doc_id` is not a
model-readable resolver; and `read_tool_output` deliberately rejects
non-background rows (`background_tools::handle_read_tool_output`). The
canonical evidence is durable and replicated, and the model cannot reach it.
Trimming is durable but model-irreversible.

The same gap appears one level up. The bytes actually sent to the provider are
never persisted. `rendered_request.rs` defines an exact-request DTO and a capture
seam, but `rendered_request_capture_factory` defaults to `None` and the only
setter (`agent/builder.rs:112`) has no production caller. A trace without its
rendered input is a log, not a training example.

## Organizing principle

Make the fact record complete before asking a projection to reproduce it.

### Fidelity contract

This design uses **canonical JSON value equality**: it can replay the same
provider input semantically and detect any changed field, array element, or
value. It does not by itself prove the literal HTTP body bytes, because object
key order and serializer formatting may differ. #523's current issue text asks
for "the exact bytes that were sent." Before the issue is closed, either amend
that wording to canonical-JSON fidelity or add exact body capture after all
provider-specific HTTP rewrites and before transport send, with a test that the
captured bytes are the bytes forwarded. Re-serializing `request_json` later is
not equivalent evidence.

There are two different deliverables here and neither substitutes for the
other:

1. **Durable capture is the correctness floor.** Persist the exact rendered
   provider request before the network call. This is the oracle required by
   #840 and by a reproducible provider harness.
2. **Time-travel reconstruction is a verified projection.** Pin every durable
   source version, persist the genuinely runtime-only leak set, replay prompt
   assembly, and compare the complete canonical value with the captured
   `request_json`. This is #523.

The earlier constant-size-envelope proposal conflated those deliverables. It
was not complete: resolved tool definitions are computed dynamically from the
assembled prompt; retry repair can rewrite provider input without rewriting
the transcript; the effective preamble includes the resolved tool surface and
active subagent targets; and the current transcript has no single multi-row
snapshot CID. Hashes detect those omissions but cannot reconstruct their
bytes. A hash-only envelope is therefore an integrity witness, not a durable
rendered input.

The tool resolver is the first projection, exposed to the model as a tool. The
run timeline, adapter projections, and eventual training samples are further
projections over the same facts. An outcome or reward is not a separate
feature under this principle — it is one more fact written next to the trace,
and a training sample is a projection over both, made verifiable by the
exact reconstruction comparison.

Build order follows from this: complete the facts first (Part 2), then expose a
projection over them (Part 1). The parts are presented below in issue order,
not build order.

## Scope

In scope:

- A bounded, model-executable resolver for trimmed tool results (#722).
- Exact, encrypted, default-on rendered-provider-request capture (#840).
- Surfacing captured requests in run timelines and adapter projections.
- Capture-verified reconstruction as a second path (#523), using capture as the
  conformance oracle.
- Making every durable render-contributing config collection `@branchable` and
  carrying its document id into the resolved runtime.
- The schema migration and replication plumbing required for the new
  collection. A collection that only exists as an `include_str!` constant is
  not a runtime collection.

Out of scope, filed separately:

- ACP policies for the `sessions` tool. This slice inherits the existing
  `agent_did` / `requester_did` filter; a follow-up brings the whole tool under
  ACP in one pass.
- The `training_safe` redaction fix (#842). It currently masks every non-empty
  string, so exports through it are useless; that is a separate defect.
- Any reward or outcome document.
- The generic `defra_query` escape hatch. It exists; this resolver is
  purpose-built and does not use or extend it.

## Part 1 — Tool-result resolver (#722)

### Data model

`tool_call_key` is already composite: `hook/persistence/helpers.rs` builds it as
`"{session_id}:{tool_call_id}"`. That delimiter encoding is **not** structurally
collision-free: `("a:b", "c")` and `("a", "b:c")` produce the same string.
The key is an indexed join aid, not an authorization boundary. Resolution must
also filter the `AgentToolCall` by the separately stored `session_id` and
`tool_call_id`, and must require the linked `AgentToolResult.session_id` to
match. A future key-format migration may use a versioned canonical tuple, but
this slice must be safe with existing rows.

One nullable indexed field:

```graphql
type AgentToolResult @branchable {
    ...
    tool_call_key: String @index     # "{session_id}:{tool_call_id}"
}
```

`AgentToolCall.tool_call_key` is already `@index(unique: true)`, so the join is
a unique-index lookup. The edge points child→parent: written once at spill
time, never mutating an existing `AgentToolCall` row, so there is no
write-ordering window in which a spill is orphaned.

`DefraSpillTruncator::spill()` (`truncation/spill.rs:11-18`) currently receives
`(tool_name, tool_input, output, metadata, conversation_doc_id)` and holds
`self.agent_did`, `self.requester_did`, `self.session_id`. It has no tool-call
identity. Threading `tool_call_id` into the truncator is the one non-trivial
refactor in this part, and it changes the `Truncator` trait signature.

Naming hazard: two different things are called `tool_call_key` today. The
persisted one is composite; `compaction::history::tool_call_key` returns bare
`call_id.unwrap_or(id)`. This work touches both files. Rename the
compaction-local one to `correlation_key`.

### Tool surface

A new action on the existing `sessions` tool
(`crates/gents/src/toolset/session_history.rs`, currently `"enum": ["list"]` at
line 176, validated at 252-255):

```json
{ "action": "read_tool_result", "session_id": "…", "tool_call_id": "…",
  "offset": 0, "max_bytes": 16384 }
```

The model never supplies a key or a document id. The runtime composes
`tool_call_key` from `session_id` and `tool_call_id`, but still applies the
explicit session/call filters above. Cross-session reads are rejected by those
structured fields and principal filters, not by delimiter folklore.

Resolution: compose key → look up `AgentToolCall` by key **and** exact
`session_id` / `tool_call_id`, filtered on `agent_did` / `requester_did` → if
an `AgentToolResult` carries that key and the same `session_id`, source is
`spilled` and the bytes come from `output_text`; otherwise source is `inline`
and they come from `AgentToolCall.result` → slice.

### Paging

Reuse `background_tools::read_retained_output_slice`, the Rust
mirror of the `readSlice` model proven in
`proofs/Proofs/Background/ToolOutput.lean`. Persisted output is the degenerate
`RetainedWindow` that model already describes (`ToolOutput.lean:44-46`):
`firstOffset = 0`, `retainedEnd = totalBytes`, nothing ever evicted. P1–P6
(contiguity, eviction detectability, within-retained, past-end-empty,
`hasMore` iff, progress) carry over unchanged.

Envelope field names match the background reads exactly — `next_offset`,
`first_available_offset`, `total_bytes`, `has_more` — so the model learns one
paging idiom. Three additions:

- `source`: `inline` | `spilled` | `unavailable`
- `tool_name`
- `delivered_to_model`, from `discarded_because_interrupted`, distinguishing
  "this result exists but you never saw it" from "you saw it and it was
  trimmed"

`max_bytes` is clamped by the same validator background reads use.

### Stub rewrite

The stub becomes executable, carrying the exact call needed to recover it. It
must survive repeated stripping idempotently: re-running `strip_tool_results`
over already-stubbed history must not re-wrap or double-annotate.

### Truncation detection

`tool_result_was_truncated` still infers recoverability from presentation text,
currently the `"[Full output: DefraDB doc"` and `"[Showing lines"` markers.
#988 removed the old bare-word `"truncated"` false positive, so Plan 2 must not
claim or re-fix that bug. The remaining design problem is semantic: a marker is
not a durable relationship and a failed spill can leave no recoverable full
output. Once `tool_call_key` exists, ask the database instead of guessing.

### Error handling

Four honest outcomes, no panics and no fabricated recovery:

- `not_found` — no call with that key visible to this principal. Identical
  response whether it never existed or belongs to someone else, so the tool is
  not an existence oracle.
- `unavailable` — the call exists but no full output is recoverable: a
  pre-migration row with no key, or a failed spill.
- `offset` past the end — empty slice, `has_more: false`, cursor parked at
  `total_bytes`, matching proven `readSlice` P4.
- Oversized `max_bytes` — clamped.

`spill.rs:109-112` currently swallows spill failure silently: the bounded text
is returned, no marker is appended, and nothing records that the full bytes
were lost. Spill failure must be recorded so `unavailable` can be reported
truthfully.

## Part 2 — Durable capture and capture-verified reconstruction (#840 / #523)

### Why the payload is stored first

Storing the full rendered payload every turn is quadratic: turn 50 contains
turns 1–49 again, multiplied by retry attempts. That cost is real, but it is an
optimization problem rather than permission to weaken the fact record.

The owned loop already has the exact provider request immediately before
`model.stream`. Persist its canonical JSON there. Later work may
content-address or delta-compress payloads, but the initial implementation must
remain lossless and self-contained. In particular, it must capture every retry
attempt: a repair attempt can differ from the canonical transcript even when
no new document was written.

### The blocker

The conversation collections are branchable, but that alone is not a snapshot:
one render reads many `AgentMessage` and `CompactionEntry` documents, and no
single session CID identifies all of those row versions. The runtime treats
message rows as append-only, but the schema does not make their payload fields
immutable. Reconstruction therefore needs explicit transcript and compaction
high-water marks plus either pinned row CIDs or an enforced append-only
contract.

The configuration side is also incomplete. `AgentBehavior`, `ToolSelection`,
and `Skill` are not branchable. `InferenceProfile` supplies sampling, context,
retry, and max-token values; `InferenceBackend` supplies provider kind and wire
API. Both contribute to the request and are not branchable either. The resolved
runtime currently discards the source document ids after snapshot assembly, so
the capture site cannot pin them.

Finally, several bytes are runtime-resolved rather than recoverable from a
single config document: effective tool definitions (including prompt-sensitive
definitions and MCP-resolved schemas), active subagent target descriptions,
the effective skill set/tool ceiling, provider normalization choice, and any
retry repair applied to the assembled input. These are the projection leak
set and must be captured explicitly.

#988 adds another member to that leak set. `build_budgeted_request` can run a
model-backed per-turn compaction after ordinary prompt assembly and inject its
continuation checkpoint directly into provider history. That result is not
saved as an `AgentCompactionEntry`; the capture callback currently receives
only the final `CompletionRequest`. Re-running the summarizer is not
deterministic reconstruction. The capture seam must therefore also retain the
effective per-turn compaction result and budget decisions, or the compaction
result must become a first-class durable fact before send.

### Design

**Persist the exact request first.** A `RenderedRequest` row is keyed uniquely
by `(agent_did, session_id, request_id, turn_index, attempt)` and contains the
canonical `request_json`, the existing component hashes, source/model
metadata, requester identity, capture format version, and provenance manifest.
It is written before the provider call. Duplicate delivery idempotently reuses
the row only when the canonical payload is identical; otherwise it fails as an
integrity violation.

**Encrypt and authorize it like participant data.** The create mutation uses
DefraDB encryption for at least `request_json` and `provenance_json`; routing
metadata may remain plaintext if whole-document encryption prevents the indexed
idempotency/timeline lookups. The collection is ACP-bound, with the agent
principal as owner and the non-empty `requester_did` granted the participant
reader relation. This is tested with owner, requester, unrelated DID, anonymous,
exact-CID, and `_commits` reads; merely adding `agent_did`/`requester_did`
fields is not an authorization boundary.

**Make all render-contributing config collections `@branchable`:**
`AgentBehavior`, `ToolSelection`, `Skill`, `InferenceProfile`, and
`InferenceBackend`. Preserve their source document ids in the resolved runtime
so the capture path can pin the exact versions actually loaded, rather than
re-querying by logical id and racing reconcile.

**Stamp CIDs at render; do not time-correlate.** This resolves #523's open
question. Time-correlation races concurrent config edits and yields a
plausible-but-wrong reconstruction, which is strictly worse than a failed one.
At render time the envelope records the commit CID of every contributing config
document.

**Persist a provenance manifest beside the payload.** It contains pinned CIDs,
session transcript/compaction boundaries, resolved-runtime fingerprint, and a
versioned leak set sufficient for replay, including #988's effective per-turn
compaction result and output-token clamp. `prompt_hash` and `tools_hash` remain
query indexes produced by the existing canonical-JSON hasher; they are not a
whole-request integrity mechanism. For Responses, the prompt index covers both
top-level `instructions` and `input`; for Chat Completions it covers `messages`.

**Install the capture sink by default.** `rendered_request_capture_factory`
stops defaulting to `None`.

**Reconstruct on demand:** time-travel read each config document at its pinned
CID → load only transcript and compaction rows within the captured boundaries
at their pinned versions → replay the same production assembly and provider
converter with the captured leak set → compare the complete canonical request
value with the captured `request_json`.

A match is canonical-JSON-exact provenance. Do not call it byte-exact unless the
actual serialized HTTP body bytes are captured; canonical JSON values do not
preserve transport whitespace or serializer formatting. A mismatch is a real
finding: either assembly/conversion changed, a contributing input was not
pinned, or the manifest version is no longer supported.

`RenderedRequest` is branchable, so DefraDB also supplies a per-field commit CID
for the stored `request_json`. That CID is the durable version/content anchor;
do not duplicate it with a self-attested `request_hash` column. Commit
signatures are separate: the pinned DefraDB exposes a nullable signature with
multiple possible algorithms, and Gents' ordinary embedded-node builders leave
block signing disabled. Report and verify a signature when one exists, but do
not claim authenticated authorship unless the write path explicitly requires
and validates one.

**Surface the fact record.** `RunTimelineRows`, timeline events, adapter
projections, and CLI `trace timeline|project` load the rendered rows. Redaction
is applied at projection time; the encrypted canonical row remains lossless.

### Epoch boundary

Rows without a full payload are not #840-complete. Rows with a payload but no
supported provenance manifest are `CapturedOnly`, not `Verified`. Requests
created before the collection migration are `Unavailable`. These states are
explicit in projections; no empty-map convention may silently stand in for
missing required provenance.

### Run timeline

#840 requires surfacing rendered inputs in the run timeline and adapter
projections. Add captured rows, ordered by completion `turn_index` and
`attempt`, to `RunTimelineRows`, timeline events, adapter projections, and CLI
trace output. The loop's completion attempt is not `InferenceCall.attempt`;
keep the counters distinct. Token usage and trigger lineage are useful but do
not establish rendered-input durability and remain separate work.

## Testing

### Live multi-turn end-to-end

`crates/gents/tests/e2e_live/context_resolver_live.rs`, gated as the existing
live tests are (`#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]`
plus an env assertion, per `goal_continuation_live.rs:61-65`):

1. Turn 1 — the model runs a tool emitting roughly 50 KB with a unique marker
   (`RESOLVER_MARKER_<uuid>`) buried mid-output. The output truncates and
   spills.
2. Turns 2–N — drive enough turns to trigger compaction, so
   `strip_tool_results` replaces that observation with a stub.
3. Inspect what the model sees. Parse captured `request_json` and assert that
   the marker is **absent** and the executable stub is **present**.
4. Recovery turn — instruct the model to retrieve the earlier result. Assert it
   calls `sessions(action="read_tool_result")` and that the marker appears in
   the returned result. This is the claim under test: the model, not an
   operator, recovers its own trimmed evidence.
5. Paging — with `max_bytes` below the result size, assert `has_more`, follow
   `next_offset`, and assert the pages are gap-free and reassemble to the
   original bytes.

Step 3 is possible because capture is on by default under Part 2. The live test
is Plan 2's final acceptance test; Part 2 first establishes the same equality
against the deterministic mock streaming backend for multi-turn, transport
retry, and repair-retry requests.

### Reconstruction

- Capture round-trip: every request body observed by the mock provider has
  exactly one equal decrypted `request_json` row, anchored by its field-commit
  CID.
- Projection round-trip: reconstruct the complete provider request and compare
  canonical values directly, not only `prompt_hash`.
- Config drift: mutate behavior, backend, profile, tool selection, and skill
  documents after capture; reconstruction uses the pinned versions.
- Compaction and retries: reconstruct a durable-compaction-entry session,
  a durable request-local per-turn provider reduction, an unchanged transport retry,
  and a repair retry whose input differs without a transcript write.
- Epoch: a payload with no supported manifest is `CapturedOnly`; a request
  before the collection migration is `Unavailable`.

### Non-live

- Cross-session key rejection.
- Spilled versus inline source selection.
- Repeated stripping is idempotent.
- Negative authorization: another principal's call resolves to `not_found`.
- Failed spill reports `unavailable`, never a fabricated recovery.

## Formal model

Paging reuses the proven `readSlice` model unchanged. Authorization remains an
external DefraDB/ACP assumption fenced by negative integration tests.

Rendered capture adds a legal-order invariant: a provider send is permitted
only after the matching `(capture_key, canonical_request)` is durable, and the
same key cannot name a different canonical request. Model and prove that
transition before the sink. Extend PromptAssembly with projection fidelity
before reporting a reconstruction as `Verified`.

The resolver's structured linkage and idempotent stub behavior require
conformance/property coverage. Include adversarial delimiter-collision cases;
the composite-key encoding does not prove session isolation. Any change to the
modeled transcript or tool-call lifecycles still begins in Lean.

## Acceptance criteria

- An agent retrieves a trimmed historical tool result through its session-history
  tool without raw GraphQL.
- Both inline and spilled results resolve correctly.
- Lookup uses the structured `tool_call_key` relationship, not regex over
  presentation prose.
- Reads are bounded and page-continuable with honest byte metadata.
- Trimming stubs carry a stable, model-executable recovery hint and survive
  repeated stripping idempotently.
- Truncation detection consults the database rather than sniffing text.
- Failed spills report `unavailable` rather than implying recoverability.
- Every provider attempt has exactly one encrypted, participant-authorized,
  default-on `RenderedRequest` row written before send.
- Parsed captured `request_json` equals the complete request observed by the
  provider; its per-field commit CID anchors the compared stored version.
- Capture is idempotent for equal canonical values and rejects a reused key with
  a different canonical value.
- The collection is installed through the migration registry and replicated
  with the conversation fact record.
- Run timelines, adapter projections, and CLI traces surface rendered requests
  with authorization and redaction enforced at read time.
- Config collections feeding reconstruction are branchable through real
  migration steps, and source document ids survive resolved-runtime assembly.
- Complete reconstruction equals captured canonical `request_json` after later
  config edits, compaction, multi-turn tool use, transport retry, and repair
  retry.
- Canonical `AgentMessage`, `AgentToolCall`, and `AgentToolResult` documents are
  never destructively rewritten by retrieval.
- `sessions(action="list")` remains compatible.
- `cargo test -p gents` and `cargo check --workspace --all-targets` pass.

## Execution gates

- Resolve canonical-JSON fidelity versus literal HTTP-body equality for #523.
  If literal bytes remain required, the capture location must move or extend to
  the transport boundary after provider-specific rewrites, while retaining the
  same fail-closed persist-before-send invariant.
- Prove the participant ACP policy can be installed before the collection SDL
  on fresh and upgraded nodes, and that encrypted key delivery follows the
  reader relationship.
- Prove whether a non-branchable collection can become branchable through the
  existing migration engine. If DefraDB cannot patch that property, use an
  explicit successor/backfill design; never rewrite frozen baseline SDL.
- ACP policy for the `sessions` tool remains a tracked follow-up, but the new
  rendered-request collection itself must satisfy #840's participant boundary.
