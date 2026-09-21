# Eval Core Contract (M1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the eval core contract: the Lean outcome model, the `EvalDefinition` configuration collection, the three fact-only runtime collections, and the pure Rust contract module with persistence. No execution.

**Architecture:** `Proofs/Eval.lean` defines the outcome vocabulary and its projection onto evidence classes; Lean emits it and Rust is checked against it. `EvalDefinition` is a typed configuration document that joins the `Collection` enum and `PackConfig`. `EvalRun`, `EvalTrial` and `EvalVerdict` are runtime documents with no status fields. A new `gents::eval` module holds outcome, scoring and document persistence.

**Tech Stack:** Lean 4 (v4.18.0) with Mathlib, Rust 1.97.1, DefraDB embedded node, `gents-lean-contract`, `serde`, `sha2`, `proptest`.

**Spec:** `docs/superpowers/specs/2026-09-21-eval-core-contract-design.md`, under the umbrella `docs/superpowers/specs/2026-09-21-eval-and-optimization-umbrella.md`.

## Global Constraints

- Order is Lean, then conformance, then Rust. Each PR targets its parent branch.
- **Narrow Lean imports only.** The Mathlib prebuilt cache is not available on the development machine, so only the Mathlib modules the repo already imports are compiled. Never import `Mathlib.Tactic` or any other broad module: it forces a from-source build of most of Mathlib. `simp`, `simp_all`, `decide`, `cases`, `rcases`, `split`, `omega`, `rename_i` and `rfl` are in Lean core. Before adding an import, confirm its `.olean` exists under `.lake/packages/mathlib/.lake/build/lib/`.
- Lean proofs contain no `sorry`. If a tactic fails, fix the tactic; never weaken a statement without recording why in the PR description.
- All four eval collections are `@branchable`. The directive is irreversible after a schema is created.
- `EvalDefinition` and `EvalVerdict` are listed in `LOCAL_AUDIT_COLLECTION_NAMES`. `EvalRun` and `EvalTrial` are not.
- No eval collection is added to `BRANCHABLE_COLLECTION_NAMES`, `CLIENT_COLLECTIONS`, `CONVERSATION_COLLECTIONS` or `CLIENT_TO_RUNTIME_COLLECTIONS`.
- `EvalDefinition` is not a `SelfConfigTarget`. Do not touch `config_client/patch.rs`, `Proofs/SelfConfig/Types.lean` or `Proofs/ScopeTemplates/State.lean`.
- `EvalRun`, `EvalTrial` and `EvalVerdict` have no status or lifecycle field. Do not add one.
- **Scores are integer basis points.** The spec's fallback is taken: the verdict column is `score_bp: Int` in `0..=10000`, null for a non-evidence verdict. The repository has no `@immutable Float` precedent, and integers are exact in both Lean and Rust.
- Configuration structs follow the repository convention: typed structs for SDL `JSON` fields, `#[serde(deny_unknown_fields)]`, the `ts_rs::TS` derive behind the `typescript` feature. Two fields are deliberately raw `serde_json::Value`: a check's `params` and a fixture document's `document`. Their schemas belong to the named check and to the app collection.
- Use `tracing`, never `println!`. Escape interpolated GraphQL strings with `escape_graphql_string`. Never emit `[]` in a mutation; use `null` for an empty nillable list.
- Baseline migration pins are computed by DefraDB. Obtain them from the pin-authoring test; never hand-write one.
- Before each push: `cargo test -p gents`, `cargo check --workspace --all-targets`, and `lake build` in `crates/gents/proofs` for Lean changes.
- Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit ...`.
- Create each PR branch with `make worktree BRANCH=<branch> BASE=<parent>`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/gents/proofs/Proofs/Eval.lean` | create | outcome kinds, provider reasons, evidence classes, `classify`, `subjectCausable`, case-class reduction |
| `crates/gents/proofs/Proofs.lean` | modify | register `Proofs.Eval` |
| `crates/gents/proofs/README.md` | modify | proof-map row |
| `crates/gents/proofs/Proofs/Conformance/Eval.lean` | create | emit `eval_outcome_cases` |
| `crates/gents/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean` | modify | three vocabularies |
| `crates/gents/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean` | modify | splice `eval_outcome_cases` |
| `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean` | modify | ledger entries |
| `crates/gents/src/lean_vocab_test/eval.rs`, `support.rs` | create/modify | snapshot structs and accessor |
| `crates/gents/tests/conformance/coverage.rs` | modify | count pin and emitted domain |
| `crates/gents/proofs/Proofs/ConfigDocuments.lean` | modify | `evalDefinition` catalog row |
| `crates/gents/src/collection.rs` | modify | `Collection::EvalDefinition` |
| `crates/gents/src/document_config/eval_definition.rs` | create | typed definition document and `validate()` |
| `crates/gents/src/document_config/{mod,pack_config,references}.rs` | modify | re-export, pack root, validation dispatch |
| `crates/gents/src/config_client/desired_state.rs` | modify | `config_projection` arm |
| `crates/gents-schemas/schemas/agent/eval_{definition,run,trial,verdict}.graphql` | create | SDL |
| `crates/gents-schemas/src/lib.rs`, `crates/gents-protocol/src/schemas.rs` | modify | catalogs, local-audit list, branchable test |
| `crates/gents-migration/src/registry.rs` | modify | four baseline entries |
| `crates/gents/src/eval/{mod,outcome,scoring,documents}.rs` | create | the contract module |
| `crates/gents/src/lib.rs` | modify | `pub mod eval;` |
| `crates/gents/tests/conformance/eval.rs`, `tests/conformance.rs` | create/modify | vocabulary and projection consumer |
| `crates/gents/src/document_config/{write_tool,surface_tool}.rs` | modify | protected-collection rule |

---

## PR A: Lean outcome model

Branch: `eval/01-lean`, base `main`.

### Task 1: `Proofs/Eval.lean`

**Files:**
- Create: `crates/gents/proofs/Proofs/Eval.lean`
- Modify: `crates/gents/proofs/Proofs.lean`, `crates/gents/proofs/README.md`

**Interfaces:**
- Produces, in `namespace Eval`: `OutcomeKind` with `toDefraDB` and `allKinds`; `ProviderReason` with `toDefraDB` and `allReasons`; `EvidenceClass` with `toDefraDB` and `allClasses`; `classify : OutcomeKind → Option ProviderReason → EvidenceClass`; `subjectCausable : OutcomeKind → Option ProviderReason → Bool`; `CaseClass`, `CaseClass.rank`, `caseClass : List EvidenceClass → CaseClass`. PR B and the optimization plan rely on these names.

- [ ] **Step 1: Write the file**

```lean
import Mathlib.Data.List.Basic

/-! Eval core contract (#1515): the closed outcome vocabulary and its projection
onto evidence classes.

The governing rule: anything the subject under evaluation could cause counts
against it; only what it cannot cause is excluded. A `provider` outcome needs a
reason, because a candidate prompt can cause a context overflow (`rejected`) but
not an outage (`unavailable`). A `provider` outcome with no reason is `unknown`:
the evaluator could not tell, and an unknown never helps a candidate.

Refinement boundaries: checks, reducers over numeric scores, and the runner are
outside this model. -/
namespace Eval

inductive OutcomeKind where
  | passed | modelAcceptance | deadline | tool | runtime | skippedPrerequisite
  | provider | infrastructure | inconclusive | grader | unknown
  deriving DecidableEq, Repr

namespace OutcomeKind
def toDefraDB : OutcomeKind → String
  | .passed => "passed"
  | .modelAcceptance => "model_acceptance"
  | .deadline => "deadline"
  | .tool => "tool"
  | .runtime => "runtime"
  | .skippedPrerequisite => "skipped_prerequisite"
  | .provider => "provider"
  | .infrastructure => "infrastructure"
  | .inconclusive => "inconclusive"
  | .grader => "grader"
  | .unknown => "unknown"
end OutcomeKind

def allKinds : List OutcomeKind :=
  [.passed, .modelAcceptance, .deadline, .tool, .runtime, .skippedPrerequisite,
   .provider, .infrastructure, .inconclusive, .grader, .unknown]

inductive ProviderReason where
  | rejected | unavailable
  deriving DecidableEq, Repr

namespace ProviderReason
def toDefraDB : ProviderReason → String
  | .rejected => "rejected"
  | .unavailable => "unavailable"
end ProviderReason

def allReasons : List ProviderReason := [.rejected, .unavailable]

inductive EvidenceClass where
  | pass | fail | notEvidence | unknown
  deriving DecidableEq, Repr

namespace EvidenceClass
def toDefraDB : EvidenceClass → String
  | .pass => "pass"
  | .fail => "fail"
  | .notEvidence => "not_evidence"
  | .unknown => "unknown"
end EvidenceClass

def allClasses : List EvidenceClass := [.pass, .fail, .notEvidence, .unknown]

def classify : OutcomeKind → Option ProviderReason → EvidenceClass
  | .passed, _ => .pass
  | .modelAcceptance, _ => .fail
  | .deadline, _ => .fail
  | .tool, _ => .fail
  | .runtime, _ => .fail
  | .skippedPrerequisite, _ => .fail
  | .provider, some .rejected => .fail
  | .provider, some .unavailable => .notEvidence
  | .provider, none => .unknown
  | .infrastructure, _ => .notEvidence
  | .inconclusive, _ => .unknown
  | .grader, _ => .unknown
  | .unknown, _ => .unknown

/-- Outcomes a subject can bring about through its own behavior. -/
def subjectCausable : OutcomeKind → Option ProviderReason → Bool
  | .modelAcceptance, _ => true
  | .deadline, _ => true
  | .tool, _ => true
  | .runtime, _ => true
  | .skippedPrerequisite, _ => true
  | .provider, some .rejected => true
  | _, _ => false

/-- A subject can never move one of its own failures out of the denominator. -/
theorem subject_causable_is_never_excluded (k : OutcomeKind) (r : Option ProviderReason)
    (h : subjectCausable k r = true) : classify k r ≠ .notEvidence := by
  cases k <;> cases r <;> simp_all [subjectCausable, classify]
  all_goals (rename_i reason; cases reason <;> simp_all [subjectCausable, classify])

theorem subject_causable_fails (k : OutcomeKind) (r : Option ProviderReason)
    (h : subjectCausable k r = true) : classify k r = .fail := by
  cases k <;> cases r <;> simp_all [subjectCausable, classify]
  all_goals (rename_i reason; cases reason <;> simp_all [subjectCausable, classify])

theorem only_passed_passes (k : OutcomeKind) (r : Option ProviderReason) :
    classify k r = .pass ↔ k = .passed := by
  cases k <;> cases r <;> simp [classify]
  all_goals (rename_i reason; cases reason <;> simp [classify])

/-! ## Case class: the class of a case-trial is the class of its worst verdict -/

inductive CaseClass where
  | scored | unknown | notEvidence
  deriving DecidableEq, Repr

namespace CaseClass
def rank : CaseClass → Nat
  | .scored => 0
  | .unknown => 1
  | .notEvidence => 2
end CaseClass

def caseClass (verdicts : List EvidenceClass) : CaseClass :=
  if verdicts.any (· == .notEvidence) then .notEvidence
  else if verdicts.any (· == .unknown) then .unknown
  else .scored

/-- Adding a verdict never improves a case's class. -/
theorem caseClass_cons_rank (v : EvidenceClass) (vs : List EvidenceClass) :
    (caseClass vs).rank ≤ (caseClass (v :: vs)).rank := by
  cases v <;> simp only [caseClass, List.any_cons] <;>
    (split <;> (try split) <;> (try split) <;> simp_all [CaseClass.rank])

theorem caseClass_nil : caseClass [] = .scored := by
  simp [caseClass]

end Eval
```

