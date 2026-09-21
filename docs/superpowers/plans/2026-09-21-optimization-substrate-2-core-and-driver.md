# Optimization Substrate, Plan 2 of 3: Core and Driver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure policy core, the evaluator and proposer seams, the local-only `OptimizationJob` record, and the round driver, proven end to end against an embedded node with scripted doubles and no model.

**Architecture:** A new `crates/gents/src/optimization/` module. `outcome`, `policy` and `target` are pure. `evaluator` and `proposer` are `async_trait` seams. `round` persists one document with a frozen `origin` and a length-guarded append-only `journal`. `driver` replays the journal and continues procedurally, so interruption and resume are the same code path.

**Tech Stack:** Rust 1.97.1, `async-trait` 0.1, `serde`, `sha2`, `proptest` 1, DefraDB embedded node, `tokio` tests.

**Spec:** `docs/superpowers/specs/2026-09-21-optimization-substrate-design.md`

**Depends on:** Plan 1 merged or stacked beneath (`optimization/03-cas`). This plan uses `DesiredStateExpectation`, `desired_state_document_digest` (public), and `lean_optimization_cases()`.

## Global Constraints

- Every fraction is an integer number of basis points (1 bp = 0.01%); the significance level is parts per million. This matches `Proofs/Optimization.lean` exactly.
- Every `Evaluator` maps a deadline to `OutcomeClass::Fail`.
- `SplitId::held_out()` is constructible only inside `crate::optimization`. Tests and evaluators read `split.kind()`.
- The proposer never holds a `ConfigAccess`. The driver is the only writer of the job.
- `OptimizationJob` is a runtime document. Do not add it to the `Collection` enum, to `CLIENT_COLLECTIONS`, to `CONVERSATION_COLLECTIONS`, or to `BRANCHABLE_COLLECTION_NAMES`. Add it to `LOCAL_AUDIT_COLLECTION_NAMES` because its journal carries prompts.
- The baseline closure is the owner's full desired configuration, read through `ConfigReferences::load_in_txn`. It is a safe superset of the reachable documents.
- Use `tracing`, never `println!`. Escape interpolated GraphQL strings with `escape_graphql_string`. Never emit `[]` in a mutation. The `journal` field is a `String` scalar holding JSON text, so the text `"[]"` inside it is a string value and is allowed.
- No `config` tool verbs. Nothing in `self_config` changes.
- Before each push: `cargo test -p gents` and `cargo check --workspace --all-targets`.
- Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit ...`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/gents/src/optimization/mod.rs` | create | module tree and re-exports |
| `crates/gents/src/optimization/outcome.rs` | create | classes, arms, imputation, tallies |
| `crates/gents/src/optimization/policy.rs` | create | `PolicyV1`, three gates, Fisher test, `decide` |
| `crates/gents/src/optimization/evaluator.rs` | create | `Evaluator`, `EvalRequest`, `EvalReport`, `SplitId` |
| `crates/gents/src/optimization/proposer.rs` | create | `Proposer`, `ProposalInput`, `Proposal` |
| `crates/gents/src/optimization/target.rs` | create | `TargetField`, closure capture, patching, structural gate |
| `crates/gents/src/optimization/round.rs` | create | `OptimizationJob` persistence, journal, derived state |
| `crates/gents/src/optimization/driver.rs` | create | replay and the round state machine |
| `crates/gents/src/lib.rs` | modify | `pub mod optimization;` |
| `crates/gents-schemas/schemas/agent/optimization_job.graphql` | create | SDL |
| `crates/gents-schemas/src/lib.rs` | modify | constants, catalogs, local-audit list |
| `crates/gents-protocol/src/schemas.rs` | modify | re-export and catalogs |
| `crates/gents-migration/src/registry.rs` | modify | baseline entry with pinned root |
| `crates/gents/tests/support/optimization.rs` | create | `ScriptedEvaluator`, `ScriptedProposer`, fixtures |
| `crates/gents/tests/e2e_runtime/optimization_driver.rs` | create | the end-to-end matrix |
| `crates/gents/tests/e2e_runtime.rs` | modify | register the new test module |
| `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean` | modify | flip `optimization_cases` to a consumer |

---

## PR 4: Pure core

Branch: `optimization/04-core`, base `optimization/03-cas`.

### Task 1: Outcomes and tallies

**Files:**
- Create: `crates/gents/src/optimization/mod.rs`, `crates/gents/src/optimization/outcome.rs`
- Modify: `crates/gents/src/lib.rs`

**Interfaces:**
- Produces: `OutcomeClass`, `Arm`, `Usage`, `CaseTrialOutcome`, `Tally`, `CaseTallies = BTreeMap<String, Tally>`, `impute(Arm, OutcomeClass) -> Option<bool>`, `tally_by_case(Arm, &[CaseTrialOutcome]) -> CaseTallies`, `merge_tallies(&mut CaseTallies, &CaseTallies)`.

- [ ] **Step 1: Module skeleton**

`crates/gents/src/optimization/mod.rs`:

```rust
//! Configuration optimization substrate (#1455). Evaluation sits behind
//! [`Evaluator`]; promotion is decided by a pure versioned policy and applied
//! through a digest-guarded, operator-only transaction.

pub mod outcome;

pub use outcome::{
    impute, merge_tallies, tally_by_case, Arm, CaseTallies, CaseTrialOutcome, OutcomeClass, Tally,
    Usage,
};
```

In `crates/gents/src/lib.rs`, add `pub mod optimization;` in alphabetical position among the `pub mod` lines (after `oneshot`).

- [ ] **Step 2: Write `outcome.rs` with its tests**

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    Pass,
    Fail,
    /// Infrastructure or provider trouble, or a skipped prerequisite.
    NotEvidence,
    /// The evaluator could not classify the trial.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Baseline,
    Candidate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseTrialOutcome {
    pub case_id: String,
    pub trial: u32,
    pub class: OutcomeClass,
    /// The evaluator's own verdict, preserved verbatim.
    pub raw_kind: String,
    /// Diagnostic text. Meaningful on the train split only.
    pub feedback: Option<String>,
    pub evidence_ref: String,
    pub usage: Usage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub pass: u64,
    pub fail: u64,
    pub excluded: u64,
}

impl Tally {
    pub fn evidence(&self) -> u64 {
        self.pass + self.fail
    }
    pub fn total(&self) -> u64 {
        self.pass + self.fail + self.excluded
    }
}

pub type CaseTallies = BTreeMap<String, Tally>;

/// `Proofs/Optimization.lean` `impute`. `None` means excluded. An unknown can
/// never help a candidate: it fails the candidate and passes the baseline.
pub fn impute(arm: Arm, class: OutcomeClass) -> Option<bool> {
    match (arm, class) {
        (_, OutcomeClass::Pass) => Some(true),
        (_, OutcomeClass::Fail) => Some(false),
        (_, OutcomeClass::NotEvidence) => None,
        (Arm::Candidate, OutcomeClass::Unknown) => Some(false),
        (Arm::Baseline, OutcomeClass::Unknown) => Some(true),
    }
}

pub fn tally_by_case(arm: Arm, outcomes: &[CaseTrialOutcome]) -> CaseTallies {
    let mut tallies = CaseTallies::new();
    for outcome in outcomes {
        let tally = tallies.entry(outcome.case_id.clone()).or_default();
        match impute(arm, outcome.class) {
            Some(true) => tally.pass += 1,
            Some(false) => tally.fail += 1,
            None => tally.excluded += 1,
        }
    }
    tallies
}

