# Optimization Substrate, Plan 3 of 3: Promotion and Live Demo Implementation Plan

> **Superseded on 2026-09-21.** See `specs/2026-09-21-optimization-on-eval-design.md` section 10.
> `promote`, `show` and the CLI carry over and gain `revert`; PR 7 is deleted. Do not execute this plan.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the digest-guarded, operator-only promotion with its CLI, then a throwaway live evaluator that demonstrates one accepted and one rejected candidate against the existing monitor eval harness, plus an A/A calibration run.

**Architecture:** `optimization::promote` rebuilds the one-field patch from the job's frozen origin and journal, checks the whole frozen closure, writes only the target document, and journals the result in the same transaction. The CLI is a thin wrapper. The live evaluator implements `Evaluator` inside the test tree only, so deleting it is the whole #1515 migration.

**Tech Stack:** Rust 1.97.1, `clap`, DefraDB embedded node, the `configurator_evals` test harness, a live local model through the D4F endpoint for PR 7 only.

**Spec:** `docs/superpowers/specs/2026-09-21-optimization-substrate-design.md`

**Depends on:** Plans 1 and 2 (`optimization/05-driver`).

## Global Constraints

- `promote` is never reachable from the model-facing `config` tool. A test pins this.
- There is no force flag. A stale job's remedy is a new job.
- Promotion checks the entire frozen closure and writes only the target document.
- Promotion trusts no stored plan: it rebuilds the patch and verifies the rebuilt digest equals the journaled `candidate_digest`.
- Only the job's owner DID may promote.
- `crates/gents-cli` may use `println!` through the existing `print_json` helper; `crates/gents/src` may not.
- PR 7 is throwaway. Everything it adds lives under `crates/gents/tests/`, `scripts/evals/` and the `Makefile`, and is marked `THROWAWAY(#1515)`.
- PR 7's splits reuse the same two monitor cases. Its held-out split is therefore not independent. This is a demo limitation, stated in its report, and is the eval-supply gap #1515 owns.
- Before each push: `cargo test -p gents`, `cargo check --workspace --all-targets`, and for PR 6 also `cargo test -p gents-cli -- --nocapture --test-threads=1`.
- Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit ...`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/gents/src/optimization/promote.rs` | create | `show`, `promote`, `PromoteOutcome` |
| `crates/gents/src/optimization/mod.rs` | modify | re-exports |
| `crates/gents/src/self_config/command.rs` | modify (tests only) | pin that the tool has no optimization surface |
| `crates/gents/tests/e2e_runtime/optimization_promote.rs` | create | promote matrix |
| `crates/gents/tests/e2e_runtime.rs` | modify | register tests |
| `crates/gents-cli/src/cli/args.rs` | modify | `Optimization` subcommand and args |
| `crates/gents-cli/src/cli/args/tests.rs` | modify | parse tests |
| `crates/gents-cli/src/commands/optimization.rs` | create | `show`, `promote` |
| `crates/gents-cli/src/commands/mod.rs`, `src/lib.rs` | modify | module and dispatch |
| `crates/gents/tests/configurator_evals/optimization_live.rs` | create (PR 7) | `HarnessEvaluator`, live demo, A/A |
| `crates/gents/tests/fixtures/optimization/monitor_baseline.json` | create (PR 7) | golden monitor configuration |
| `scripts/evals/run-optimization.mjs`, `Makefile` | create/modify (PR 7) | `make live-optimization-eval` |

---

## PR 6: Promotion and CLI

Branch: `optimization/06-promote`, base `optimization/05-driver`.

### Task 1: `show` and `promote`

**Files:**
- Create: `crates/gents/src/optimization/promote.rs`
- Modify: `crates/gents/src/optimization/mod.rs`, `crates/gents/src/optimization/round.rs`

**Interfaces:**
- Consumes: `load_in_txn` (make it `pub(super)` in `round.rs`), `append_in_txn`, `checkpoint`, `derive_state`, `capture_closure`, `closure_digests`, `apply_text`, `target_digest`, `current_text`, `decide`, `evidence`, `DesiredStateExpectation`, `stale_expectation`, `apply_desired_state_plan`.
- Produces:
  - `pub async fn show(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<Value>`
  - `pub enum PromoteOutcome { Promoted { target_digest: String }, Refused { drifted: Vec<DriftedRef> } }`
  - `pub async fn promote(access: &ConfigAccess, caller: &str, owner: &str, job_id: &str, digest: &str) -> Result<PromoteOutcome>`

- [ ] **Step 1: Expose the in-transaction loader**

In `round.rs` change `async fn load_in_txn` to `pub(super) async fn load_in_txn`.

- [ ] **Step 2: Write `promote.rs`**