- [ ] **Step 2: Register and map**

Append to `crates/gents/proofs/Proofs.lean`:

```lean

import Proofs.Eval
```

In `crates/gents/proofs/README.md`, in the `| File | Contents |` table after the `Proofs/ApplyReconcile.lean` row, add:

```
| `Proofs/Eval.lean` | Eval core contract (#1515): closed outcome-kind vocabulary, provider reasons, the projection onto evidence classes, the theorem that no subject-causable outcome is excluded from the denominator, and monotone case-class reduction. Checks, numeric reducers and the runner are refinement boundaries |
```

- [ ] **Step 3: Build**

Run: `cd crates/gents/proofs && lake build`
Expected: success, no `sorry`. The first build compiles Mathlib and is slow.
Likely repair points: the three `cases k <;> cases r` proofs. If `rename_i reason` fails because a branch has no `some` binder, replace each proof with `cases k <;> rcases r with _ | (_ | _) <;> simp_all [subjectCausable, classify]`. For `caseClass_cons_rank`, a fallback is `cases v <;> simp [caseClass, CaseClass.rank] <;> split <;> simp_all <;> split <;> simp_all`. Statements stay as written.

- [ ] **Step 4: Commit and open PR A**

```bash
git add crates/gents/proofs/Proofs/Eval.lean crates/gents/proofs/Proofs.lean crates/gents/proofs/README.md
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(eval): outcome vocabulary and evidence-class projection (#1515)"
```

PR description: baseline `main`; new owner `Proofs/Eval`; no deletions; validation `lake build`. The `lean-proofs` CI job is skipped on non-tip stacked PRs, so paste the local build result.

---

## PR B: Conformance emission

Branch: `eval/02-conformance`, base `eval/01-lean`.

`LeanContractSnapshot` is `#[serde(deny_unknown_fields)]`, and `lean_contract_coverage_ledger_accounts_for_every_emitted_domain` requires the ledger to match the emitted domains exactly. So the Lean emission, the Rust structs and the ledger entries land together. Consumers arrive in PR E, so the entries start as `followUpCoverage`.

### Task 2: Emit vocabularies and projection cases

**Files:**
- Create: `crates/gents/proofs/Proofs/Conformance/Eval.lean`, `crates/gents/src/lean_vocab_test/eval.rs`
- Modify: `Catalog.lean`, `Snapshot.lean`, `CoverageLedger.lean`, `crates/gents/src/lean_vocab_test/support.rs`, `crates/gents/tests/conformance/coverage.rs`

**Interfaces:**
- Produces: vocabularies `EvalOutcomeKind`, `EvalProviderReason`, `EvalEvidenceClass`; snapshot key `eval_outcome_cases`; Rust `lean_eval_outcome_cases() -> &'static [LeanEvalOutcomeCase]` with fields `kind: String`, `provider_reason: Option<String>`, `class: String`, `subject_causable: bool`.

- [ ] **Step 1: Lean cases**

Create `crates/gents/proofs/Proofs/Conformance/Eval.lean`:

```lean
import Proofs.Eval
import Proofs.Conformance.ContractTypes

/-! Generated witnesses for the eval outcome projection. Rust must classify every
(kind, provider reason) combination exactly as the model does. -/
namespace Conformance.Eval

open Conformance.Contracts
open _root_.Eval

private def boolJson (b : Bool) : String := if b then "true" else "false"

private def reasonJson : Option ProviderReason → String
  | none => "null"
  | some r => jsonString r.toDefraDB

private def row (k : OutcomeKind) (r : Option ProviderReason) : String :=
  "{\"kind\":" ++ jsonString k.toDefraDB
    ++ ",\"provider_reason\":" ++ reasonJson r
    ++ ",\"class\":" ++ jsonString (classify k r).toDefraDB
    ++ ",\"subject_causable\":" ++ boolJson (subjectCausable k r) ++ "}"

def evalOutcomeCasesJson : String :=
  jsonArray (allKinds.flatMap fun k =>
    ([none] ++ allReasons.map some).map fun r => row k r)

end Conformance.Eval
```

That is 11 kinds times 3 reason options: 33 rows.

- [ ] **Step 2: Vocabularies**

In `crates/gents/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean`, add `import Proofs.Eval` after the file's last `import`, and add these three entries at the end of the `vocabularies` list, before the closing `]`:

```lean
  , { domain := "EvalOutcomeKind", values := Eval.allKinds.map Eval.OutcomeKind.toDefraDB }
  , { domain := "EvalProviderReason", values := Eval.allReasons.map Eval.ProviderReason.toDefraDB }
  , { domain := "EvalEvidenceClass", values := Eval.allClasses.map Eval.EvidenceClass.toDefraDB }
```

- [ ] **Step 3: Snapshot key**

In `Snapshot.lean`, add `import Proofs.Conformance.Eval` after the last `import`. Directly after the two lines

```lean
    ++ "\"apply_reconcile_cases\":"
      ++ ApplyReconcile.ContractCases.applyReconcileCasesJson ++ ","
```

insert:

```lean
    ++ "\"eval_outcome_cases\":"
      ++ Conformance.Eval.evalOutcomeCasesJson ++ ","
```

If Track 0's `publish_if_cases` lines are already present after `apply_reconcile_cases`, insert after those instead.

- [ ] **Step 4: Ledger**

In `CoverageLedger.lean`, inside `featureSurfaceRequirements`, directly after the `"apply-reconcile"` entry, add:

```lean
  , { feature := "eval"
    , required := [Surface.operatorCli]
    , deferred := [(Surface.operatorUi, "#1515")]
    }
```

Inside `vocabularyCoverage`, as the last three entries before the closing `]`:

```lean
  , tagged (followUpCoverage
      "vocabulary"
      "EvalOutcomeKind"
      "Consumed by conformance::eval once gents::eval::outcome lands in the eval contract stack.")
      "eval" [Surface.operatorCli]
  , tagged (followUpCoverage
      "vocabulary"
      "EvalProviderReason"
      "Consumed by conformance::eval once gents::eval::outcome lands in the eval contract stack.")
      "eval" [Surface.operatorCli]
  , tagged (followUpCoverage
      "vocabulary"
      "EvalEvidenceClass"
      "Consumed by conformance::eval once gents::eval::outcome lands in the eval contract stack.")
      "eval" [Surface.operatorCli]
```

Inside `caseCoverage`, directly after the `apply_reconcile_cases` entry (or after `publish_if_cases` if Track 0 landed):

```lean
  , tagged (followUpCoverage
      "eval_outcome_cases"
      "EvalOutcomeCases"
      "Consumed by conformance::eval once gents::eval::outcome lands in the eval contract stack.")
      "eval" [Surface.operatorCli]
```

- [ ] **Step 5: Rust structs**

Create `crates/gents/src/lean_vocab_test/eval.rs`:

```rust
use serde::Deserialize;

/// One row of `Proofs/Eval.lean` `classify` and `subjectCausable`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEvalOutcomeCase {
    pub(crate) kind: String,
    pub(crate) provider_reason: Option<String>,
    pub(crate) class: String,
    pub(crate) subject_causable: bool,
}
```

In `crates/gents/src/lean_vocab_test/support.rs`, directly after

```rust
#[path = "triggers_runtime_apply.rs"]
mod triggers_runtime_apply;
```

add:

```rust
#[path = "eval.rs"]
mod eval;
```

and directly after `pub(crate) use triggers_runtime_apply::*;` add `pub(crate) use eval::*;`.

In `LeanContractSnapshot`, directly after `pub(crate) apply_reconcile_cases: Vec<LeanApplyReconcileCase>,` (or after `publish_if_cases` if present) add:

```rust
    pub(crate) eval_outcome_cases: Vec<LeanEvalOutcomeCase>,
```

After `lean_apply_reconcile_case` add:

```rust
pub(crate) fn lean_eval_outcome_cases() -> &'static [LeanEvalOutcomeCase] {
    &lean_contract_snapshot().eval_outcome_cases
}
```

- [ ] **Step 6: Coverage pins**

In `crates/gents/tests/conformance/coverage.rs`, after `assert_eq!(lean_contract_snapshot().apply_reconcile_cases.len(), 8);` add:

```rust
    assert_eq!(lean_contract_snapshot().eval_outcome_cases.len(), 33);
```

After the `if !snapshot.apply_reconcile_cases.is_empty() { ... }` block add:

```rust
    if !snapshot.eval_outcome_cases.is_empty() {
        emitted.insert((
            "eval_outcome_cases".to_string(),
            "EvalOutcomeCases".to_string(),
        ));
    }
```

Vocabularies need no Rust change here: the ledger test reads them from the snapshot.

- [ ] **Step 7: Build, run, commit**

Run: `cd crates/gents/proofs && lake build`
Run: `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
Expected: both succeed. If the feature-matrix test rejects the new `eval` feature because it has no consumer on its required surface yet, change the four new ledger entries' feature tag from `"eval"` to `"apply-reconcile"`, delete the `featureSurfaceRequirements` entry, and note in the PR description that Task 8 restores both.

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "proofs(conformance): emit eval outcome vocabularies and projection cases"
```

PR description: baseline `eval/01-lean`; owners `Conformance` snapshot and `lean_vocab_test`; the four new domains are `followUpCoverage` until PR E; validation as in Step 7.

---

## PR C: `EvalDefinition` configuration collection

Branch: `eval/03-definition`, base `eval/02-conformance`.

`crates/gents/src/collection.rs` has a test that parses `ConfigDocuments.lean`, so the Lean catalog row and the Rust variant must land in one PR. Within the PR, edit Lean first.

### Task 3: Catalog, collection, document, schema

**Files:**
- Modify: `crates/gents/proofs/Proofs/ConfigDocuments.lean`, `crates/gents/src/collection.rs`, `crates/gents/src/document_config/{mod,pack_config,references}.rs`, `crates/gents/src/config_client/desired_state.rs`, `crates/gents-schemas/src/lib.rs`, `crates/gents-protocol/src/schemas.rs`, `crates/gents-migration/src/registry.rs`
- Create: `crates/gents/src/document_config/eval_definition.rs`, `crates/gents-schemas/schemas/agent/eval_definition.graphql`

**Interfaces:**
- Produces: `Collection::EvalDefinition` (`dir_name` `"eval_definitions"`, `graphql_type` `"EvalDefinition"`, `unique_field` `"definition_id"`); `document_config::{EvalDefinition, EvalSubject, EvalSubjectKind, EvalFixtures, EvalFixtureDocument, EvalCase, EvalStage, EvalCheckRef, EvalSplit, EvalTier, EvalReducer}`; `EvalDefinition::validate(&self) -> Result<()>`; `PackConfig.eval_definitions`.

- [ ] **Step 1: Lean catalog row**

In `ConfigDocuments.lean`: add `| evalDefinition` as the last constructor before `deriving`; append `, .evalDefinition` to the `all` list; add as the last `documentSpec` row:

```lean
  | .evalDefinition => ⟨"EvalDefinition", "definition_id", "automation", ["definition_id", "agent_did", "comparability_version", "title", "subject", "fixtures", "cases", "tags"]⟩
```

`"automation"` is deliberate: the catalog's categories are the seven self-config grant categories, and a new one would touch the grant model.

- [ ] **Step 2: Write the failing document tests**