/// Re-runs append trials; they never replace earlier ones.
pub fn merge_tallies(into: &mut CaseTallies, more: &CaseTallies) {
    for (case_id, tally) in more {
        let entry = into.entry(case_id.clone()).or_default();
        entry.pass += tally.pass;
        entry.fail += tally.fail;
        entry.excluded += tally.excluded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(case_id: &str, class: OutcomeClass) -> CaseTrialOutcome {
        CaseTrialOutcome {
            case_id: case_id.into(),
            trial: 0,
            class,
            raw_kind: "raw".into(),
            feedback: None,
            evidence_ref: "ref".into(),
            usage: Usage::default(),
        }
    }

    #[test]
    fn imputation_matches_lean() {
        for row in &crate::lean_vocab_test::lean_optimization_cases().imputation {
            let arm = match row.arm.as_str() {
                "baseline" => Arm::Baseline,
                "candidate" => Arm::Candidate,
                other => panic!("unknown arm {other}"),
            };
            let class: OutcomeClass =
                serde_json::from_value(serde_json::Value::String(row.class.clone())).unwrap();
            let got = match impute(arm, class) {
                Some(true) => "pass",
                Some(false) => "fail",
                None => "excluded",
            };
            assert_eq!(got, row.counts_as, "{} {}", row.arm, row.class);
        }
    }

    #[test]
    fn unknown_never_helps_a_candidate() {
        let outcomes = [outcome("a", OutcomeClass::Unknown)];
        assert_eq!(tally_by_case(Arm::Candidate, &outcomes)["a"].fail, 1);
        assert_eq!(tally_by_case(Arm::Baseline, &outcomes)["a"].pass, 1);
    }

    #[test]
    fn merge_appends() {
        let mut into = tally_by_case(Arm::Candidate, &[outcome("a", OutcomeClass::Pass)]);
        let more = tally_by_case(
            Arm::Candidate,
            &[outcome("a", OutcomeClass::Fail), outcome("b", OutcomeClass::NotEvidence)],
        );
        merge_tallies(&mut into, &more);
        assert_eq!(into["a"], Tally { pass: 1, fail: 1, excluded: 0 });
        assert_eq!(into["b"], Tally { pass: 0, fail: 0, excluded: 1 });
    }
}
```

- [ ] **Step 3: Run**

Run: `RUST_MIN_STACK=16777216 cargo test -p gents --lib optimization::outcome`
Expected: 3 passed. `imputation_matches_lean` builds the Lean contract on first use, which is slow once per process.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/src/optimization crates/gents/src/lib.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): outcome classes, imputation and tallies"
```

### Task 2: Promotion policy

**Files:**
- Create: `crates/gents/src/optimization/policy.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes: `Tally`, `CaseTallies`.
- Produces: `POLICY_VERSION: &str = "v1"`, `PolicyV1`, `Mode`, `RejectReason`, `Decision`, `Evidence`, `Gates`, `evidence(&CaseTallies, &CaseTallies) -> Evidence`, `sufficient`, `no_case_regression`, `improves`, `fisher_one_sided(a, b, c, d) -> f64`, `gates`, `decide_gates(Mode, Gates) -> Decision`, `decide(Mode, &PolicyV1, &Evidence) -> Decision`.

- [ ] **Step 1: Write the failing tests first**

Create `policy.rs` containing only the test module below plus `use super::outcome::*;`, run `cargo test -p gents --lib optimization::policy`, and confirm it fails to compile for missing items. Then add the implementation from Step 2 above the test module.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn t(pass: u64, fail: u64, excluded: u64) -> Tally {
        Tally { pass, fail, excluded }
    }

    fn ev(cases: &[(Tally, Tally)]) -> Evidence {
        Evidence {
            cases_match: true,
            cases: cases
                .iter()
                .enumerate()
                .map(|(i, (b, c))| (format!("case-{i}"), *b, *c))
                .collect(),
        }
    }

    fn params() -> PolicyV1 {
        PolicyV1 {
            min_trials: 4,
            max_not_evidence_bp: 2000,
            max_asymmetry_bp: 1000,
            case_tolerance_bp: 1000,
            min_effect_bp: 1000,
            alpha_ppm: 50_000,
            max_reruns: 1,
        }
    }

    #[test]
    fn fisher_matches_known_values() {
        // Lady tasting tea: 1/70 and 17/70.
        assert!((fisher_one_sided(4, 0, 0, 4) - 1.0 / 70.0).abs() < 1e-12);
        assert!((fisher_one_sided(3, 1, 1, 3) - 17.0 / 70.0).abs() < 1e-12);
        assert!((fisher_one_sided(0, 4, 4, 0) - 1.0).abs() < 1e-12);
        assert_eq!(fisher_one_sided(0, 0, 0, 0), 1.0);
    }

    #[test]
    fn gates_and_decisions_match_lean() {
        let lean = crate::lean_vocab_test::lean_optimization_cases();
        let policy = PolicyV1 {
            min_trials: lean.params.min_trials,
            max_not_evidence_bp: lean.params.max_not_evidence_bp,
            max_asymmetry_bp: lean.params.max_asymmetry_bp,
            case_tolerance_bp: lean.params.case_tolerance_bp,
            ..params()
        };
        for row in &lean.gates {
            let evidence = Evidence {
                cases_match: row.cases_match,
                cases: row
                    .cases
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        (
                            format!("case-{i}"),
                            t(c.baseline.pass, c.baseline.fail, c.baseline.excluded),
                            t(c.candidate.pass, c.candidate.fail, c.candidate.excluded),
                        )
                    })
                    .collect(),
            };
            assert_eq!(sufficient(&policy, &evidence), row.sufficient, "{}", row.name);
            assert_eq!(
                no_case_regression(&policy, &evidence),
                row.no_case_regression,
                "{}",
                row.name
            );
        }
        for row in &lean.decisions {
            let mode = if row.mode == "improve" { Mode::Improve } else { Mode::Confirm };
            let decision = decide_gates(
                mode,
                Gates {
                    sufficient: row.sufficient,
                    no_case_regression: row.no_case_regression,
                    improves: row.improves,
                },
            );
            let got = match decision {
                Decision::Accept => "accept",
                Decision::Inconclusive => "inconclusive",
                Decision::Reject(RejectReason::CaseRegression) => "reject_case_regression",
                Decision::Reject(RejectReason::NoImprovement) => "reject_no_improvement",
            };
            assert_eq!(got, row.decision, "{row:?}");
        }
    }

    #[test]
    fn clear_improvement_is_accepted_and_a_tie_is_rejected() {
        let better = ev(&[(t(10, 30, 0), t(34, 6, 0)), (t(12, 28, 0), t(30, 10, 0))]);
        assert_eq!(decide(Mode::Improve, &params(), &better), Decision::Accept);
        let tie = ev(&[(t(20, 20, 0), t(20, 20, 0))]);
        assert_eq!(
            decide(Mode::Improve, &params(), &tie),
            Decision::Reject(RejectReason::NoImprovement)
        );
        assert_eq!(decide(Mode::Confirm, &params(), &tie), Decision::Accept);
    }

    #[test]
    fn mismatched_cases_are_inconclusive() {
        let mut baseline = CaseTallies::new();
        baseline.insert("a".into(), t(5, 5, 0));
        let mut candidate = CaseTallies::new();
        candidate.insert("b".into(), t(9, 1, 0));
        let evidence = evidence(&baseline, &candidate);
        assert!(!evidence.cases_match);
        assert_eq!(decide(Mode::Improve, &params(), &evidence), Decision::Inconclusive);
    }

    proptest! {
        /// Turning one candidate pass into a fail never produces an Accept that
        /// was not already there.
        #[test]
        fn losing_a_candidate_pass_never_creates_an_accept(
            bp in 0u64..40, bf in 0u64..40, cp in 1u64..40, cf in 0u64..40,
        ) {
            prop_assume!(bp + bf == cp + cf);
            let before = ev(&[(t(bp, bf, 0), t(cp, cf, 0))]);
            let after = ev(&[(t(bp, bf, 0), t(cp - 1, cf + 1, 0))]);
            for mode in [Mode::Improve, Mode::Confirm] {
                if decide(mode, &params(), &after) == Decision::Accept {
                    prop_assert_eq!(decide(mode, &params(), &before), Decision::Accept);
                }
            }
        }

        #[test]
        fn decide_is_total(
            bp in 0u64..60, bf in 0u64..60, bx in 0u64..60,
            cp in 0u64..60, cf in 0u64..60, cx in 0u64..60, m in any::<bool>(),
        ) {
            let _ = decide(
                if m { Mode::Improve } else { Mode::Confirm },
                &params(),
                &ev(&[(t(bp, bf, bx), t(cp, cf, cx))]),
            );
        }
    }
}
```

- [ ] **Step 2: Implementation**

```rust
use serde::{Deserialize, Serialize};

use super::outcome::{CaseTallies, Tally};

pub const POLICY_VERSION: &str = "v1";

/// Frozen into a job at start. A rule change is a new version, never an edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyV1 {
    pub min_trials: u64,
    pub max_not_evidence_bp: u64,
    pub max_asymmetry_bp: u64,
    pub case_tolerance_bp: u64,
    pub min_effect_bp: u64,
    pub alpha_ppm: u64,
    pub max_reruns: u32,
}

impl PolicyV1 {
    /// Placeholder defaults. They are NOT calibrated; the A/A run in plan 3
    /// sets defensible values for a given evaluator.
    pub fn uncalibrated() -> Self {
        Self {
            min_trials: 20,
            max_not_evidence_bp: 2000,
            max_asymmetry_bp: 1000,
            case_tolerance_bp: 1500,
            min_effect_bp: 1000,
            alpha_ppm: 50_000,
            max_reruns: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// All three gates, on the validation split.
    Improve,
    /// Gates 1 and 2 only, once, on the held-out split.
    Confirm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    CaseRegression,
    NoImprovement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision", content = "reason")]
pub enum Decision {
    Accept,
    Reject(RejectReason),
    Inconclusive,
}

/// Per-case `(case_id, baseline, candidate)` tallies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub cases_match: bool,
    pub cases: Vec<(String, Tally, Tally)>,
}

pub fn evidence(baseline: &CaseTallies, candidate: &CaseTallies) -> Evidence {
    let cases_match = baseline.keys().eq(candidate.keys());
    let cases = baseline
        .iter()
        .filter_map(|(id, b)| candidate.get(id).map(|c| (id.clone(), *b, *c)))
        .collect();
    Evidence { cases_match, cases }
}

fn pool(tallies: impl Iterator<Item = Tally>) -> Tally {
    tallies.fold(Tally::default(), |a, t| Tally {
        pass: a.pass + t.pass,
        fail: a.fail + t.fail,
        excluded: a.excluded + t.excluded,
    })
}

/// Gate 1, `Optimization.sufficient`.
pub fn sufficient(policy: &PolicyV1, evidence: &Evidence) -> bool {
    let b = pool(evidence.cases.iter().map(|c| c.1));
    let c = pool(evidence.cases.iter().map(|c| c.2));
    let (bt, ct) = (b.total() as u128, c.total() as u128);
    let (bx, cx) = (b.excluded as u128, c.excluded as u128);
    evidence.cases_match
        && !evidence.cases.is_empty()
        && evidence.cases.iter().all(|(_, b, c)| {
            b.total() == c.total()
                && policy.min_trials <= b.evidence()
                && policy.min_trials <= c.evidence()
        })
        && bx * 10_000 <= policy.max_not_evidence_bp as u128 * bt
        && cx * 10_000 <= policy.max_not_evidence_bp as u128 * ct
        && (bx * ct).abs_diff(cx * bt) * 10_000 <= policy.max_asymmetry_bp as u128 * (bt * ct)
}

/// Gate 2, `Optimization.noCaseRegression`.
pub fn no_case_regression(policy: &PolicyV1, evidence: &Evidence) -> bool {
    evidence.cases.iter().all(|(_, b, c)| {
        let (bp, be) = (b.pass as u128, b.evidence() as u128);
        let (cp, ce) = (c.pass as u128, c.evidence() as u128);
        bp * ce * 10_000 <= cp * be * 10_000 + policy.case_tolerance_bp as u128 * (be * ce)
    })
}

fn ln_factorials(n: usize) -> Vec<f64> {
    let mut table = vec![0.0f64; n + 1];
    for k in 1..=n {
        table[k] = table[k - 1] + (k as f64).ln();
    }
    table
}

/// One-sided Fisher exact p-value for the 2x2 table
/// `[[cand_pass a, cand_fail b], [base_pass c, base_fail d]]`: the probability
/// of at least `a` candidate passes given the margins.
pub fn fisher_one_sided(a: u64, b: u64, c: u64, d: u64) -> f64 {
    let (a, b, c, d) = (a as usize, b as usize, c as usize, d as usize);
    let (row1, row2, col1, n) = (a + b, c + d, a + c, a + b + c + d);
    if n == 0 {
        return 1.0;
    }
    let lf = ln_factorials(n);
    let ln_choose = |n: usize, k: usize| lf[n] - lf[k] - lf[n - k];
    let denominator = ln_choose(n, col1);
    let upper = row1.min(col1);
    let mut p = 0.0;
    for x in a..=upper {
        if col1 - x > row2 {
            continue;
        }
        p += (ln_choose(row1, x) + ln_choose(row2, col1 - x) - denominator).exp();
    }
    p.min(1.0)
}

/// Gate 3: significant at `alpha_ppm` and at least `min_effect_bp` better. Ties reject.
pub fn improves(policy: &PolicyV1, evidence: &Evidence) -> bool {
    let b = pool(evidence.cases.iter().map(|c| c.1));
    let c = pool(evidence.cases.iter().map(|c| c.2));
    let (bp, be) = (b.pass as u128, b.evidence() as u128);
    let (cp, ce) = (c.pass as u128, c.evidence() as u128);
    if be == 0 || ce == 0 || cp * be <= bp * ce {
        return false;
    }
    let effect_ok = (cp * be - bp * ce) * 10_000 >= policy.min_effect_bp as u128 * (be * ce);
    let p = fisher_one_sided(c.pass, c.fail, b.pass, b.fail);
    effect_ok && p * 1_000_000.0 <= policy.alpha_ppm as f64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gates {
    pub sufficient: bool,
    pub no_case_regression: bool,
    pub improves: bool,
}

pub fn gates(policy: &PolicyV1, evidence: &Evidence) -> Gates {
    Gates {
        sufficient: sufficient(policy, evidence),
        no_case_regression: no_case_regression(policy, evidence),
        improves: improves(policy, evidence),
    }
}

/// `Optimization.decideGates`. The first failing gate decides.
pub fn decide_gates(mode: Mode, gates: Gates) -> Decision {
    if !gates.sufficient {
        Decision::Inconclusive
    } else if !gates.no_case_regression {
        Decision::Reject(RejectReason::CaseRegression)
    } else if mode == Mode::Confirm || gates.improves {
        Decision::Accept
    } else {
        Decision::Reject(RejectReason::NoImprovement)
    }
}

pub fn decide(mode: Mode, policy: &PolicyV1, evidence: &Evidence) -> Decision {
    decide_gates(mode, gates(policy, evidence))
}
```

Add to `mod.rs`:

```rust
pub mod policy;

pub use policy::{
    decide, decide_gates, evidence, gates, Decision, Evidence, Gates, Mode, PolicyV1,
    RejectReason, POLICY_VERSION,
};
```

- [ ] **Step 3: Run**

Run: `RUST_MIN_STACK=16777216 cargo test -p gents --lib optimization::policy`
Expected: all pass. If `gates_and_decisions_match_lean` disagrees on a gate row, the Rust arithmetic differs from `Proofs/Optimization.lean`; align Rust to Lean, do not edit the expected values.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): deterministic three-gate promotion policy v1"
```

### Task 3: Seams, targets, and the ledger flip

**Files:**
- Create: `crates/gents/src/optimization/evaluator.rs`, `proposer.rs`, `target.rs`
- Modify: `crates/gents/src/optimization/mod.rs`, `crates/gents/proofs/Proofs/Conformance/CoverageLedger.lean`

**Interfaces:**
- Produces:
  - `SplitKind { Train, Validation, HeldOut }`, `SplitId` with `SplitId::TRAIN`, `SplitId::VALIDATION`, `pub(in crate::optimization) fn held_out()`, `pub fn kind(&self) -> SplitKind`
  - `EvalRequest { arm, plan: DesiredStateApplyPlan, split: SplitId, trials: u32, trial_offset: u32, deadline_secs: Option<u64> }`
  - `EvalReport { outcomes: Vec<CaseTrialOutcome>, provenance: String }`
  - `trait Evaluator { async fn provenance(&self) -> Result<String>; async fn evaluate(&self, EvalRequest) -> Result<EvalReport>; }`
  - `Rejection { round: u32, text: String, rationale: String, reason: String }`
  - `ProposalInput { current_text: String, train: Vec<CaseTrialOutcome>, rejections: Vec<Rejection> }`, `Proposal { text: String, rationale: String }`, `trait Proposer { async fn propose(&self, ProposalInput) -> Result<Proposal>; }`
  - `TargetField`, `Target { field, owner, id }`, `FrozenDocument { collection, owner, id, digest }`, `Closure = Vec<(Collection, Value)>`
  - `capture_closure(txn, owner) -> Result<Closure>`, `closure_digests(&Closure) -> Result<Vec<FrozenDocument>>`, `current_text(&Closure, &Target) -> Result<String>`, `apply_text(&Closure, &Target, &str) -> Result<Closure>`, `plan_for(&Closure) -> Result<DesiredStateApplyPlan>`, `target_digest(&Closure, &Target) -> Result<String>`, `structural_check(...) -> Result<(), String>`, `MAX_TARGET_TEXT_BYTES: usize = 32 * 1024`.

- [ ] **Step 1: `evaluator.rs`**

```rust
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::outcome::{Arm, CaseTrialOutcome};
use crate::config_client::DesiredStateApplyPlan;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitKind {
    Train,
    Validation,
    HeldOut,
}

