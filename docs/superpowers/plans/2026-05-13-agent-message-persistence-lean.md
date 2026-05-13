# AgentMessage Persistence + Ordering Lean Implementation Plan

**Status:** Plan
**Date:** 2026-05-13
**Issue:** #191
**Approved design:** `docs/superpowers/specs/2026-05-13-agent-message-persistence-lean-design.md`
**Scope:** Lean proof modules plus generated conformance vectors and Rust test consumers. No production Rust changes.

## Summary

Create `Proofs/Transcript/` as the durable transcript model for one session. The model owns cross-row coherence between `AgentMessage` and `AgentToolCall`: sequence ordering, assistant-turn reservation, completed tool-call/result-message pairing, #160 tool-result dedupe, and the explicit hook drain vs destructor-abandon boundary.

Conformance is emitted as case rows, not a finite phase machine, because the important obligations are cross-row predicates over small row sets.

## File footprint

Lean:

- `crates/defra-agent/proofs/Proofs/Transcript.lean`
- `crates/defra-agent/proofs/Proofs/Transcript/State.lean`
- `crates/defra-agent/proofs/Proofs/Transcript/Transition.lean`
- `crates/defra-agent/proofs/Proofs/Transcript/Dedupe.lean`
- `crates/defra-agent/proofs/Proofs/Transcript/Properties.lean`
- `crates/defra-agent/proofs/Proofs/Transcript/Executable.lean`
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean`
- `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Types.lean`
- `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Transcript.lean`
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- `crates/defra-agent/proofs/Proofs.lean`

Rust tests/support only:

- `crates/defra-agent/src/lean_vocab_test.rs`
- `crates/defra-agent/tests/state_machine_conformance.rs`
- `crates/defra-agent/tests/support/conformance_consumers.rs`

Docs:

- This plan.

## Task 1: Add Transcript Lean skeleton

Create the module directory and barrel files:

- `Proofs/Transcript.lean`
- `Proofs/Transcript/State.lean`
- `Proofs/Transcript/Transition.lean`
- `Proofs/Transcript/Dedupe.lean`
- `Proofs/Transcript/Properties.lean`
- `Proofs/Transcript/Executable.lean`

Register `import Proofs.Transcript` in `Proofs.lean`.

Initial verification:

```bash
cd crates/defra-agent/proofs
lake build Proofs.Transcript
```

Expected: skeleton builds, no theorem bodies yet except trivial `rfl` round-trips if added.

## Task 2: Define transcript state vocabulary

In `State.lean`, define:

- `Sequence`, `MessageId`, `LogicalResultId`, `PayloadHash`.
- `ToolResultKey` with `sessionId`, `logicalResultId`, `payloadHash`.
- `MessageRole.user | assistant`, with `toDefraDB` / `fromDefraDB?` round-trip theorem.
- `MessageKind.ordinary | assistantToolCalls (callIds : Finset ToolExecution.ToolCallId) | toolResult (callId key)`.
- `MessageRow`, `ToolCallRow`, `AssistantTurn`, `TranscriptState`.

Implementation detail from the approved design: add `TranscriptState.assistantTurn : Option AssistantTurn` so the model can represent `TranscriptTurnState::AssistantBuilding` without weakening `ToolCallReservedByMessage`.

Local helpers:

- `MessageRow.isToolResultFor`
- `ToolCallRow.isCompleted`
- `TranscriptState.messageCount`
- `TranscriptState.toolCallCount`
- `TranscriptState.hasToolResultKey`
- `TranscriptState.toolResultMessageCount`
- `TranscriptState.toolCallById?`

Verification:

```bash
lake build Proofs.Transcript.State
```

## Task 3: Define coherence predicates

Still in `State.lean`, define decidable predicates/functions:

- `StrictlyIncreasingMessages`
- `MessageSequencesUnique`
- `ToolCallIdsUnique`
- `ToolResultKeysUnique`
- `NextSeqAboveRows`
- `ToolCallReservedByMessage`
- `CompletedToolCallsPaired`
- `ToolResultMessagesPaired`
- `PairClosed`
- `OrderedBySequence`
- `Coherent`
- `RetainsPairs pre post`

Keep definitions recursive/list-based where possible. Avoid needing a sort theorem: the model stores `messages` in append/read order, and `OrderedBySequence` states that this list is strictly increasing by `sequence`.

Verification:

```bash
lake build Proofs.Transcript.State
```

## Task 4: Add transcript transitions

In `Transition.lean`, define relational transitions:

- `append_user`
- `begin_assistant_tool_call`
- `persist_assistant`
- `complete_tool_with_result`
- `observe_duplicate_tool_result`
- `append_distinct_tool_result`
- `cancel_in_flight`
- `fail_in_flight`
- `timeout_in_flight`
- `abandon_hook_ownership`

The successful `complete_tool_with_result` transition updates the tool-call row to `.completed`, attaches `resultKey? = some key`, appends the user tool-result message, removes the call from `inFlight`, and advances `nextSeq`.

`abandon_hook_ownership` clears in-memory ownership only. It must not change durable `ToolCallRow.state`.

Add `Trace` in the same style as the existing session/tool execution models.

Verification:

```bash
lake build Proofs.Transcript.Transition
```

## Task 5: Prove ordering and reservation properties

In `Properties.lean`, prove:

- `append_preserves_ordered`
- `tool_call_reserves_assistant_sequence`
- `persist_assistant_closes_reserved_tool_call_sequence`
- `append_user_advances_nextSeq`
- `begin_assistant_tool_call_advances_or_reuses_assistant_sequence`

Use helper lemmas for append-preserves-strict-increasing and membership in appended singleton lists.

Verification:

```bash
lake build Proofs.Transcript.Properties
```

## Task 6: Prove pair-closure properties

In `Properties.lean`, prove:

- `complete_tool_with_result_preserves_coherent`
- `completed_tool_has_exactly_one_result_message`
- `tool_result_message_has_completed_tool_call`
- `explicit_inflight_drain_removes_ownership`
- `abandon_hook_ownership_not_strong_drain`

The negative hook theorem should be concrete: construct or state a pre/post where `inFlight` is cleared but a durable row remains `.running`, showing strong drain does not follow from destructor ownership clearing.

Verification:

```bash
lake build Proofs.Transcript.Properties
```

## Task 7: Prove #160 dedupe properties

In `Dedupe.lean`, prove:

- `duplicate_tool_result_observation_noops`
- `dedupe_exactly_one_row_per_key`
- `distinct_tool_result_keys_append_distinct_rows`
- `toolResultKey_session_scoped`

The model treats `PayloadHash` as an abstract stable value. Do not model FNV/hash correctness.

Verification:

```bash
lake build Proofs.Transcript.Dedupe
```

## Task 8: Add executable conformance case rows

In `Executable.lean`, define a small case-row type and finite row list for:

- `ordering_user_assistant_tool_result`
- `dedupe_duplicate_reuses_sequence`
- `distinct_result_ids_append_distinct_rows`
- `completed_tool_pair_closed`
- `explicit_drain_terminalizes_ownership`
- `drop_abandon_not_strong_drain`

Each row should include enough fields for Rust to assert behavior without reimplementing Lean:

- case name and group
- action name
- legal/expected classification
- pre/post message count
- pre/post tool-call count
- pre/post in-flight count
- relevant sequence numbers
- relevant result-key id/hash ids
- expected `pair_closed`
- expected `ordered`
- expected `duplicate_reused_sequence`
- expected `strong_drain`

Verification:

```bash
lake build Proofs.Transcript.Executable
```

## Task 9: Register conformance JSON

Add `TranscriptCase` to `Proofs/Conformance/ContractCases/Types.lean`.

Create `Proofs/Conformance/ContractCases/Transcript.lean` that imports `Proofs.Transcript.Executable` and exposes `transcriptConformanceCases`.

Update:

- `Proofs/Conformance/ContractCases.lean`
- `Proofs/Conformance/Contracts/Json.lean`
- `Proofs/Conformance/CoverageLedger.lean`

JSON field:

```json
"transcript_conformance_cases": [...]
```

Coverage ledger consumer:

```text
state_machine_conformance::generated_transcript_cases_pin_agent_message_ordering_contract
```

Verification:

```bash
lake build Proofs.Conformance.Contracts
lake env lean --run Proofs/Conformance/Contracts.lean > /tmp/defra-agent-contracts.json
```

Expected: JSON parses and includes `transcript_conformance_cases`.

## Task 10: Add Rust deserialization and consumer tests

In `lean_vocab_test.rs`, add:

- `transcript_conformance_cases: Vec<LeanTranscriptCase>`
- `LeanTranscriptCase` struct
- helper `lean_transcript_cases()`

In `state_machine_conformance.rs`, add:

- `generated_transcript_cases_pin_agent_message_ordering_contract`

The test should:

- consume all generated transcript rows,
- assert expected case count and case names,
- map the #160 dedupe row to the existing hook/session behavior shape,
- assert pair-closure/ordering fields from Lean,
- assert the destructor boundary row has `expected_strong_drain = false`.

This is a conformance consumer test, not a new production-path implementation.

Verification:

```bash
cargo test -p defra-agent --test state_machine_conformance generated_transcript_cases_pin_agent_message_ordering_contract -- --nocapture
```

## Task 11: Register Rust conformance consumer

Update `tests/support/conformance_consumers.rs` with the new consumer id and make sure the registry test sees it.

Verification:

```bash
cargo test -p defra-agent --test state_machine_conformance lean_boundary_metadata_is_typed_and_reviewable -- --nocapture
```

## Task 12: Run focused runtime regression tests

Run the existing hook tests that correspond to the generated transcript rows:

```bash
cargo test -p defra-agent duplicate_tool_result_message_observation_reuses_transcript_row -- --nocapture
cargo test -p defra-agent tool_result_message_dedupe_preserves_distinct_result_ids -- --nocapture
cargo test -p defra-agent tool_call_after_saved_assistant_starts_new_turn_without_orphan_result -- --nocapture
```

Expected: all pass without production Rust edits.

## Task 13: Full verification

Run:

```bash
cd crates/defra-agent/proofs
lake build Proofs.Transcript
lake build Proofs.Conformance.Contracts
grep -R "sorry" -n Proofs/Transcript Proofs/Conformance/ContractCases/Transcript.lean
cd ../../..
cargo test -p defra-agent --test state_machine_conformance -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected:

- no `sorry`,
- Lean builds,
- conformance test passes,
- formatting/diff checks pass.

## Task 14: PR

Open PR:

```text
Add Lean model for AgentMessage persistence + ordering
```

PR body must include:

- `Closes #191`
- `Refs #183`
- `Refs #160`
- `Refs #184`
- `Refs #190`
- Audit verdict moved from Open to Modeled for AgentMessage persistence + ordering.
- Named theorems proved.
- Generated conformance vectors registered.
- Exclusion: hook destructor does not satisfy strong drain; explicit drain transitions do. The negative theorem `abandon_hook_ownership_not_strong_drain` records the current boundary. A future runtime change to terminalize on `Drop` can replace it with a positive theorem.

Report the PR URL back in the thread.
