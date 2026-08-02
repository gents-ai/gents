# PromptAssembly: a mechanical Lean→Rust contract

**Date:** 2026-08-01
**Status:** Implemented. Lean layers land with zero `sorry`s; the contract emits
  10 sanitize, 5 layer, and 7 repair witnesses; the coverage ledger reports
  `consumer` strength on both required surfaces.
**Issue:** #992
**Branch:** `promptassembly-lean-contract`

## Problem

`crates/gents/proofs/README.md` lists PromptAssembly among the proven areas:
`sanitize` soundness / fixpoint / idempotence / split-stability over the
permissive transcript, loop-threading validity at the `run_loop_stream` entry
chokepoint, and the fixed layer order of the assembled request.

Every other core machine — `Request`, `Process`, `Persistence`,
`StorageObservation`, `RuntimeReconcile`, `SessionRecovery`, `InferenceCall`,
`ManagedExec` — emits a contract through `Proofs.Conformance.Contracts` that
Rust tests fetch at test time by running the Lean generator and parsing
sentinel-delimited JSON. That link cannot drift.

**PromptAssembly emits nothing.** Its fence,
`crates/gents/tests/conformance/prompt_assembly.rs`, is *social*: a human read
the spec and wrote assertions against a test-local oracle
(`assert_provider_valid`, 60 lines of hand-rolled pairing logic). Nothing
mechanically ties the Rust sanitizer to the function the Lean theorems quantify
over. A change to either side that breaks the correspondence does not fail a
test.

This failure mode has already bitten once. `src/agent/loop_stream.rs`
(`repair_provider_input`) documents a repair that rewrote only `new_messages`
while the poisoned tool-call arguments lived in loaded history. The doc comment
records the verdict: *"The fence described a transform that did not exist."*

## What the models actually say

The Lean side is in better shape than the issue implies. Verified by reading:

- `Proofs/PromptAssembly/Executable.lean` defines
  `sanitize = dropUnpairedCalls ∘ dropOrphanedResults` executably.
- `Proofs/PromptAssembly/Properties.lean` proves `sanitize_sound`,
  `sanitize_fixpoint`, `sanitize_idempotent`, `sanitize_split_stable`, and
  `threaded_turn_fixpoint`, with zero `sorry`s.
- `Proofs/PromptAssembly/Template.lean` proves `assembleWithContext_tail`,
  fixing the order as `... contextPreamble, prompt`.
- `Proofs/PromptAssembly/ToolArgs.lean` proves `repair_is_payload_only` and
  `repair_idempotent`.

The models say what the README claims. The missing piece is extraction and
consumption, not proof.

## The finding: a divergence the current model cannot express

Production composes **three** transforms (`src/compaction.rs:200`):

```rust
normalize_assistant_content_order(
    drop_unpaired_tool_calls(
        drop_orphaned_tool_results(messages)))
```

Lean's `sanitize` models only the inner two. `normalize_assistant_content_order`
— reorder assistant content to text → reasoning → tool calls — has no Lean
counterpart at all. So "the Rust sanitizer implements the proven function" is
false at the composition level: a third transform rides along unmodeled.

Faithfully modeling that ordering forces the row model to carry a content list.
The moment it does, a second and more serious divergence becomes visible:

- **Rust** `drop_unpaired_tool_calls` (`src/compaction/history.rs:68-81`) filters
  an assistant message's content, keeping tool calls whose key is resolved and
  **all non-call content unconditionally**, then keeps the message when the
  filtered content is non-empty. An assistant message with text plus an unpaired
  tool call therefore **survives, carrying its text**.
- **Lean** `filterCallsBy` (`Executable.lean:39-43`) sees a row whose
  `callIds ∩ resolved = ∅` and **drops the row entirely**.

Assistant-text-plus-tool-calls is the *common* production shape:
`AssistantTurnAccumulator::build_message` writes exactly that, and
`normalize_assistant_content_order` exists precisely because such messages carry
both. The Lean model is unfaithful to production in the shape that matters most.

