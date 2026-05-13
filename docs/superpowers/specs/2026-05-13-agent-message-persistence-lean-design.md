# AgentMessage Persistence + Ordering Lean Design

**Status:** Design
**Date:** 2026-05-13
**Tracks:** issue #191; parent #183; motivating bug #160; downstream consumers #184 and #190.
**Scope:** Lean-only transcript state machine and generated conformance vectors for the Rust runtime. No production Rust behavior changes.

## 1. Goal

Add a new Lean module, `Proofs/Transcript/`, that models the durable transcript contract for one session: `AgentMessage` rows, `AgentToolCall` rows, their shared ordering vocabulary, tool-result message dedupe by `message_key`, and the coherent-state obligations that compaction and streaming response models will consume.

The model closes the audit's top gap: storage observation currently proves whether a mutation succeeded or failed, but not whether the resulting transcript is ordered and coherent across message rows and tool-call rows.

## 2. Why now

The #160 fix made tool-result transcript persistence idempotent by deriving a stable `AgentMessage.message_key` from `(session_id, logical result id, payload hash)` and upserting through that key. That bug class was an ordering bug: duplicate observations could materialize duplicate user-role tool-result messages and shift later sequence assumptions.

The same transcript vocabulary is needed by:

- #184 compaction, which must not retain or delete only one half of a tool-call/tool-result pair.
- #190 streaming response lifecycle, which needs stable final-message ordering.
- R4 subagent tooling, which reads `AgentToolCall` and `AgentMessage` as one transcript surface.

## 3. Verified obligations

| ID | Source | Obligation | Where it lands |
|---|---|---|---|
| #191-A | issue #191 acceptance | Tool-call/message-pair atomicity for coherent transcript states: completed native tool calls have exactly one paired tool-result message, and each durable tool-result message points back to a completed tool call when it is runtime-owned. | `Proofs/Transcript/Properties.lean`; conformance case in `state_machine_conformance.rs`. |
| #191-B | issue #191 acceptance | Append-order monotonicity within a session: durable messages and tool-call reserved message sequences are read in total sequence order. | `Proofs/Transcript/Properties.lean`; generated ordering cases. |
| #191-C | issue #191 + #160 | Tool-result dedupe: repeated observations with the same `(session, logical result id, payload hash)` resolve to exactly one durable user tool-result message row. | `Proofs/Transcript/Dedupe.lean`; generated #160 regression vector. |
| #191-D | issue #191 hook-drop clause | Hook in-flight drain semantics. Current Rust `Drop` clears in-memory ownership and intentionally leaves durable `running` rows for startup recovery. The stronger "committed or removed on drop" property is not true today. | Design-level boundary and derived requirement; explicit drain transitions (`cancel_in_flight`, `fail_in_flight`, timeout sweep) are modeled, destructor `Drop` is not claimed to satisfy stronger drain semantics. |
| #184 handoff | issue #184 | Compaction must preserve pair closure and retained-window ordering. | Exposed predicates: `Coherent`, `PairClosed`, `OrderedBySequence`, `RetainsPairs`. |
| #190 handoff | issue #190 | Streaming response terminal rows can rely on stable transcript sequence vocabulary. | Exposed `Sequence` and message-order predicates; no response-state model here. |

## 4. Model

### 4.1 Module placement

Use a new directory:

```text
crates/defra-agent/proofs/Proofs/Transcript/
  State.lean
  Transition.lean
  Dedupe.lean
  Properties.lean
  Executable.lean
```

and a barrel:

```text
crates/defra-agent/proofs/Proofs/Transcript.lean
```

This is not an extension of `Proofs/Session/`: the existing session model is a request queue model, while this proof owns cross-row transcript coherence between `AgentMessage` and `AgentToolCall`.

### 4.2 Abstract vocabulary

Lean abstracts over runtime strings with small opaque natural identifiers:

- `MessageId`, `ToolCallId`, `LogicalResultId`, and `PayloadHash`.
- `Sequence := Nat`, matching persisted `AgentMessage.sequence` and `AgentToolCall.message_sequence`.
- `ToolResultKey := SessionId × LogicalResultId × PayloadHash`, matching #160's stable message-key input.

Rows:

```lean
inductive MessageRole
  | user
  | assistant

inductive MessageKind
  | ordinary
  | assistantToolCall (callId : ToolCallId)
  | toolResult (callId : ToolCallId) (key : ToolResultKey)

structure MessageRow where
  sessionId : SessionId
  sequence : Sequence
  role : MessageRole
  kind : MessageKind

structure ToolCallRow where
  sessionId : SessionId
  callId : ToolCallId
  messageSequence : Sequence
  state : ToolExecution.ToolCallState
  resultKey? : Option ToolResultKey

structure TranscriptState where
  sessionId : SessionId
  nextSeq : Sequence
  messages : List MessageRow
  toolCalls : List ToolCallRow
  inFlight : Finset ToolCallId
```

The model imports `Proofs.Basic` and `Proofs.ToolExecution.State` for `SessionId` and persisted tool-call state vocabulary. It does not import `Proofs.Session`, `Proofs.Subagent`, or `Proofs.Composed`; those modules can consume transcript predicates later.

### 4.3 State predicates

Core predicates:

- `MessageSequencesUnique s`: no two messages in a session share a sequence.
- `ToolCallIdsUnique s`: no two tool-call rows in a session share `callId`.
- `ToolResultKeysUnique s`: no two durable tool-result messages share `ToolResultKey`.
- `OrderedBySequence s`: message reads sorted by `sequence` form the transcript order.
- `ToolCallReservedByMessage s`: each `ToolCallRow.messageSequence` is reserved by an assistant tool-call message with the same call id, or by an assistant-building transition before the message is persisted.
- `CompletedToolCallsPaired s`: every completed native tool call with `resultKey? = some key` has exactly one user tool-result message carrying `key`.
- `ToolResultMessagesPaired s`: every runtime-owned user tool-result message has a completed tool-call row with the same call id and key.
- `Coherent s`: conjunction of uniqueness, ordering, reservation, pair closure, and `nextSeq` monotonicity.

The assistant-building exception matches `TranscriptTurnState::AssistantBuilding`: a tool-call row can reserve a future assistant message sequence before the assistant `AgentMessage` is persisted. The completed-pair predicate is only required after the result-message append transition succeeds.

## 5. Transitions

The relational model has one-session transitions:

- `appendUser`: append an ordinary user message, reset assistant turn.
- `beginAssistantToolCall`: reserve a new assistant sequence and create a running `AgentToolCall`.
- `persistAssistant`: persist the assistant message for the reserved sequence.
- `completeToolWithResult`: terminalize a running native tool call and append the paired tool-result message as one abstract successful transcript step.
- `observeDuplicateToolResult`: no-op when the same `ToolResultKey` already has a durable message.
- `appendDistinctToolResult`: append when the key differs.
- `cancelInFlight`, `failInFlight`, `timeoutInFlight`: explicit drain transitions for hook paths that call lifecycle terminalizers.
- `abandonHookOwnership`: clear `inFlight` ownership without changing durable rows. This models the current destructor boundary and is deliberately excluded from `Coherent` preservation unless recovery later terminalizes the row.

The abstract `completeToolWithResult` transition represents the successful hook path after both the `AgentToolCall` completion mutation and `AgentMessage` tool-result mutation have succeeded. Physical cross-row transaction atomicity is not claimed; storage failure behavior stays under `StorageObservation` and hook failure policy tests.

## 6. Properties

Minimum theorems:

- `append_preserves_ordered`
- `tool_call_reserves_assistant_sequence`
- `persist_assistant_closes_reserved_tool_call_sequence`
- `complete_tool_with_result_preserves_coherent`
- `completed_tool_has_exactly_one_result_message`
- `tool_result_message_has_completed_tool_call`
- `duplicate_tool_result_observation_noops`
- `dedupe_exactly_one_row_per_key`
- `distinct_tool_result_keys_append_distinct_rows`
- `explicit_inflight_drain_removes_ownership`
- `abandon_hook_ownership_not_strong_drain`

The last theorem is intentionally negative/documentary: it states that clearing `inFlight` alone does not imply terminalized durable rows. That gives #191 a precise statement of the runtime gap instead of weakening the transcript invariant.

## 7. Conformance vectors

Extend `Proofs.Conformance.Contracts` with transcript cases:

- `transcript_ordering_cases`: user → assistant tool-call → tool-result sequence shape.
- `transcript_dedupe_cases`: #160 duplicate result observation reuses the first sequence.
- `transcript_distinct_result_cases`: same payload with different logical result ids stays distinct.
- `transcript_pairing_cases`: completed tool call paired with exactly one user tool-result message.
- `transcript_hook_boundary_cases`: explicit drain is terminal/ownership-clearing; destructor abandon is reported as a boundary, not a strong drain proof.

Rust consumer:

- Add deserialization fields in `crates/defra-agent/src/lean_vocab_test.rs`.
- Add `state_machine_conformance.rs` tests that consume the generated rows and compare them against existing runtime behavior using the hook/session APIs and direct DefraDB queries.
- Register consumers in `tests/support/conformance_consumers.rs` and coverage ledger entries in Lean.

## 8. Derived requirements

- #184 must use `RetainsPairs`/`Coherent` rather than re-inventing transcript ordering.
- #190 should use `Sequence` and `OrderedBySequence` to tie final response materialization to transcript order.
- A follow-up should decide whether hook `Drop` should synchronously terminalize/remove durable running tool rows. Current code explicitly leaves them for startup recovery, so #191 cannot honestly prove "drop commits or removes every in-flight row" without a Rust behavior change.
- If the runtime wants true cross-row atomicity for `AgentToolCall` completion plus tool-result `AgentMessage`, it needs a transactional write path or a recovery/repair path for fail-open partial completion. This issue models the successful transcript contract and names the boundary.

## 9. Out of scope

- Production Rust changes.
- DefraDB transaction semantics or proof of physical cross-collection atomicity.
- Compaction strategy proofs. #184 consumes this module.
- AgentResponse streaming lifecycle. #190 consumes this module.
- Subagent parent/child bridge correctness beyond using existing `ToolExecution.ToolCallState` vocabulary.
- Hash-function correctness for the #160 key. Lean treats `PayloadHash` as an abstract stable value supplied by the runtime.

## 10. Open questions

- Whether to make the hook destructor boundary a separate follow-up issue or attach it to #191's PR body as a deferred exclusion.
- Whether the conformance emitter should expose transcript cases as a standalone `transcript_conformance_cases` array or as a `Transcript` state-machine contract plus richer case rows. The implementation plan should prefer richer case rows, because the important checks are cross-row predicates rather than a small finite phase graph.