/// Opaque split handle. The evaluator owns which cases belong to each split.
/// Only the driver's finalize step can name the held-out split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SplitId(SplitKind);

impl SplitId {
    pub const TRAIN: SplitId = SplitId(SplitKind::Train);
    pub const VALIDATION: SplitId = SplitId(SplitKind::Validation);

    pub(in crate::optimization) fn held_out() -> Self {
        SplitId(SplitKind::HeldOut)
    }

    pub fn kind(&self) -> SplitKind {
        self.0
    }
}

pub struct EvalRequest {
    pub arm: Arm,
    /// The complete configuration to seed into an isolated node.
    pub plan: DesiredStateApplyPlan,
    pub split: SplitId,
    /// Trials per case.
    pub trials: u32,
    /// First trial index, so re-runs append instead of colliding.
    pub trial_offset: u32,
    pub deadline_secs: Option<u64>,
}

pub struct EvalReport {
    pub outcomes: Vec<CaseTrialOutcome>,
    /// Digest over graders, fixtures and cohort. Must equal `provenance()`.
    pub provenance: String,
}

/// The #1515 seam. Rules every implementation must follow: evaluate only in
/// isolation from the live node; classify a deadline as `Fail`; keep the
/// evaluator's own verdict in `raw_kind`; put diagnostic text in `feedback`.
#[async_trait]
pub trait Evaluator: Send + Sync {
    async fn provenance(&self) -> Result<String>;
    async fn evaluate(&self, request: EvalRequest) -> Result<EvalReport>;
}
```

- [ ] **Step 2: `proposer.rs`**

```rust
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::outcome::CaseTrialOutcome;

/// One prior candidate that did not become the checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub round: u32,
    pub text: String,
    pub rationale: String,
    pub reason: String,
}

pub struct ProposalInput {
    pub current_text: String,
    /// Train-split outcomes of the current checkpoint, with feedback.
    pub train: Vec<CaseTrialOutcome>,
    /// Never contains inconclusive or budget-exhausted rounds.
    pub rejections: Vec<Rejection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub text: String,
    pub rationale: String,
}

/// Receives data and returns a value. A proposer never holds a `ConfigAccess`.
#[async_trait]
pub trait Proposer: Send + Sync {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal>;
}
```

- [ ] **Step 3: `target.rs` with tests**

```rust
use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config_client::{
    desired_state_document_digest, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::Collection;

pub const MAX_TARGET_TEXT_BYTES: usize = 32 * 1024;

/// The only fields an optimization job may change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetField {
    AgentContextSystemPrompt,
    TaskPromptTemplate,
}

impl TargetField {
    pub fn collection(&self) -> Collection {
        match self {
            Self::AgentContextSystemPrompt => Collection::AgentContext,
            Self::TaskPromptTemplate => Collection::Task,
        }
    }

