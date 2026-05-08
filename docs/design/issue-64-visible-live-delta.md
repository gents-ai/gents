# Issue #64 — Protocol-level visible live delta for streaming assistant output

Status: design pass, pre-implementation
GitHub: https://github.com/sourcenetwork/defra-agent/issues/64
Branch: `design/issue-64-live-delta`

## TL;DR

`AgentResponse.content` / `AgentResponse.reasoning` are redefined to mean **the
live tail of the active assistant segment** — the visible bytes streamed since
the most recent commit boundary in this turn. The writer resets them on every
tool boundary and on finalize. The schema is unchanged.

This is a writer-side fix, not a schema change. It is the minimum protocol
change that lets every client (desktop today, iPhone next) render streaming
assistant output with a ~10-line render rule and no string diffing.

## Current state

### How the surface is shaped today

- `AgentMessage` rows are the durable transcript, ordered by `sequence`.
- `AgentToolCall` rows attach to a transcript message via `message_sequence`.
- `AgentResponse` is a per-`request_id` row with cumulative `content` and
  `reasoning` fields, plus `status`, `progress_seq`,
  `materialized_message_sequence`, `materialized_at`, `interrupted_at`,
  `completed_at`, `error_message`, `token_count`.

`AgentResponse.content` is currently the cumulative buffer for the entire
turn — it is never reset. Partial assistant turns are persisted into
`AgentMessage` at every tool-result boundary
(`crates/defra-agent/src/agent/stream_processor.rs:111-126`), but the response
buffer keeps accumulating.

### Why that hurts clients

Because `AgentResponse.content` is cumulative and partial `AgentMessage` rows
are committed under the same turn, the live overlay clients want to render
(only the bytes streamed since the last commit) is the *suffix* of
`AgentResponse.content` after subtracting the persisted prefixes.

The desktop client implements that subtraction in
`apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs:58-79`
(`live_overlay_suffix`). It walks committed assistant texts in the active
turn and trims each off the front of the cumulative live text.

This is brittle:

- Markdown normalization runs on the persisted-side
  (`present_persisted_message`) but not the live-side, so whitespace and
  formatting can diverge.
- Reasoning isn't prefix-stripped at all — the bridge takes the full
  cumulative reasoning for the overlay.
- If any prefix fails to match, the whole cumulative content is shown,
  duplicating already-committed assistant text.
- Every new client (iPhone, web, CLI) re-implements the same heuristic
  against the same brittle surface.

The Lean model in `crates/defra-agent/proofs/Proofs/Client/Types.lean`
captures the 6 client turn states correctly; it does not model the overlay
rendering rule.

## Options considered

**A. Writer-side tail reset, no schema change (chosen).**
Redefine `content`/`reasoning` as the live tail. Writer resets at every
commit boundary. Document the contract; clients implement a small render
rule. Replication-lag race is documented as known-acceptable.

**A+. A plus an explicit `after_message_sequence: Int` anchor.**
Same writer change, plus a new optional Int field that pins the overlay
slot atomically. Eliminates the lag race. Forward-compatible with A — A
can be upgraded to A+ later by adding the field; clients prefer when
present and fall back to `max sequence` when absent.

**B. Sidecar `AgentResponseOverlay` document.**
Leave cumulative semantics in place; add a parallel overlay row keyed by
`request_id`. Doubles the write rate, doubles the replication surface,
doesn't kill the brittle path for anyone still reading `content`.

**C. Bridge-only synthesis, no protocol change.**
Polish the desktop heuristic and document it. Ships fastest, doesn't solve
the issue — every other client repeats the same heuristic.

**Decision: A.** It's the minimum change that eliminates the heuristic.
The schema stays as-is. If the lag race bites the iPhone-over-P2P case in
practice, A+ is a small additive follow-up.

## Protocol contract

The contract for two existing fields changes; the schema does not.

> **`AgentResponse.content`** and **`AgentResponse.reasoning`** represent the
> **live tail** of the active assistant segment — the visible bytes streamed
> since the most recent commit boundary in this turn. They are reset to empty
> whenever a partial assistant turn or a tool-result is persisted as an
> `AgentMessage`, and again on finalize. They are not a transcript record. The
> transcript is `AgentMessage`.

Other `AgentResponse` fields are unchanged:

