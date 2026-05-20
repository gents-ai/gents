# Issue #277 — Plan C: Lean ledger promotion + consumer coverage

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Prereq:** Plans A + B landed.

**Goal:** Move `interrupt-and-cancel.operatorUi` from the `deferred` list to a satisfied entry by registering consumer-coverage rows that point at the new desktop tests, and keep `lake build` green.

**Architecture:** `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` carries two lists: a feature matrix (`featureSurfaceCoverage`) that names which surfaces each feature must reach, and a `consumerCoverage`/`boundaryCoverage` list that pins specific Rust tests to specific (feature, surface) pairs. To promote a deferred row, you add consumer entries that satisfy each `(feature, surface)` and remove the deferred placeholder.

Current state — `CoverageLedger.lean:161-167`:
```lean
, { feature := "interrupt-and-cancel"
  , required := [Surface.agentFacing]
  , deferred :=
      [ (Surface.operatorCli, "#266")
      , (Surface.operatorUi, "#277")
      ]
  }
```

This plan only addresses the `(Surface.operatorUi, "#277")` row. The CLI row stays deferred until #266 lands.

**Tech Stack:** Lean 4 (Lake), Cargo. Existing pattern at `CoverageLedger.lean:281-320` shows what consumer entries look like.

---

## File Structure

**Modify:**
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:161-167` — drop the `(Surface.operatorUi, "#277")` deferred entry, add `Surface.operatorUi` to `required`.
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:230+` (within `consumerCoverage` lists) — add 3 entries pointing at the new desktop tests. Choose section by category — likely the `stateMachineCoverage` or feature-specific list; read the surrounding entries to pick the right home.

**Reference:**
- `CoverageLedger.lean:291-295` — existing consumer entry for `CancelCause` vocabulary (agentFacing surface) to mirror.
- The desktop tests written in Plans A + B that satisfy operatorUi coverage:
  - `bridge::tests::operations_interrupt::interrupt_request_cascade_returns_accepted_when_signature_matches` (Plan A Task 6)
  - `bridge::tests::cause_derivation::user_cancelled_when_root_has_interrupt_and_no_parent_cascade` (Plan A Task 8)
  - `components::cancelUx::CascadeCancelDialog::confirm_with_matching_signature_returns_accepted` (Plan B Task 4) — Lean ledger pins Rust tests by name, not JS, so this entry uses the Rust bridge-test name that proves the *contract*; the JS test is corroborating but not pinned.

---

## Verification

```bash
cd crates/defra-agent/proofs && lake build
cargo check
```

---

### Task 1: Add consumer-coverage entries for the three operatorUi behaviors

**Files:** `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`

- [ ] **Step 1:** Read `CoverageLedger.lean` end-to-end so you understand which named list to extend. Look for where `interrupt-and-cancel` already appears as a feature string — the new entries go in the same lexical neighborhood as existing `interrupt-and-cancel` rows.

- [ ] **Step 2:** Add three entries (or as many as needed to match the operatorUi requirement granularity — read the existing patterns; a single consumer entry per (feature, surface) may suffice):

```lean
, tagged (consumerCoverage
    "state_machine"
    "InterruptRequestResult"
    "bridge::tests::operations_interrupt::interrupt_request_cascade_returns_accepted_when_signature_matches")
    "interrupt-and-cancel" [Surface.operatorUi]
, tagged (consumerCoverage
    "state_machine"
    "CancelCauseDerivation"
    "bridge::tests::cause_derivation::user_cancelled_when_root_has_interrupt_and_no_parent_cascade")
    "interrupt-and-cancel" [Surface.operatorUi]
, tagged (consumerCoverage
    "state_machine"
    "CascadePreviewSignature"
    "bridge::tests::operations_cascade::preview_returns_four_classified_groups_and_a_signature")
    "interrupt-and-cancel" [Surface.operatorUi]
```

- [ ] **Step 3:** Run `cd crates/defra-agent/proofs && lake build`. Expected: PASS — the new entries should parse and contribute coverage.

- [ ] **Step 4:** Commit
```bash
git commit -am "proofs: register operatorUi consumer coverage for interrupt-and-cancel (#277)"
```

---

### Task 2: Promote operatorUi from deferred to required

**Files:** `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:161-167`

- [ ] **Step 1:** Edit the `interrupt-and-cancel` feature entry:

```lean
, { feature := "interrupt-and-cancel"
  , required := [Surface.agentFacing, Surface.operatorUi]
  , deferred :=
      [ (Surface.operatorCli, "#266")
      ]
  }
```

(Note: `operatorCli` remains deferred — that ships with issue #266. Only the operatorUi row promotes here.)

- [ ] **Step 2:** Run `lake build`.

Expected outcomes:
- If the coverage entries from Task 1 fully satisfy `Surface.operatorUi`: PASS, and the ledger is green.
- If the proof fails because coverage is insufficient: read the error message, add more entries (e.g., for the `unknownPolicy` variant if a separate test name is required), and re-run. Do NOT relax the requirement — the right fix is adding evidence, not weakening the contract.

- [ ] **Step 3:** Run `cargo check` to confirm nothing in the Rust workspace breaks (the ledger informs but does not gate the Rust build today).

- [ ] **Step 4:** Commit
```bash
git commit -am "proofs: promote interrupt-and-cancel operatorUi to required (#277)"
```

---

### Task 3: Final verification + PR retitle

- [ ] **Step 1:** Full verification
```bash
cd crates/defra-agent/proofs && lake build
cd /Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-277
cargo check
cargo test -p defra-agent-desktop-tauri
( cd apps/desktop-tauri && npm test )
```
All four must pass.

- [ ] **Step 2:** Push and retitle the PR:
```bash
git push origin design/issue-277-cancel-ux-prototype
gh pr edit 325 --title "Interrupt/cancel UX bundle: prototype + impl (#277)"
```

- [ ] **Step 3:** Update PR #325 body: add a "Phase 2 — Impl" section summarizing the commits from Plans A, B, C and listing the manual smoke-test results.

---

## Self-Review

- **Spec coverage:** Plan C delivers the only thing Phase 2 amendment §4 specifies (ledger promotion + consumer registration).
- **Placeholder scan:** Task 2 Step 2 conditionally describes outcomes ("if PASS / if FAIL") because the exact granularity Lean requires can only be discovered by running `lake build`. The executor follows the error message — that's not a placeholder, it's the actual feedback loop the ledger design expects.
- **Risk:** If `lake build` finds the new consumer entries don't satisfy the operatorUi requirement (likely if there's a "surface needs N distinct evidence kinds" rule we haven't seen), the executor may need to read deeper into `CoverageLedger.lean`'s `satisfies` definition. That's an investigation step, not a plan change.