    pub fn field_name(&self) -> &'static str {
        match self {
            Self::AgentContextSystemPrompt => "system_prompt",
            Self::TaskPromptTemplate => "prompt_template",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub field: TargetField,
    pub owner: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenDocument {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub digest: String,
}

pub type Closure = Vec<(Collection, Value)>;

/// The owner's full desired configuration: a superset of what the target reaches.
pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure> {
    let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let mut closure: Closure = references
        .documents()
        .map(|((collection, _), value)| (*collection, value.clone()))
        .collect();
    closure.sort_by_key(|(collection, value)| {
        (*collection, document_id(*collection, value).unwrap_or_default())
    });
    Ok(closure)
}

fn document_id(collection: Collection, value: &Value) -> Option<String> {
    value.get(collection.unique_field())?.as_str().map(str::to_owned)
}

fn document_owner(value: &Value) -> Option<String> {
    value.get("agent_did")?.as_str().map(str::to_owned)
}

pub fn closure_digests(closure: &Closure) -> Result<Vec<FrozenDocument>> {
    closure
        .iter()
        .map(|(collection, value)| {
            Ok(FrozenDocument {
                collection: *collection,
                owner: document_owner(value).context("closure document has no agent_did")?,
                id: document_id(*collection, value).context("closure document has no logical ID")?,
                digest: desired_state_document_digest(value)?,
            })
        })
        .collect()
}

fn target_index(closure: &Closure, target: &Target) -> Result<usize> {
    closure
        .iter()
        .position(|(collection, value)| {
            *collection == target.field.collection()
                && document_id(*collection, value).as_deref() == Some(target.id.as_str())
                && document_owner(value).as_deref() == Some(target.owner.as_str())
        })
        .with_context(|| {
            format!(
                "target {} {:?}/{:?} is not in the baseline closure",
                target.field.collection().graphql_type(),
                target.owner,
                target.id
            )
        })
}

pub fn current_text(closure: &Closure, target: &Target) -> Result<String> {
    let (_, value) = &closure[target_index(closure, target)?];
    Ok(value
        .get(target.field.field_name())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// The closure with exactly one field of exactly one document replaced.
pub fn apply_text(closure: &Closure, target: &Target, text: &str) -> Result<Closure> {
    let index = target_index(closure, target)?;
    let mut patched = closure.clone();
    patched[index]
        .1
        .as_object_mut()
        .context("target document is not an object")?
        .insert(target.field.field_name().to_owned(), Value::String(text.to_owned()));
    Ok(patched)
}

pub fn target_digest(closure: &Closure, target: &Target) -> Result<String> {
    desired_state_document_digest(&closure[target_index(closure, target)?].1)
}

pub fn plan_for(closure: &Closure) -> Result<DesiredStateApplyPlan> {
    DesiredStateApplyPlan::new(
        closure
            .iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection: *collection,
                add: value.clone(),
                update: value.clone(),
            })
            .collect(),
    )
}

fn placeholders(template: &str) -> Result<BTreeSet<String>> {
    Ok(crate::template::parse_template_for_validation(template)
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .into_iter()
        .map(|reference| reference.path.join("."))
        .collect())
}

/// The gate before any evaluation spend. `Err` carries diagnostics that are
/// journaled as a rejection. Reference validation against the home node is the
/// caller's job (`validate_desired_state_plan`), because it needs a transaction.
pub fn structural_check(
    closure: &Closure,
    target: &Target,
    text: &str,
    seen_digests: &BTreeSet<String>,
) -> Result<(), String> {
    let check = || -> Result<()> {
        if text.trim().is_empty() {
            bail!("candidate text is empty");
        }
        if text.len() > MAX_TARGET_TEXT_BYTES {
            bail!("candidate text exceeds {MAX_TARGET_TEXT_BYTES} bytes");
        }
        if target.field == TargetField::TaskPromptTemplate {
            let before = placeholders(&current_text(closure, target)?)?;
            let after = placeholders(text)?;
            if before != after {
                bail!("candidate changes the template placeholder set: {before:?} -> {after:?}");
            }
        }
        let patched = apply_text(closure, target, text)?;
        let digest = target_digest(&patched, target)?;
        if seen_digests.contains(&digest) {
            bail!("candidate is identical to the checkpoint or to an earlier candidate");
        }
        plan_for(&patched)?;
        Ok(())
    };
    check().map_err(|error| format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn closure() -> Closure {
        vec![
            (
                Collection::AgentContext,
                json!({"agent_did":"did:key:o","context_id":"ctx","system_prompt":"be careful"}),
            ),
            (
                Collection::Task,
                json!({"agent_did":"did:key:o","task_id":"t","behavior_id":"b",
                       "prompt_template":"Summarize {{ doc.message }}"}),
            ),
        ]
    }

    fn ctx() -> Target {
        Target { field: TargetField::AgentContextSystemPrompt, owner: "did:key:o".into(), id: "ctx".into() }
    }

    fn task() -> Target {
        Target { field: TargetField::TaskPromptTemplate, owner: "did:key:o".into(), id: "t".into() }
    }

    #[test]
    fn apply_text_changes_exactly_one_field() {
        let patched = apply_text(&closure(), &ctx(), "be bold").unwrap();
        assert_eq!(current_text(&patched, &ctx()).unwrap(), "be bold");
        assert_eq!(patched[1], closure()[1]);
        let mut expected = closure()[0].1.clone();
        expected["system_prompt"] = "be bold".into();
        assert_eq!(patched[0].1, expected);
    }

    #[test]
    fn structural_check_rejects_bad_candidates() {
        let mut seen = BTreeSet::new();
        seen.insert(target_digest(&closure(), &ctx()).unwrap());
        assert!(structural_check(&closure(), &ctx(), "be careful", &seen)
            .unwrap_err()
            .contains("identical"));
        assert!(structural_check(&closure(), &ctx(), "  ", &seen).unwrap_err().contains("empty"));
        let huge = "x".repeat(MAX_TARGET_TEXT_BYTES + 1);
        assert!(structural_check(&closure(), &ctx(), &huge, &seen).unwrap_err().contains("exceeds"));
        assert!(structural_check(&closure(), &task(), "Summarize it", &seen)
            .unwrap_err()
            .contains("placeholder"));
        assert!(structural_check(&closure(), &task(), "Shorten {{ doc.message }}", &seen).is_ok());
        assert!(structural_check(&closure(), &ctx(), "be bold", &seen).is_ok());
    }

    #[test]
    fn missing_target_is_an_error() {
        let missing = Target { id: "nope".into(), ..ctx() };
        assert!(current_text(&closure(), &missing).is_err());
    }
}
```

`ConfigApplyTxn` must be nameable from `crate::config_client`. If it is not already re-exported there, add it to the `pub use txn::{...}` list in `config_client/mod.rs`.

- [ ] **Step 4: Re-exports**

Add to `mod.rs`:

```rust
pub mod evaluator;
pub mod proposer;
pub mod target;

pub use evaluator::{EvalReport, EvalRequest, Evaluator, SplitId, SplitKind};
pub use proposer::{Proposal, ProposalInput, Proposer, Rejection};
pub use target::{
    apply_text, capture_closure, closure_digests, current_text, plan_for, structural_check,
    target_digest, Closure, FrozenDocument, Target, TargetField, MAX_TARGET_TEXT_BYTES,
};
```

- [ ] **Step 5: Flip the ledger entry**

In `CoverageLedger.lean`, replace the `optimization_cases` `followUpCoverage` entry with:

```lean
  , tagged (consumerCoverage
      "optimization_cases"
      "OptimizationCases"
      "optimization::policy::tests::gates_and_decisions_match_lean")
      "optimization" [Surface.operatorCli]
```

If Plan 1's contingency was used, also restore the `"optimization"` `featureSurfaceRequirements` entry from Plan 1 Task 4 Step 4.

- [ ] **Step 6: Validate and commit**

Run:
- `cargo test -p gents --lib optimization`
- `cd crates/gents/proofs && lake build`
- `RUST_MIN_STACK=16777216 cargo test -p gents --test conformance`
- `cargo check --workspace --all-targets`

Expected: all succeed.

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): evaluator and proposer seams, target patching, structural gate"
```

PR 4 description: baseline `optimization/03-cas`; new owner `gents::optimization`; no deletions; validation as above.

---

## PR 5: Job record, driver, end-to-end matrix

Branch: `optimization/05-driver`, base `optimization/04-core`.

### Task 4: The `OptimizationJob` collection

**Files:**
- Create: `crates/gents-schemas/schemas/agent/optimization_job.graphql`
- Modify: `crates/gents-schemas/src/lib.rs`, `crates/gents-protocol/src/schemas.rs`, `crates/gents-migration/src/registry.rs`

- [ ] **Step 1: SDL**

```graphql
# Frozen origin and an append-only journal for one configuration optimization
# job (#1455). A driver process's durable notebook: the runtime never claims it.
# Local-only: the journal carries candidate prompts.
type OptimizationJob
    @index(fields: ["owner_agent_did", "job_id"], unique: true) {
    job_id: String @immutable
    owner_agent_did: String @index @immutable
    origin: JSON @immutable
    journal: String
    journal_len: Int
    state: String @index
    created_at: String @index
}
```

- [ ] **Step 2: Register in `gents-schemas/src/lib.rs`**

After the `CALLBACK_INVOCATION` constants add:

```rust
pub const OPTIMIZATION_JOB_NAME: &str = "OptimizationJob";
pub const OPTIMIZATION_JOB: &str = include_str!("../schemas/agent/optimization_job.graphql");
```

Append `OPTIMIZATION_JOB,` as the last element of `ALL` and `OPTIMIZATION_JOB_NAME,` as the last element of `ALL_COLLECTION_NAMES` (the two arrays must stay index-aligned). Add `OPTIMIZATION_JOB_NAME,` to `LOCAL_AUDIT_COLLECTION_NAMES`. Do not add it to `BRANCHABLE_COLLECTION_NAMES`.

- [ ] **Step 3: Mirror in `gents-protocol/src/schemas.rs`**

Add `OPTIMIZATION_JOB, OPTIMIZATION_JOB_NAME,` to the `pub use gents_schemas::{...}` list, then append `OPTIMIZATION_JOB,` to that file's `ALL` and `OPTIMIZATION_JOB_NAME,` to its `ALL_COLLECTION_NAMES`, each as the last element.

- [ ] **Step 4: Run the catalog tests**

Run: `cargo test -p gents-schemas && cargo test -p gents-protocol`
Expected: PASS, including `all_contains_every_agent_schema_file`, `collection_names_align_with_sdl_arrays` and `prompt_bearing_audit_facts_are_classified_and_not_bulk_synced`.

- [ ] **Step 5: Author the baseline pin**

Append to `DEFAULT_BASELINE` in `crates/gents-migration/src/registry.rs`, as the last entry, with a temporary pin:

```rust
    baseline_entry!(
        gents_protocol::schemas::OPTIMIZATION_JOB_NAME,
        gents_protocol::schemas::OPTIMIZATION_JOB,
        "PIN"
    ),
```

Run: `cargo test -p gents-migration --test phase_b_steps canonical_catalog_pins_for_authoring -- --nocapture`
Expected: the test reports the computed root VersionID for `OptimizationJob` as a mismatch against `"PIN"`. Copy the reported `bafyrei...` value over `"PIN"`.

Run: `cargo test -p gents-migration`
Expected: PASS, including `default_baseline_matches_ordered_protocol_catalog` and `default_baseline_covers_every_protocol_collection_once`. If the ordering test fails, move the entry so `DEFAULT_BASELINE` follows the catalog order.

- [ ] **Step 6: Confirm it is not replicated**

Run: `cargo test -p gents-desktop-core the_desktop_does_not_replicate_plaintext_provider_bodies`
Expected: PASS. Then run `grep -n "OptimizationJob" crates/gents/src/agent/p2p_reconcile/templates.rs` and expect no output.

- [ ] **Step 7: Commit**

```bash
git add crates/gents-schemas crates/gents-protocol crates/gents-migration
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(schemas): local-only OptimizationJob collection"
```

### Task 5: Job persistence and journal

**Files:**
- Create: `crates/gents/src/optimization/round.rs`
- Modify: `crates/gents/src/optimization/mod.rs`
- Test: inline `#[cfg(test)]` module using an embedded node

**Interfaces:**
- Consumes: `Target`, `FrozenDocument`, `PolicyV1`, `Decision`, `CaseTallies`, `Usage`, `SplitKind`, `Arm`.
- Produces:
  - `Budgets { max_rounds: u32, max_trials: u64, max_tokens: u64, deadline_unix_secs: Option<u64> }`
  - `JobOrigin { target, closure, policy, policy_version, evaluator_provenance, trials, budgets }`
  - `JobState { Running, ReadyToPromote, NothingToPromote, Exhausted, Failed { reason: String }, Promoted, Stale }`
  - `DriftedRef { collection: String, id: String }`
  - `JournalEntry` (variants below), `JobRecord { job_id, owner, origin, journal }`
  - `derive_state(&[JournalEntry]) -> JobState`, `checkpoint(&[JournalEntry]) -> Option<(String, String)>` returning `(text, candidate_digest)`
  - `create_job`, `load_job`, `append`, `append_in_txn`, and the typed error `JournalConflict` with `journal_conflict(&anyhow::Error) -> bool`.

- [ ] **Step 1: Write `round.rs`**

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::evaluator::SplitKind;
use super::outcome::{Arm, CaseTallies, Usage};
use super::policy::{Decision, PolicyV1};
use super::target::{FrozenDocument, Target};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    pub max_rounds: u32,
    /// Total case-trial outcomes across every evaluation of the job.
    pub max_trials: u64,
    pub max_tokens: u64,
    pub deadline_unix_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOrigin {
    pub target: Target,
    /// Digests of the owner's full desired configuration at freeze.
    pub closure: Vec<FrozenDocument>,
    pub policy: PolicyV1,
    pub policy_version: String,
    pub evaluator_provenance: String,
    pub trials: u32,
    pub budgets: Budgets,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum JobState {
    Running,
    ReadyToPromote,
    NothingToPromote,
    Exhausted,
    Failed { reason: String },
    Promoted,
    Stale,
}

impl JobState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::ReadyToPromote => "ready_to_promote",
            Self::NothingToPromote => "nothing_to_promote",
            Self::Exhausted => "exhausted",
            Self::Failed { .. } => "failed",
            Self::Promoted => "promoted",
            Self::Stale => "stale",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftedRef {
    pub collection: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum JournalEntry {
    Frozen,
    /// Written before every evaluator call; an unmatched one means interruption.
    EvaluationStarted { round: Option<u32>, arm: Arm, split: SplitKind },
    /// Per-case tallies only. Feedback and raw outcomes stay with the evaluator.
    Evaluated {
        round: Option<u32>,
        arm: Arm,
        split: SplitKind,
        tallies: CaseTallies,
        evidence_refs: Vec<String>,
        usage: Usage,
    },
    /// The partial outcomes of the matching `EvaluationStarted` are discarded.
    Interrupted,
    Proposed { round: u32, text: String, rationale: String, candidate_digest: String },
    StructuralReject { round: u32, diagnostics: String },
    /// `baseline` and `candidate` are exactly the inputs to `policy::decide`.
    Decided {
        round: Option<u32>,
        attempt: u32,
        mode: super::policy::Mode,
        baseline: CaseTallies,
        candidate: CaseTallies,
        decision: Decision,
        policy_version: String,
    },
    /// A candidate that was never evaluated is not a rejection.
    BudgetExhausted { round: Option<u32> },
    Finalized { state: JobState },
    Promoted { by: String, target_digest: String },
    PromotionRefused { drifted: Vec<DriftedRef> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRecord {
    pub job_id: String,
    pub owner: String,
    pub origin: JobOrigin,
    pub journal: Vec<JournalEntry>,
}

pub fn derive_state(journal: &[JournalEntry]) -> JobState {
    let mut state = JobState::Running;
    for entry in journal {
        match entry {
            JournalEntry::Finalized { state: finalized } => state = finalized.clone(),
            JournalEntry::Promoted { .. } => state = JobState::Promoted,
            JournalEntry::PromotionRefused { .. } => state = JobState::Stale,
            _ => {}
        }
    }
    state
}

/// The retained checkpoint: the text of the last round whose improve decision
/// was `Accept`. Never the best candidate ever seen.
pub fn checkpoint(journal: &[JournalEntry]) -> Option<(String, String)> {
    let mut accepted = None;
    for entry in journal {
        if let JournalEntry::Decided { round: Some(round), decision: Decision::Accept, .. } = entry {
            accepted = journal.iter().find_map(|candidate| match candidate {
                JournalEntry::Proposed { round: r, text, candidate_digest, .. } if r == round => {
                    Some((text.clone(), candidate_digest.clone()))
                }
                _ => None,
            });
        }
    }
    accepted
}

#[derive(Debug)]
pub struct JournalConflict;

impl std::fmt::Display for JournalConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("optimization job journal moved; another writer appended first")
    }
}

impl std::error::Error for JournalConflict {}

pub fn journal_conflict(error: &anyhow::Error) -> bool {
    error.downcast_ref::<JournalConflict>().is_some()
}

fn filter(owner: &str, job_id: &str) -> String {
    format!(
        r#"owner_agent_did: {{ _eq: "{}" }}, job_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(job_id)
    )
}

pub async fn create_job(
    access: &ConfigAccess,
    job_id: &str,
    owner: &str,
    origin: &JobOrigin,
) -> Result<JobRecord> {
    let input = json!({
        "job_id": job_id,
        "owner_agent_did": owner,
        "origin": serde_json::to_value(origin)?,
        "journal": "[]",
        "journal_len": 0,
        "state": JobState::Running.label(),
        "created_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    let variables = json!({ "input": input });
    access
        .transact("optimization.create_job", |txn| {
            let variables = &variables;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: OptimizationJobMutationInputArg!) { create_OptimizationJob(input: $input) { _docID } }",
                    variables,
                )
                .await
            })
        })
        .await
        .context("create_OptimizationJob")?;
    tracing::info!(job_id, owner, "optimization job created");
    Ok(JobRecord {
        job_id: job_id.to_owned(),
        owner: owner.to_owned(),
        origin: origin.clone(),
        journal: Vec::new(),
    })
}

fn decode(row: &Value) -> Result<JobRecord> {
    Ok(JobRecord {
        job_id: row["job_id"].as_str().context("job_id")?.to_owned(),
        owner: row["owner_agent_did"].as_str().context("owner_agent_did")?.to_owned(),
        origin: serde_json::from_value(row["origin"].clone()).context("decoding job origin")?,
        journal: serde_json::from_str(row["journal"].as_str().unwrap_or("[]"))
            .context("decoding job journal")?,
    })
}

async fn load_in_txn(txn: &ConfigApplyTxn<'_>, owner: &str, job_id: &str) -> Result<Option<JobRecord>> {
    let response = txn
        .execute(&format!(
            "{{ OptimizationJob(filter: {{ {} }}) {{ job_id owner_agent_did origin journal journal_len state }} }}",
            filter(owner, job_id)
        ))
        .await?;
    response["data"]["OptimizationJob"]
        .as_array()
        .and_then(|rows| rows.first())
        .map(decode)
        .transpose()
}

pub async fn load_job(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<Option<JobRecord>> {
    let (owner, job_id) = (owner.to_owned(), job_id.to_owned());
    access
        .transact("optimization.load_job", |txn| {
            let (owner, job_id) = (owner.clone(), job_id.clone());
            Box::pin(async move { load_in_txn(txn, &owner, &job_id).await })
        })
        .await
}

/// `Optimization.appendIf`: the update matches only while `journal_len` still
/// equals the length this writer read.
pub async fn append_in_txn(
    txn: &ConfigApplyTxn<'_>,
    job: &JobRecord,
    entry: &JournalEntry,
) -> Result<()> {
    let mut journal = job.journal.clone();
    journal.push(entry.clone());
    let variables = json!({ "input": {
        "journal": serde_json::to_string(&journal)?,
        "journal_len": journal.len(),
        "state": derive_state(&journal).label(),
    }});
    let response = txn
        .execute_with_variables(
            &format!(
                "mutation($input: OptimizationJobMutationInputArg!) {{ update_OptimizationJob(filter: {{ {}, journal_len: {{ _eq: {} }} }}, input: $input) {{ _docID }} }}",
                filter(&job.owner, &job.job_id),
                job.journal.len()
            ),
            &variables,
        )
        .await?;
    let matched = response["data"]["update_OptimizationJob"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty());
    if matched {
        Ok(())
    } else {
        Err(anyhow::Error::new(JournalConflict))
    }
}

pub async fn append(access: &ConfigAccess, job: &mut JobRecord, entry: JournalEntry) -> Result<()> {
    {
        let (job_ref, entry_ref) = (&*job, &entry);
        access
            .transact("optimization.append_journal", |txn| {
                Box::pin(async move { append_in_txn(txn, job_ref, entry_ref).await })
            })
            .await?;
    }
    job.journal.push(entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defra_node::EmbeddedNode;
    use crate::optimization::target::TargetField;
    use std::sync::Arc;

    async fn access() -> ConfigAccess {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        ConfigAccess::Local(node)
    }

    fn origin() -> JobOrigin {
        JobOrigin {
            target: Target {
                field: TargetField::AgentContextSystemPrompt,
                owner: "did:key:o".into(),
                id: "ctx".into(),
            },
            closure: Vec::new(),
            policy: PolicyV1::uncalibrated(),
            policy_version: "v1".into(),
            evaluator_provenance: "sha256:eval".into(),
            trials: 4,
            budgets: Budgets { max_rounds: 2, max_trials: 1000, max_tokens: 0, deadline_unix_secs: None },
        }
    }

    #[tokio::test]
    async fn create_append_and_reload_roundtrip() {
        let access = access().await;
        let mut job = create_job(&access, "job-1", "did:key:o", &origin()).await.unwrap();
        append(&access, &mut job, JournalEntry::Frozen).await.unwrap();
        let loaded = load_job(&access, "did:key:o", "job-1").await.unwrap().unwrap();
        assert_eq!(loaded, job);
        assert_eq!(derive_state(&loaded.journal), JobState::Running);
    }

    #[tokio::test]
    async fn a_stale_writer_cannot_append() {
        let access = access().await;
        let mut job = create_job(&access, "job-2", "did:key:o", &origin()).await.unwrap();
        let mut stale = job.clone();
        append(&access, &mut job, JournalEntry::Frozen).await.unwrap();
        let error = append(&access, &mut stale, JournalEntry::Interrupted).await.unwrap_err();
        assert!(journal_conflict(&error), "{error:#}");
        let loaded = load_job(&access, "did:key:o", "job-2").await.unwrap().unwrap();
        assert_eq!(loaded.journal, vec![JournalEntry::Frozen]);
    }

    #[test]
    fn checkpoint_is_the_last_accepted_round_not_the_best() {
        let decided = |round, decision| JournalEntry::Decided {
            round: Some(round),
            attempt: 0,
            mode: crate::optimization::Mode::Improve,
            baseline: CaseTallies::new(),
            candidate: CaseTallies::new(),
            decision,
            policy_version: "v1".into(),
        };
        let proposed = |round, text: &str| JournalEntry::Proposed {
            round,
            text: text.into(),
            rationale: String::new(),
            candidate_digest: format!("d{round}"),
        };
        let journal = vec![
            proposed(1, "one"),
            decided(1, Decision::Accept),
            proposed(2, "two"),
            decided(2, Decision::Reject(crate::optimization::RejectReason::NoImprovement)),
        ];
        assert_eq!(checkpoint(&journal), Some(("one".into(), "d1".into())));
        assert_eq!(checkpoint(&journal[..1]), None);
    }
}
```

If `crate::schema::ensure_runtime_schemas` does not register the `ALL` catalog (it may only cover `RUNTIME_ALL`), register `gents_protocol::schemas::OPTIMIZATION_JOB` explicitly in the test helper with `node.add_schema(gents_protocol::schemas::OPTIMIZATION_JOB).await.unwrap();`.

- [ ] **Step 2: Re-export, run, commit**

Add to `mod.rs`:

```rust
pub mod round;

pub use round::{
    append, checkpoint, create_job, derive_state, journal_conflict, load_job, Budgets, DriftedRef,
    JobOrigin, JobRecord, JobState, JournalEntry,
};
```

Run: `cargo test -p gents --lib optimization::round`
Expected: 3 passed.

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): OptimizationJob record with a length-guarded journal"
```

### Task 6: The driver

**Files:**
- Create: `crates/gents/src/optimization/driver.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes: everything above, plus `crate::config_client::validate_desired_state_plan` (make it `pub` if it is `pub(crate)`; it is in the same crate, so `pub(crate)` already suffices).
- Produces: `JobSpec { job_id, owner, target, policy, trials, budgets }`, `Driver::new(&ConfigAccess, &dyn Evaluator, &dyn Proposer)`, `Driver::run(&self, JobSpec) -> Result<JobRecord>`.

Behavior, in order:

1. Load the job, or create it: capture the closure in a transaction, record its digests and the evaluator provenance in `origin`, append `Frozen`.
2. Re-capture the closure and compare digests with `origin`. A difference finalizes the job as `Failed { reason: "baseline_drifted" }`.
3. If the last journal entry is an `EvaluationStarted`, append `Interrupted`.
4. Replay the journal into a `Progress` value, then continue.
5. Every evaluator call goes through `evaluate()`: budget check, `EvaluationStarted`, call, provenance check, `Evaluated`.
6. Finalize as the spec describes.

- [ ] **Step 1: Write `driver.rs`**

```rust
use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};

use super::evaluator::{EvalRequest, Evaluator, SplitId};
use super::outcome::{merge_tallies, tally_by_case, Arm, CaseTallies, CaseTrialOutcome, Usage};
use super::policy::{decide, evidence, Decision, Mode, PolicyV1, POLICY_VERSION};
use super::proposer::{ProposalInput, Proposer, Rejection};
use super::round::{
    append, checkpoint, create_job, derive_state, load_job, Budgets, JobOrigin, JobRecord,
    JobState, JournalEntry,
};
use super::target::{
    apply_text, capture_closure, closure_digests, current_text, plan_for, structural_check,
    target_digest, Closure, Target,
};
use crate::config_client::ConfigAccess;

pub struct JobSpec {
    pub job_id: String,
    pub owner: String,
    pub target: Target,
    pub policy: PolicyV1,
    pub trials: u32,
    pub budgets: Budgets,
}

pub struct Driver<'a> {
    access: &'a ConfigAccess,
    evaluator: &'a dyn Evaluator,
    proposer: &'a dyn Proposer,
}

/// Everything the driver needs to continue, rebuilt from the journal alone.
#[derive(Default)]
struct Progress {
    original_validation: Option<CaseTallies>,
    checkpoint_validation: Option<CaseTallies>,
    rounds_started: u32,
    open_round: Option<OpenRound>,
    rejections: Vec<Rejection>,
    seen_digests: BTreeSet<String>,
    trials_spent: u64,
    tokens_spent: u64,
    per_arm_estimate: u64,
}

struct OpenRound {
    round: u32,
    text: String,
    rationale: String,
    baseline: CaseTallies,
    candidate: Option<CaseTallies>,
    attempts: u32,
}

fn replay(journal: &[JournalEntry], max_reruns: u32) -> Progress {
    let mut progress = Progress::default();
    for entry in journal {
        match entry {
            JournalEntry::Evaluated { round, arm, split, tallies, usage, .. } => {
                let outcomes: u64 = tallies.values().map(|t| t.total()).sum();
                progress.trials_spent += outcomes;
                progress.tokens_spent += usage.total();
                if *split != super::evaluator::SplitKind::Validation {
                    continue;
                }
                match (round, arm) {
                    (None, Arm::Baseline) => {
                        progress.per_arm_estimate = outcomes;
                        progress.original_validation = Some(tallies.clone());
                        progress.checkpoint_validation = Some(tallies.clone());
                    }
                    (Some(r), arm) => {
                        if let Some(open) = progress.open_round.as_mut().filter(|o| o.round == *r) {
                            match arm {
                                Arm::Baseline => merge_tallies(&mut open.baseline, tallies),
                                Arm::Candidate => match open.candidate.as_mut() {
                                    Some(existing) => merge_tallies(existing, tallies),
                                    None => open.candidate = Some(tallies.clone()),
                                },
                            }
                        }
                    }
                    _ => {}
                }
            }
            JournalEntry::Proposed { round, text, rationale, candidate_digest } => {
                progress.rounds_started = *round;
                progress.seen_digests.insert(candidate_digest.clone());
                progress.open_round = Some(OpenRound {
                    round: *round,
                    text: text.clone(),
                    rationale: rationale.clone(),
                    baseline: progress.checkpoint_validation.clone().unwrap_or_default(),
                    candidate: None,
                    attempts: 0,
                });
            }
            JournalEntry::StructuralReject { round, diagnostics } => {
                if let Some(open) = progress.open_round.take().filter(|o| o.round == *round) {
                    progress.rejections.push(Rejection {
                        round: *round,
                        text: open.text,
                        rationale: open.rationale,
                        reason: format!("structural: {diagnostics}"),
                    });
                }
            }
            JournalEntry::Decided { round: Some(round), attempt, decision, candidate, .. } => {
                let Some(open) = progress.open_round.as_mut().filter(|o| o.round == *round) else {
                    continue;
                };
                open.attempts = attempt + 1;
                match decision {
                    Decision::Accept => {
                        progress.checkpoint_validation = Some(candidate.clone());
                        progress.open_round = None;
                    }
                    Decision::Reject(reason) => {
                        let open = progress.open_round.take().expect("open round");
                        progress.rejections.push(Rejection {
                            round: *round,
                            text: open.text,
                            rationale: open.rationale,
                            reason: format!("{reason:?}"),
                        });
                    }
                    // Stays open for a re-run until `max_reruns` is used up (see below).
                    Decision::Inconclusive => {}
                }
            }
            JournalEntry::BudgetExhausted { .. } => progress.open_round = None,
            _ => {}
        }
    }
    // A final inconclusive closes its round. It is never a rejection.
    if progress.open_round.as_ref().is_some_and(|open| open.attempts > max_reruns) {
        progress.open_round = None;
    }
    progress
}

impl<'a> Driver<'a> {
    pub fn new(
        access: &'a ConfigAccess,
        evaluator: &'a dyn Evaluator,
        proposer: &'a dyn Proposer,
    ) -> Self {
        Self { access, evaluator, proposer }
    }

    async fn closure(&self, owner: &str) -> Result<Closure> {
        let owner = owner.to_owned();
        self.access
            .transact("optimization.capture_closure", |txn| {
                let owner = owner.clone();
                Box::pin(async move { capture_closure(txn, &owner).await })
            })
            .await
    }

    async fn references_valid(&self, closure: &Closure) -> Result<()> {
        let plan = plan_for(closure)?;
        self.access
            .transact("optimization.validate_candidate", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::config_client::validate_desired_state_plan(txn, plan).await
                })
            })
            .await
    }