- `status` — `streaming` / `complete` / `error` (terminal).
- `progress_seq` — strict-monotonic version cursor; bumps at lifecycle
  boundaries (`RequestLifecycle::advance`).
- `token_count` — cumulative across the turn (metering, not rendering).
- `materialized_message_sequence` / `materialized_at` — set when the final
  assistant turn is persisted.
- `error_message` — diagnostic on `error`; not part of the live overlay.
- `interrupted_at` — set when the operator cancels; turn becomes terminal
  via `lifecycle_state`, overlay hides via the hide rule.

### Hide rule

The overlay is hidden when **any** of the following holds:

- `AgentResponse.status` is terminal (`complete` or `error`).
- `AgentResponse.materialized_message_sequence` is set and the matching
  `AgentMessage` is observed.
- The derived turn state is terminal (`completed`, `failed`, `superseded`,
  `interrupted`).
- Both `content` and `reasoning` are empty (or whitespace-only).

### Replication-lag note

There is a brief window where `AgentResponse.content` for a post-tool tail
can replicate before the tool-result `AgentMessage` that explains the
boundary. During this window the overlay may render at a lower-sequence
slot than its true anchor. This is self-healing within milliseconds on a
local DefraDB node and within seconds on slow P2P links. If this becomes a
visible problem, the forward-compatible fix is the optional
`after_message_sequence: Int` field (A+).

## Client rendering rules

A compliant client renders an active turn with this algorithm:

```
input:
  committed_messages : [AgentMessage]   # filtered to the active turn
  tool_calls         : [AgentToolCall]  # filtered to the active turn
  active_response    : AgentResponse?   # tip response for the active turn
  derived_turn       : ClientTurnState? # from existing client-protocol

output:
  timeline : [TimelineItem]

algorithm:
  sort committed_messages by sequence ASC
  group tool_calls by message_sequence
  for each msg in committed_messages:
    emit UserMessage / AssistantMessage / ToolMessage based on role
    if tool_calls grouped at msg.sequence:
      emit ToolGroup
  emit any tool_calls whose message_sequence has no matching committed
    message yet (lag fallback)

  if should_show_overlay(active_response, derived_turn):
    emit LiveAssistant {
      content   : active_response.content,
      reasoning : active_response.reasoning,
    }

should_show_overlay(r, t):
  r is not None
  AND r.materialized_message_sequence is None
  AND r.status not in {"complete", "error"}
  AND t in {WaitingForClaim, Streaming}
  AND (r.content non-empty OR r.reasoning non-empty)
```

This becomes the canonical version of the existing TS / Swift / Rust
examples in `crates/defra-agent/proofs/client-state-machine.md`.

### Concrete in-tree changes

- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs:58-79`
  (`live_overlay_suffix`) — **delete**.
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs:98-135`
  (`active_turn_committed_assistant_texts`) — **delete** (only used by
  `live_overlay_suffix`).
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs:233-247`
  — replace the call to `live_overlay_suffix(...)` with a direct read of
  `overlay.content` / `overlay.reasoning`.
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/session.rs:110-126`
  (`active_response_overlay` filter) — tighten to the canonical hide rule:
  only `WaitingForClaim` / `Streaming` turns, non-terminal response status, no
  materialization, and a non-empty tail.
- `apps/desktop-tauri/src/components/Transcript.tsx:181-198`
  (`liveAssistant` rendering) — no change; already renders the overlay
  block correctly given the bridge inputs.

iPhone client implements the same algorithm against the same filter set,
following the spec doc and the LiveOverlay conformance cases.

## Backend write-path plan

Two surgical changes in the runtime, plus one materialization signature
update in the desktop core. Schema unchanged.

### `crates/defra-agent/src/streaming.rs` — `DefraStreamWriter`

Add a new method:

```rust
/// Reset the live tail at a commit boundary.
/// Clears in-memory tail buffers and persists empty
/// content/reasoning on the streaming response row.
pub async fn reset_tail(&self, doc_id: &str) -> Result<()>;
```

Behavior:

