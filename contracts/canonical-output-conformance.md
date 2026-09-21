# Canonical output conformance (#1571)

Stack: [specification #1585](https://github.com/gents-ai/gents/pull/1585) →
[Lean #1586](https://github.com/gents-ai/gents/pull/1586) → generated conformance →
native implementation and consumers. The foundation remains intentionally red;
do not merge it without the validated implementation stack.

## Adapter contract

Generate inputs and expected observations from the executable Lean owners, not
from a second Rust state machine. Native adapters receive only the inputs; the
harness compares independently observed results with the generated expectation.
Do not construct native rows from expected results or replace a missing owner
with a test-only policy implementation.

The initial bridge covers immutable output projection, composed execution
operations and explicit request lease renewal. Fixture decoding, successful Lean
evaluation and an unbound assertion runner are not native conformance coverage.
Coverage-ledger entries stay `followUp` until a registered test calls the actual
owner. Existing integration tests using retired response/progress APIs must be
migrated, not made green with compatibility generators.

Fixture numeric identities are model symbols, not production document IDs. A
native fixture must create actual authorized documents, retain the symbol-to-ID
mapping, and normalize observations back through that mapping. The genesis/ACP
experiments below must independently test the database behavior rather than
treating that test-fixture mapping as its proof.

Four universal properties are proved over the Lean application trace: sequence
bounds, claim coherence, tool coherence and closure uniqueness. Generated finite
cases exercise their critical boundaries but do not prove those properties of
Rust. Native observations must include durable rows and ownership/accounting,
not just a successful return code.

## External premises to test

| Premise / owner | Required native experiment and observation |
| --- | --- |
| DefraDB transaction adapter | Fail each write in closure/header/tool-intent publication and recovery generation-swap/accounting batches; observe all-or-none committed facts. Lose the commit acknowledgement and replay exact identities without duplicate dispatch or terminal effects. |
| Mutation write gate | Race renewal, recovery, publication and dispatch through the existing per-node gate; observe the serial winner and unchanged loser. Probe a second process and remote merge separately: the model excludes their serialization, so their results must not be presented as a proved mutex guarantee. Conflicting facts must remain diagnosable and revocable. |
| Clock and independent renewal owner | Block provider/tool reads while the timer renews on bounded cadence; reject early, stale-deadline and expired renewals. Suspend past expiry and resume: the old owner cannot publish or dispatch. Exercise forward/backward clock changes and document the native clock mapping; Lean uses abstract nondecreasing time, not a wall-clock guarantee. |
| Genesis identity and create adapter | Concurrent identical creates resolve to the same immutable identity; changed genesis content does not overwrite it. Distinct documents at one source ordinal/closure project as conflict, not first-writer-wins. Use the pinned DefraDB, not a fake ID map. |
| ACP and hydration owner | Supply real allowed/denied/missing observations for message roots and segment dependencies. Follow an authorized fork reference across sessions without relaxing requester/agent scope; foreign and denied dependencies never leak payload. Test reorder, denial and duplicate delivery independently of owner authorization. |

These experiments are implementation-layer obligations until executable native
consumers exist. They are not justified merely by the Lean build or by the
fixture serializer accepting the data.

## Remaining bridge breadth

After the initial projection/execution/lease slice, migrate the existing
transcript/provider-input, compaction/cursor, fork/hydration, retry, background
continuation and client-observation consumers in their current owners. Preserve
interrupt timestamps, late background delivery and physical request/tool binding
checks when removing their obsolete response-row assertions. Keep per-domain
follow-ups in the coverage ledger until each native binding is registered and run.

The decoder no longer contains `LeanResponseTransitionCase`,
`LeanResponseInterruptFlowCase`, or lease response/progress fields. Existing
`tests/conformance/streaming_compaction.rs`, its entry points, and desktop
response-overlay tests still reference those retired interfaces: these are
explicit compile-time migration obligations, not passing conformance consumers.
The compaction fixture now exposes publication readiness, provider fixpoint and
turn boundary instead of a response status; its native caller must change with
the compaction owner. The old consumer registrations were removed, not rebranded.

Validation at this layer: `lake build`, import-closure checks, generated JSON
extraction and structural checks, and Rust formatting. Run native Rust suites and
the integrated workspace checks once the implementation layer replaces the
deleted schema/runtime contracts. No native pass is claimed by this layer alone.