    fn budget_allows(origin: &JobOrigin, progress: &Progress, arms: u64, reserve: bool) -> bool {
        let budgets = &origin.budgets;
        let estimate = progress.per_arm_estimate.max(1);
        let reserved = if reserve { 2 * estimate } else { 0 };
        let deadline_ok = budgets.deadline_unix_secs.is_none_or(|deadline| {
            (chrono::Utc::now().timestamp().max(0) as u64) < deadline
        });
        let tokens_ok = budgets.max_tokens == 0 || progress.tokens_spent < budgets.max_tokens;
        deadline_ok
            && tokens_ok
            && progress.trials_spent + arms * estimate + reserved <= budgets.max_trials
    }

    /// The single path to the evaluator.
    #[allow(clippy::too_many_arguments)]
    async fn evaluate(
        &self,
        job: &mut JobRecord,
        progress: &mut Progress,
        round: Option<u32>,
        arm: Arm,
        split: SplitId,
        closure: &Closure,
        trial_offset: u32,
    ) -> Result<(CaseTallies, Vec<CaseTrialOutcome>)> {
        append(
            self.access,
            job,
            JournalEntry::EvaluationStarted { round, arm, split: split.kind() },
        )
        .await?;
        let report = self
            .evaluator
            .evaluate(EvalRequest {
                arm,
                plan: plan_for(closure)?,
                split,
                trials: job.origin.trials,
                trial_offset,
                deadline_secs: None,
            })
            .await?;
        if report.provenance != job.origin.evaluator_provenance {
            let state = JobState::Failed { reason: "evaluator_changed".into() };
            append(self.access, job, JournalEntry::Finalized { state }).await?;
            bail!("evaluator provenance changed mid-job");
        }
        let mut outcomes = report.outcomes;
        if split.kind() != super::evaluator::SplitKind::Train {
            // Rule 4 of the seam: feedback never leaves the train split.
            for outcome in &mut outcomes {
                outcome.feedback = None;
            }
        }
        let tallies = tally_by_case(arm, &outcomes);
        let usage = outcomes.iter().fold(Usage::default(), |acc, o| Usage {
            input_tokens: acc.input_tokens + o.usage.input_tokens,
            output_tokens: acc.output_tokens + o.usage.output_tokens,
        });
        let count = outcomes.len() as u64;
        append(
            self.access,
            job,
            JournalEntry::Evaluated {
                round,
                arm,
                split: split.kind(),
                tallies: tallies.clone(),
                evidence_refs: outcomes.iter().map(|o| o.evidence_ref.clone()).collect(),
                usage,
            },
        )
        .await?;
        progress.trials_spent += count;
        progress.tokens_spent += usage.total();
        Ok((tallies, outcomes))
    }