The current pure-row projection — where a row is *either* `ordinary` *or*
`assistantToolCalls` *or* `toolResult`, with no intra-message content — is why
nobody noticed. The divergence is unrepresentable in it.

**Resolution: the model moves to Rust.** Rust's behavior is correct — dropping
the row would silently delete assistant prose from the provider-bound history.
Lean is the thing that is wrong here, and the enriched model adopts Rust's rule.

Measured on the same three-row input (user prose, assistant prose + unresolved
call, user prose):

| model | output rows |
| --- | --- |
| `sanitize` (row-only) | 2 — the assistant row is dropped whole |
| `sanitizeForProvider` (enriched) | 3 — the row survives, demoted to `.ordinary` |
| production Rust | 3 — message kept, carrying its text |

The emitted witness `assistant-prose-survives-its-unpaired-call` pins exactly
this.

### Two more, found in review

Review of PR #999 surfaced two further gaps, both real:

**Empty messages.** Rust drops them, asymmetrically — `drop_orphaned_tool_results`
pushes a user message only when content survives, while assistant messages ride
through and are pruned by `drop_unpaired_tool_calls`. The row-only model kept
every `.ordinary` row. Modeled via `emptyUserRow` / `emptyAssistantRow`, with
`NonDegenerate` as the invariant the two prunes establish; the fixpoint theorems
now take it as a hypothesis, because an input still carrying an empty row is not
a fixpoint. Witnesses `empty-messages-are-dropped` (3 → 1) and
`empty-assistant-message-between-paired-turns` (4 → 3) pin it.

**Duplicate call ids — a production bug.** `MessageKind.assistantToolCalls`
carries a `Finset`, so the model cannot see the same id twice in one turn. That
blindness hid a defect: Rust paired through a `HashSet`, so a turn announcing
one id twice was closed by a *single* result while *both* calls survived —
provider-invalid output from the function whose entire job is to prevent it.
Measured: 2 calls, 1 result.

Fixed in `drop_unpaired_tool_calls` by dropping duplicate occurrences within a
turn (cross-turn reuse is untouched, since pairing resets per turn). That also
makes the `Finset` abstraction *true* of production output rather than merely
assumed. Modeling multiplicity properly would mean replacing `Finset` in the
shared `Transcript.MessageKind`, which every pairing theorem in `Transcript` and
`PairingReconcile` is stated over — a larger change than this one, so the
occurrence-level behaviour is fenced in Rust and the boundary documented.

## Design

### Lean layer 1 — `Proofs/PromptAssembly/Content.lean`

A content-item type (`text` / `other` / `call callId`) and

```
normalize items = items.filter isText ++ items.filter isOther ++ items.filter isCall
```

Obligations:

- `normalize_idempotent`
- `normalize_perm` — `normalize items` is a permutation of `items`
- **`callsOf (normalize items) = callsOf items`** — the load-bearing lemma:
  reordering cannot change what a row announces. Everything in layer 2 rests on
  this.
- relative order within each category is preserved (immediate from `filter`)

### Lean layer 2 — `Proofs/PromptAssembly/Provider.lean`

`ProviderRow` = the existing abstract `MessageRow` plus a content list, with a
`Coherent` predicate tying `row.kind` to the content. Content-aware lifts of both
existing transforms, and

```
sanitizeForProvider = normalizeOrder ∘ dropUnpairedCallsP ∘ dropOrphanedResultsP
```

— the actual three-stage Rust composition. Soundness, fixpoint, and idempotence
are re-proven at this layer.