```rust
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

use super::policy::{decide, evidence};
use super::round::{
    append_in_txn, checkpoint, derive_state, load_in_txn, DriftedRef, JobRecord, JobState,
    JournalEntry,
};
use super::target::{
    apply_text, capture_closure, closure_digests, current_text, target_digest, FrozenDocument,
};
use crate::config_client::{
    apply_desired_state_plan, stale_expectation, ConfigAccess, ConfigApplyTxn,
    DesiredStateApplyDocument, DesiredStateApplyPlan, DesiredStateExpectation,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromoteOutcome {
    Promoted { target_digest: String },
    /// The frozen closure no longer matches the live configuration.
    Refused { drifted: Vec<DriftedRef> },
}

/// Raised inside the promoting transaction so it rolls back; the caller then
/// journals the refusal in a second transaction.
#[derive(Debug)]
struct PromotionStale {
    drifted: Vec<DriftedRef>,
}

impl std::fmt::Display for PromotionStale {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "baseline changed since the job froze it: {:?}", self.drifted)
    }
}

impl std::error::Error for PromotionStale {}

fn drift(frozen: &[FrozenDocument], live: &[FrozenDocument]) -> Vec<DriftedRef> {
    let key = |d: &FrozenDocument| (d.collection, d.owner.clone(), d.id.clone());
    let mut drifted = Vec::new();
    for document in frozen {
        if live.iter().find(|l| key(l) == key(document)).map(|l| &l.digest) != Some(&document.digest) {
            drifted.push(DriftedRef {
                collection: document.collection.graphql_type().to_owned(),
                id: document.id.clone(),
            });
        }
    }
    for document in live {
        if !frozen.iter().any(|f| key(f) == key(document)) {
            drifted.push(DriftedRef {
                collection: document.collection.graphql_type().to_owned(),
                id: document.id.clone(),
            });
        }
    }
    drifted
}

async fn require_job(txn: &ConfigApplyTxn<'_>, owner: &str, job_id: &str) -> Result<JobRecord> {
    load_in_txn(txn, owner, job_id)
        .await?
        .with_context(|| format!("no optimization job {job_id:?} owned by {owner:?}"))
}

/// Everything an operator needs to decide, recomputed from the journal.
pub async fn show(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<Value> {
    let (owner, job_id) = (owner.to_owned(), job_id.to_owned());
    access
        .transact("optimization.show", |txn| {
            let (owner, job_id) = (owner.clone(), job_id.clone());
            Box::pin(async move {
                let job = require_job(txn, &owner, &job_id).await?;
                let live = capture_closure(txn, &owner).await?;
                let drifted = drift(&job.origin.closure, &closure_digests(&live)?);
                let decisions: Vec<Value> = job
                    .journal
                    .iter()
                    .filter_map(|entry| match entry {
                        JournalEntry::Decided { round, attempt, mode, baseline, candidate, decision, policy_version } => {
                            let recomputed = decide(*mode, &job.origin.policy, &evidence(baseline, candidate));
                            Some(json!({
                                "round": round, "attempt": attempt, "mode": mode,
                                "decision": decision, "policy_version": policy_version,
                                "recomputed_matches": recomputed == *decision,
                                "baseline": baseline, "candidate": candidate,
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                let checkpoint = checkpoint(&job.journal);
                Ok(json!({
                    "job_id": job.job_id,
                    "owner": job.owner,
                    "state": derive_state(&job.journal),
                    "target": job.origin.target,
                    "live_text": current_text(&live, &job.origin.target).ok(),
                    "checkpoint_text": checkpoint.as_ref().map(|(text, _)| text),
                    "checkpoint_digest": checkpoint.as_ref().map(|(_, digest)| digest),
                    "baseline_drifted": drifted,
                    "decisions": decisions,
                    "journal_entries": job.journal.len(),
                }))
            })
        })
        .await
}

pub async fn promote(
    access: &ConfigAccess,
    caller: &str,
    owner: &str,
    job_id: &str,
    digest: &str,
) -> Result<PromoteOutcome> {
    ensure!(
        caller == owner,
        "optimization job {job_id:?} is owned by {owner:?}; {caller:?} may not promote it"
    );
    let (owner_s, job_s, digest_s) = (owner.to_owned(), job_id.to_owned(), digest.to_owned());
    let attempt = access
        .transact("optimization.promote", |txn| {
            let (owner, job_id, digest) = (owner_s.clone(), job_s.clone(), digest_s.clone());
            Box::pin(async move {
                let job = require_job(txn, &owner, &job_id).await?;
                let state = derive_state(&job.journal);
                ensure!(
                    state == JobState::ReadyToPromote,
                    "job {job_id:?} is {}, not ready_to_promote",
                    state.label()
                );
                let (text, candidate_digest) =
                    checkpoint(&job.journal).context("ready job has no checkpoint")?;
                ensure!(
                    digest == candidate_digest,
                    "digest {digest:?} is not this job's checkpoint {candidate_digest:?}; run show again"
                );

                let live = capture_closure(txn, &owner).await?;
                let drifted = drift(&job.origin.closure, &closure_digests(&live)?);
                if !drifted.is_empty() {
                    return Err(anyhow::Error::new(PromotionStale { drifted }));
                }

                // Rebuild the patch; never trust a stored plan.
                let patched = apply_text(&live, &job.origin.target, &text)?;
                let rebuilt = target_digest(&patched, &job.origin.target)?;
                ensure!(
                    rebuilt == candidate_digest,
                    "rebuilt candidate digest {rebuilt:?} differs from the journaled {candidate_digest:?}"
                );
                let (collection, document) = patched
                    .into_iter()
                    .find(|(collection, value)| {
                        *collection == job.origin.target.field.collection()
                            && value.get(collection.unique_field()).and_then(Value::as_str)
                                == Some(job.origin.target.id.as_str())
                    })
                    .context("patched target document missing")?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection,
                    add: document.clone(),
                    update: document,
                }])?
                .with_expected(
                    job.origin
                        .closure
                        .iter()
                        .map(|frozen| DesiredStateExpectation {
                            collection: frozen.collection,
                            owner: frozen.owner.clone(),
                            id: frozen.id.clone(),
                            digest: Some(frozen.digest.clone()),
                        })
                        .collect(),
                )?;
                if let Err(error) = apply_desired_state_plan(txn, &plan).await {
                    if let Some(stale) = stale_expectation(&error) {
                        let drifted = stale
                            .drifted
                            .iter()
                            .map(|d| DriftedRef {
                                collection: d.collection.graphql_type().to_owned(),
                                id: d.id.clone(),
                            })
                            .collect();
                        return Err(anyhow::Error::new(PromotionStale { drifted }));
                    }
                    return Err(error);
                }
                append_in_txn(
                    txn,
                    &job,
                    &JournalEntry::Promoted { by: owner.clone(), target_digest: rebuilt.clone() },
                )
                .await?;
                Ok(rebuilt)
            })
        })
        .await;

    match attempt {
        Ok(target_digest) => {
            tracing::info!(job_id, owner, %target_digest, "optimization checkpoint promoted");
            Ok(PromoteOutcome::Promoted { target_digest })
        }
        Err(error) => {
            let Some(stale) = error.downcast_ref::<PromotionStale>() else {
                return Err(error);
            };
            let drifted = stale.drifted.clone();
            let (owner_s, job_s, refused) = (owner.to_owned(), job_id.to_owned(), drifted.clone());
            access
                .transact("optimization.promotion_refused", |txn| {
                    let (owner, job_id, drifted) = (owner_s.clone(), job_s.clone(), refused.clone());
                    Box::pin(async move {
                        let job = require_job(txn, &owner, &job_id).await?;
                        append_in_txn(txn, &job, &JournalEntry::PromotionRefused { drifted }).await
                    })
                })
                .await?;
            tracing::warn!(job_id, owner, ?drifted, "optimization promotion refused: baseline drifted");
            Ok(PromoteOutcome::Refused { drifted })
        }
    }
}
```