    pub async fn run(&self, spec: JobSpec) -> Result<JobRecord> {
        let closure = self.closure(&spec.owner).await?;
        let mut job = match load_job(self.access, &spec.owner, &spec.job_id).await? {
            Some(job) => job,
            None => {
                current_text(&closure, &spec.target)?;
                let origin = JobOrigin {
                    target: spec.target.clone(),
                    closure: closure_digests(&closure)?,
                    policy: spec.policy.clone(),
                    policy_version: POLICY_VERSION.to_owned(),
                    evaluator_provenance: self.evaluator.provenance().await?,
                    trials: spec.trials,
                    budgets: spec.budgets.clone(),
                };
                let mut job = create_job(self.access, &spec.job_id, &spec.owner, &origin).await?;
                append(self.access, &mut job, JournalEntry::Frozen).await?;
                job
            }
        };
        if derive_state(&job.journal) != JobState::Running {
            return Ok(job);
        }
        if closure_digests(&closure)? != job.origin.closure {
            let state = JobState::Failed { reason: "baseline_drifted".into() };
            append(self.access, &mut job, JournalEntry::Finalized { state }).await?;
            return Ok(job);
        }
        if matches!(job.journal.last(), Some(JournalEntry::EvaluationStarted { .. })) {
            append(self.access, &mut job, JournalEntry::Interrupted).await?;
        }

        let target = job.origin.target.clone();
        let policy = job.origin.policy.clone();
        let mut progress = replay(&job.journal, policy.max_reruns);
        progress.seen_digests.insert(target_digest(&closure, &target)?);

        if progress.original_validation.is_none() {
            let (tallies, _) = self
                .evaluate(&mut job, &mut progress, None, Arm::Baseline, SplitId::VALIDATION, &closure, 0)
                .await?;
            progress.per_arm_estimate = tallies.values().map(|t| t.total()).sum();
            progress.original_validation = Some(tallies.clone());
            progress.checkpoint_validation = Some(tallies);
        }

        let mut exhausted = false;
        loop {
            let checkpoint_text = checkpoint(&job.journal)
                .map(|(text, _)| text)
                .unwrap_or(current_text(&closure, &target)?);
            let checkpoint_closure = apply_text(&closure, &target, &checkpoint_text)?;

            if progress.open_round.is_none() {
                if progress.rounds_started >= job.origin.budgets.max_rounds {
                    break;
                }
                // One train arm plus one candidate arm.
                if !Self::budget_allows(&job.origin, &progress, 2, true) {
                    append(self.access, &mut job, JournalEntry::BudgetExhausted { round: None }).await?;
                    exhausted = true;
                    break;
                }
                let round = progress.rounds_started + 1;
                let (_, train) = self
                    .evaluate(&mut job, &mut progress, Some(round), Arm::Baseline, SplitId::TRAIN,
                              &checkpoint_closure, 0)
                    .await?;
                let proposal = self
                    .proposer
                    .propose(ProposalInput {
                        current_text: checkpoint_text.clone(),
                        train,
                        rejections: progress.rejections.clone(),
                    })
                    .await
                    .context("proposer failed")?;
                let candidate_digest = apply_text(&closure, &target, &proposal.text)
                    .and_then(|patched| target_digest(&patched, &target))
                    .unwrap_or_else(|_| "invalid".into());
                let mut seen = progress.seen_digests.clone();
                if let Some((_, digest)) = checkpoint(&job.journal) {
                    seen.insert(digest);
                }
                let structural = structural_check(&closure, &target, &proposal.text, &seen);
                append(self.access, &mut job, JournalEntry::Proposed {
                    round,
                    text: proposal.text.clone(),
                    rationale: proposal.rationale.clone(),
                    candidate_digest,
                }).await?;
                let structural = match structural {
                    Ok(()) => self
                        .references_valid(&apply_text(&closure, &target, &proposal.text)?)
                        .await
                        .map_err(|error| format!("{error:#}")),
                    Err(diagnostics) => Err(diagnostics),
                };
                if let Err(diagnostics) = structural {
                    append(self.access, &mut job, JournalEntry::StructuralReject { round, diagnostics })
                        .await?;
                }
                progress = replay(&job.journal, policy.max_reruns);
                progress.seen_digests.insert(target_digest(&closure, &target)?);
                continue;
            }

            let (round, text, attempts) = {
                let open = progress.open_round.as_ref().expect("open round");
                (open.round, open.text.clone(), open.attempts)
            };
            let candidate_closure = apply_text(&closure, &target, &text)?;
            let offset = attempts * job.origin.trials;
            let needs_candidate =
                progress.open_round.as_ref().is_some_and(|o| o.candidate.is_none()) || attempts > 0;
            if needs_candidate {
                let arms = if attempts > 0 { 2 } else { 1 };
                if !Self::budget_allows(&job.origin, &progress, arms, true) {
                    append(self.access, &mut job, JournalEntry::BudgetExhausted { round: Some(round) })
                        .await?;
                    exhausted = true;
                    break;
                }
                if attempts > 0 {
                    // A re-run appends trials to both arms equally.
                    self.evaluate(&mut job, &mut progress, Some(round), Arm::Baseline,
                                  SplitId::VALIDATION, &checkpoint_closure, offset).await?;
                }
                self.evaluate(&mut job, &mut progress, Some(round), Arm::Candidate,
                              SplitId::VALIDATION, &candidate_closure, offset).await?;
                progress = replay(&job.journal, policy.max_reruns);
                progress.seen_digests.insert(target_digest(&closure, &target)?);
            }
            let open = progress.open_round.as_ref().expect("open round");
            let candidate = open.candidate.clone().unwrap_or_default();
            let decision = decide(Mode::Improve, &policy, &evidence(&open.baseline, &candidate));
            append(self.access, &mut job, JournalEntry::Decided {
                round: Some(round),
                attempt: attempts,
                mode: Mode::Improve,
                baseline: open.baseline.clone(),
                candidate,
                decision,
                policy_version: job.origin.policy_version.clone(),
            }).await?;
            tracing::info!(job_id = %job.job_id, round, attempt = attempts, ?decision, "optimization round decided");
            progress = replay(&job.journal, policy.max_reruns);
            progress.seen_digests.insert(target_digest(&closure, &target)?);
        }

        let state = match checkpoint(&job.journal) {
            None if exhausted => JobState::Exhausted,
            None => JobState::NothingToPromote,
            Some((text, _)) => {
                let original = self
                    .evaluate(&mut job, &mut progress, None, Arm::Baseline, SplitId::held_out(), &closure, 0)
                    .await?
                    .0;
                let confirmed_closure = apply_text(&closure, &target, &text)?;
                let confirmed = self
                    .evaluate(&mut job, &mut progress, None, Arm::Candidate, SplitId::held_out(),
                              &confirmed_closure, 0)
                    .await?
                    .0;
                let decision = decide(Mode::Confirm, &policy, &evidence(&original, &confirmed));
                append(self.access, &mut job, JournalEntry::Decided {
                    round: None,
                    attempt: 0,
                    mode: Mode::Confirm,
                    baseline: original,
                    candidate: confirmed,
                    decision,
                    policy_version: job.origin.policy_version.clone(),
                }).await?;
                match decision {
                    Decision::Accept => JobState::ReadyToPromote,
                    Decision::Inconclusive => {
                        JobState::Failed { reason: "held_out_inconclusive".into() }
                    }
                    Decision::Reject(_) => JobState::Failed { reason: "held_out_regression".into() },
                }
            }
        };
        append(self.access, &mut job, JournalEntry::Finalized { state }).await?;
        Ok(job)
    }
}
```

`replay` is the only place a round is opened or closed, so a resumed run and an uninterrupted run reach the same state from the same journal.

- [ ] **Step 2: Re-export and check**

Add to `mod.rs`:

```rust
pub mod driver;