The changed clause (keep the row when non-call content survives; its kind becomes
`ordinary` when no calls remain) does not break soundness. Sketch: the
`.ordinary` case of `ActiveBlockValidFrom` requires `pending = ∅` and recursion
with `∅`. At that point `pending ∩ Rtail = ∅` is already discharged as `hstart`
in the existing proof, and the induction hypothesis `ih callIds` delivers
`ActiveBlockValidFrom (callIds ∩ resolvedIn tail)`, which under `hempty` *is*
`ActiveBlockValidFrom ∅`. It goes through.

`Coherent` is preserved by `normalizeRow` exactly because of layer 1's call-set
invariance.

### Lean layer 3 — refinement

A **conditional** refinement lemma relating `sanitizeForProvider` to the existing
`sanitize`: they agree exactly on calls-only assistant rows, and the enriched
model is the authority elsewhere. Stated honestly as conditional rather than
unconditional, because the divergence above is real.

The existing `sanitize` theorems are left untouched, so `Transcript` and
`Compaction` consumers do not move.

### The contract

Witness rows emitted from `Proofs/Conformance/Contracts.lean`, following the
shape the existing machines use. **Expected outputs are computed by running the
Lean functions, never hand-written** — that is precisely what makes the fence
mechanical rather than social. Families:

1. **sanitize witnesses** — input transcript → expected sanitized output, covering
   unpaired calls, orphaned results, result-before-call, result-after-conversation
   -resumes, and the mixed-content case above.
2. **idempotence** — `sanitize (sanitize input)` emitted alongside `sanitize input`.
3. **split-stability** — for every suffix index, `sanitize (drop k input)`.
4. **layer order** — `(skillCount, summaryCount, conversationLen) → [slot names]`
   from `Template.assembleWithContext`.
5. **repair** — from `ToolArgs.repairArgs`, with payload-only and idempotence flags.

Provider-validity per expected output is computed by the Lean decision procedure,
so Rust checks the same predicate the theorems state.

### Rust consumption

One total projection between the emitted row and `llm::message::Message`, with
the round-trip asserted, then the rows drive the **production** functions
directly: `sanitize_history_for_provider`, `assemble_new_messages`,
`repair_provider_input`. The test-local `assert_provider_valid` oracle is
deleted.

Below-the-model details keep dedicated, clearly-labelled Rust-only tests, because
the Lean row model cannot express them: pairing on `call_id.unwrap_or(id)`
(`history.rs::tool_call_key`), and multi-item user messages.

### Chokepoint

Two source-scan tests, in the style `conformance_consumers.rs` already
establishes:

1. an allowlist of `run_loop_stream` / `run_loop_to_text` call sites;
2. a scan for direct provider invocations (`.stream_completion(`, `.completion(`)
   outside the allowlisted admission/loop seam — the form a genuine bypass would
   actually take.

Verified by reading during design: every production completion is born in
`run_loop_stream`. Callers are `agent/daemon/inference.rs`, `oneshot.rs`,
`compaction.rs` (summarization), and `agent/daemon/title.rs`. `inference_http.rs`
is HTTP transport only; `completion_factory.rs` builds models and `LoopConfig`,
not messages; `admission/client.rs` wraps a model and receives an
already-assembled request. No bypass found.

Production also threads exactly **one** tool result per user message
(`loop_stream.rs:655`), so the one-result-per-row projection is faithful rather
than a convenient fiction.

## Acceptance

- `lake env lean --run Proofs/Conformance/Contracts.lean` emits `PromptAssembly` rows
- perturbing the Rust sanitizer fails a conformance test — demonstrated, not assumed
- the coverage-ledger test accounts for the new domain
- zero `sorry`s, `lake build` clean
- `cargo test -p gents` and `cargo check --workspace --all-targets` pass

## Conflict note

PR #995 (`worktree-review-fixes`) touches `Conformance/Boundaries.lean`,
`ContractCases/LifecycleTransitions.lean`, `tests/conformance/coverage.rs` (the
review-discipline boundary id list), and `src/lean_vocab_test/support.rs`. Adding
a boundary here touches the same id list — rebase rather than fight it.
