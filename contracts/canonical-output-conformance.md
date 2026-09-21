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

The checked handoff inventory is the mapping for this work:

| Inventory | Responsibility |
| --- | --- |
| [Execution](canonical-output-map-execution.json) | Gate operations, application trace, retry, queue/claim handover, tool delivery, Goal/background continuation, restart and the four invariants. |
| [Projection](canonical-output-map-projection.json) | Reconstruction, provider input/compaction, live observation, terminal output, delegation, forks/hydration and client observation joins. |
| [Native premises](canonical-output-map-native.json) | Lease actions, transaction/gate/clock/genesis/ACP experiments, SDL/catalog and pairing routes. |

Each entry identifies actual Lean declarations, existing generated groups, intended
native owner modules, consumer locations, required boundary cases and retired
bindings. Owner/consumer paths identify where to implement or migrate a binding;
their presence **does not claim that the new Rust implementation already exists**.
Fixture status is independent of native status:

- `concrete_inputs`: an exported input/expectation boundary exists for the named slice;
  the required-cases list still identifies missing variants and joins.
- `summary_only`: an emitted witness/count/Boolean exists, but is insufficient to
  drive the native operation. Export the actual inputs before writing the adapter.
- `not_exported`: the model or external obligation is mapped, but its concrete
  conformance fixture/experiment remains to be implemented.
- Native `pending` is expected at this layer. Only the existing consumer registry
  and coverage ledger can establish native coverage after implementation/testing.
  Structural trace identity/composition may instead be `not_applicable`.

`lake build` checks all constructors of `Gate.Operation`,
`SessionComposition.Trace` and `RequestExecutionLease.Action` are assigned exactly
once. It also rejects stale model declarations, missing paths, unknown fixture
groups and duplicate mapping IDs. CI additionally compares groups with the actual
generated JSON and runs negative controls for omissions and stale references:

```sh
python3 .github/scripts/check-canonical-output-map.py
python3 .github/scripts/test-canonical-output-map.py
# After lake build:
python3 .github/scripts/check-canonical-output-map.py --check-export
```

This is an inventory check, not a proof of adapter fidelity or a claim that every
Lean declaration has a native test. Cross-owner requirements and external premises
still require review; adding a new independent model family requires extending
the mapping/checker's explicit scope. Do not turn missing Rust into fake coverage
or implement a parallel test-only state machine.

Migrate the named consumers in their existing owners. Preserve interrupt
timestamps, late background delivery and physical request/tool binding when
removing obsolete response-row assertions. Keep per-domain follow-ups in the
coverage ledger until each native binding is registered and run.

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
