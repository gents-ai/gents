# Canonical output conformance (#1571)

Stack: [specification #1585](https://github.com/gents-ai/gents/pull/1585) →
[Lean #1586](https://github.com/gents-ai/gents/pull/1586) → generated conformance →
native implementation and consumers. That stack merged in #1612; follow-through
binding work is tracked in #1629 and the handoff inventory below.

## Adapter contract

Implementation checkpoint: the Rust descendant compiles the production row
codecs and lease policy directly in `gents-lean-contract/tests/native_output_policy.rs`.
Generated active-lease cases drive begin, append authorization, producer
authorization and bounded renewal. This is pure-helper coverage, not transaction,
timer, recovery or tool-handoff coverage. The full generated snapshot is decoded
by `runtime_snapshot`; successful decoding alone is not native execution.

The protocol has a sealed-stream reconstruction primitive and request-only
client projections. Unit regressions alone do not bind the generated complete
message/live-output projection cases. The embedded-database execution adapter
exercises publication, terminalization, recovery and tool operations through
their native owners for its exported native scripts. This does not establish
coverage of every projection, history consumer or external premise. The handoff
inventory records the remaining obligations; retired response/progress helpers
must not be restored to fill them.

Generate inputs and expected observations from the executable Lean owners, not
from a second Rust state machine. Native adapters receive only the inputs; the
harness compares independently observed results with the generated expectation.
Do not construct native rows from expected results or replace a missing owner
with a test-only policy implementation.
The projection and execution harnesses accept asynchronous, fallible native
adapters; execution initializes one fixture and retains it across the full script.
Adapters receive modeled inputs only and normalize database observations back to
fixture symbols before returning. Adapter errors are test failures, not modeled
rejections; a modeled rejection still returns the observed unchanged durable state.

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

## Exported execution scripts

`canonical_execution_gate_cases` carries native scripts, explicitly model-only
scripts with binding gaps, and summary witnesses. The native runner executes
every exported `native_execution` script through every step; it does not execute
`model_execution` scripts. Every script expectation is computed by folding the inputs through
the modeled gate (`acquire`, `commit`, release through the scheduling owner) and
projecting the resulting world; none is a literal.

| Family | Exported scenarios (not all natively bound) |
| --- | --- |
| Tool seam | pending remote recovery cancels before dispatch; running recovery records handoff; completion rejected while a foreground tool runs; close, deliver, then complete |
| Lease ordering | renewal wins before recovery; output does not renew; stale writer loses after recovery; dispatched tool wait renews explicitly |
| Publication and tools | a short Complete closure cannot truncate committed flushes; a background tool closes after its parent is terminal; spawned admission replays inertly; conflicting child-document admission is model-only because the native owner derives child identity rather than accepting it |
| Integrity | a distinct replicated twin, then revocation with a pending tool and with a running tool |
| Compaction join | cursor eligibility; a late foreground result rejected after a background receipt |
| Scheduling | a suspended same-task holder publishes nothing |

Compaction cursor scripts remain model-only. The adapter reads the cursor through
the native prompt-compaction owner, but observing no cursor in uncompacted
fixtures is not evidence of cursor-transition conformance. Likewise, a symbolic-ID
mapping failure is an adapter error, not a native admission rejection. The
execution map records the remaining binding obligations, including conflicting
children claiming the same parent tool after recovery.

A script step is either a gate commit or `deliver_replicated_segment`. The second
models a remote merge: it bypasses the local mutation gate, so a native fixture
must insert it as a replicated fact and never route it through the execution
owner. A holder whose commit was rejected is still in the storage phase and
observes `storageReturned` before it can release. Rejection is not an unlock.

Observations carry full `segments` and `messages`, plus `compaction_cursor`, tool
and lease fields; there are no parallel row counters. Adapters normalize physical identities
back to fixture symbols. The exporter sorts by symbolic ID and canonical JSON;
the native harness compares full facts independent of arrival order, preserving
twins and duplicate multiplicity. Equal counts alone do
not establish preservation: a rejected or inert write must leave the exact
observed immutable facts unchanged.

Two summaries deliberately remain summaries. `foreign_request_same_generation_unchanged`
needs a nonempty seed, which the empty-seed contract forbids. The wake-publication
half of `explicit_background_parent_terminal_late_delivery` belongs to the
background continuation owner rather than a gate operation.

## Payload presentation before provider-input sizing

`canonical_payload_presentation_cases` measures only reconstructed payload fields.
Headers select presentations of stored streams, and neither payload length
bounds the other:

| Case | Stored payload bytes | Presented payload bytes |
| --- | --- | --- |
| Full presentation | 3 | 3 |
| Inline literals around a range | 1 | 3 |
| Head and tail window with a marker | 10 | 7 |
| Missing dependency | no measurement | no measurement |

These numbers are **not** provider request sizes or token estimates. They exclude
inline metadata, URLs, escaping, request structure and tool schemas. The adapter
observes payload lengths at the native reconstruction boundary; it must not bind
this assertion to the output or input size of `provider_input::estimate_request`.
Missing dependencies yield no measurement, not a smaller fallback.
`generated_payload_presentation_cases_use_native_reconstruction` runs all four
generated cases through native canonical message reconstruction and compares the
presented payload-field bytes (or absent measurement) with Lean expectations.
The stored-byte values remain model expectations; this binding does not exercise
provider serialization, its estimator, or a compaction threshold decision.

The existing `provider_input` owner projects complete native messages into the
provider-specific body and estimates its serialized JSON. Keep that owner:
reconstructed message → provider request projection → request estimate →
compaction threshold. The formal join now resolves selected canonical message
identities through the existing publication/reconstruction owner, then passes
that exact ordered native list to a fallible provider-projection callback. The
reduction owner estimates that projected request, not an independent payload
count. After reduction it rebuilds from the checkpoint and retained suffix and
projects and estimates again before authorizing dispatch. Provider-view repair
also requires fresh admission; shrinking the history is not evidence of fit.

`compaction_projection_join_cases` exercises this composition with explicit
projection/estimation observations. Those observations are controlled inputs at
the native-owner boundary, not a Lean implementation of provider serialization.
`compaction_canonical_projection_cases` supplies the actual immutable records
and selected identities and compares the complete native messages passed to
initial projection and checkpoint rebuilding. Missing selected identities and
loading dependencies stop the composition instead of shortening the request.
`repaired_projection_admission_cases` exercises fresh admission after a provider
view has been repaired; it does not model the repair transformation itself.
The latter two groups separate `input` from `expected`, so native adapters cannot
receive expected messages or decisions as construction inputs.
The complete fixed request context (system prompt, tools, documents and provider
parameters) belongs to that callback. An honest end-to-end test must still run
the real native projection owner. Budget safety is relative to its estimate, not
a proof that the estimate equals the provider's actual tokenizer count. The
concrete experiment is tracked separately as
`native.external-projected-request-threshold`, with `not_exported`/`pending`
status. The reducer/cursor row fixtures remain `summary_only`; payload fixtures
have their own reconstruction-only map entry. No tokenizer or reduction decision
is modeled by the payload-presentation fixture group.

## External premises to test

ACP policy binding and per-dependency authorization are explicitly deferred by
product scope to future work, not a desktop acceptance gate for this refactor.
Preserve existing ownership, enrollment and requester/agent scoping. The ACP
experiment below remains a future obligation, not evidence of implemented
document-read enforcement or a reason to classify missing rows as denied.
In the pinned DefraDB, a collection without `@policy` neither registers its
documents as creator-owned ACP objects nor applies document-read ACP filtering.
Gents ownership fields and signing provenance must not be described as an
automatic per-document read restriction.

| Premise / owner | Required native experiment and observation |
| --- | --- |
| Provider request projection and compaction admission | Reconstruct messages, build the real `CompletionRequest`, and project/serialize it through the configured `ProviderInputCounter`. Observe the actual request estimate passed to `ReductionAdmission::for_input`, including metadata, schemas, framing and escaping rather than a payload sum. Construct measured requests at the effective budget and one estimated token above it: equality does not trigger reduction; strictly greater does. Reproject after reduction and reject dispatch if still over budget. This is implementation-layer work, not an exported Lean input-size fixture. |
| DefraDB transaction adapter | Fail each write in closure/header/tool-intent publication and recovery generation-swap/accounting batches; observe all-or-none committed facts. Lose the commit acknowledgement and replay exact identities without duplicate dispatch or terminal effects. |
| Mutation write gate | Race renewal, recovery, publication and dispatch through the existing per-node gate; observe the serial winner and unchanged loser. Probe a second process and remote merge separately: the model excludes their serialization, so their results must not be presented as a proved mutex guarantee. Conflicting facts must remain diagnosable and revocable. |
| Clock and independent renewal owner | Block provider/tool reads while the timer renews on bounded cadence; reject early, stale-deadline and expired renewals. Suspend past expiry and resume: the old owner cannot publish or dispatch. Exercise forward/backward clock changes and document the native clock mapping; Lean uses abstract nondecreasing time, not a wall-clock guarantee. |
| Genesis identity and create adapter | Concurrent identical creates resolve to the same immutable identity; changed genesis content does not overwrite it. Distinct documents at one source ordinal/closure project as conflict, not first-writer-wins. Use the pinned DefraDB, not a fake ID map. |
| ACP and hydration owner | Supply real allowed/denied/missing observations for message roots and segment dependencies. Follow an authorized fork reference across sessions without relaxing requester/agent scope; foreign and denied dependencies never leak payload. Test reorder, denial and duplicate delivery independently of owner authorization. |

These experiments are implementation-layer obligations until executable native
consumers exist. They are not justified merely by the Lean build or by the
fixture serializer accepting the data.

`tests/defradb_genesis_identity.rs` now exercises concurrent identical canonical
segment creates, duplicate-create rejection, and changed-content identity against
the pinned embedded database. It verifies exact persisted records without an ID
stub. This covers the database identity premise, not writer replay, lost commit
acknowledgements, or projection of conflicting closures; the broader handoff
entry remains pending.

## Remaining bridge breadth

The checked handoff inventory is the mapping for this work:

| Inventory | Responsibility |
| --- | --- |
| [Execution](canonical-output-map-execution.json) | Gate operations, application trace, retry, queue/claim handover, tool delivery, Goal/background continuation, restart and the four invariants. |
| [Projection](canonical-output-map-projection.json) | Reconstruction, provider input/compaction, live observation, terminal output, delegation, forks/hydration and client observation joins. |
| [Native premises](canonical-output-map-native.json) | Lease actions, transaction/gate/clock/genesis/ACP experiments, SDL/catalog and pairing routes. |
| [Tool joins](canonical-output-map-tools.json) | Tool lifecycle actions, closure authority, physical parent links and notification authority. |
| [Control joins](canonical-output-map-control.json) | Retry mediation, session queue actions and write-gate scheduling events. |

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

`lake build` checks constructor assignments exactly once across ten selected
vocabularies: the outer gate/application trace/lease actions; tool lifecycle,
closure authority and child-bridge events; retry actions and canonical retry
operations; session queue actions and write-gate scheduling events. It also
rejects stale model declarations, missing paths, unknown fixture groups and
duplicate mapping IDs. CI compares the source inventory with Lean's actual
inductive constructor metadata, resolves referenced declarations with Lean, and
checks fixture groups against generated JSON.
The exported execution envelope must retain concrete input scripts, unique case
names and one decision observation per operation; summary witnesses must succeed.
These are fixture-integrity checks, not a substitute for Rust decoding or native
execution.

Nested seam entries must state their `admission` boundary, disposition and reason.
The boundary must be a mapped model declaration. `admitted` means that entry
point handles the action subject to its guards, not unconditional acceptance;
`rejected` is relative to the named boundary; `routed` names the owner through
which it must pass instead. Cancellation intent is not a confirmed host stop,
and a durable child outcome is not unscoped permission to finish any parent.
Automated queue removal is distinct from wake acknowledgement: only finishing
the completed physical claimed wake acknowledges its captured attempted bindings;
failed or cancelled attempts acknowledge none.
Mapping checks enforce the inventory and references; the disposition's semantic
accuracy still requires model review and subsequent conformance cases.

Negative controls cover omissions, duplicate assignments, stale references,
namespace mistakes and missing admission boundaries:

```sh
python3 .github/scripts/check-canonical-output-map.py
python3 .github/scripts/test-canonical-output-map.py
# After lake build:
python3 .github/scripts/check-canonical-output-map.py --check-export
```

The foundation is red, so these new Rust decoders have not been compiled or run
here. A temporary structural lint checks a deliberately limited subset of their
declarations against generated JSON. It is not Rust parsing or Serde execution;
passing it does not establish that the actual decoders accept the contract:

```sh
python3 .github/scripts/test-lean-rust-decoders.py
# After lake build:
python3 .github/scripts/check-lean-rust-decoders.py
```

Unsupported attributes/layouts in reachable types must fail closed. Types
re-exported from `gents-protocol` are presence-only observations, not validated
decoders. Retire this lint when actual decoder tests can run; do not expand it
into another implementation of Serde.

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

## Implementation entry order

1. Bind immutable reconstruction and explicit lease renewal to their production
   owners, with authorized fixture creation and symbolic-ID normalization. Run the
   concrete projection/lease fixtures and the database-premise experiments above.
2. Bind the composed execution scripts to those same owners and the existing tool
   lifecycle. Observe persisted facts after each accepted or rejected operation;
   never synthesize the observation from the requested action. Extend concrete
   exports for summary-only/unexported mappings before claiming those seams.
3. Migrate provider input, compaction, hydration/client and continuation consumers
   in their named owners; delete their obsolete response-based assertions with
   the replaced implementation. Register each real native consumer and advance
   its ledger status only after running it. Validate the integrated stack before
   merging any layer.

The separate queue UX audit is [#1589](https://github.com/gents-ai/gents/issues/1589),
after this refactor. It does not authorize changing queue/notification policy in
these fixtures.