- Acquire `self.buffers` lock; clear `StreamBuffer.content` and
  `StreamBuffer.reasoning`. Leave `StreamBuffer.token_count` cumulative
  (it's a metering field).
- Issue an `update_AgentResponse` mutation gated by
  `status: { _eq: "streaming" }`, setting `content: ""`, `reasoning: ""`.
  `progress_seq` is owned by the lifecycle layer and already advances at
  the same boundary, so the reset itself does not bump it.
- Same retry / status-mismatch handling as `flush_snapshot`.

Update `finalize` (the existing terminal mutation builder
`build_finalize_mutation`) to write `content: ""` and `reasoning: ""`
alongside `status` / `completed_at`. Today it writes the cumulative buffer
back; under the new contract it writes empty.

### `crates/defra-agent/src/agent/stream_processor.rs` — call-site changes

Insert a `reset_tail` call immediately after each successful `persist_message`
/ `persist_stream_tool_result_message`:

- `StreamedUserContent::ToolResult` arm
  (`stream_processor.rs:105-127`): after the partial assistant turn AND
  the tool-result message are persisted.
- `MultiTurnStreamItem::FinalResponse` arm
  (`stream_processor.rs:128-143`): after the final `persist_message` and
  `mark_current_response_materialized`. Note: `finalize` will also clear
  the tail in its terminal mutation; the explicit `reset_tail` here
  guarantees the cleared state is visible to subscribers between the
  partial-turn persistence and the terminal mutation.

The `StreamedAssistantContent::ToolCall` arm
(`stream_processor.rs:96-104`) does NOT persist the partial assistant
turn today — it accumulates on the in-memory `AssistantTurnAccumulator`
and persists at the next `ToolResult`. The reset stays paired with the
persistence call, so this arm is unchanged.

### `crates/defra-agent/src/agent/stream_processor.rs::persist_partial_turn`

This helper is called on the interrupt path
(`stream_processor.rs:158-172`). Add a `reset_tail` call after a
successful persistence; the overlay then shows nothing post-interrupt,
which matches the existing visual behavior.

### `crates/defra-agent-desktop-core/src/client/core/materialization.rs`

`MaterializationSignature` (`materialization.rs:24-35`) keeps
`response_content_len` and `response_reasoning_len`, but their *meaning*
changes: previously they measured turn-cumulative growth; under
tail-reset they measure the *current tail* length, which resets to 0 at
every commit boundary and grows during active streaming. That is
actually a cleaner stall signal — the detector observes either
"tail length still growing" (active) or "tail length unchanged for N
seconds" (stalled). Doc-comment update only; no detector logic change.

`progress_seq` increments at `RequestLifecycle::advance` (lifecycle
boundaries) but not on every flush, so it alone is insufficient as a
within-boundary signal — keep the length fields. The 5-second
`MATERIALIZATION_STALL_THRESHOLD` remains appropriate.

## Migration and backcompat

This is a single-repo, single-deployment runtime. There are no external
clients reading `AgentResponse.content` with cumulative semantics. The
desktop client and the runtime ship together.

Migration steps:

1. Update `client-state-machine.md` (docs commit, lands first).
2. Update Lean model + conformance cases (lands before code so the proof
   is the source of truth, per the project flow described in
   `CLAUDE.md`).
3. Land the writer change (`streaming.rs` + `stream_processor.rs`) and the
   bridge change (`timeline.rs`) together. Once the writer resets the
   tail, leaving `live_overlay_suffix` in place would still work but
   becomes dead code — delete in the same commit.
4. Update `materialization.rs` signature. Independent of the bridge
   change; can land before or after.

Old persisted `AgentResponse` rows from prior runs may carry cumulative
content. They will hide via the terminal-status hide rule (those turns
are already `complete` / `error` / `interrupted`), so no backfill is
needed.

## Proof impacts

The 6-state turn projection in `Proofs/Client/Types.lean` is unchanged.
The overlay isn't a new client state — it's a render rule applied to the
existing `streaming` / `waitingForClaim` states.

### Lean changes

`Proofs/Client/Types.lean::ResponseSnapshot` — extend with one bool:

```lean
structure ResponseSnapshot where
  status    : ResponseStatus
  tailEmpty : Bool
  deriving DecidableEq, Repr
```

`deriveAttempt` and `deriveTurn` are unchanged; they operate on `status`,
not on tail bytes. All existing T2 / T3 / T4 / T5 proofs continue to hold.

`Proofs/ClientShell/Projection.lean` — add an `OverlayBlock` slot to the
`ChatView` and a pure helper:

```lean
def projectActiveOverlay
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    : Option OverlayBlock
```

implementing the `should_show_overlay` predicate. New theorems:

- **O1 (single overlay):** `projectActiveOverlay` returns at most one
  `OverlayBlock`. Trivial by construction.
- **O2 (terminal hides overlay):** if `turn.isTerminal = true`,
  `projectActiveOverlay` returns `none`.
- **O3 (materialized hides overlay):** if
  `resp.materializedMessageSequence ≠ none`, `projectActiveOverlay`
  returns `none`.

**O4 (no-leak invariant)** — "the writer never leaves a tail that is a
prefix of any committed assistant message in the same turn" — is stated
as a writer-side axiom, exercised by the LiveOverlay conformance cases
below rather than re-derived in Lean.

### Conformance generators

Add `Proofs/Conformance/ContractCases/LiveOverlay.lean` emitting:

| Case | Stream pattern |
|---|---|
| `pre_first_tool` | tokens then final, no tools |
| `post_tool_resumed` | tokens, tool-call, tool-result, tokens, final |
| `interleaved_two_tools` | tokens, tool, tokens, tool, tokens, final |
| `tool_first_no_pre_text` | tool-call, tool-result, tokens, final |
| `interrupted_mid_stream` | tokens, interrupt |
| `error_mid_stream` | tokens, provider error |
| `materialized_final` | tokens, final, observed materialization |

Register the domain in `Proofs/Conformance/CoverageLedger.lean` with a
runtime consumer pointer (`tests/live_overlay_conformance.rs`, see
below). Update `client-state-machine.md` with the new "Live Overlay"
subsection.

## Test plan

### Lean

`cd crates/defra-agent/proofs && lake build` stays green after the
`Types.lean` extension and the new theorems.

### Rust unit tests

- `crates/defra-agent/src/streaming/tests.rs` — new test for `reset_tail`:
  begin, write tokens, reset, write more, assert the persisted row's
  `content` carries only post-reset bytes.
- `crates/defra-agent/src/agent/stream_processor/tests.rs` — drive
  `Text → ToolCall → ToolResult → Text → FinalResponse` and assert the
  response row's `content` history follows the expected reset pattern.
  Cover the interrupt path via `persist_partial_turn`.
- `crates/defra-agent-desktop-core/src/client/core/materialization.rs`
  unit tests — verify the stall detector still trips when the *current
  tail* length plateaus (e.g. tokens emitted then silence within a
  boundary), and recovers when length resumes growing or boundary
  counters advance.
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_timeline.rs`
  — replace any tests that depend on `live_overlay_suffix` semantics with
  tests that assert overlay = `response.content` directly under the new
  contract. Cover post-tool-resumed and materialized-hide.

### Rust integration tests

- New file `crates/defra-agent/tests/live_overlay_conformance.rs`. For
  each generated `LiveOverlay` conformance case, drive a streaming
  session against a mock backend that emits the case's stream, then
  assert at each observable checkpoint:
  - `AgentResponse.content` matches the expected tail (cumulative writer
    behavior would fail here).
  - Applying the documented render algorithm to
    `(committed_messages, tool_calls, response, derived_turn)` produces
    the expected timeline.

### TypeScript projection tests

- `apps/desktop-tauri/src/lib/chat-shell.test.ts` — consume the generated
  `LiveOverlay` frontend conformance rows (same pattern as the existing
  `frontend_client_shell_cases`) and assert the projected timeline.

### Smoke coverage

The existing desktop test surface
(`apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/`,
`apps/desktop-tauri/src/lib/chat-shell.test.ts`) covers the
end-to-end render path. No separate manual smoke step planned —
regressions surface through those suites and the new conformance test.

## Risks and open questions

1. **Replication-lag mis-slotting.** Brief, self-healing on local nodes;
   may be visible on iPhone-over-P2P. Forward-compatible mitigation is
   the optional `after_message_sequence: Int` field. Defer until
   observed in practice.
2. **`token_count` semantics.** Kept cumulative across the turn — it's a
   metering field, not a rendering field. Documented in the contract.
3. **Stall detector signal under tail-reset.**
   `MaterializationSignature` (`materialization.rs:24-35`) uses
   `response_content_len` / `response_reasoning_len` as a within-boundary
   liveness signal. Under tail-reset semantics those fields measure the
   *current tail* rather than cumulative turn length — actually a cleaner
   stall signal, because the tail resets to 0 after every commit and grows
   monotonically during active streaming. `progress_seq` only bumps at
   `RequestLifecycle::advance` (lifecycle boundaries, not every flush), so
   it alone is insufficient as a within-boundary signal. The fields stay in
   the signature; their *meaning* shifts from "turn-cumulative growth" to
   "current-tail growth". Doc-comment update only; no detector logic
   change.
4. **`@branchable` revisions.** Adds a small constant number of extra
   revisions per turn (resets at boundaries + finalize). No consumer
   relies on revision count today.
5. **Single-mutation atomicity for boundary persist + reset.** Folding
   them into one mutation block would eliminate the same-node race
   window. Today they're called sequentially across two crates
   (`DefraSessionHook::persist_message` then
   `DefraStreamWriter::reset_tail`). Practical answer: keep separate,
   document the tiny window. Revisit if it bites.

## Codex-ready implementation breakdown

Six commits, each independently reviewable and `cargo check` /
`lake build` / `cargo test -p <package>` clean.

1. **`crates/defra-agent/proofs/client-state-machine.md`** — update contract.
   Replace the "AgentMessage" rendering note. Add a "Live Overlay"
   subsection with the tail-only contract, the hide rule, and the
   render algorithm with TS / Swift / Rust pseudocode.

2. **Lean model + conformance.**
   `Proofs/Client/Types.lean` extends `ResponseSnapshot` with
   `tailEmpty`. `Proofs/ClientShell/Projection.lean` adds
   `OverlayBlock`, `projectActiveOverlay`, theorems O1–O3.
   `Proofs/Conformance/ContractCases/LiveOverlay.lean` lands with the
   case table. `Proofs/Conformance/CoverageLedger.lean` registers the
   domain. `lake build` green.

3. **`crates/defra-agent/src/streaming.rs`** — writer change.
   Add `DefraStreamWriter::reset_tail`. Update
   `build_finalize_mutation` to write empty content/reasoning. Update
   `streaming/tests.rs`.

4. **`crates/defra-agent/src/agent/stream_processor.rs`** — call-site
   change. Insert `reset_tail` after each `persist_message` /
   `persist_stream_tool_result_message` success in the `ToolResult` arm
   and the `FinalResponse` arm, plus in `persist_partial_turn`. Update
   `stream_processor/tests.rs`.

5. **`crates/defra-agent-desktop-core/src/client/core/materialization.rs`**
   — stall signature meaning update. Doc-comment on
   `MaterializationSignature` to reflect that
   `response_content_len` / `response_reasoning_len` now measure the
   *current tail* (resets at every commit boundary), not turn-cumulative
   growth. Add a test asserting the detector still trips on within-boundary
   silence; struct shape unchanged.

6. **Bridge + integration tests.** Delete `live_overlay_suffix` and
   `active_turn_committed_assistant_texts` in
   `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs`.
   Replace the call site to read `overlay.content` /
   `overlay.reasoning` directly. Add
   `crates/defra-agent/tests/live_overlay_conformance.rs` driving the
   new conformance cases against a mock backend. Confirm
   `apps/desktop-tauri/src/lib/chat-shell.test.ts` consumes the new
   frontend conformance rows.

`cargo test --workspace` + `lake build` verify the whole change.

## References

- Issue: https://github.com/sourcenetwork/defra-agent/issues/64
- Schema: `crates/defra-agent-protocol/schemas/agent/agent_response.graphql`
- Writer: `crates/defra-agent/src/streaming.rs`
- Stream processor: `crates/defra-agent/src/agent/stream_processor.rs`
- History: `crates/defra-agent/src/session/history.rs`
- Bridge: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/{session,timeline}.rs`
- Frontend: `apps/desktop-tauri/src/components/Transcript.tsx`
- Lean: `crates/defra-agent/proofs/Proofs/Client*.lean`,
  `crates/defra-agent/proofs/Proofs/ClientShell*.lean`
- Client protocol doc:
  `crates/defra-agent/proofs/client-state-machine.md`