If `bail` is unused after compilation, remove it from the `use anyhow` line.

Add to `mod.rs`:

```rust
pub mod promote;

pub use promote::{promote, show, PromoteOutcome};
```

- [ ] **Step 3: Check and commit**

Run: `cargo check -p gents`
Expected: success.

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): digest-guarded operator promotion and show"
```

### Task 2: Promotion matrix and the tool-surface pin

**Files:**
- Create: `crates/gents/tests/e2e_runtime/optimization_promote.rs`
- Modify: `crates/gents/tests/e2e_runtime.rs`, `crates/gents/src/self_config/command.rs` (test module only)

**Interfaces:**
- Consumes: `seed_config`, `spec`, `ScriptedEvaluator`, `ScriptedProposer`, `OWNER`, `BASELINE_PROMPT` from `tests/support/optimization.rs` (Plan 2 Task 7).

- [ ] **Step 1: Write the tests**

```rust
use gents::config_client::ConfigAccess;
use gents::optimization::{
    derive_state, load_job, promote, show, Driver, JobState, JournalEntry, PromoteOutcome,
};

use crate::support::optimization::{
    seed_config, spec, ScriptedEvaluator, ScriptedProposer, BASELINE_PROMPT, OWNER,
};
use crate::support::{test_db, TestDb};

async fn ready_job(name: &str) -> (TestDb, ConfigAccess, String) {
    let db = test_db(name).await;
    let (access, target) = seed_config(&db).await;
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["GOOD prompt"]));
    let job = Driver::new(&access, &evaluator, &proposer).run(spec(name, target, 1)).await.unwrap();
    assert_eq!(derive_state(&job.journal), JobState::ReadyToPromote);
    let view = show(&access, OWNER, name).await.unwrap();
    let digest = view["checkpoint_digest"].as_str().unwrap().to_owned();
    (db, access, digest)
}

async fn live_prompt(access: &ConfigAccess) -> String {
    let response = access
        .execute(r#"{ AgentContext(filter:{context_id:{_eq:"monitor-context"}}) { system_prompt } }"#)
        .await
        .unwrap();
    response["data"]["AgentContext"][0]["system_prompt"].as_str().unwrap().to_owned()
}

pub(super) async fn promotes_the_checkpoint_once() {
    let (_db, access, digest) = ready_job("promote-ok").await;
    let outcome = promote(&access, OWNER, OWNER, "promote-ok", &digest).await.unwrap();
    assert!(matches!(outcome, PromoteOutcome::Promoted { .. }));
    assert_eq!(live_prompt(&access).await, "GOOD prompt");
    let job = load_job(&access, OWNER, "promote-ok").await.unwrap().unwrap();
    assert_eq!(derive_state(&job.journal), JobState::Promoted);
    // A second promote is refused by state, and changes nothing.
    assert!(promote(&access, OWNER, OWNER, "promote-ok", &digest).await.is_err());
    assert_eq!(live_prompt(&access).await, "GOOD prompt");
}

pub(super) async fn show_recomputes_every_decision() {
    let (_db, access, _) = ready_job("promote-show").await;
    let view = show(&access, OWNER, "promote-show").await.unwrap();
    assert_eq!(view["state"]["state"], "ready_to_promote");
    assert_eq!(view["live_text"], BASELINE_PROMPT);
    assert_eq!(view["checkpoint_text"], "GOOD prompt");
    assert!(view["baseline_drifted"].as_array().unwrap().is_empty());
    let decisions = view["decisions"].as_array().unwrap();
    assert!(!decisions.is_empty());
    assert!(decisions.iter().all(|d| d["recomputed_matches"] == true));
}

pub(super) async fn a_stale_closure_is_refused_and_nothing_is_written() {
    let (_db, access, digest) = ready_job("promote-stale").await;
    // The operator edits a document the job never targets.
    access
        .write(
            "test.promote.drift",
            r#"mutation { update_InferenceBackend(filter:{backend_id:{_eq:"local"}},input:{name:"Edited"}){_docID} }"#,
        )
        .await
        .unwrap();
    let outcome = promote(&access, OWNER, OWNER, "promote-stale", &digest).await.unwrap();
    let PromoteOutcome::Refused { drifted } = outcome else { panic!("expected refusal") };
    assert_eq!(drifted.len(), 1);
    assert_eq!(drifted[0].collection, "InferenceBackend");
    assert_eq!(live_prompt(&access).await, BASELINE_PROMPT, "target must be untouched");
    let job = load_job(&access, OWNER, "promote-stale").await.unwrap().unwrap();
    assert_eq!(derive_state(&job.journal), JobState::Stale);
    assert!(matches!(job.journal.last(), Some(JournalEntry::PromotionRefused { .. })));
    // No force path: a stale job stays stale.
    assert!(promote(&access, OWNER, OWNER, "promote-stale", &digest).await.is_err());
}

pub(super) async fn an_edited_target_is_refused() {
    let (_db, access, digest) = ready_job("promote-target-edit").await;
    access
        .write(
            "test.promote.target_edit",
            r#"mutation { update_AgentContext(filter:{context_id:{_eq:"monitor-context"}},input:{system_prompt:"operator edit"}){_docID} }"#,
        )
        .await
        .unwrap();
    let outcome = promote(&access, OWNER, OWNER, "promote-target-edit", &digest).await.unwrap();
    assert!(matches!(outcome, PromoteOutcome::Refused { .. }));
    assert_eq!(live_prompt(&access).await, "operator edit", "the user's edit survives");
}

pub(super) async fn a_foreign_did_cannot_promote() {
    let (_db, access, digest) = ready_job("promote-foreign").await;
    let error = promote(&access, "did:key:intruder", OWNER, "promote-foreign", &digest)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("may not promote"));
    assert_eq!(live_prompt(&access).await, BASELINE_PROMPT);
    // Looking the job up under the intruder's own DID finds nothing.
    assert!(promote(&access, "did:key:intruder", "did:key:intruder", "promote-foreign", &digest)
        .await
        .is_err());
}