Create `crates/gents/src/document_config/eval_definition.rs` containing only this test module, then run `cargo test -p gents --lib document_config::eval_definition` and confirm it fails to compile.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition() -> serde_json::Value {
        json!({
            "definition_id": "monitor-findings",
            "agent_did": "did:key:owner",
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [{
                "case_id": "disk-warning",
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [
                        {"check": "mailbox_findings", "params": {"expect": ["disk"]}, "tier": "acceptance"},
                        {"check": "llm_judge", "params": {"rubric": "clear"}, "tier": "development"}
                    ]
                }]
            }]
        })
    }

    fn parse(value: serde_json::Value) -> EvalDefinition {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_minimal_definition_validates_and_defaults() {
        let parsed = parse(definition());
        parsed.validate().unwrap();
        assert_eq!(parsed.cases[0].reducer, EvalReducer::WeightedMean);
        assert_eq!(parsed.cases[0].stages[0].checks[0].weight, 1);
        assert!(parsed.tags.is_empty());
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert!(serialized.get("tags").is_none(), "an empty list is omitted, never []");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = definition();
        value["status"] = "running".into();
        assert!(serde_json::from_value::<EvalDefinition>(value).is_err());
    }

    fn invalid(mutate: impl FnOnce(&mut serde_json::Value), needle: &str) {
        let mut value = definition();
        mutate(&mut value);
        let error = parse(value).validate().unwrap_err();
        assert!(format!("{error:#}").contains(needle), "{error:#}");
    }

    #[test]
    fn structural_rules_are_enforced() {
        invalid(|v| v["comparability_version"] = 0.into(), "comparability_version");
        invalid(|v| v["cases"] = json!([]), "at least one case");
        invalid(|v| v["cases"][0]["stages"] = json!([]), "at least one stage");
        invalid(|v| v["cases"][0]["stages"][0]["deadline_secs"] = 0.into(), "deadline_secs");
        invalid(|v| v["cases"][0]["stages"][0]["checks"][0]["weight"] = 0.into(), "weight");
        invalid(|v| v["cases"][0]["stages"][0]["checks"][0]["check"] = " ".into(), "check name");
        invalid(
            |v| {
                let case = v["cases"][0].clone();
                v["cases"].as_array_mut().unwrap().push(case);
            },
            "duplicate case_id",
        );
        invalid(
            |v| {
                let stage = v["cases"][0]["stages"][0].clone();
                v["cases"][0]["stages"].as_array_mut().unwrap().push(stage);
            },
            "duplicate stage_id",
        );
    }

    #[test]
    fn a_case_needs_an_acceptance_check_and_judges_stay_development() {
        invalid(
            |v| v["cases"][0]["stages"][0]["checks"][0]["tier"] = "development".into(),
            "acceptance-tier check",
        );
        invalid(
            |v| v["cases"][0]["stages"][0]["checks"][1]["tier"] = "acceptance".into(),
            "llm_judge",
        );
    }
}
```

- [ ] **Step 3: Implement the document**

Put this above the test module:

```rust
use std::collections::BTreeSet;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