pub use driver::{Driver, JobSpec};
```

Run: `cargo check -p gents`
Expected: success. Fix borrow errors by cloning small values; do not restructure the control flow.

- [ ] **Step 3: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): journal-replaying round driver"
```

### Task 7: Scripted doubles and the end-to-end matrix

**Files:**
- Create: `crates/gents/tests/support/optimization.rs`
- Create: `crates/gents/tests/e2e_runtime/optimization_driver.rs`
- Modify: `crates/gents/tests/support/mod.rs` (add `pub mod optimization;`), `crates/gents/tests/e2e_runtime.rs`

**Interfaces:**
- Produces, for Plan 3 too: `ScriptedEvaluator`, `ScriptedProposer`, `seed_config(&TestDb) -> (ConfigAccess, Target)`, `spec(job_id, target, max_rounds) -> JobSpec`, `OWNER`.

The scripted evaluator derives every outcome from the candidate text, so it is deterministic and survives resume. Text markers: `GOOD` passes 9 of 10, `BAD` passes 1 of 10, `REGRESS-B` passes case `a` always and case `b` never, `FLAKY` returns `NotEvidence` for half its trials, `HELDOUT-BAD` behaves like `GOOD` except on the held-out split where it passes 1 of 10. Anything else passes 5 of 10.

- [ ] **Step 1: `tests/support/optimization.rs`**

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Result};
use async_trait::async_trait;
use gents::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::optimization::{
    Arm, Budgets, CaseTrialOutcome, EvalReport, EvalRequest, Evaluator, JobSpec, OutcomeClass,
    PolicyV1, Proposal, ProposalInput, Proposer, SplitKind, Target, TargetField, Usage,
};
use gents::Collection;
use serde_json::json;

use super::TestDb;

pub const OWNER: &str = "did:key:optimization-owner";
pub const BASELINE_PROMPT: &str = "You are a careful monitor.";

pub async fn seed_config(db: &TestDb) -> (ConfigAccess, Target) {
    let access = ConfigAccess::Local(db.node.clone());
    let documents = vec![
        (Collection::AgentContext,
         json!({"agent_did": OWNER, "context_id": "monitor-context", "system_prompt": BASELINE_PROMPT})),
        (Collection::InferenceBackend,
         json!({"agent_did": OWNER, "backend_id": "local", "name": "Local",
                "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:8000/v1",
                "auth": {"kind": "unauthenticated"}})),
    ];
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact("test.optimization.seed", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
    let target = Target {
        field: TargetField::AgentContextSystemPrompt,
        owner: OWNER.into(),
        id: "monitor-context".into(),
    };
    (access, target)
}

pub fn policy() -> PolicyV1 {
    PolicyV1 {
        min_trials: 10,
        max_not_evidence_bp: 2000,
        max_asymmetry_bp: 1000,
        case_tolerance_bp: 1500,
        min_effect_bp: 1000,
        alpha_ppm: 50_000,
        max_reruns: 1,
    }
}

pub fn spec(job_id: &str, target: Target, max_rounds: u32) -> JobSpec {
    JobSpec {
        job_id: job_id.into(),
        owner: OWNER.into(),
        target,
        policy: policy(),
        trials: 20,
        budgets: Budgets { max_rounds, max_trials: 100_000, max_tokens: 0, deadline_unix_secs: None },
    }
}

pub struct ScriptedEvaluator {
    pub provenance: Mutex<String>,
    pub calls: Mutex<Vec<(Arm, SplitKind)>>,
    /// Fail the Nth evaluate call (1-based) once, to simulate a crash.
    pub fail_on_call: Option<usize>,
    count: AtomicUsize,
}

impl ScriptedEvaluator {
    pub fn new() -> Self {
        Self {
            provenance: Mutex::new("sha256:scripted-v1".into()),
            calls: Mutex::new(Vec::new()),
            fail_on_call: None,
            count: AtomicUsize::new(0),
        }
    }

    pub fn failing_on(call: usize) -> Self {
        Self { fail_on_call: Some(call), ..Self::new() }
    }

    fn class(text: &str, case: &str, split: SplitKind, trial: u32) -> OutcomeClass {
        let slot = trial % 10;
        let pass_upto = |n: u32| if slot < n { OutcomeClass::Pass } else { OutcomeClass::Fail };
        if text.contains("FLAKY") && trial % 2 == 0 {
            return OutcomeClass::NotEvidence;
        }
        if text.contains("REGRESS-B") {
            return if case == "a" { OutcomeClass::Pass } else { OutcomeClass::Fail };
        }
        if text.contains("HELDOUT-BAD") {
            return pass_upto(if split == SplitKind::HeldOut { 1 } else { 9 });
        }
        if text.contains("GOOD") {
            pass_upto(9)
        } else if text.contains("BAD") {
            pass_upto(1)
        } else {
            pass_upto(5)
        }
    }
}

#[async_trait]
impl Evaluator for ScriptedEvaluator {
    async fn provenance(&self) -> Result<String> {
        Ok(self.provenance.lock().unwrap().clone())
    }

    async fn evaluate(&self, request: EvalRequest) -> Result<EvalReport> {
        let call = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        self.calls.lock().unwrap().push((request.arm, request.split.kind()));
        if self.fail_on_call == Some(call) {
            bail!("scripted evaluator crash on call {call}");
        }
        let text = request
            .plan
            .documents()
            .iter()
            .find(|document| document.collection == Collection::AgentContext)
            .and_then(|document| document.add["system_prompt"].as_str())
            .unwrap_or_default()
            .to_owned();
        let mut outcomes = Vec::new();
        for case in ["a", "b"] {
            for trial in request.trial_offset..request.trial_offset + request.trials {
                outcomes.push(CaseTrialOutcome {
                    case_id: case.into(),
                    trial,
                    class: Self::class(&text, case, request.split.kind(), trial),
                    raw_kind: "scripted".into(),
                    // Deliberately set on every split: the driver must strip it.
                    feedback: Some(format!("feedback for {case}")),
                    evidence_ref: format!("scripted://{case}/{trial}"),
                    usage: Usage { input_tokens: 10, output_tokens: 5 },
                });
            }
        }
        Ok(EvalReport { outcomes, provenance: self.provenance.lock().unwrap().clone() })
    }
}

/// Returns its texts in order, one per round. Records what it was given.
pub struct ScriptedProposer {
    texts: Vec<String>,
    next: AtomicUsize,
    pub inputs: Mutex<Vec<(usize, usize, bool)>>,
}

impl ScriptedProposer {
    pub fn new(texts: &[&str]) -> Self {
        Self {
            texts: texts.iter().map(|text| (*text).to_owned()).collect(),
            next: AtomicUsize::new(0),
            inputs: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Proposer for ScriptedProposer {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal> {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push((
            input.train.len(),
            input.rejections.len(),
            input.train.iter().all(|outcome| outcome.feedback.is_some()),
        ));
        let text = self.texts.get(index).cloned().unwrap_or_else(|| format!("filler {index}"));
        Ok(Proposal { text, rationale: format!("scripted proposal {index}") })
    }
}
```

- [ ] **Step 2: The matrix, `tests/e2e_runtime/optimization_driver.rs`**

```rust
use gents::optimization::{
    checkpoint, derive_state, Arm, Decision, Driver, JobRecord, JobState, JournalEntry,
    RejectReason, SplitKind,
};

use crate::support::optimization::{seed_config, spec, ScriptedEvaluator, ScriptedProposer};
use crate::support::{test_db, TestDb};

/// The returned `TestDb` owns the node's temporary directory.
async fn run(
    name: &str,
    evaluator: &ScriptedEvaluator,
    proposer: &ScriptedProposer,
    max_rounds: u32,
) -> (TestDb, JobRecord) {
    let db = test_db(name).await;
    let (access, target) = seed_config(&db).await;
    let job = Driver::new(&access, evaluator, proposer)
        .run(spec(name, target, max_rounds))
        .await
        .unwrap();
    (db, job)
}

fn decisions(job: &JobRecord) -> Vec<(Option<u32>, Decision)> {
    job.journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::Decided { round, decision, .. } => Some((*round, *decision)),
            _ => None,
        })
        .collect()
}

pub(super) async fn accepts_a_clear_improvement() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["GOOD prompt"]));
    let (_, job) = run("opt-accept", &evaluator, &proposer, 1).await;
    assert_eq!(derive_state(&job.journal), JobState::ReadyToPromote);
    assert_eq!(checkpoint(&job.journal).unwrap().0, "GOOD prompt");
    assert_eq!(decisions(&job), vec![(Some(1), Decision::Accept), (None, Decision::Accept)]);
}