pub(super) async fn a_wrong_digest_is_refused() {
    let (_db, access, _) = ready_job("promote-digest").await;
    let error = promote(&access, OWNER, OWNER, "promote-digest", "sha256:wrong").await.unwrap_err();
    assert!(format!("{error:#}").contains("run show again"));
    assert_eq!(live_prompt(&access).await, BASELINE_PROMPT);
}

pub(super) async fn a_job_that_is_not_ready_cannot_be_promoted() {
    let db = test_db("promote-not-ready").await;
    let (access, target) = seed_config(&db).await;
    let (evaluator, proposer) = (ScriptedEvaluator::new(), ScriptedProposer::new(&["BAD prompt"]));
    Driver::new(&access, &evaluator, &proposer)
        .run(spec("promote-not-ready", target, 1))
        .await
        .unwrap();
    let error = promote(&access, OWNER, OWNER, "promote-not-ready", "sha256:any").await.unwrap_err();
    assert!(format!("{error:#}").contains("not ready_to_promote"));
}
```

- [ ] **Step 2: Register**

In `crates/gents/tests/e2e_runtime.rs` add:

```rust
#[path = "e2e_runtime/optimization_promote.rs"]
mod optimization_promote;
```

and one wrapper per function in this form, named `optimization_promote_<function name>`:

```rust
#[tokio::test]
async fn optimization_promote_promotes_the_checkpoint_once() {
    optimization_promote::promotes_the_checkpoint_once().await
}
```

for: `promotes_the_checkpoint_once`, `show_recomputes_every_decision`, `a_stale_closure_is_refused_and_nothing_is_written`, `an_edited_target_is_refused`, `a_foreign_did_cannot_promote`, `a_wrong_digest_is_refused`, `a_job_that_is_not_ready_cannot_be_promoted`.

- [ ] **Step 3: Pin the model-facing tool surface**

In the `#[cfg(test)] mod tests` of `crates/gents/src/self_config/command.rs` add:

```rust
    /// #1455: promotion is operator-only. The model-facing tool has no
    /// optimization resource and no promote verb.
    #[test]
    fn config_tool_exposes_no_optimization_surface() {
        let every_category: BTreeSet<String> =
            crate::config_client::SELF_CONFIG_CATEGORIES.iter().map(|c| (*c).to_owned()).collect();
        let resources = model_resources(&every_category, true);
        assert!(!resources.iter().any(|r| r.contains("optimiz") || r.contains("promote")));
        let usage = CONFIG_USAGE.to_lowercase();
        assert!(!usage.contains("optimiz"));
        assert!(!usage.contains("promote"));
    }
```

`SELF_CONFIG_CATEGORIES` is defined in `crates/gents/src/config_client/patch.rs`. If it is not re-exported from `crate::config_client`, reference it as `crate::config_client::patch::SELF_CONFIG_CATEGORIES`.

- [ ] **Step 4: Run and commit**

Run:
- `cargo test -p gents --test e2e_runtime optimization_promote_`
- `cargo test -p gents --lib self_config::command::tests::config_tool_exposes_no_optimization_surface`

Expected: 7 passed, then 1 passed.

```bash
git add crates/gents
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(optimization): promotion matrix and model-tool surface pin"
```

### Task 3: CLI `gents optimization show|promote`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs`, `crates/gents-cli/src/cli/args/tests.rs`, `crates/gents-cli/src/commands/mod.rs`, `crates/gents-cli/src/lib.rs`
- Create: `crates/gents-cli/src/commands/optimization.rs`

**Interfaces:**
- Consumes: `gents::optimization::{show, promote, PromoteOutcome}`, `crate::{print_json, resolve_agent_did, resolve_config_access}`.

- [ ] **Step 1: Failing parse tests**

Append to `crates/gents-cli/src/cli/args/tests.rs`:

```rust
#[test]
fn optimization_promote_requires_a_job_and_a_digest() {
    let cli = Cli::try_parse_from([
        "gents", "optimization", "promote", "job-1", "--digest", "sha256:abc", "--home", "/tmp/h",
    ])
    .unwrap();
    let Command::Optimization { command: OptimizationCommand::Promote(args) } = cli.command else {
        panic!("expected optimization promote");
    };
    assert_eq!(args.job.job_id, "job-1");
    assert_eq!(args.digest, "sha256:abc");
    assert!(Cli::try_parse_from(["gents", "optimization", "promote", "job-1"]).is_err());
    assert!(Cli::try_parse_from(["gents", "optimization", "promote", "job-1", "--force"]).is_err());
}

#[test]
fn optimization_show_takes_a_job() {
    let cli = Cli::try_parse_from(["gents", "optimization", "show", "job-1"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Optimization { command: OptimizationCommand::Show(_) }
    ));
}
```

Run: `cargo test -p gents-cli --lib optimization_`
Expected: compile error, `no variant named Optimization`.

- [ ] **Step 2: Arguments**

In `args.rs`, add to `enum Command` directly after the `Mailbox` variant:

```rust
    #[command(about = "Inspect and promote configuration optimization jobs")]
    Optimization {
        #[command(subcommand)]
        command: OptimizationCommand,
    },
```

and after the `MailboxCommand` block:

```rust
#[derive(Subcommand)]
pub(crate) enum OptimizationCommand {
    #[command(about = "Show a job's state, recomputed decisions and checkpoint digest")]
    Show(OptimizationJobArgs),
    #[command(about = "Promote a ready job's checkpoint into the live configuration")]
    Promote(OptimizationPromoteArgs),
}

#[derive(clap::Args)]
pub(crate) struct OptimizationJobArgs {
    #[arg(value_name = "JOB_ID")]
    pub(crate) job_id: String,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for a running runtime")]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct OptimizationPromoteArgs {
    #[command(flatten)]
    pub(crate) job: OptimizationJobArgs,
    #[arg(long, value_name = "SHA256", help = "Checkpoint digest printed by `optimization show`")]
    pub(crate) digest: String,
}
```

- [ ] **Step 3: Command module**

`crates/gents-cli/src/commands/optimization.rs`:

```rust
use anyhow::Result;
use gents::optimization::PromoteOutcome;
use serde_json::json;

use crate::cli::args::{OptimizationCommand, OptimizationJobArgs, OptimizationPromoteArgs};
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(command: OptimizationCommand) -> Result<()> {
    match command {
        OptimizationCommand::Show(args) => show(args).await,
        OptimizationCommand::Promote(args) => promote(args).await,
    }
}

async fn show(args: OptimizationJobArgs) -> Result<()> {
    let principal = resolve_agent_did(args.home.as_deref(), None)?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    print_json(&gents::optimization::show(&access, &principal, &args.job_id).await?)
}

async fn promote(args: OptimizationPromoteArgs) -> Result<()> {
    let home = args.job.home.as_deref();
    let principal = resolve_agent_did(home, None)?;
    let (access, _) = resolve_config_access(home, args.job.graphql.as_deref()).await?;
    let outcome = gents::optimization::promote(
        &access,
        &principal,
        &principal,
        &args.job.job_id,
        &args.digest,
    )
    .await?;
    match outcome {
        PromoteOutcome::Promoted { target_digest } => print_json(&json!({
            "ok": true, "job_id": args.job.job_id, "promoted": true, "target_digest": target_digest,
        })),
        PromoteOutcome::Refused { drifted } => {
            print_json(&json!({
                "ok": false, "job_id": args.job.job_id, "promoted": false,
                "reason": "baseline changed since the job froze it; start a new job",
                "drifted": drifted,
            }))?;
            anyhow::bail!("promotion refused: baseline drifted")
        }
    }
}
```

In `commands/mod.rs` add `pub(crate) mod optimization;` after `pub(crate) mod mailbox;`. In `lib.rs`, after the `Command::Mailbox` dispatch arm add:

```rust
        Command::Optimization { command } => commands::optimization::dispatch(command).await,
```

- [ ] **Step 4: Run and commit**

Run:
- `cargo test -p gents-cli --lib optimization_`
- `cargo test -p gents-cli -- --nocapture --test-threads=1`
- `cargo check --workspace --all-targets`

Expected: all succeed. If another exhaustive `match` over `Command` exists (for example in telemetry or help rendering), add the `Optimization` arm there with the same treatment `Mailbox` gets.

```bash
git add crates/gents-cli
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(cli): gents optimization show and promote"
```

PR 6 description: baseline `optimization/05-driver`; owners `gents::optimization::promote` and the CLI; no deletions; a promoted prompt reaches requests claimed after the runtime's control watcher reconciles the changed `AgentContext` (5 second debounce), which is existing behavior; config replicates runtime-to-client only, so under the one-active-runtime convention a local digest check is not bypassed by a late merge.

---

## PR 7: Throwaway live evaluator

Branch: `optimization/07-live-throwaway`, base `optimization/06-promote`.

This PR needs a live model and is never part of CI. It is the least certain part of the series, because the existing monitor suites have the configurator model author the monitor inside each trial, so no fixed baseline monitor configuration exists yet. Task 4 creates one.

### Task 4: Golden monitor baseline fixture

**Files:**
- Modify: `crates/gents/tests/configurator_evals/onboarding_scenarios.rs`
- Create: `crates/gents/tests/fixtures/optimization/monitor_baseline.json`

- [ ] **Step 1: Persist the configured snapshot**

In `run_monitor_trial`, inside the `monitor-configure` acceptance block, directly before `Ok(snapshot)`, add:

```rust
                // THROWAWAY(#1515): source for fixtures/optimization/monitor_baseline.json
                reporting::write_json_new(&evidence.join("monitor-configured-snapshot.json"), &snapshot)
                    .map_err(stages::infrastructure)?;
```

- [ ] **Step 2: Produce one passing trial**

Run: `GENTS_LIVE_CONFIG_RUNS=1 GENTS_LIVE_CONFIG_CONCURRENCY=1 make live-mailbox-eval`
Expected: a run directory under `~/.gents-eval/` whose trial has all five `monitor-*` cases passing. Repeat until one does.

- [ ] **Step 3: Build the fixture**

