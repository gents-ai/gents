# Lean Verification Sweep

**Branch:** `worktree-lean-verification-sweep`
**Created:** 2026-06-02 (triage session)
**Owner:** Jack

## Mission

Close the highest-leverage Lean proof-coverage gaps in one bundle. These issues
are nearly all *additive* to `crates/defra-agent/proofs/Proofs/` (new theorems and
files) plus their conformance consumers, so they share one `lake build` and one
worktree with low conflict risk.

## Hard rule: spec-first

Per `CLAUDE.md` — **the Lean proofs are the source of truth for all state-machine
behavior.** For every issue below:

1. Start in the Lean spec (`crates/defra-agent/proofs/`). Model the change; verify
   it doesn't violate existing safety/liveness properties.
2. Update conformance tests (`tests/state_machine_conformance.rs`,
   `tests/lifecycle_regression.rs`, and the per-feature consumers in `tests/`).
3. Update the Rust implementation to satisfy the new tests and match the spec.

No `sorry` left behind. Every new theorem must `lake build` clean.

## Scope exclusions (the "P2P boundary" carve-out)

- **#155 is OUT.** It's the cross-node P2P verification direction-setter and is
  TLA+/Iris territory, not Lean. Leave it for a later sweep.
- **#349 gap 3 (P2P request→response pairing under partition) is OUT.** It's the
  P2P boundary living inside an otherwise-kept issue. Close gaps 1 & 2 only; leave
  gap 3 open on #349 and let it ride with #155.

## Execution order