pub(super) async fn rejects_no_improvement_and_case_regression() {
    let (evaluator, proposer) =
        (ScriptedEvaluator::new(), ScriptedProposer::new(&["BAD prompt", "REGRESS-B prompt"]));
    let (_, job) = run("opt-reject", &evaluator, &proposer, 2).await;
    assert_eq!(derive_state(&job.journal), JobState::NothingToPromote);
    assert_eq!(
        decisions(&job),
        vec![
            (Some(1), Decision::Reject(RejectReason::CaseRegression)),
            (Some(2), Decision::Reject(RejectReason::CaseRegression)),
        ]
    );
    // The second proposal saw the first as a rejection.
    assert_eq!(proposer.inputs.lock().unwrap()[1].1, 1);
    // Held-out is never touched when nothing was accepted.
    assert!(!evaluator.calls.lock().unwrap().iter().any(|(_, split)| *split == SplitKind::HeldOut));
}

pub(super) async fn a_tie_is_rejected_as_no_improvement() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["same quality"]));
    let (_, job) = run("opt-tie", &evaluator, &proposer, 1).await;
    assert_eq!(decisions(&job), vec![(Some(1), Decision::Reject(RejectReason::NoImprovement))]);
}

pub(super) async fn structural_reject_spends_no_candidate_evaluation() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["   "]));
    let (_, job) = run("opt-structural", &evaluator, &proposer, 1).await;
    assert!(job.journal.iter().any(|e| matches!(e, JournalEntry::StructuralReject { .. })));
    assert!(!evaluator.calls.lock().unwrap().iter().any(|(arm, _)| *arm == Arm::Candidate));
    assert_eq!(derive_state(&job.journal), JobState::NothingToPromote);
}

pub(super) async fn inconclusive_reruns_once_then_stays_inconclusive() {
    let (evaluator, proposer) =
        (ScriptedEvaluator::new(), ScriptedProposer::new(&["FLAKY GOOD", "GOOD prompt"]));
    let (_, job) = run("opt-inconclusive", &evaluator, &proposer, 2).await;
    let all = decisions(&job);
    assert_eq!(all[0], (Some(1), Decision::Inconclusive));
    assert_eq!(all[1], (Some(1), Decision::Inconclusive));
    assert_eq!(all[2], (Some(2), Decision::Accept));
    // An inconclusive round is never handed to the proposer as a rejection.
    assert_eq!(proposer.inputs.lock().unwrap()[1].1, 0);
}

pub(super) async fn held_out_regression_fails_the_job() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["HELDOUT-BAD"]));
    let (_, job) = run("opt-heldout", &evaluator, &proposer, 1).await;
    assert_eq!(
        derive_state(&job.journal),
        JobState::Failed { reason: "held_out_regression".into() }
    );
}

pub(super) async fn held_out_is_requested_only_at_finalize() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["GOOD prompt"]));
    run("opt-heldout-order", &evaluator, &proposer, 1).await;
    let calls = evaluator.calls.lock().unwrap();
    let first_held_out = calls.iter().position(|(_, split)| *split == SplitKind::HeldOut).unwrap();
    assert!(calls[first_held_out..].iter().all(|(_, split)| *split == SplitKind::HeldOut));
    assert_eq!(calls.len() - first_held_out, 2);
}

pub(super) async fn feedback_reaches_the_proposer_only_from_train() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["GOOD prompt"]));
    let (_, job) = run("opt-feedback", &evaluator, &proposer, 1).await;
    let inputs = proposer.inputs.lock().unwrap();
    assert!(inputs[0].0 > 0 && inputs[0].2, "train outcomes keep their feedback");
    let journal = serde_json::to_string(&job.journal).unwrap();
    assert!(!journal.contains("feedback for"), "feedback is never journaled");
}

pub(super) async fn budget_exhaustion_is_not_a_rejection() {
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["GOOD prompt"]));
    let db = test_db("opt-budget").await;
    let (access, target) = seed_config(&db).await;
    let mut job_spec = spec("opt-budget", target, 3);
    // Room for the baseline arm only.
    job_spec.budgets.max_trials = 60;
    let job = Driver::new(&access, &evaluator, &proposer).run(job_spec).await.unwrap();
    assert!(job.journal.iter().any(|e| matches!(e, JournalEntry::BudgetExhausted { .. })));
    assert_eq!(derive_state(&job.journal), JobState::Exhausted);
    assert!(decisions(&job).is_empty());
}

pub(super) async fn evaluator_change_fails_the_job() {
    let (evaluator, proposer) = (ScriptedEvaluator::failing_on(2), ScriptedProposer::new(&["GOOD"]));
    let db = test_db("opt-provenance").await;
    let (access, target) = seed_config(&db).await;
    let driver = Driver::new(&access, &evaluator, &proposer);
    assert!(driver.run(spec("opt-provenance", target.clone(), 1)).await.is_err());
    *evaluator.provenance.lock().unwrap() = "sha256:scripted-v2".into();
    assert!(driver.run(spec("opt-provenance", target, 1)).await.is_err());
    let job = gents::optimization::load_job(&access, crate::support::optimization::OWNER, "opt-provenance")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        derive_state(&job.journal),
        JobState::Failed { reason: "evaluator_changed".into() }
    );
}

pub(super) async fn baseline_drift_fails_the_job_on_resume() {
    let (evaluator, proposer) = (ScriptedEvaluator::failing_on(2), ScriptedProposer::new(&["GOOD"]));
    let db = test_db("opt-drift").await;
    let (access, target) = seed_config(&db).await;
    let driver = Driver::new(&access, &evaluator, &proposer);
    assert!(driver.run(spec("opt-drift", target.clone(), 1)).await.is_err());
    access
        .write(
            "test.optimization.drift",
            r#"mutation { update_InferenceBackend(filter:{backend_id:{_eq:"local"}},input:{name:"Edited"}){_docID} }"#,
        )
        .await
        .unwrap();
    let job = driver.run(spec("opt-drift", target, 1)).await.unwrap();
    assert_eq!(derive_state(&job.journal), JobState::Failed { reason: "baseline_drifted".into() });
}

/// Crash the evaluator at each call in turn, resume with a healthy one, and
/// require the same journal as an uninterrupted run once bookkeeping is removed.
pub(super) async fn resume_after_any_interruption_matches_an_uninterrupted_run() {
    fn essential(job: &JobRecord) -> Vec<JournalEntry> {
        let mut kept = Vec::new();
        for entry in &job.journal {
            match entry {
                JournalEntry::Interrupted => {
                    kept.pop(); // the dangling EvaluationStarted
                }
                other => kept.push(other.clone()),
            }
        }
        kept
    }

    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["GOOD prompt"]));
    let (_, clean) = run("opt-clean", &evaluator, &proposer, 1).await;
    let total_calls = evaluator.calls.lock().unwrap().len();

    for crash_at in 1..=total_calls {
        let name = format!("opt-crash-{crash_at}");
        let db = test_db(&name).await;
        let (access, target) = seed_config(&db).await;
        let crashing = ScriptedEvaluator::failing_on(crash_at);
        let first_proposer = ScriptedProposer::new(&["GOOD prompt"]);
        assert!(Driver::new(&access, &crashing, &first_proposer)
            .run(spec(&name, target.clone(), 1))
            .await
            .is_err());
        let healthy = ScriptedEvaluator::new();
        let second_proposer = ScriptedProposer::new(&["GOOD prompt"]);
        let resumed = Driver::new(&access, &healthy, &second_proposer)
            .run(spec(&name, target, 1))
            .await
            .unwrap();
        assert!(resumed.journal.contains(&JournalEntry::Interrupted), "crash_at {crash_at}");
        assert_eq!(essential(&resumed), essential(&clean), "crash_at {crash_at}");
    }
}
```

A crash during the train evaluation of a round resumes before `Proposed`, so the proposer is called again with index 0 of a fresh `ScriptedProposer`; that is why the resumed run gets its own proposer.

- [ ] **Step 3: Register the module and its tests**

In `crates/gents/tests/support/mod.rs` add `pub mod optimization;`.

In `crates/gents/tests/e2e_runtime.rs` add, with the other `#[path]` modules:

```rust
#[path = "e2e_runtime/optimization_driver.rs"]
mod optimization_driver;
```

and at the end of the file one wrapper per function, in this exact form:

```rust
#[tokio::test]
async fn optimization_accepts_a_clear_improvement() {
    optimization_driver::accepts_a_clear_improvement().await
}
```

Repeat that wrapper for: `rejects_no_improvement_and_case_regression`, `a_tie_is_rejected_as_no_improvement`, `structural_reject_spends_no_candidate_evaluation`, `inconclusive_reruns_once_then_stays_inconclusive`, `held_out_regression_fails_the_job`, `held_out_is_requested_only_at_finalize`, `feedback_reaches_the_proposer_only_from_train`, `budget_exhaustion_is_not_a_rejection`, `evaluator_change_fails_the_job`, `baseline_drift_fails_the_job_on_resume`, `resume_after_any_interruption_matches_an_uninterrupted_run`. Each wrapper is named `optimization_<function name>`.

- [ ] **Step 4: Run the matrix**

Run: `cargo test -p gents --test e2e_runtime optimization_`
Expected: 12 passed.

Two expectations encode scripted arithmetic, so check them if they fail before touching the driver. With 20 trials per case, `BAD` (2 of 20 per case against the baseline's 10 of 20) fails gate 2 before gate 3, which is why round 1 of the reject test is `CaseRegression`. `FLAKY GOOD` excludes half its trials, which breaks the asymmetry cap, so it is `Inconclusive` on both attempts.

- [ ] **Step 5: Full validation and commit**

Run: `cargo test -p gents` then `cargo check --workspace --all-targets`
Expected: both succeed.

```bash
git add crates/gents/tests
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(optimization): scripted doubles and the driver end-to-end matrix"
```

PR 5 description: baseline `optimization/04-core`; new collection `OptimizationJob` (local-only, in `LOCAL_AUDIT_COLLECTION_NAMES`, absent from every replication list); new baseline pin; owners `gents::optimization::{round, driver}`; no deletions; the stale-promote, unauthorized-promote and tool-surface rows of the matrix arrive in Plan 3 with `promote`.