From that trial's `evidence/monitor-configured-snapshot.json`, write `crates/gents/tests/fixtures/optimization/monitor_baseline.json` as a JSON array of `{"collection": "<GraphQL type>", "document": {...}}` rows containing, for the working monitor behavior only: its `AgentBehavior`, its `AgentContext`, its `Tools`, every `Skill` in `skill_ids`, and its `Task`, `Trigger` and `EventSource` rows from `monitor-automation-config.json`. Remove `_docID` and `updated_at` from each document. Replace every occurrence of the trial's owner DID with the literal `{{OWNER}}`, and the workspace root with `{{ROOT}}`. Do not include `InferenceBackend`, `InferenceProfile` or principal rows; the harness binds those per trial.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/tests
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(evals): THROWAWAY(#1515) golden monitor baseline fixture"
```

### Task 5: `HarnessEvaluator`

**Files:**
- Create: `crates/gents/tests/configurator_evals/optimization_live.rs`
- Modify: `crates/gents/tests/configurator_evals/runner.rs` (one `mod` line)

**Interfaces:**
- Consumes from the harness (verify each signature first with the grep in Step 1): `super::retained_trial_db`, `crate::support::live_inference::{bind_d4f_backend_for_model, boot_d4f_agent_with_options, D4F_BACKEND_ID}`, `install_onboarding_profiles`, `super::install_eval_workspace_root`, `submit_mailbox_input`, `assert_mailbox_findings`, `mailbox_automation`, `stages::{EvaluationFailure, stage_timeout}`.
- Produces: `classify(kind: Option<&str>) -> OutcomeClass`, `rehome(&str, owner, root) -> Result<Vec<(Collection, Value)>>`, `HarnessEvaluator`, `false_accept_rate(...)`.

- [ ] **Step 1: Verify the harness signatures**

Run: `grep -n "pub(super) async fn retained_trial_db\|async fn install_onboarding_profiles\|async fn submit_mailbox_input\|fn assert_mailbox_findings\|async fn mailbox_automation\|pub async fn bind_d4f_backend_for_model\|pub async fn boot_d4f_agent_with_options\|async fn install_eval_workspace_root" -A8 crates/gents/tests/configurator_evals/*.rs crates/gents/tests/support/live_inference.rs`
Expected: one definition each. Use the printed parameter lists in Step 3. Make `install_onboarding_profiles`, `submit_mailbox_input`, `assert_mailbox_findings` and `mailbox_automation` `pub(super)` if they are private.

- [ ] **Step 2: Pure parts with tests**

```rust
//! THROWAWAY(#1515): an `Evaluator` over the monitor-mailbox harness. Delete
//! this file, its fixture, `scripts/evals/run-optimization.mjs` and the
//! `live-optimization-eval` make target when native evaluation lands.

use anyhow::{Context, Result};
use gents::optimization::OutcomeClass;
use gents::Collection;
use serde_json::Value;

/// Map the harness's failure kinds (`stages::EvaluationFailure::KINDS`) onto the
/// substrate's classes. A deadline is a failure by substrate rule.
pub(super) fn classify(failure_kind: Option<&str>) -> OutcomeClass {
    match failure_kind {
        None => OutcomeClass::Pass,
        Some("model_acceptance" | "deadline" | "tool" | "runtime") => OutcomeClass::Fail,
        Some("provider" | "infrastructure" | "prerequisite") => OutcomeClass::NotEvidence,
        // `inconclusive` is an evaluator limitation; `grader` is a grader defect.
        Some(_) => OutcomeClass::Unknown,
    }
}

pub(super) fn rehome(fixture: &str, owner: &str, root: &str) -> Result<Vec<(Collection, Value)>> {
    let rendered = fixture.replace("{{OWNER}}", owner).replace("{{ROOT}}", root);
    let rows: Vec<Value> = serde_json::from_str(&rendered)?;
    rows.into_iter()
        .map(|row| {
            let name = row["collection"].as_str().context("fixture row has no collection")?;
            let collection = Collection::ALL
                .iter()
                .copied()
                .find(|c| c.graphql_type() == name)
                .with_context(|| format!("unknown collection {name}"))?;
            Ok((collection, row["document"].clone()))
        })
        .collect()
}

/// Share of A/A comparisons the policy accepted. Must be near zero.
pub(super) fn false_accept_rate(accepts: usize, comparisons: usize) -> f64 {
    if comparisons == 0 { 0.0 } else { accepts as f64 / comparisons as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_harness_kind_is_classified_and_deadline_fails() {
        for kind in super::super::stages::EvaluationFailure::KINDS {
            let _ = classify(Some(kind));
        }
        assert_eq!(classify(Some("deadline")), OutcomeClass::Fail);
        assert_eq!(classify(Some("infrastructure")), OutcomeClass::NotEvidence);
        assert_eq!(classify(Some("inconclusive")), OutcomeClass::Unknown);
        assert_eq!(classify(Some("unknown")), OutcomeClass::Unknown);
        assert_eq!(classify(None), OutcomeClass::Pass);
    }

    #[test]
    fn rehome_substitutes_owner_and_root() {
        let fixture = r#"[{"collection":"AgentContext","document":{"agent_did":"{{OWNER}}","context_id":"c","system_prompt":"root {{ROOT}}"}}]"#;
        let rows = rehome(fixture, "did:key:trial", "/tmp/ws").unwrap();
        assert_eq!(rows[0].0, Collection::AgentContext);
        assert_eq!(rows[0].1["agent_did"], "did:key:trial");
        assert_eq!(rows[0].1["system_prompt"], "root /tmp/ws");
    }
}
```

In `runner.rs`, next to the other `mod` lines for this directory, add `mod optimization_live;`.

Run: `cargo test -p gents --test e2e_configurator optimization_live`
Expected: 2 passed.

- [ ] **Step 3: The evaluator**

Append to `optimization_live.rs`. One trial boots an isolated retained node, binds the model, seeds the rehomed baseline with the request's target text substituted into the monitor's `AgentContext.system_prompt`, boots the agent, and runs the two monitor checks exactly as `run_monitor_trial` runs cases 3 and 4.

```rust
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use gents::optimization::{
    CaseTrialOutcome, EvalReport, EvalRequest, Evaluator, Usage,
};
use sha2::{Digest, Sha256};

const FIXTURE: &str = include_str!("../fixtures/optimization/monitor_baseline.json");
const CASES: [&str; 2] = ["monitor-mailbox-output", "monitor-deduplicate"];

pub(super) struct HarnessEvaluator {
    pub model: String,
    pub run_dir: PathBuf,
}

impl HarnessEvaluator {
    /// The target text the driver wants evaluated: the system prompt of the
    /// single AgentContext in the request's plan.
    fn target_text(request: &EvalRequest) -> Result<String> {
        request
            .plan
            .documents()
            .iter()
            .find(|d| d.collection == Collection::AgentContext)
            .and_then(|d| d.add["system_prompt"].as_str())
            .map(str::to_owned)
            .context("request plan has no AgentContext system_prompt")
    }

    async fn trial(&self, text: &str, split: &str, arm: &str, trial: u32) -> Vec<(String, Option<String>, Option<String>)> {
        let artifacts = self.run_dir.join(format!("{split}-{arm}-{trial}"));
        match self.trial_inner(text, &artifacts).await {
            Ok(results) => results,
            // A trial that could not run at all says nothing about the config.
            Err(error) => CASES
                .iter()
                .map(|case| ((*case).to_owned(), Some("infrastructure".to_owned()), Some(format!("{error:#}"))))
                .collect(),
        }
    }
}
```

`trial_inner(&self, text, artifacts) -> Result<Vec<(case_id, failure_kind, error)>>` is the harness glue. Write it by copying the body of `run_monitor_trial` and making exactly these changes, using the signatures from Step 1:

1. Keep: `retained_trial_db`, schema install, identity, `bind_d4f_backend_for_model`, `install_onboarding_profiles`, `install_eval_workspace_root`, `boot_d4f_agent_with_options`, `ActivationFence`.
2. Remove: `install_setup_configurator` and the three model-authoring stages (`monitor-preview`, `monitor-configure`, `monitor-edit-in-place`).
3. Add, after the profiles are installed and before booting the agent: `let mut rows = rehome(FIXTURE, &owner, &root.to_string_lossy())?;`, set `row.1["system_prompt"] = text.into()` on the `AgentContext` row, build a `DesiredStateApplyPlan` from the rows and apply it in one `ConfigAccess::Local(db.node.clone()).transact(...)`, exactly as `tests/support/optimization.rs::seed_config` does.
4. Read the monitor behavior id from the fixture's `AgentBehavior` row and the automation with `mailbox_automation(db.node.as_ref(), &owner, id)`.
5. Keep the final loop over `monitor-mailbox-output` and `monitor-deduplicate` unchanged, wrapped in `stages::checked` so per-case results are written.
6. Return `stages::case_results(&[CaseId::new(CASES[0]), CaseId::new(CASES[1])], &evidence)?` mapped to `(case_id, failure_kind, error)`.

Then the trait implementation:

```rust
#[async_trait]
impl Evaluator for HarnessEvaluator {
    async fn provenance(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(FIXTURE.as_bytes());
        hasher.update(include_bytes!("optimization_live.rs"));
        hasher.update(include_bytes!("onboarding_scenarios.rs"));
        hasher.update(include_bytes!("stages.rs"));
        hasher.update(self.model.as_bytes());
        Ok(format!("sha256:{:x}", hasher.finalize()))
    }

    async fn evaluate(&self, request: EvalRequest) -> Result<EvalReport> {
        let text = Self::target_text(&request)?;
        let split = format!("{:?}", request.split.kind()).to_lowercase();
        let arm = format!("{:?}", request.arm).to_lowercase();
        let mut outcomes = Vec::new();
        for trial in request.trial_offset..request.trial_offset + request.trials {
            for (case_id, failure_kind, error) in self.trial(&text, &split, &arm, trial).await {
                outcomes.push(CaseTrialOutcome {
                    class: classify(failure_kind.as_deref()),
                    raw_kind: failure_kind.unwrap_or_else(|| "passed".into()),
                    feedback: error,
                    evidence_ref: self
                        .run_dir
                        .join(format!("{split}-{arm}-{trial}"))
                        .display()
                        .to_string(),
                    case_id,
                    trial,
                    usage: Usage::default(),
                });
            }
        }
        Ok(EvalReport { outcomes, provenance: self.provenance().await? })
    }
}
```

All three splits run the same two cases. Trials run sequentially because one local model is the bottleneck. `usage` is left at zero: token rollup from `InferenceCall` is not wired in this throwaway.

- [ ] **Step 4: Compile and commit**

Run: `cargo test -p gents --test e2e_configurator --no-run`
Expected: success.

```bash
git add crates/gents/tests
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(evals): THROWAWAY(#1515) HarnessEvaluator over the monitor suite"
```

### Task 6: Live demo and A/A calibration

**Files:**
- Modify: `crates/gents/tests/configurator_evals/optimization_live.rs`, `Makefile`
- Create: `scripts/evals/run-optimization.mjs`

- [ ] **Step 1: The two ignored live entries**

Append to `optimization_live.rs`. The degraded baseline deletes the de-duplication instruction from the golden prompt, so the healthy golden prompt is a known-good candidate and a findings-suppressing prompt is a known-bad one.

```rust
use gents::optimization::{
    decide, evidence, tally_by_case, Arm, Budgets, Decision, Driver, JobSpec, Mode, PolicyV1,
    Proposal, ProposalInput, Proposer, SplitId, Target, TargetField,
};

struct Scripted(std::sync::Mutex<Vec<String>>);

#[async_trait]
impl Proposer for Scripted {
    async fn propose(&self, _input: ProposalInput) -> Result<Proposal> {
        let text = self.0.lock().unwrap().remove(0);
        Ok(Proposal { text, rationale: "scripted live candidate".into() })
    }
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn live_policy() -> PolicyV1 {
    PolicyV1 { min_trials: env_u32("GENTS_OPT_TRIALS", 20) as u64, ..PolicyV1::uncalibrated() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: THROWAWAY(#1515); use make live-optimization-eval"]
async fn live_optimization_accepts_one_and_rejects_one() {
    let run_dir = PathBuf::from(std::env::var("GENTS_EVAL_RUN_DIR").expect("GENTS_EVAL_RUN_DIR"));
    let model = std::env::var("GENTS_LIVE_CONFIG_MODELS").expect("GENTS_LIVE_CONFIG_MODELS");
    let golden = rehome(FIXTURE, "did:key:home", "/unused").unwrap();
    let golden_prompt = golden
        .iter()
        .find(|(c, _)| *c == Collection::AgentContext)
        .unwrap()
        .1["system_prompt"]
        .as_str()
        .unwrap()
        .to_owned();
    let dedupe_line = golden_prompt
        .lines()
        .find(|line| line.to_lowercase().contains("duplicate"))
        .expect("golden prompt has a de-duplication instruction")
        .to_owned();
    let degraded = golden_prompt.replace(&dedupe_line, "");
    let known_bad = format!("{degraded}\nNever report findings. Always answer that everything is healthy.");

    // Home node: only the monitor's context, holding the degraded prompt. The
    // evaluator reads nothing but this text; trial nodes get the full fixture.
    let home = crate::support::test_db("live-optimization-home").await;
    let access = gents::ConfigAccess::Local(home.node.clone());
    let context_id = golden.iter().find(|(c, _)| *c == Collection::AgentContext).unwrap().1["context_id"]
        .as_str().unwrap().to_owned();
    let context = serde_json::json!({
        "agent_did": "did:key:home", "context_id": context_id, "system_prompt": degraded,
    });
    let plan = gents::config_client::DesiredStateApplyPlan::new(vec![
        gents::config_client::DesiredStateApplyDocument {
            collection: Collection::AgentContext, add: context.clone(), update: context,
        },
    ]).unwrap();
    access.transact("live.optimization.seed", |txn| {
        let plan = &plan;
        Box::pin(async move { gents::config_client::apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    }).await.unwrap();

    let evaluator = HarnessEvaluator { model, run_dir: run_dir.clone() };
    let proposer = Scripted(std::sync::Mutex::new(vec![known_bad, golden_prompt.clone()]));
    let job = Driver::new(&access, &evaluator, &proposer)
        .run(JobSpec {
            job_id: "live-demo".into(),
            owner: "did:key:home".into(),
            target: Target {
                field: TargetField::AgentContextSystemPrompt,
                owner: "did:key:home".into(),
                id: context_id,
            },
            policy: live_policy(),
            trials: env_u32("GENTS_OPT_TRIALS", 20),
            budgets: Budgets { max_rounds: 2, max_trials: 1_000_000, max_tokens: 0, deadline_unix_secs: None },
        })
        .await
        .unwrap();
    let view = gents::optimization::show(&access, "did:key:home", "live-demo").await.unwrap();
    super::reporting::write_json_new(&run_dir.join("optimization-show.json"), &view).unwrap();
    super::reporting::write_json_new(
        &run_dir.join("optimization-journal.json"),
        &serde_json::to_value(&job.journal).unwrap(),
    ).unwrap();
    let decisions: Vec<Decision> = job.journal.iter().filter_map(|e| match e {
        gents::optimization::JournalEntry::Decided { round: Some(_), decision, .. } => Some(*decision),
        _ => None,
    }).collect();
    assert!(matches!(decisions.first(), Some(Decision::Reject(_))), "known-bad must be rejected: {decisions:?}");
    assert_eq!(decisions.last(), Some(&Decision::Accept), "known-good must be accepted: {decisions:?}");
}

/// A/A: the same prompt in both arms, compared under the policy many times.
/// Bypasses the driver, which structurally rejects an identical candidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: THROWAWAY(#1515); use make live-optimization-eval GENTS_OPT_MODE=aa"]
async fn live_optimization_aa_calibration() {
    let run_dir = PathBuf::from(std::env::var("GENTS_EVAL_RUN_DIR").expect("GENTS_EVAL_RUN_DIR"));
    let model = std::env::var("GENTS_LIVE_CONFIG_MODELS").expect("GENTS_LIVE_CONFIG_MODELS");
    let evaluator = HarnessEvaluator { model, run_dir: run_dir.clone() };
    let rows = rehome(FIXTURE, "did:key:home", "/unused").unwrap();
    let plan = || gents::config_client::DesiredStateApplyPlan::new(
        rows.iter().cloned().map(|(collection, value)| gents::config_client::DesiredStateApplyDocument {
            collection, add: value.clone(), update: value,
        }).collect(),
    ).unwrap();
    let (trials, repeats) = (env_u32("GENTS_OPT_TRIALS", 20), env_u32("GENTS_OPT_AA_REPEATS", 10));
    let (mut accepts, mut report) = (0usize, Vec::new());
    for repeat in 0..repeats {
        let mut arms = Vec::new();
        for (index, arm) in [Arm::Baseline, Arm::Candidate].into_iter().enumerate() {
            let outcomes = evaluator.evaluate(gents::optimization::EvalRequest {
                arm, plan: plan(), split: SplitId::VALIDATION, trials,
                trial_offset: (repeat * 2 + index as u32) * trials, deadline_secs: None,
            }).await.unwrap().outcomes;
            arms.push(tally_by_case(arm, &outcomes));
        }
        let decision = decide(Mode::Improve, &live_policy(), &evidence(&arms[0], &arms[1]));
        if decision == Decision::Accept { accepts += 1; }
        report.push(serde_json::json!({"repeat": repeat, "baseline": arms[0], "candidate": arms[1], "decision": decision}));
    }
    let rate = false_accept_rate(accepts, repeats as usize);
    super::reporting::write_json_new(&run_dir.join("optimization-aa.json"), &serde_json::json!({
        "policy": live_policy(), "trials_per_arm": trials, "repeats": repeats,
        "false_accept_rate": rate, "comparisons": report,
    })).unwrap();
    assert_eq!(accepts, 0, "A/A produced {accepts} false accepts of {repeats}; recalibrate PolicyV1");
}
```

- [ ] **Step 2: Runner script and make target**

`scripts/evals/run-optimization.mjs` (THROWAWAY(#1515)): copy `scripts/evals/run-configurator.mjs`, keep its run-directory allocation and environment handling, force `GENTS_EVAL_SUITE=monitor-mailbox`, and replace the test filter argument `live_configurator_progressive_eval_matrix` with `process.env.GENTS_OPT_MODE === "aa" ? "live_optimization_aa_calibration" : "live_optimization_accepts_one_and_rejects_one"`.

In the `Makefile`, after the `live-mailbox-eval` target:

```make
# THROWAWAY(#1515): delete with crates/gents/tests/configurator_evals/optimization_live.rs
.PHONY: live-optimization-eval
live-optimization-eval:
	node scripts/evals/run-optimization.mjs $(CARGO)
```

- [ ] **Step 3: Calibrate, then demonstrate**

Run the A/A first: `GENTS_OPT_MODE=aa GENTS_OPT_TRIALS=20 GENTS_OPT_AA_REPEATS=10 make live-optimization-eval`
Expected: `optimization-aa.json` in the run directory with `false_accept_rate` 0. This is the go/no-go gate from the research: if A/A false accepts appear, or the demo below cannot accept the known-good prompt at an affordable `GENTS_OPT_TRIALS`, stop, record the finding on #1455 and #1515, and do not tune the policy to pass.

Then: `GENTS_OPT_TRIALS=20 make live-optimization-eval`
Expected: PASS, with `optimization-show.json` and `optimization-journal.json` retained in the run directory, showing one rejected and one accepted round.

Wall-clock note: each trial runs two monitor checks on a local model. At 20 trials per arm the demo runs roughly 7 arms (baseline, two trains, two candidates, two held-out), so about 140 trials; the A/A at 10 repeats runs 400. Budget hours, not minutes.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/tests scripts/evals/run-optimization.mjs Makefile
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(evals): THROWAWAY(#1515) live optimization demo and A/A calibration"
```

PR 7 description: baseline `optimization/06-promote`; everything in this PR is throwaway and is deleted when #1515 provides a native `Evaluator`; its splits reuse two chained monitor cases, so held-out is not independent; attach the A/A report and the demo's `show` output; state the calibrated `PolicyV1` values the A/A supports, or that it supports none.