### 0. Baseline (do first)
```bash
cd crates/defra-agent/proofs
lake exe cache get      # REQUIRED in a fresh worktree — fetches prebuilt Mathlib
                        # v4.18.0 oleans. Skipping this compiles Mathlib from
                        # source (very slow). The worktree has no .lake cache.
lake build              # must be clean before starting
cd - && cargo test -p defra-agent     # conformance suite green
```
Note: macOS parallel `cargo test -p defra-agent --lib` SIGABRT is **fixed** as of
the v0.14.1 defradb.rs pin (#292 closed) — a clean parallel run is now the baseline.

### 1. #337 — Conformance coverage audit (DO THIS FIRST — it scopes the rest)

> Audit conformance coverage for composed state-machine workflow gaps.

The trigger was a real cross-machine gap: the live interrupt path stamped
`AgentResponse.interrupted_at` and marked `AgentRequest` interrupted but left the
streaming response non-terminal — atomic edges were covered, the *composed
workflow* was not.

**Why first:** the proof tree is already mature. Files like
`Proofs/Recovery/Sweeps/RequestResponse.lean`, `Proofs/Recovery/Sweeps/ToolCalls.lean`,
`Proofs/CrossMachineComposed/{Foreground,State,ToolTermination,UniqueCallIds}.lean`,
and `Proofs/Conformance/Contracts/Machines/SessionRecovery.lean` already exist. So
**parts of #349 below may already be covered.** The audit decides what is genuinely
missing before we write a line of new proof.

Audit tasks:
- Classify generated cases: atomic edge / trace / composed workflow / coverage metadata.
- Identify boundary edges whose safety/liveness depends on a *later* runtime call,
  not just the immediate transition.
- Audit these flows for multi-machine coherence: interrupt, deadline, recovery,
  tool cancel, child-process cancel, retry, provider failure.
- Add composed Lean witnesses where runtime behavior needs >1 machine to advance
  together (`Proofs/CrossMachineComposed/`).
- Add Rust consumers that drive the *real daemon/runtime path*, not isolated
  transition fns.
- `Proofs/Conformance/CoverageLedger.lean` + `Deviations.lean`: reject unknown
  feature tags, list every required surface, document intentional gaps (e.g.
  MCPHealth `reconnecting` if still unimplemented).

**Deliverable:** an inventory (what's covered vs. missing) that re-scopes issues 2–5
before implementing them.

### 2. #341 — Extend CodexShim proofs to Client-proof parity

Existing: `Proofs/CodexShim/Projection.lean` (20 thms), `Proofs/CodexShim/TurnLifecycle.lean`.
Gaps vs. `Proofs/Client/`:
- **Critical — Terminal Coherence (T3 equivalent).** Client has
  `terminal_coherence` (`Proofs/Client/Terminal.lean`). CodexShim has none. Prove
  `codex_turn_terminates_precisely`: the shim is terminal **exactly when** the
  request is effectively terminal OR `localInterruptAcked` OR a terminal response —
  and that the `localInterruptAcked` shortcut is *sound*, not premature.
- **Turn-lifecycle monotonicity.** Client has `lifecycle_transition_monotonic`
  (`Proofs/Client/Lifecycle.lean`). CodexShim proves *projection* monotonicity but
  not *turn-lifecycle* monotonicity. Prove `turn_lifecycle_never_regresses` over a
  `TurnPhase.lexOrd` ranking.
- **Local-interrupt coherence.** Prove `local_interrupt_requires_interruptible`:
  `localInterruptAcked = true → requestState ∈ {processing, inputRequired}`.

Files: extend `CodexShim/Projection.lean`, `CodexShim/TurnLifecycle.lean`, add a new
file for local-interrupt coherence. Sync the JSON contract
(`Proofs/Conformance/Contracts/Json/CodexShim.lean`) + Rust consumer.

### 3. #349 — Proof-coverage tracking (gaps 1 & 2 ONLY)

- **Gap 1 — Session-recovery correctness.** If a tool call was in-flight when the
  daemon died, recovery must converge to the same state as if the interruption
  never happened (reflect result / re-execute / not hang). **Check first:**
  `Proofs/Recovery/Sweeps/{RequestResponse,ToolCalls}.lean` and
  `Proofs/SessionRecovery.lean` may already cover much of this — the #337 audit
  tells you what's left. Related: #342 (Codex ThreadResume recovery gaps).
- **Gap 2 — Subagent-delegation safety.** Prove delegation terminates (no livelock
  of mutually-delegating agents; `subagent_depth` bounds nesting; graph acyclic &
  bounded) and that cascade-cancel is correct under arbitrary delegation graphs.
  Note: cascade-cancel Rust↔Lean parity already landed (#336 / B3). Build on
  `Proofs/EventDelivery/SubagentSource.lean`,
  `Proofs/Conformance/Contracts/Machines/Subagent.lean`, `Proofs/Background/`,
  `Proofs/ToolExecution/CancelCause.lean`.
- **Gap 3 — P2P pairing under partition. OUT OF SCOPE** (see exclusions). Leave the
  checkbox open on #349.

### 4. #282 — CLI Lean-driven trigger-dispatch conformance consumer

Placeholder `#TBD-cli-task-run-lean` from the feature-matrix design
(`docs/superpowers/specs/2026-05-20-feature-matrix-design.md` §3, row `triggers`).
The `config task run` CLI already exists; what's missing is a **Lean-driven
conformance consumer** exercising trigger dispatch end-to-end from the CLI surface,
tagged feature `triggers`, surface `operatorCli`. Build on the existing
`Proofs/Conformance/Triggers/*` + `tests/trigger_conformance.rs`. Related: #264.

### 5. #57 — ApplyReconcile delete semantics (small)

`Proofs/ApplyReconcile/` has no `delete` constructor; T-Conv is scoped accordingly.
When the CLI gains `live_only` document removal, add `ApplyStep.delete` and prove
**T-Delete-safety**: delete is permitted only when no live document references the
target. Spec: `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md`.
Tracker-only today — confirm the CLI removal feature is actually wanted before
landing the model, or land the model ahead of the feature as a guard.

## Build & test

```bash
cd crates/defra-agent/proofs && lake build      # proofs
cargo test -p defra-agent                         # full conformance suite
cargo test -p defra-agent -- <name>               # one test
```

## Suggested PR strategy

`#337` (audit) likely lands first as its own PR (inventory + any quick composed
witnesses it surfaces). Then `#341`, `#349(1,2)`, `#282`, `#57` as separate PRs (or
grouped if the audit shows them tightly coupled). Keep each PR's spec change driving
its conformance change driving its Rust change, per CLAUDE.md.

## Triage provenance

Created during the 2026-06-02 issue-triage session. In the same pass: #292 closed
(SIGABRT fixed by the v0.14.1 pin); labels added — #61/#128 `cli,impl`, #180 `impl`,
#338 `cli`.
