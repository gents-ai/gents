# Durable context resolution: recoverable tool results and reconstructible provider inputs

Date: 2026-08-06
Issues: #722 (primary), #840 (folded in), #523 (resolved by this design)
Cluster: `context-memory`

## Problem

Two reference markers already exist in the transcript, and nothing can follow
either of them.

1. Every tool result in loaded history is unconditionally replaced with a stub
   (`crates/gents/src/compaction/history.rs:272-279`):

   ```
   [tool: read, call_id: call-1, 5000 bytes — see DefraDB AgentToolCall for full output]
   ```

2. Oversized output spills to a document and appends
   `[Full output: DefraDB doc {id}]` (`crates/gents/src/truncation/spill.rs:119`).

Neither is executable. `AgentToolResult.output_text` is never queried from
`crates/gents/src/**`; `conversation_doc_id` is written and never read;
`read_tool_output` refuses non-background rows
(`background_tools.rs:993-999`). The canonical evidence is durable and
replicated, and the model cannot reach it. Trimming is durable but
model-irreversible.

The same gap appears one level up. The bytes actually sent to the provider are
never persisted. `rendered_request.rs` defines a complete DTO and a capture
seam, but `rendered_request_capture_factory` defaults to `None` and the only
setter (`agent/builder.rs:112`) has no production caller. A trace without its
rendered input is a log, not a training example.

## Scope

In scope:

- A bounded, model-executable resolver for trimmed tool results (#722).
- Durable, reconstructible provider inputs via hash-verified reconstruction
  (#840), which resolves #523's open design question.
- Making the config collections that determine the preamble `@branchable`.

Out of scope, filed separately:

- ACP policies for the `sessions` tool. This slice inherits the existing
  `agent_did` / `requester_did` filter; a follow-up brings the whole tool under
  ACP in one pass.
- The `training_safe` redaction fix (#842). It currently masks every non-empty
  string, so exports through it are useless; that is a separate defect.
- Any reward or outcome document.
- Schema migration mechanics. Explicitly deferred.
- The generic `defra_query` escape hatch. It exists; this resolver is
  purpose-built and does not use or extend it.

## Part 1 — Tool-result resolver (#722)

### Data model

`tool_call_key` is already composite: `hook/persistence/helpers.rs:162` builds
it as `"{session_id}:{tool_call_id}"`. The session id is a prefix of the key,
so a key minted in one session structurally cannot match a row in another.
Cross-session linkage integrity is therefore a property of the key format, not
a runtime check, and needs no proof.

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
persisted one is composite; `compaction/history.rs:318` returns bare
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
`tool_call_key` from `session_id` and `tool_call_id`, which makes a
cross-session read unreachable rather than merely rejected.

Resolution: compose key → look up `AgentToolCall` by unique index, filtered on
`agent_did` / `requester_did` → if an `AgentToolResult` carries that key,
source is `spilled` and the bytes come from `output_text`; otherwise source is
`inline` and they come from `AgentToolCall.result` → slice.

### Paging

Reuse `read_retained_output_slice` (`background_tools.rs:1094-1137`), the Rust
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

`tool_result_was_truncated` (`history.rs:297-304`) currently decides whether
output was truncated by sniffing text for `"[Full output: DefraDB doc"`,
`"Showing lines"`, or the bare word `"truncated"`. A tool result whose content
merely contains the word "truncated" — a compiler warning, a log line, a diff —
is misclassified as truncated and stubbed as though full output existed
elsewhere. Once `tool_call_key` exists, ask the database instead of guessing.

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

## Part 2 — Hash-verified reconstruction (#840, resolving #523)

### Why not store the payload

Storing the full rendered payload every turn is quadratic: turn 50's
`messages_json` contains turns 1–49, multiplied by every retry attempt. The
canonical messages are already persisted. The rendered input is a deterministic
function of them, and `PromptAssembly.render_determined` already proves the
render depends only on the variables actually read. Storing the output of a
proven-deterministic function alongside its inputs is duplication.

### The blocker

22 collections are `@branchable` — `AgentMessage`, `AgentToolCall`,
`AgentToolResult`, `AgentRequest`, `CompactionEntry`, `AgentSession`, the
entire conversation side. Time-travel reads are available for every row that
carries a turn.

`AgentBehavior` is not, and it holds `system_prompt`, `model_name`,
`tool_selection_id`, `inference_profile_id`, `skill_refs`, and
`compaction_threshold` — everything determining the preamble. `tool_selection`
and `skill` are likewise not branchable.

So the conversation half of a render is reconstructible today and the
configuration half is not. Editing a behavior's system prompt silently makes
every historical rendered request unreconstructible, with no way to distinguish
a capture bug from a legitimate config edit.

### Design

**Make the render-contributing config collections `@branchable`:**
`agent_behavior`, `tool_selection`, `skill`. Without this, reconstruction is
unsound and no amount of hashing repairs it.

**Stamp CIDs at render; do not time-correlate.** This resolves #523's open
question. Time-correlation races concurrent config edits and yields a
plausible-but-wrong reconstruction, which is strictly worse than a failed one.
At render time the envelope records the commit CID of every contributing config
document.

**Persist a constant-size envelope**, not the payload: `prompt_hash`,
`tools_hash`, `sampling_json`, `tool_choice_json`, `model_name`, `source`,
`turn_index`, `attempt`, and the pinned CID set. `prompt_hash` and `tools_hash`
are SHA-256 over canonical JSON with sorted keys, already implemented at
`rendered_request.rs:140-159`.

#840 calls for this collection to be encrypted to session participants. Because
the envelope is hashes and identifiers rather than prompt text, it carries far
less sensitive content than the payload would have — the confidentiality
requirement is correspondingly weaker, and the reconstruction inputs
(`AgentMessage`, config documents) retain whatever protection they already
have. The implementation plan selects the concrete mechanism; this design
requires only that the envelope be no more widely readable than the documents
it references.

**Install the capture sink by default.** `rendered_request_capture_factory`
stops defaulting to `None`.

**Reconstruct on demand:** time-travel read each config document at its pinned
CID → replay PromptAssembly over canonical messages bounded by the
`CompactionEntry` state → hash the result → compare against `prompt_hash`.

A match is byte-exact provenance. A mismatch is a real finding: either the
sanitizer is nondeterministic or a contributing input was not pinned. The
verification is self-checking — `render_determined` says that if reconstruction
and capture disagree, something outside the model is wrong.

### Epoch boundary

Rendered requests captured before the config collections became branchable
cannot be reconstructed. Report that honestly, the same way `unavailable` is
reported for pre-migration tool results. Do not imply recoverability.

### Run timeline

#840 requires surfacing rendered inputs in the run timeline and adapter
projections. `run_timeline_fetch.rs:347-370` does not select
`prompt_tokens` / `completion_tokens` / `cached_input_tokens` from
`InferenceCall`, and the `AgentRequest` selection omits `caused_by_trigger_id`,
`caused_by_trigger_kind`, and `execution_origin`. Since this work is already in
that fetch layer, add those fields. This unblocks part of #991 and #841 at
near-zero marginal cost.

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
3. Inspect what the model sees. Assert against captured `messages_json` that
   the marker is **absent** and the executable stub is **present**.
4. Recovery turn — instruct the model to retrieve the earlier result. Assert it
   calls `sessions(action="read_tool_result")` and that the marker appears in
   the returned result. This is the claim under test: the model, not an
   operator, recovers its own trimmed evidence.
5. Paging — with `max_bytes` below the result size, assert `has_more`, follow
   `next_offset`, and assert the pages are gap-free and reassemble to the
   original bytes.

Step 3 is possible because capture is on by default under Part 2.
`rendered_request` is already consumed by `compaction/tests.rs` and
`loop_stream/tests.rs`, so the mechanism is proven at unit scope; this is its
first end-to-end use.

### Reconstruction

- Round-trip: capture a rendered request, reconstruct it, assert
  `prompt_hash` matches.
- Config drift: capture, mutate the behavior's `system_prompt`, reconstruct at
  the pinned CID, assert the hash still matches — the edit must not corrupt
  history.
- Epoch: a pre-branchable capture reports unreconstructible rather than
  mismatching.

### Non-live

- Cross-session key rejection.
- Spilled versus inline source selection.
- Repeated stripping is idempotent.
- Negative authorization: another principal's call resolves to `not_found`.
- Failed spill reports `unavailable`, never a fabricated recovery.

## Formal model

No new Lean is required for authorization; that is enforced by filtering today
and by ACP later, and neither is a theorem. Cross-session linkage needs no
proof either, because the composite key format makes it structural.

Paging reuses the proven `readSlice` model unchanged.

If threading `tool_call_id` into the truncator or making config collections
branchable changes a modeled transcript or tool-call invariant, update Lean
first per the foundation flow. Otherwise record why the existing read-only
tool-policy model suffices.

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
- Config collections feeding the render are `@branchable`.
- Rendered-request capture is on by default and persists a constant-size
  envelope with pinned CIDs.
- A captured rendered request reconstructs to a matching `prompt_hash`, and a
  later config edit does not corrupt that.
- Run timeline includes `InferenceCall` token fields and request trigger
  lineage.
- Canonical `AgentMessage`, `AgentToolCall`, and `AgentToolResult` documents are
  never destructively rewritten by retrieval.
- `sessions(action="list")` remains compatible.
- `cargo test -p gents` and `cargo check --workspace --all-targets` pass.

## Open questions

None blocking. Migration mechanics are deferred by decision; ACP policy for the
`sessions` tool is a tracked follow-up.