/// The model-judge check. It is development-tier in v1: a subject's output is
/// untrusted input to a judge model.
pub const LLM_JUDGE_CHECK: &str = "llm_judge";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalSplit {
    Train,
    Validation,
    HeldOut,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalTier {
    /// Reported, may carry feedback, never enters a score.
    Development,
    /// Deterministic checks only in v1. The only tier that enters a score.
    Acceptance,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalReducer {
    #[default]
    WeightedMean,
    All,
    LastStage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalSubjectKind {
    Behavior,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalSubject {
    pub kind: EvalSubjectKind,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub inference_slots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalFixtureDocument {
    pub collection: String,
    /// An app-collection row. Its schema belongs to that collection.
    #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
    pub document: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalFixtures {
    /// Pack-asset digests to materialize in the trial workspace.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub assets: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EvalFixtureDocument>>", optional = nullable))]
    pub documents: Vec<EvalFixtureDocument>,
    /// App-collection SDL to install in the trial before its documents.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub schemas: Vec<String>,
}

fn default_weight() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalCheckRef {
    /// A name in the builtin check registry. Definitions never contain code.
    pub check: String,
    /// Interpreted by the named check. Its schema belongs to that check.
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
    pub params: serde_json::Value,
    pub tier: EvalTier,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalStage {
    pub stage_id: String,
    pub prompt: String,
    pub deadline_secs: u64,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EvalCheckRef>>", optional = nullable))]
    pub checks: Vec<EvalCheckRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalCase {
    pub case_id: String,
    pub split: EvalSplit,
    #[serde(default)]
    pub reducer: EvalReducer,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fixtures: Option<EvalFixtures>,
    pub stages: Vec<EvalStage>,
}

/// A pack-carried eval definition. Identity is `(definition_id,
/// comparability_version, desired_state_document_digest)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalDefinition {
    pub definition_id: String,
    pub agent_did: String,
    /// Bumped by the author when cases, checks, reducers or judge settings
    /// change. Runs never compare across different values.
    pub comparability_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub title: Option<String>,
    pub subject: EvalSubject,
    /// Fixtures shared by every case; a case may add its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fixtures: Option<EvalFixtures>,
    pub cases: Vec<EvalCase>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl EvalDefinition {
    pub fn validate(&self) -> Result<()> {
        let id = &self.definition_id;
        ensure!(!id.trim().is_empty(), "eval definition requires a definition_id");
        ensure!(
            self.comparability_version >= 1,
            "eval definition {id} comparability_version must be at least 1"
        );
        ensure!(!self.cases.is_empty(), "eval definition {id} requires at least one case");
        let mut case_ids = BTreeSet::new();
        for case in &self.cases {
            let case_id = &case.case_id;
            ensure!(!case_id.trim().is_empty(), "eval definition {id} has a case with no case_id");
            ensure!(case_ids.insert(case_id), "eval definition {id} has duplicate case_id {case_id}");
            ensure!(
                !case.stages.is_empty(),
                "eval definition {id} case {case_id} requires at least one stage"
            );
            let mut stage_ids = BTreeSet::new();
            let mut acceptance = 0usize;
            for stage in &case.stages {
                let stage_id = &stage.stage_id;
                ensure!(
                    !stage_id.trim().is_empty(),
                    "eval definition {id} case {case_id} has a stage with no stage_id"
                );
                ensure!(
                    stage_ids.insert(stage_id),
                    "eval definition {id} case {case_id} has duplicate stage_id {stage_id}"
                );
                ensure!(
                    stage.deadline_secs > 0,
                    "eval definition {id} case {case_id} stage {stage_id} deadline_secs must be positive"
                );
                for check in &stage.checks {
                    ensure!(
                        !check.check.trim().is_empty(),
                        "eval definition {id} case {case_id} stage {stage_id} has an empty check name"
                    );
                    ensure!(
                        check.weight >= 1,
                        "eval definition {id} case {case_id} check {} weight must be at least 1",
                        check.check
                    );
                    if check.tier == EvalTier::Acceptance {
                        ensure!(
                            check.check != LLM_JUDGE_CHECK,
                            "eval definition {id} case {case_id}: {LLM_JUDGE_CHECK} is development-tier only"
                        );
                        acceptance += 1;
                    }
                }
            }
            ensure!(
                acceptance > 0,
                "eval definition {id} case {case_id} requires at least one acceptance-tier check"
            );
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Wire the collection**

`crates/gents/src/collection.rs`: add `EvalDefinition,` as the last enum variant; change both `[Collection; 26]` to `[Collection; 27]`; append `Self::EvalDefinition,` to `ALL`; add the arms `Self::EvalDefinition => Some("eval_definitions"),` to `dir_name`, `Self::EvalDefinition => "EvalDefinition",` to `graphql_type`, and `Self::EvalDefinition => "definition_id",` to `unique_field`.

`crates/gents/src/document_config/mod.rs`: add `mod eval_definition;` after `mod datastore_tool_surface;` (or in alphabetical position in the `mod` block) and, in the re-export block:

```rust
pub use eval_definition::{
    EvalCase, EvalCheckRef, EvalDefinition, EvalFixtureDocument, EvalFixtures, EvalReducer,
    EvalSplit, EvalStage, EvalSubject, EvalSubjectKind, EvalTier, LLM_JUDGE_CHECK,
};
```

`crates/gents/src/document_config/pack_config.rs`: after the `graphs` field add:

```rust
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<EvalDefinition>>", optional = nullable))]
    pub eval_definitions: Vec<EvalDefinition>,
```

If `PackConfig` derives `Eq`, drop `Eq` from that derive: `EvalDefinition` contains `serde_json::Value` and is only `PartialEq`. Then run `cargo check -p gents` and fix any `Eq` bound this exposes by using `PartialEq`.

`crates/gents/src/config_client/desired_state.rs`, in `config_projection`, as the last arm:

```rust
        Collection::EvalDefinition => project::<EvalDefinition>(value),
```

`crates/gents/src/document_config/references.rs`, in `validate_document`, directly above the `Collection::Skill | Collection::CallbackModule | Collection::GraphDefinition => {}` arm:

```rust
            // Check names resolve against the builtin registry at run time, not
            // against configuration documents; there are no outgoing references.
            Collection::EvalDefinition => decode::<EvalDefinition>(value)?.validate()?,
```

- [ ] **Step 5: SDL and catalogs**

Create `crates/gents-schemas/schemas/agent/eval_definition.graphql`:

```graphql
# A pack-carried eval definition (#1515). Holds acceptance checks and held-out
# case bodies, so it is local-audit: never bulk-synced, never on a P2P list.
# `@branchable` is deliberate and irreversible; it is the precondition for
# branchable sync and for collection-scoped ACP reads.
type EvalDefinition @branchable @index(fields: ["agent_did", "definition_id"], unique: true) {
    definition_id: String @immutable
    agent_did: String @index @immutable
    comparability_version: Int
    title: String
    subject: JSON
    fixtures: JSON
    cases: JSON
    tags: [String]
}
```

`crates/gents-schemas/src/lib.rs`: after the `GRAPH_DEFINITION` constants add

```rust
pub const EVAL_DEFINITION_NAME: &str = "EvalDefinition";
pub const EVAL_DEFINITION: &str = include_str!("../schemas/agent/eval_definition.graphql");
```

append `EVAL_DEFINITION,` as the last element of `ALL` and `EVAL_DEFINITION_NAME,` as the last element of `ALL_COLLECTION_NAMES`, and add `EVAL_DEFINITION_NAME,` to `LOCAL_AUDIT_COLLECTION_NAMES`.

`crates/gents-protocol/src/schemas.rs`: add `EVAL_DEFINITION, EVAL_DEFINITION_NAME,` to the `pub use gents_schemas::{...}` list; append `EVAL_DEFINITION,` to that file's `ALL` and `EVAL_DEFINITION_NAME,` to its `ALL_COLLECTION_NAMES`, each in the position that keeps the agent-domain block in the same order as `gents_schemas::ALL`.

- [ ] **Step 6: Baseline pin**

Append to `DEFAULT_BASELINE` in `crates/gents-migration/src/registry.rs`, in the position matching the catalog order:

```rust
    baseline_entry!(
        gents_protocol::schemas::EVAL_DEFINITION_NAME,
        gents_protocol::schemas::EVAL_DEFINITION,
        "PIN"
    ),
```

Run: `cargo test -p gents-migration --test phase_b_steps canonical_catalog_pins_for_authoring -- --nocapture`
Expected: the computed root VersionID for `EvalDefinition` is reported as a mismatch against `"PIN"`. Replace `"PIN"` with the reported `bafyrei...` value.

- [ ] **Step 7: Pack round-trip test**

Append to the test module in `eval_definition.rs`:

```rust
    #[test]
    fn a_pack_config_carries_eval_definitions() {
        let pack: crate::document_config::PackConfig = serde_json::from_value(json!({
            "agent_principal": {"agent_did": "did:key:owner"},
            "eval_definitions": [definition()]
        }))
        .unwrap();
        assert_eq!(pack.eval_definitions.len(), 1);
        let plan = crate::config_client::DesiredStateApplyPlan::from_pack_config(&pack).unwrap();
        assert!(plan
            .documents()
            .iter()
            .any(|document| document.collection == crate::Collection::EvalDefinition));
    }
```

If `AgentPrincipal` requires more fields than `agent_did`, copy the minimal principal JSON from an existing `PackConfig` test in `crates/gents/src/document_config/tests.rs`.

- [ ] **Step 8: Run everything this PR touches**

Run, in order:
- `cd crates/gents/proofs && lake build`
- `cargo test -p gents --lib document_config::eval_definition`
- `cargo test -p gents --lib collection::tests`
- `cargo test -p gents-schemas && cargo test -p gents-protocol && cargo test -p gents-migration`
- `cargo test -p gents-desktop-core the_desktop_does_not_replicate_plaintext_provider_bodies`
- `cargo test -p gents`
- `cargo check --workspace --all-targets`

Expected: all succeed. `vocabulary_matches_canonical_lean_document_catalog` and `authored_names_match_pack_config_document_roots` pass only when the Lean row, the enum and the `PackConfig` field agree in name and order.

- [ ] **Step 9: Commit and open PR C**

```bash
git add crates
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(config): EvalDefinition configuration collection and pack root (#1515)"
```

PR description: baseline `eval/02-conformance`; owners `ConfigDocuments`, `Collection`, `document_config`, the schema catalogs; one new baseline pin; `EvalDefinition` is deliberately absent from `SelfConfigTarget`, `BRANCHABLE_COLLECTION_NAMES` and every P2P list, and is local-audit.

---

## PR D: Runtime collections

Branch: `eval/04-runtime-collections`, base `eval/03-definition`.

### Task 4: `EvalRun`, `EvalTrial`, `EvalVerdict`

**Files:**
- Create: `crates/gents-schemas/schemas/agent/eval_run.graphql`, `eval_trial.graphql`, `eval_verdict.graphql`
- Modify: `crates/gents-schemas/src/lib.rs`, `crates/gents-protocol/src/schemas.rs`, `crates/gents-migration/src/registry.rs`

**Interfaces:**
- Produces: `EVAL_RUN_NAME`, `EVAL_TRIAL_NAME`, `EVAL_VERDICT_NAME` and their SDL constants, re-exported from `gents_protocol::schemas`.

- [ ] **Step 1: Write the failing schema tests**

In the test module of `crates/gents-schemas/src/lib.rs` add:

```rust
    #[test]
    fn eval_collections_are_branchable_and_carry_no_lifecycle() {
        for name in [EVAL_DEFINITION_NAME, EVAL_RUN_NAME, EVAL_TRIAL_NAME, EVAL_VERDICT_NAME] {
            let sdl = ALL_COLLECTION_NAMES
                .iter()
                .position(|candidate| candidate == &name)
                .map(|index| ALL[index])
                .unwrap_or_else(|| panic!("eval collection {name} has no registered SDL"));
            let declaration = sdl
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("type "))
                .unwrap_or_else(|| panic!("eval collection {name} has no type declaration"));
            assert!(declaration.contains("@branchable"), "{name} must be @branchable: {declaration}");
            for forbidden in ["lifecycle_state", "status:", "state:"] {
                assert!(!sdl.contains(forbidden), "{name} must hold facts only, found {forbidden}");
            }
        }
    }

    #[test]
    fn eval_placement_is_decided_per_collection() {
        for name in [EVAL_DEFINITION_NAME, EVAL_VERDICT_NAME] {
            assert!(is_local_audit_collection(name), "{name} holds protected material");
        }
        for name in [EVAL_RUN_NAME, EVAL_TRIAL_NAME] {
            assert!(!is_local_audit_collection(name), "{name} holds no message bodies");
        }
        for name in [EVAL_DEFINITION_NAME, EVAL_RUN_NAME, EVAL_TRIAL_NAME, EVAL_VERDICT_NAME] {
            assert!(!BRANCHABLE_COLLECTION_NAMES.contains(&name), "{name} is not bulk-synced");
        }
    }
```

Run: `cargo test -p gents-schemas eval_`
Expected: compile error, `cannot find value EVAL_RUN_NAME`.

- [ ] **Step 2: SDL**

`eval_run.graphql`:

```graphql
# One frozen eval invocation (#1515). Facts only: a run's progress is derived from
# its trials. `invalidated` is the only mutable field.
type EvalRun @branchable @index(fields: ["owner_agent_did", "run_id"], unique: true) {
    run_id: String @immutable
    owner_agent_did: String @index @immutable
    evaluator_did: String @immutable
    origin: JSON @immutable
    created_at: String @index @immutable
    invalidated: JSON
}
```

`eval_trial.graphql`:

```graphql
# One execution of one case in one cell (#1515). Identity is written at
# provisioning; `completion` is written once. A null completion is a fact: the
# runner did not finish this trial. Trial state derives from the referenced
# requests. `home_hint` is a locator, never identity.
type EvalTrial @branchable @index(fields: ["owner_agent_did", "trial_id"], unique: true) {
    trial_id: String @immutable
    owner_agent_did: String @index @immutable
    run_id: String @index @immutable
    cell_id: String @immutable
    case_id: String @immutable
    trial_index: Int @immutable
    attempt: Int @immutable
    trial_agent_did: String @immutable
    session_id: String @immutable
    seed: Int @immutable
    home_hint: String
    created_at: String @immutable
    completion: JSON
}
```

`eval_verdict.graphql`:

```graphql
# One check's result for one trial (#1515). Append-only: every field is immutable
# and a re-grade appends a row naming the verdict it supersedes. `feedback` can
# quote transcripts, so the collection is local-audit. `score_bp` is basis points
# in 0..=10000 and null for a non-evidence verdict.
type EvalVerdict @branchable @index(fields: ["owner_agent_did", "verdict_id"], unique: true) {
    verdict_id: String @immutable
    owner_agent_did: String @index @immutable
    run_id: String @index @immutable
    trial_id: String @index @immutable
    stage_id: String @immutable
    check: String @immutable
    check_version: String @immutable
    tier: String @immutable
    outcome_kind: String @immutable
    provider_reason: String @immutable
    score_bp: Int @immutable
    weight: Int @immutable
    raw: JSON @immutable
    feedback: String @immutable
    regrade_of: String @immutable
    created_at: String @immutable
}
```

`provider_reason` and `weight` are columns the spec implied but did not list: the projection needs the reason, and a reducer needs the weight without re-reading the definition.

- [ ] **Step 3: Register**

`crates/gents-schemas/src/lib.rs`: after the `EVAL_DEFINITION` constants add the six constants (`EVAL_RUN_NAME = "EvalRun"`, `EVAL_RUN = include_str!("../schemas/agent/eval_run.graphql")`, and the same for `EVAL_TRIAL` and `EVAL_VERDICT`); append the three SDL constants to `ALL` and the three names to `ALL_COLLECTION_NAMES`, in the same order; add `EVAL_VERDICT_NAME,` to `LOCAL_AUDIT_COLLECTION_NAMES`.

`crates/gents-protocol/src/schemas.rs`: add the six names to the `pub use` list and append them to that file's `ALL` and `ALL_COLLECTION_NAMES` in the same order.

- [ ] **Step 4: Pins**

Append three `baseline_entry!` rows with `"PIN"` to `DEFAULT_BASELINE`, in catalog order, run `cargo test -p gents-migration --test phase_b_steps canonical_catalog_pins_for_authoring -- --nocapture`, and replace each `"PIN"` with the reported value.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p gents-schemas && cargo test -p gents-protocol && cargo test -p gents-migration && cargo test -p gents-desktop-core the_desktop_does_not_replicate_plaintext_provider_bodies`
Expected: all pass.
Run: `grep -n "EvalRun\|EvalTrial\|EvalVerdict\|EvalDefinition" crates/gents/src/agent/p2p_reconcile/templates.rs`
Expected: no output.

```bash
git add crates/gents-schemas crates/gents-protocol crates/gents-migration
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(schemas): fact-only EvalRun, EvalTrial and EvalVerdict collections (#1515)"
```

PR description: baseline `eval/03-definition`; three new `@branchable` collections with three baseline pins; `EvalVerdict` local-audit; `EvalRun` and `EvalTrial` join the desktop's bulk subscription by design, because they hold no message bodies.

---

## PR E: The contract module

Branch: `eval/05-contract`, base `eval/04-runtime-collections`.

### Task 5: Outcomes and the Lean consumer

**Files:**
- Create: `crates/gents/src/eval/mod.rs`, `crates/gents/src/eval/outcome.rs`, `crates/gents/tests/conformance/eval.rs`
- Modify: `crates/gents/src/lib.rs`, `crates/gents/tests/conformance.rs`, `CoverageLedger.lean`

**Interfaces:**
- Produces: `OutcomeKind` (`ALL`, `as_str`, `parse`), `ProviderReason` (`ALL`, `as_str`, `parse`), `EvidenceClass` (`ALL`, `as_str`), `TAXONOMY_VERSION: &str = "1"`, `classify(OutcomeKind, Option<ProviderReason>) -> EvidenceClass`, `subject_causable(OutcomeKind, Option<ProviderReason>) -> bool`.

- [ ] **Step 1: Write the failing conformance test**

Create `crates/gents/tests/conformance/eval.rs`:

```rust
use gents::eval::{classify, subject_causable, EvidenceClass, OutcomeKind, ProviderReason};

use crate::lean_vocab_test::{lean_eval_outcome_cases, lean_vocabulary_values};

pub(super) fn rust_eval_outcome_vocabulary_and_projection_match_lean() {
    assert_eq!(lean_vocabulary_values("EvalOutcomeKind"), OutcomeKind::ALL.map(OutcomeKind::as_str));
    assert_eq!(
        lean_vocabulary_values("EvalProviderReason"),
        ProviderReason::ALL.map(ProviderReason::as_str)
    );
    assert_eq!(
        lean_vocabulary_values("EvalEvidenceClass"),
        EvidenceClass::ALL.map(EvidenceClass::as_str)
    );
    let cases = lean_eval_outcome_cases();
    assert_eq!(cases.len(), OutcomeKind::ALL.len() * (ProviderReason::ALL.len() + 1));
    for case in cases {
        let kind = OutcomeKind::parse(&case.kind).expect("kind");
        let reason = case.provider_reason.as_deref().map(|r| ProviderReason::parse(r).expect("reason"));
        assert_eq!(classify(kind, reason).as_str(), case.class, "{case:?}");
        assert_eq!(subject_causable(kind, reason), case.subject_causable, "{case:?}");
    }
}
```

In `crates/gents/tests/conformance.rs` add, with the other `#[path]` modules:

```rust
#[path = "conformance/eval.rs"]
mod eval;
```

and with the other wrappers:

```rust
#[test]
fn rust_eval_outcome_vocabulary_and_projection_match_lean() {
    eval::rust_eval_outcome_vocabulary_and_projection_match_lean();
}
```

Run: `cargo test -p gents --test conformance rust_eval_outcome --no-run`
Expected: compile error, `unresolved import gents::eval`.

- [ ] **Step 2: Implement**

`crates/gents/src/eval/mod.rs`:

```rust
//! Eval core contract (#1515): the outcome vocabulary, scoring rules and the
//! fact-only run, trial and verdict documents. Execution lives in the runner.

pub mod outcome;

pub use outcome::{
    classify, subject_causable, EvidenceClass, OutcomeKind, ProviderReason, TAXONOMY_VERSION,
};
```

In `crates/gents/src/lib.rs` add `pub mod eval;` after `pub mod error;`.

`crates/gents/src/eval/outcome.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Frozen into every run's origin and embedded in reports.
pub const TAXONOMY_VERSION: &str = "1";

/// `Proofs/Eval.lean` `OutcomeKind`. Order matches the Lean vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Passed,
    ModelAcceptance,
    Deadline,
    Tool,
    Runtime,
    SkippedPrerequisite,
    Provider,
    Infrastructure,
    Inconclusive,
    Grader,
    Unknown,
}

impl OutcomeKind {
    pub const ALL: [OutcomeKind; 11] = [
        Self::Passed,
        Self::ModelAcceptance,
        Self::Deadline,
        Self::Tool,
        Self::Runtime,
        Self::SkippedPrerequisite,
        Self::Provider,
        Self::Infrastructure,
        Self::Inconclusive,
        Self::Grader,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::ModelAcceptance => "model_acceptance",
            Self::Deadline => "deadline",
            Self::Tool => "tool",
            Self::Runtime => "runtime",
            Self::SkippedPrerequisite => "skipped_prerequisite",
            Self::Provider => "provider",
            Self::Infrastructure => "infrastructure",
            Self::Inconclusive => "inconclusive",
            Self::Grader => "grader",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReason {
    /// A 4xx such as context overflow or a content-policy refusal.
    Rejected,
    /// A 5xx, a connection failure, rate limiting.
    Unavailable,
}

impl ProviderReason {
    pub const ALL: [ProviderReason; 2] = [Self::Rejected, Self::Unavailable];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Pass,
    Fail,
    NotEvidence,
    Unknown,
}

impl EvidenceClass {
    pub const ALL: [EvidenceClass; 4] = [Self::Pass, Self::Fail, Self::NotEvidence, Self::Unknown];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotEvidence => "not_evidence",
            Self::Unknown => "unknown",
        }
    }
}

/// `Eval.classify`. Anything the subject could cause counts against it.
pub fn classify(kind: OutcomeKind, reason: Option<ProviderReason>) -> EvidenceClass {
    use OutcomeKind::*;
    match (kind, reason) {
        (Passed, _) => EvidenceClass::Pass,
        (ModelAcceptance | Deadline | Tool | Runtime | SkippedPrerequisite, _) => EvidenceClass::Fail,
        (Provider, Some(ProviderReason::Rejected)) => EvidenceClass::Fail,
        (Provider, Some(ProviderReason::Unavailable)) => EvidenceClass::NotEvidence,
        (Provider, None) => EvidenceClass::Unknown,
        (Infrastructure, _) => EvidenceClass::NotEvidence,
        (Inconclusive | Grader | Unknown, _) => EvidenceClass::Unknown,
    }
}

/// `Eval.subjectCausable`.
pub fn subject_causable(kind: OutcomeKind, reason: Option<ProviderReason>) -> bool {
    use OutcomeKind::*;
    matches!(
        (kind, reason),
        (ModelAcceptance | Deadline | Tool | Runtime | SkippedPrerequisite, _)
            | (Provider, Some(ProviderReason::Rejected))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subject_causable_outcome_is_excluded() {
        for kind in OutcomeKind::ALL {
            for reason in [None, Some(ProviderReason::Rejected), Some(ProviderReason::Unavailable)] {
                if subject_causable(kind, reason) {
                    assert_eq!(classify(kind, reason), EvidenceClass::Fail, "{kind:?} {reason:?}");
                }
            }
        }
    }

    #[test]
    fn names_round_trip() {
        for kind in OutcomeKind::ALL {
            assert_eq!(OutcomeKind::parse(kind.as_str()), Some(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }
}
```

- [ ] **Step 3: Flip the ledger entries to consumers**

In `CoverageLedger.lean`, change the four `followUpCoverage` entries from Task 2 to `consumerCoverage`, each with the consumer string `"conformance::rust_eval_outcome_vocabulary_and_projection_match_lean"` in place of the follow-up sentence. If Task 2's contingency was used, restore the `"eval"` feature tag and the `featureSurfaceRequirements` entry.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p gents --lib eval::outcome`
Run: `cd crates/gents/proofs && lake build`
Run: `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
Expected: all pass.

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(eval): outcome vocabulary and evidence-class projection, conformant with Lean"
```

### Task 6: Scoring

**Files:**
- Create: `crates/gents/src/eval/scoring.rs`
- Modify: `crates/gents/src/eval/mod.rs`

**Interfaces:**
- Consumes: `OutcomeKind`, `ProviderReason`, `EvidenceClass`, `classify`; `EvalReducer`, `EvalTier` from `document_config`.
- Produces:
  - `SCORE_BP_MAX: u32 = 10_000`
  - `VerdictView { verdict_id, stage_index: usize, check, tier, kind, provider_reason, score_bp: Option<u32>, weight: u32, regrade_of: Option<String> }`
  - `latest_verdicts(Vec<VerdictView>) -> Vec<VerdictView>`
  - `CaseTrialScore { NotEvidence, Unknown, Scored(u32) }`, `case_trial_score(EvalReducer, &[VerdictView]) -> CaseTrialScore`
  - `TrialScore { case_id: String, trial_index: u32, score: CaseTrialScore }`
  - `Pair { case_id, trial_index, baseline_bp: u32, candidate_bp: u32 }`, `PairedEvidence { pairs, keys: usize, dropped_baseline: usize, dropped_candidate: usize }`, `pair_trials(&[TrialScore], &[TrialScore]) -> PairedEvidence`
  - `case_means_bp(&[TrialScore]) -> BTreeMap<String, u32>`, `headline_bp(&[TrialScore]) -> Option<u32>`
  - `RunHeader { definition_id, comparability_version: i64, split: EvalSplit, invalidated: bool }`, `exposure(&[RunHeader], &str, i64, EvalSplit) -> usize`

- [ ] **Step 1: Failing tests**

Create `scoring.rs` with only this module and confirm `cargo test -p gents --lib eval::scoring` fails to compile:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::{EvalReducer, EvalSplit, EvalTier};
    use crate::eval::{OutcomeKind, ProviderReason};

    fn v(id: &str, stage: usize, kind: OutcomeKind, score: Option<u32>, weight: u32) -> VerdictView {
        VerdictView {
            verdict_id: id.into(),
            stage_index: stage,
            check: format!("check-{id}"),
            tier: EvalTier::Acceptance,
            kind,
            provider_reason: None,
            score_bp: score,
            weight,
            regrade_of: None,
        }
    }

    #[test]
    fn reducers() {
        let verdicts = [
            v("a", 0, OutcomeKind::Passed, Some(10_000), 1),
            v("b", 1, OutcomeKind::ModelAcceptance, Some(4_000), 3),
        ];
        assert_eq!(case_trial_score(EvalReducer::WeightedMean, &verdicts), CaseTrialScore::Scored(5_500));
        assert_eq!(case_trial_score(EvalReducer::All, &verdicts), CaseTrialScore::Scored(0));
        assert_eq!(case_trial_score(EvalReducer::LastStage, &verdicts), CaseTrialScore::Scored(4_000));
        let all_pass = [v("a", 0, OutcomeKind::Passed, Some(10_000), 1)];
        assert_eq!(case_trial_score(EvalReducer::All, &all_pass), CaseTrialScore::Scored(10_000));
    }

    #[test]
    fn development_checks_never_enter_a_score() {
        let mut judge = v("j", 0, OutcomeKind::ModelAcceptance, Some(0), 100);
        judge.tier = EvalTier::Development;
        let verdicts = [v("a", 0, OutcomeKind::Passed, Some(10_000), 1), judge];
        assert_eq!(case_trial_score(EvalReducer::WeightedMean, &verdicts), CaseTrialScore::Scored(10_000));
    }

    #[test]
    fn a_case_trial_takes_the_class_of_its_worst_verdict() {
        let pass = v("a", 0, OutcomeKind::Passed, Some(10_000), 1);
        let unknown = v("b", 0, OutcomeKind::Inconclusive, None, 1);
        let mut outage = v("c", 0, OutcomeKind::Provider, None, 1);
        outage.provider_reason = Some(ProviderReason::Unavailable);
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[pass.clone(), unknown.clone()]),
            CaseTrialScore::Unknown
        );
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[pass, unknown, outage]),
            CaseTrialScore::NotEvidence
        );
        assert_eq!(case_trial_score(EvalReducer::WeightedMean, &[]), CaseTrialScore::Unknown);
    }

    #[test]
    fn a_fail_kind_scores_zero_even_without_a_check_score() {
        let deadline = v("d", 0, OutcomeKind::Deadline, None, 1);
        assert_eq!(case_trial_score(EvalReducer::WeightedMean, &[deadline]), CaseTrialScore::Scored(0));
    }

    #[test]
    fn regrades_supersede_without_destroying() {
        let original = v("v1", 0, OutcomeKind::ModelAcceptance, Some(0), 1);
        let mut regrade = v("v2", 0, OutcomeKind::Passed, Some(10_000), 1);
        regrade.check = original.check.clone();
        regrade.regrade_of = Some("v1".into());
        let latest = latest_verdicts(vec![original, regrade]);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].verdict_id, "v2");
    }

    fn t(case: &str, index: u32, score: CaseTrialScore) -> TrialScore {
        TrialScore { case_id: case.into(), trial_index: index, score }
    }

    #[test]
    fn pairing_drops_not_evidence_and_imputes_unknown_worst_case() {
        let baseline = [
            t("a", 0, CaseTrialScore::Scored(5_000)),
            t("a", 1, CaseTrialScore::NotEvidence),
            t("b", 0, CaseTrialScore::Unknown),
        ];
        let candidate = [
            t("a", 0, CaseTrialScore::Scored(9_000)),
            t("a", 1, CaseTrialScore::Scored(9_000)),
            t("b", 0, CaseTrialScore::Unknown),
        ];
        let paired = pair_trials(&baseline, &candidate);
        assert_eq!(paired.keys, 3);
        assert_eq!(paired.dropped_baseline, 1);
        assert_eq!(paired.dropped_candidate, 0);
        assert_eq!(paired.pairs.len(), 2);
        let b = paired.pairs.iter().find(|p| p.case_id == "b").unwrap();
        assert_eq!((b.baseline_bp, b.candidate_bp), (10_000, 0), "an unknown never helps a candidate");
    }

    #[test]
    fn an_unmatched_trial_is_a_dropped_pair() {
        let baseline = [t("a", 0, CaseTrialScore::Scored(5_000))];
        let paired = pair_trials(&baseline, &[]);
        assert!(paired.pairs.is_empty());
        assert_eq!(paired.dropped_candidate, 1);
    }

    #[test]
    fn headline_weights_cases_equally_and_counts_unknown_as_failure() {
        let trials = [
            t("a", 0, CaseTrialScore::Scored(10_000)),
            t("a", 1, CaseTrialScore::Scored(10_000)),
            t("a", 2, CaseTrialScore::Scored(10_000)),
            t("b", 0, CaseTrialScore::Unknown),
            t("c", 0, CaseTrialScore::NotEvidence),
        ];
        assert_eq!(case_means_bp(&trials).get("a"), Some(&10_000));
        assert_eq!(case_means_bp(&trials).get("b"), Some(&0));
        assert_eq!(case_means_bp(&trials).get("c"), None, "no evidence, no mean");
        assert_eq!(headline_bp(&trials), Some(5_000));
        assert_eq!(headline_bp(&[]), None);
    }

    #[test]
    fn exposure_counts_rows_and_skips_invalidated_runs() {
        let run = |split, invalidated| RunHeader {
            definition_id: "d".into(),
            comparability_version: 2,
            split,
            invalidated,
        };
        let runs = [
            run(EvalSplit::Validation, false),
            run(EvalSplit::Validation, true),
            run(EvalSplit::HeldOut, false),
        ];
        assert_eq!(exposure(&runs, "d", 2, EvalSplit::Validation), 1);
        assert_eq!(exposure(&runs, "d", 1, EvalSplit::Validation), 0);
    }
}
```

- [ ] **Step 2: Implement**

```rust
use std::collections::{BTreeMap, BTreeSet};

use super::outcome::{classify, EvidenceClass, OutcomeKind, ProviderReason};
use crate::document_config::{EvalReducer, EvalSplit, EvalTier};

pub const SCORE_BP_MAX: u32 = 10_000;

/// One verdict row as scoring sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerdictView {
    pub verdict_id: String,
    /// Position of the verdict's stage in the case, for `LastStage`.
    pub stage_index: usize,
    pub check: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    pub regrade_of: Option<String>,
}

/// Drop every verdict that a later verdict names in `regrade_of`. The rows stay
/// in the database; this only selects what consumers read.
pub fn latest_verdicts(verdicts: Vec<VerdictView>) -> Vec<VerdictView> {
    let superseded: BTreeSet<String> =
        verdicts.iter().filter_map(|verdict| verdict.regrade_of.clone()).collect();
    verdicts
        .into_iter()
        .filter(|verdict| !superseded.contains(&verdict.verdict_id))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseTrialScore {
    NotEvidence,
    Unknown,
    /// Basis points in `0..=SCORE_BP_MAX`.
    Scored(u32),
}

fn verdict_bp(verdict: &VerdictView) -> u32 {
    match classify(verdict.kind, verdict.provider_reason) {
        EvidenceClass::Pass => verdict.score_bp.unwrap_or(SCORE_BP_MAX).min(SCORE_BP_MAX),
        // A fail kind with no check score (the stage never ran) scores zero.
        _ => verdict.score_bp.unwrap_or(0).min(SCORE_BP_MAX),
    }
}

/// `Eval.caseClass`, then the declared reducer over acceptance-tier verdicts.
/// A case-trial with no acceptance verdict is `Unknown`: totality was violated.
pub fn case_trial_score(reducer: EvalReducer, verdicts: &[VerdictView]) -> CaseTrialScore {
    let acceptance: Vec<&VerdictView> =
        verdicts.iter().filter(|verdict| verdict.tier == EvalTier::Acceptance).collect();
    if acceptance.is_empty() {
        return CaseTrialScore::Unknown;
    }
    let classes: Vec<EvidenceClass> =
        acceptance.iter().map(|v| classify(v.kind, v.provider_reason)).collect();
    if classes.contains(&EvidenceClass::NotEvidence) {
        return CaseTrialScore::NotEvidence;
    }
    if classes.contains(&EvidenceClass::Unknown) {
        return CaseTrialScore::Unknown;
    }
    let weighted_mean = |items: &[&VerdictView]| -> u32 {
        let weight: u64 = items.iter().map(|v| v.weight.max(1) as u64).sum();
        let total: u64 = items.iter().map(|v| verdict_bp(v) as u64 * v.weight.max(1) as u64).sum();
        (total / weight) as u32
    };
    CaseTrialScore::Scored(match reducer {
        EvalReducer::WeightedMean => weighted_mean(&acceptance),
        EvalReducer::All => {
            if acceptance.iter().all(|v| verdict_bp(v) == SCORE_BP_MAX) {
                SCORE_BP_MAX
            } else {
                0
            }
        }
        EvalReducer::LastStage => {
            let last = acceptance.iter().map(|v| v.stage_index).max().unwrap_or(0);
            let tail: Vec<&VerdictView> =
                acceptance.iter().copied().filter(|v| v.stage_index == last).collect();
            weighted_mean(&tail)
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialScore {
    pub case_id: String,
    pub trial_index: u32,
    pub score: CaseTrialScore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub case_id: String,
    pub trial_index: u32,
    pub baseline_bp: u32,
    pub candidate_bp: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairedEvidence {
    pub pairs: Vec<Pair>,
    /// Distinct `(case, trial_index)` keys seen in either cell.
    pub keys: usize,
    /// Keys lost because the baseline side was not evidence or was missing.
    pub dropped_baseline: usize,
    pub dropped_candidate: usize,
}

/// `exclude_not_evidence_v1`. A pair is `(case, trial_index)`, joined by the
/// shared seed. It counts only when both sides are evidence. An unknown is
/// imputed worst-case: it fails the candidate and passes the baseline.
pub fn pair_trials(baseline: &[TrialScore], candidate: &[TrialScore]) -> PairedEvidence {
    let index = |trials: &[TrialScore]| -> BTreeMap<(String, u32), CaseTrialScore> {
        trials.iter().map(|t| ((t.case_id.clone(), t.trial_index), t.score)).collect()
    };
    let (baseline, candidate) = (index(baseline), index(candidate));
    let keys: BTreeSet<&(String, u32)> = baseline.keys().chain(candidate.keys()).collect();
    let mut paired = PairedEvidence { keys: keys.len(), ..PairedEvidence::default() };
    for key in keys {
        let side = |score: Option<&CaseTrialScore>, unknown_bp: u32| match score {
            Some(CaseTrialScore::Scored(bp)) => Some(*bp),
            Some(CaseTrialScore::Unknown) => Some(unknown_bp),
            Some(CaseTrialScore::NotEvidence) | None => None,
        };
        let b = side(baseline.get(key), SCORE_BP_MAX);
        let c = side(candidate.get(key), 0);
        match (b, c) {
            (Some(baseline_bp), Some(candidate_bp)) => paired.pairs.push(Pair {
                case_id: key.0.clone(),
                trial_index: key.1,
                baseline_bp,
                candidate_bp,
            }),
            (b, c) => {
                if b.is_none() {
                    paired.dropped_baseline += 1;
                }
                if c.is_none() {
                    paired.dropped_candidate += 1;
                }
            }
        }
    }
    paired
}

/// Per-case mean over evidence trials, for a single-run report: an unknown
/// counts as failure, a not-evidence trial is excluded.
pub fn case_means_bp(trials: &[TrialScore]) -> BTreeMap<String, u32> {
    let mut sums: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for trial in trials {
        let bp = match trial.score {
            CaseTrialScore::Scored(bp) => bp,
            CaseTrialScore::Unknown => 0,
            CaseTrialScore::NotEvidence => continue,
        };
        let entry = sums.entry(trial.case_id.clone()).or_default();
        entry.0 += bp as u64;
        entry.1 += 1;
    }
    sums.into_iter().map(|(case, (sum, count))| (case, (sum / count) as u32)).collect()
}

/// Mean over cases, equally weighted. The case is the clustering unit.
pub fn headline_bp(trials: &[TrialScore]) -> Option<u32> {
    let means = case_means_bp(trials);
    if means.is_empty() {
        return None;
    }
    Some((means.values().map(|bp| *bp as u64).sum::<u64>() / means.len() as u64) as u32)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunHeader {
    pub definition_id: String,
    pub comparability_version: i64,
    pub split: EvalSplit,
    pub invalidated: bool,
}

/// Split exposure is a count of rows, never a counter field.
pub fn exposure(
    runs: &[RunHeader],
    definition_id: &str,
    comparability_version: i64,
    split: EvalSplit,
) -> usize {
    runs.iter()
        .filter(|run| {
            !run.invalidated
                && run.definition_id == definition_id
                && run.comparability_version == comparability_version
                && run.split == split
        })
        .count()
}
```

Add to `mod.rs`:

```rust
pub mod scoring;

pub use scoring::{
    case_means_bp, case_trial_score, exposure, headline_bp, latest_verdicts, pair_trials,
    CaseTrialScore, Pair, PairedEvidence, RunHeader, TrialScore, VerdictView, SCORE_BP_MAX,
};
```

- [ ] **Step 3: Run and commit**

Run: `cargo test -p gents --lib eval::scoring`
Expected: 9 passed.

```bash
git add crates/gents/src/eval
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(eval): reducers, case classes, seed pairing and exposure counting"
```

### Task 7: Documents and persistence

**Files:**
- Create: `crates/gents/src/eval/documents.rs`
- Modify: `crates/gents/src/eval/mod.rs`

**Interfaces:**
- Consumes: `ConfigAccess`, `escape_graphql_string`, `EvalSplit`, `EvalTier`, `OutcomeKind`, `ProviderReason`, `TAXONOMY_VERSION`.
- Produces:
  - `DefinitionRef { definition_id, comparability_version: i64, digest }`, `SubjectRef { pack_digest, behavior_id }`, `CellSpec { cell_id, label, subject: SubjectRef, inference_profile_id }`
  - `RunOrigin { definition, split, case_ids, cells, trials_per_case: u32, seed_base: i64, deadline_secs: Option<u64>, concurrency: u32, denominator_policy, taxonomy_version, max_infra_retries: u32, check_registry_version, source_commit, source_dirty: bool, purpose }`
  - `DENOMINATOR_POLICY_V1: &str = "exclude_not_evidence_v1"`
  - `RunRecord { run_id, owner, evaluator_did, origin, created_at, invalidated: Option<Invalidation> }`, `Invalidation { at, by, reason }`
  - `TrialIdentity`, `StageCompletion`, `Anchor`, `TrialUsage`, `TrialCompletion`, `TrialRecord`
  - `VerdictDraft`, `VerdictRecord`
  - `create_run`, `load_run`, `invalidate_run`, `create_trial`, `complete_trial`, `load_trials`, `append_verdict`, `load_verdicts`
  - typed errors `AlreadyCompleted` and `FeedbackOffTrain` with `already_completed(&Error) -> bool` and `feedback_off_train(&Error) -> bool`.

- [ ] **Step 1: Failing tests**

Create `documents.rs` with this test module only, and confirm it fails to compile:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::defra_node::EmbeddedNode;
    use std::sync::Arc;

    const OWNER: &str = "did:key:eval-owner";

    async fn access() -> ConfigAccess {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        for sdl in [
            gents_protocol::schemas::EVAL_RUN,
            gents_protocol::schemas::EVAL_TRIAL,
            gents_protocol::schemas::EVAL_VERDICT,
        ] {
            node.add_schema(sdl).await.unwrap();
        }
        ConfigAccess::Local(node)
    }

    fn origin(split: EvalSplit) -> RunOrigin {
        RunOrigin {
            definition: DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:def".into(),
            },
            split,
            case_ids: vec!["disk-warning".into()],
            cells: vec![CellSpec {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                subject: SubjectRef { pack_digest: "sha256:pack".into(), behavior_id: "monitor".into() },
                inference_profile_id: "local".into(),
            }],
            trials_per_case: 2,
            seed_base: 1000,
            deadline_secs: Some(600),
            concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(),
            taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 1,
            check_registry_version: "checks-1".into(),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            purpose: "eval".into(),
        }
    }

    fn identity(run_id: &str, trial_id: &str) -> TrialIdentity {
        TrialIdentity {
            trial_id: trial_id.into(),
            run_id: run_id.into(),
            cell_id: "baseline".into(),
            case_id: "disk-warning".into(),
            trial_index: 0,
            attempt: 0,
            trial_agent_did: "did:key:trial".into(),
            session_id: format!("session-{trial_id}"),
            seed: 1000,
            home_hint: Some("/tmp/trial".into()),
        }
    }

    fn completion() -> TrialCompletion {
        TrialCompletion {
            ended_at: "2026-09-21T00:00:00Z".into(),
            stages: vec![StageCompletion {
                stage_id: "check".into(),
                request_id: Some("req-1".into()),
                terminal_state: Some("completed".into()),
                failure_kind: None,
            }],
            usage: TrialUsage { input_tokens: Some(10), output_tokens: None },
            anchor: Anchor { terminal_states: vec!["completed".into()], requests: 1, inference_calls: 3 },
        }
    }

    fn draft(run_id: &str, trial_id: &str, verdict_id: &str, feedback: Option<&str>) -> VerdictDraft {
        VerdictDraft {
            verdict_id: verdict_id.into(),
            run_id: run_id.into(),
            trial_id: trial_id.into(),
            stage_id: "check".into(),
            check: "mailbox_findings".into(),
            check_version: "1".into(),
            tier: EvalTier::Acceptance,
            kind: OutcomeKind::Passed,
            provider_reason: None,
            score_bp: Some(10_000),
            weight: 1,
            raw: serde_json::json!({"detail": "ok"}),
            feedback: feedback.map(str::to_owned),
            regrade_of: None,
        }
    }

    #[tokio::test]
    async fn a_run_round_trips_and_can_only_be_invalidated() {
        let access = access().await;
        let run = create_run(&access, "run-1", OWNER, "did:key:evaluator", &origin(EvalSplit::Validation))
            .await
            .unwrap();
        assert!(run.invalidated.is_none());
        invalidate_run(&access, OWNER, "run-1", OWNER, "grader bug").await.unwrap();
        let loaded = load_run(&access, OWNER, "run-1").await.unwrap().unwrap();
        assert_eq!(loaded.origin, run.origin);
        assert_eq!(loaded.invalidated.unwrap().reason, "grader bug");
    }

    #[tokio::test]
    async fn a_trial_completion_is_written_once() {
        let access = access().await;
        create_run(&access, "run-2", OWNER, "did:key:evaluator", &origin(EvalSplit::Validation))
            .await
            .unwrap();
        create_trial(&access, OWNER, &identity("run-2", "trial-1")).await.unwrap();
        let before = load_trials(&access, OWNER, "run-2").await.unwrap();
        assert!(before[0].completion.is_none(), "a null completion is a fact");
        complete_trial(&access, OWNER, "trial-1", &completion()).await.unwrap();
        let error = complete_trial(&access, OWNER, "trial-1", &completion()).await.unwrap_err();
        assert!(already_completed(&error), "{error:#}");
        let after = load_trials(&access, OWNER, "run-2").await.unwrap();
        assert_eq!(after[0].completion.as_ref().unwrap().usage.output_tokens, None, "missing usage stays null");
    }

    #[tokio::test]
    async fn feedback_is_refused_off_the_train_split() {
        let access = access().await;
        let error = append_verdict(&access, OWNER, EvalSplit::Validation, &draft("r", "t", "v1", Some("hint")))
            .await
            .unwrap_err();
        assert!(feedback_off_train(&error), "{error:#}");
        append_verdict(&access, OWNER, EvalSplit::Train, &draft("r", "t", "v2", Some("hint"))).await.unwrap();
        append_verdict(&access, OWNER, EvalSplit::Validation, &draft("r", "t", "v3", None)).await.unwrap();
        let verdicts = load_verdicts(&access, OWNER, "r").await.unwrap();
        assert_eq!(verdicts.len(), 2);
    }

    #[tokio::test]
    async fn a_non_evidence_verdict_stores_a_null_score() {
        let access = access().await;
        let mut outage = draft("r2", "t", "v1", None);
        outage.kind = OutcomeKind::Provider;
        outage.provider_reason = Some(ProviderReason::Unavailable);
        outage.score_bp = Some(10_000);
        append_verdict(&access, OWNER, EvalSplit::Validation, &outage).await.unwrap();
        let verdicts = load_verdicts(&access, OWNER, "r2").await.unwrap();
        assert_eq!(verdicts[0].score_bp, None, "nobody can average a fabricated score");
        assert_eq!(verdicts[0].provider_reason, Some(ProviderReason::Unavailable));
    }
}
```

- [ ] **Step 2: Implement**

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::outcome::{classify, EvidenceClass, OutcomeKind, ProviderReason};
use crate::config_client::ConfigAccess;
use crate::document_config::{EvalSplit, EvalTier};
use crate::graphql::escape_graphql_string;

pub const DENOMINATOR_POLICY_V1: &str = "exclude_not_evidence_v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionRef {
    pub definition_id: String,
    pub comparability_version: i64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectRef {
    pub pack_digest: String,
    pub behavior_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellSpec {
    pub cell_id: String,
    pub label: String,
    pub subject: SubjectRef,
    pub inference_profile_id: String,
}

/// Frozen at creation and never rewritten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOrigin {
    pub definition: DefinitionRef,
    pub split: EvalSplit,
    pub case_ids: Vec<String>,
    pub cells: Vec<CellSpec>,
    pub trials_per_case: u32,
    /// A trial's seed is `seed_base + trial_index`, shared by every cell.
    pub seed_base: i64,
    pub deadline_secs: Option<u64>,
    pub concurrency: u32,
    pub denominator_policy: String,
    pub taxonomy_version: String,
    pub max_infra_retries: u32,
    pub check_registry_version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    /// `"eval"` or `"optimization:<job_id>"`.
    pub purpose: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalidation {
    pub at: String,
    pub by: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub run_id: String,
    pub owner: String,
    pub evaluator_did: String,
    pub origin: RunOrigin,
    pub created_at: String,
    pub invalidated: Option<Invalidation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialIdentity {
    pub trial_id: String,
    pub run_id: String,
    pub cell_id: String,
    pub case_id: String,
    pub trial_index: u32,
    /// A resumed trial is a new row with `attempt + 1`.
    pub attempt: u32,
    pub trial_agent_did: String,
    /// With `trial_agent_did`, the durable evidence reference.
    pub session_id: String,
    pub seed: i64,
    /// A locator only. Never identity.
    pub home_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageCompletion {
    pub stage_id: String,
    pub request_id: Option<String>,
    pub terminal_state: Option<String>,
    pub failure_kind: Option<String>,
}

/// Lets a reader tell a deleted trial home from a mismatched one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub terminal_states: Vec<String>,
    pub requests: u32,
    pub inference_calls: u32,
}

/// A missing total is `None`, never zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialCompletion {
    pub ended_at: String,
    pub stages: Vec<StageCompletion>,
    pub usage: TrialUsage,
    pub anchor: Anchor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialRecord {
    pub identity: TrialIdentity,
    pub created_at: String,
    pub completion: Option<TrialCompletion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerdictDraft {
    pub verdict_id: String,
    pub run_id: String,
    pub trial_id: String,
    pub stage_id: String,
    pub check: String,
    pub check_version: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    pub raw: Value,
    pub feedback: Option<String>,
    pub regrade_of: Option<String>,
}

pub type VerdictRecord = VerdictDraft;

#[derive(Debug)]
pub struct AlreadyCompleted;
impl std::fmt::Display for AlreadyCompleted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("eval trial completion is write-once and is already set")
    }
}
impl std::error::Error for AlreadyCompleted {}
pub fn already_completed(error: &anyhow::Error) -> bool {
    error.downcast_ref::<AlreadyCompleted>().is_some()
}

#[derive(Debug)]
pub struct FeedbackOffTrain;
impl std::fmt::Display for FeedbackOffTrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("eval verdict feedback is only permitted on the train split")
    }
}
impl std::error::Error for FeedbackOffTrain {}
pub fn feedback_off_train(error: &anyhow::Error) -> bool {
    error.downcast_ref::<FeedbackOffTrain>().is_some()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

async fn create(access: &ConfigAccess, operation: &'static str, name: &'static str, input: Value) -> Result<()> {
    let variables = json!({ "input": input });
    let mutation = format!(
        "mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"
    );
    access
        .transact(operation, |txn| {
            let (variables, mutation) = (&variables, &mutation);
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await.map(|_| ()) })
        })
        .await
        .with_context(|| format!("create_{name}"))
}

async fn rows(access: &ConfigAccess, operation: &'static str, query: String, name: &'static str) -> Result<Vec<Value>> {
    let response = access
        .transact(operation, |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?;
    Ok(response["data"][name].as_array().cloned().unwrap_or_default())
}

pub async fn create_run(
    access: &ConfigAccess,
    run_id: &str,
    owner: &str,
    evaluator_did: &str,
    origin: &RunOrigin,
) -> Result<RunRecord> {
    let created_at = now();
    create(access, "eval.create_run", "EvalRun", json!({
        "run_id": run_id, "owner_agent_did": owner, "evaluator_did": evaluator_did,
        "origin": serde_json::to_value(origin)?, "created_at": created_at,
    }))
    .await?;
    tracing::info!(run_id, owner, purpose = %origin.purpose, "eval run frozen");
    Ok(RunRecord {
        run_id: run_id.into(),
        owner: owner.into(),
        evaluator_did: evaluator_did.into(),
        origin: origin.clone(),
        created_at,
        invalidated: None,
    })
}

pub async fn load_run(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Option<RunRecord>> {
    let found = rows(access, "eval.load_run", format!(
        r#"{{ EvalRun(filter: {{ owner_agent_did: {{ _eq: "{}" }}, run_id: {{ _eq: "{}" }} }}) {{ run_id owner_agent_did evaluator_did origin created_at invalidated }} }}"#,
        escape_graphql_string(owner), escape_graphql_string(run_id)
    ), "EvalRun").await?;
    found.first().map(|row| {
        Ok(RunRecord {
            run_id: row["run_id"].as_str().context("run_id")?.into(),
            owner: row["owner_agent_did"].as_str().context("owner_agent_did")?.into(),
            evaluator_did: row["evaluator_did"].as_str().unwrap_or_default().into(),
            origin: serde_json::from_value(row["origin"].clone()).context("decoding run origin")?,
            created_at: row["created_at"].as_str().unwrap_or_default().into(),
            invalidated: match &row["invalidated"] {
                Value::Null => None,
                value => Some(serde_json::from_value(value.clone()).context("decoding invalidation")?),
            },
        })
    }).transpose()
}

/// The only mutation a run admits.
pub async fn invalidate_run(access: &ConfigAccess, owner: &str, run_id: &str, by: &str, reason: &str) -> Result<()> {
    let variables = json!({ "input": { "invalidated": { "at": now(), "by": by, "reason": reason } } });
    let mutation = format!(
        r#"mutation($input: EvalRunMutationInputArg!) {{ update_EvalRun(filter: {{ owner_agent_did: {{ _eq: "{}" }}, run_id: {{ _eq: "{}" }} }}, input: $input) {{ _docID }} }}"#,
        escape_graphql_string(owner), escape_graphql_string(run_id)
    );
    let response = access
        .transact("eval.invalidate_run", |txn| {
            let (variables, mutation) = (&variables, &mutation);
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        })
        .await?;
    anyhow::ensure!(
        response["data"]["update_EvalRun"].as_array().is_some_and(|rows| !rows.is_empty()),
        "no eval run {run_id:?} owned by {owner:?}"
    );
    tracing::warn!(run_id, by, reason, "eval run invalidated");
    Ok(())
}

pub async fn create_trial(access: &ConfigAccess, owner: &str, identity: &TrialIdentity) -> Result<()> {
    create(access, "eval.create_trial", "EvalTrial", json!({
        "trial_id": identity.trial_id, "owner_agent_did": owner, "run_id": identity.run_id,
        "cell_id": identity.cell_id, "case_id": identity.case_id,
        "trial_index": identity.trial_index, "attempt": identity.attempt,
        "trial_agent_did": identity.trial_agent_did, "session_id": identity.session_id,
        "seed": identity.seed, "home_hint": identity.home_hint, "created_at": now(),
    }))
    .await
}

/// Write-once. The read and the write share one transaction.
pub async fn complete_trial(access: &ConfigAccess, owner: &str, trial_id: &str, completion: &TrialCompletion) -> Result<()> {
    let filter = format!(
        r#"owner_agent_did: {{ _eq: "{}" }}, trial_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(owner), escape_graphql_string(trial_id)
    );
    let variables = json!({ "input": { "completion": serde_json::to_value(completion)? } });
    let trial_id = trial_id.to_owned();
    access
        .transact("eval.complete_trial", |txn| {
            let (filter, variables, trial_id) = (&filter, &variables, &trial_id);
            Box::pin(async move {
                let current = txn
                    .execute(&format!("{{ EvalTrial(filter: {{ {filter} }}) {{ completion }} }}"))
                    .await?;
                let row = current["data"]["EvalTrial"]
                    .as_array()
                    .and_then(|rows| rows.first())
                    .with_context(|| format!("no eval trial {trial_id:?}"))?;
                if !row["completion"].is_null() {
                    return Err(anyhow::Error::new(AlreadyCompleted));
                }
                txn.execute_with_variables(
                    &format!("mutation($input: EvalTrialMutationInputArg!) {{ update_EvalTrial(filter: {{ {filter} }}, input: $input) {{ _docID }} }}"),
                    variables,
                )
                .await
                .map(|_| ())
            })
        })
        .await
}

pub async fn load_trials(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
    let found = rows(access, "eval.load_trials", format!(
        r#"{{ EvalTrial(filter: {{ owner_agent_did: {{ _eq: "{}" }}, run_id: {{ _eq: "{}" }} }}) {{ trial_id run_id cell_id case_id trial_index attempt trial_agent_did session_id seed home_hint created_at completion }} }}"#,
        escape_graphql_string(owner), escape_graphql_string(run_id)
    ), "EvalTrial").await?;
    found.iter().map(|row| {
        let text = |field: &str| row[field].as_str().unwrap_or_default().to_owned();
        Ok(TrialRecord {
            identity: TrialIdentity {
                trial_id: text("trial_id"),
                run_id: text("run_id"),
                cell_id: text("cell_id"),
                case_id: text("case_id"),
                trial_index: row["trial_index"].as_u64().unwrap_or_default() as u32,
                attempt: row["attempt"].as_u64().unwrap_or_default() as u32,
                trial_agent_did: text("trial_agent_did"),
                session_id: text("session_id"),
                seed: row["seed"].as_i64().unwrap_or_default(),
                home_hint: row["home_hint"].as_str().map(str::to_owned),
            },
            created_at: text("created_at"),
            completion: match &row["completion"] {
                Value::Null => None,
                value => Some(serde_json::from_value(value.clone()).context("decoding trial completion")?),
            },
        })
    }).collect()
}

/// Append-only. `split` is the run's split: the runner, not the check, enforces
/// that feedback exists only on train. A non-evidence verdict stores a null score.
pub async fn append_verdict(access: &ConfigAccess, owner: &str, split: EvalSplit, draft: &VerdictDraft) -> Result<()> {
    if draft.feedback.is_some() && split != EvalSplit::Train {
        return Err(anyhow::Error::new(FeedbackOffTrain));
    }
    let score_bp = match classify(draft.kind, draft.provider_reason) {
        EvidenceClass::NotEvidence | EvidenceClass::Unknown => None,
        _ => draft.score_bp.map(|bp| bp.min(super::scoring::SCORE_BP_MAX)),
    };
    create(access, "eval.append_verdict", "EvalVerdict", json!({
        "verdict_id": draft.verdict_id, "owner_agent_did": owner, "run_id": draft.run_id,
        "trial_id": draft.trial_id, "stage_id": draft.stage_id, "check": draft.check,
        "check_version": draft.check_version,
        "tier": serde_json::to_value(draft.tier)?,
        "outcome_kind": draft.kind.as_str(),
        "provider_reason": draft.provider_reason.map(ProviderReason::as_str),
        "score_bp": score_bp, "weight": draft.weight, "raw": draft.raw,
        "feedback": draft.feedback, "regrade_of": draft.regrade_of, "created_at": now(),
    }))
    .await
}

pub async fn load_verdicts(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<VerdictRecord>> {
    let found = rows(access, "eval.load_verdicts", format!(
        r#"{{ EvalVerdict(filter: {{ owner_agent_did: {{ _eq: "{}" }}, run_id: {{ _eq: "{}" }} }}) {{ verdict_id run_id trial_id stage_id check check_version tier outcome_kind provider_reason score_bp weight raw feedback regrade_of }} }}"#,
        escape_graphql_string(owner), escape_graphql_string(run_id)
    ), "EvalVerdict").await?;
    found.iter().map(|row| {
        let text = |field: &str| row[field].as_str().unwrap_or_default().to_owned();
        Ok(VerdictRecord {
            verdict_id: text("verdict_id"),
            run_id: text("run_id"),
            trial_id: text("trial_id"),
            stage_id: text("stage_id"),
            check: text("check"),
            check_version: text("check_version"),
            tier: serde_json::from_value(row["tier"].clone()).context("decoding verdict tier")?,
            kind: OutcomeKind::parse(&text("outcome_kind")).context("unknown outcome_kind")?,
            provider_reason: row["provider_reason"].as_str().and_then(ProviderReason::parse),
            score_bp: row["score_bp"].as_u64().map(|bp| bp as u32),
            weight: row["weight"].as_u64().unwrap_or(1) as u32,
            raw: row["raw"].clone(),
            feedback: row["feedback"].as_str().map(str::to_owned),
            regrade_of: row["regrade_of"].as_str().map(str::to_owned),
        })
    }).collect()
}
```

Add to `mod.rs`:

```rust
pub mod documents;

pub use documents::{
    already_completed, append_verdict, complete_trial, create_run, create_trial,
    feedback_off_train, invalidate_run, load_run, load_trials, load_verdicts, Anchor, CellSpec,
    DefinitionRef, Invalidation, RunOrigin, RunRecord, StageCompletion, SubjectRef,
    TrialCompletion, TrialIdentity, TrialRecord, TrialUsage, VerdictDraft, VerdictRecord,
    DENOMINATOR_POLICY_V1,
};
```

- [ ] **Step 3: Run and commit**

Run: `cargo test -p gents --lib eval::documents`
Expected: 4 passed. If the `@immutable` directive rejects writing a null to an immutable nullable column, omit null-valued keys from the `create` input instead of sending `null`: build the input map and remove entries whose value is `Value::Null` before the mutation.

```bash
git add crates/gents/src/eval
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(eval): fact-only run, trial and verdict persistence"
```

PR E description: baseline `eval/04-runtime-collections`; new owner `gents::eval`; the four conformance domains flip from follow-up to consumer; no deletions.

---

## PR F: Protected collections

Branch: `eval/06-protected-collections`, base `eval/05-contract`.

### Task 8: A datastore surface may never name an eval collection

**Files:**
- Modify: `crates/gents/src/document_config/write_tool.rs`, `crates/gents/src/document_config/surface_tool.rs`

**Interfaces:**
- Produces: `PROTECTED_DATASTORE_COLLECTIONS: &[&str]` and `pub(crate) fn reject_protected_collection_name(collection: &str) -> Result<()>` in `write_tool.rs`.

The repository has no reserved-collection list for datastore surfaces today. Every write and query declaration, whether declared on `Tools` or expanded from a `DatastoreToolSurface`, passes through `WriteToolDecl::validate()` or `validate_query_tool_declarations()`. Both already call `validate_collection_identifier`. The new rule sits beside those calls, not inside `validate_collection_identifier`, which lives in `gents-protocol` and is shared far beyond datastore surfaces.

- [ ] **Step 1: Failing tests**

In the test module of `crates/gents/src/document_config/write_tool.rs` add:

```rust
    #[test]
    fn eval_and_optimization_collections_are_never_exposed() {
        for name in super::PROTECTED_DATASTORE_COLLECTIONS {
            let error = super::reject_protected_collection_name(name).unwrap_err();
            assert!(format!("{error:#}").contains("protected"), "{error:#}");
        }
        for name in ["EvalDefinition", "EvalRun", "EvalTrial", "EvalVerdict", "OptimizationJob"] {
            assert!(super::PROTECTED_DATASTORE_COLLECTIONS.contains(&name), "{name}");
        }
        assert!(super::reject_protected_collection_name("Notes").is_ok());
    }
```

Run: `cargo test -p gents --lib document_config::write_tool::tests::eval_and_optimization`
Expected: compile error, `cannot find value PROTECTED_DATASTORE_COLLECTIONS`.

- [ ] **Step 2: Implement**

In `write_tool.rs`, near `is_reserved_builtin_tool_name`:

```rust
/// Collections no datastore tool may read or write. They hold protected eval
/// material and promotion evidence; an agent in the launching home, including
/// the live version of a behavior under optimization, must not reach them.
pub const PROTECTED_DATASTORE_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::EVAL_DEFINITION_NAME,
    gents_protocol::schemas::EVAL_RUN_NAME,
    gents_protocol::schemas::EVAL_TRIAL_NAME,
    gents_protocol::schemas::EVAL_VERDICT_NAME,
    "OptimizationJob",
];

pub(crate) fn reject_protected_collection_name(collection: &str) -> Result<()> {
    if PROTECTED_DATASTORE_COLLECTIONS.contains(&collection) {
        anyhow::bail!("collection {collection:?} is protected and cannot be exposed through a datastore tool");
    }
    Ok(())
}
```

`"OptimizationJob"` is a literal because that collection lands with the optimization stack; replace it with the schema constant there.

In `WriteToolDecl::validate()`, directly after the `validate_collection_identifier(&self.collection)` call, add:

```rust
        reject_protected_collection_name(&self.collection)?;
```

In `surface_tool.rs`, in `validate_query_tool_declarations`, directly after its `validate_collection_identifier(&decl.collection)` call, add:

```rust
        super::write_tool::reject_protected_collection_name(&decl.collection)?;
```

- [ ] **Step 3: Declaration-level tests**

In the test module of `surface_tool.rs`, after `query_decl_rejects_reserved_filter_names`, add:

```rust
    #[test]
    fn surface_entries_cannot_name_a_protected_collection() {
        let create = SurfaceToolDecl::Create(WriteToolDecl {
            notification: None,
            tool_name: "write_verdict".into(),
            collection: "EvalVerdict".into(),
            description: "forge a verdict".into(),
            fields: vec![WriteToolField { name: "score_bp".into(), required: true, fill: None }],
            output_obligation: None,
        });
        let query = SurfaceToolDecl::Query(QueryToolDecl {
            tool_name: "query_definition".into(),
            collection: "EvalDefinition".into(),
            description: String::new(),
            fields: vec!["cases".into()],
            filter_fields: Vec::new(),
        });
        for decl in [create, query] {
            let error = decl.validate().unwrap_err();
            assert!(format!("{error:#}").contains("protected"), "{error:#}");
        }
    }
```

The struct literals match `create_entry_without_kind_round_trips` and `query_decl_rejects_empty_projection` in the same module. If `WriteToolDecl` or `WriteToolField` is not already imported there, add it to the module's `use super::...` line.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p gents --lib document_config::write_tool document_config::surface_tool`
Run: `cargo test -p gents && cargo check --workspace --all-targets`
Expected: all pass.

```bash
git add crates/gents/src/document_config
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(config): datastore tools can never expose eval or optimization collections"
```

PR F description: baseline `eval/05-contract`; owner `document_config::{write_tool, surface_tool}`; closes threat T4's datastore path from the eval core contract spec; `OptimizationJob` is named ahead of its schema by design.

---

## Out of scope for M1

The canary test and everything that runs a trial (spec 2). The check registry (spec 3). Reports, `compare` and the CLI (spec 4). `OptimizationJob` and the policy (spec 5; see `2026-09-21-optimization-policy-and-lean.md`). ACP on eval collections, until the disagreement recorded in the umbrella is settled.
