# Eval Runner (M2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land `gents::eval::runner`: a library that freezes a run, executes one fresh embedded trial home per case-trial through an injectable `TrialExecutor`, grades evidence with named checks, and writes fact-only `EvalTrial` and `EvalVerdict` rows, with resume by attempt and a NotEvidence breaker.

**Architecture:** Four stacked PRs. PR 1 moves the reusable #1512 test support into the library with no behavior change. PR 2 adds the pure core: executor types, the scripted executor, planning and grading, and the seed check registry. PR 3 adds the I/O loop: freezing, recording behind a `Recorder` trait, `run` and `resume`. PR 4 adds `EmbeddedExecutor` and the canary. Every outcome the runner writes uses the M1 vocabulary; M2 adds no Lean.

**Tech Stack:** Rust 1.97.1, tokio, `futures::stream::StreamExt::buffer_unordered`, DefraDB via `gents::defra_node::EmbeddedNode`, `serde_json`, `sha2`, `tempfile`.

**Spec:** `docs/superpowers/specs/2026-09-21-eval-runner-design.md` (all four sections approved and self-reviewed). Umbrella: `2026-09-21-eval-and-optimization-umbrella.md` section 6, milestone M2. Contract: `2026-09-21-eval-core-contract-design.md`.

**Depends on:** `eval/05-contract` at the pinned commit `595fd8bc2` (`gents::eval::{outcome, scoring, documents}`, the four schemas, `EvalDefinition`). This plan runs under `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`.

## Deviations from the spec, decided at planning

- **PR 1 bases on the pinned `eval/05-contract`, not on `main`.** The spec put PR 1 on `main`. The scout found that the classifier PR 1 moves would return `OutcomeKind`, which exists only on the M1 branch. PR 1 therefore stays free of `gents::eval` types (its classifier returns the same `&'static str` kinds `stages.rs` returns today; PR 2 maps them with `OutcomeKind::parse`) and bases on the pin like the others, so the stack is linear. It can be rebased onto `main` later without change.
- **The moved support keeps one-line wrappers in `tests/`.** `test_db` is compiled into eight test binaries through `mod support;`. The bodies move into the library; `TestDb`, `retained_trial_db`, `observe_request`, `retain_request_evidence` and `boot_d4f_agent_with_options` become thin wrappers over the library calls. Nothing is duplicated; the wrappers are the deletion.
- **The seed check registry lands in PR 2, not PR 4.** `grade.rs` cannot be tested without a registry.
- **Crash simulation is a `Recorder` trait, not a `ConfigAccess` wrapper.** `ConfigAccess` is an enum. `record.rs` defines `trait Recorder` with a `DocumentRecorder` over `ConfigAccess`; tests wrap it with a `FaultingRecorder` that fails the Nth write.
- **`CellSpec` carries `inference_profile_id`, not a full binding** (that is how M1 landed). The runner resolves the profile and its backend from the launching home and copies both documents into the trial.

## Global Constraints

- Base every branch on `eval/05-contract` at `595fd8bc2`. Stack: `eval/10-runner-support` → `eval/11-runner-core` → `eval/12-runner-loop` → `eval/13-runner-embedded`. Each PR targets its parent. Create worktrees from the main checkout with `make worktree BRANCH=<branch> DIR=/Users/iron-arch-mage/Repos/Source/gents-<dir> BASE=<parent>`.
- `gents::eval::{outcome, scoring, documents}` and `EvalDefinition` are frozen. A task that needs a change there stops and messages the orchestrator.
- **rustfmt is a CI gate.** `cargo fmt --all --check` exits 0 before every commit. `mod` and `use` lines are in rustfmt order; where this plan says "add a module", place it where rustfmt sorts it.
- Foreground commands only: never background a command, never `sleep`, never poll. `CARGO_BUILD_JOBS=4`. Redirect long output to a log file and grep it.
- Escape every interpolated GraphQL string with `graphql::escape_graphql_string()`. Never emit `[]` in a mutation. `tracing`, never `println!`. No `unwrap`/`expect` on input-dependent paths outside tests.
- Nothing protected enters a `TrialSpec`: no check names or params, tiers, splits, `case_id`, other cases.
- `EvalRun.origin` and `EvalTrial.completion` carry only ids, closed enums, counts, digests and timestamps. The types in `documents.rs` enforce it; never widen them.
- `TrialExecutor::execute` never returns `Err`. Every fault is evidence with an `OutcomeKind`.
- The runner's own I/O failures propagate as `Err`; they are never written as `infrastructure` verdicts.
- `provider_reason` is present whenever a stage's `failure_kind` is `Provider` (M1 deferred this to the runner).
- No Lean is added. Known pre-existing failure, unrelated: conformance test `generated_r5_cross_principal_cases_drive_production_dispatch`.
- Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit ...`, message ending `Co-Authored-By: <implementing model> <noreply@anthropic.com>`. Never push, never open a PR.

## File Structure

| File | PR | Responsibility |
|---|---|---|
| `crates/gents/src/eval/runner/mod.rs` | 1 (stub), 3 | `run`, `resume`, `RunOutcome`, the loop and breaker |
| `crates/gents/src/eval/runner/embedded/mod.rs` | 1 | module root; PR 4 adds `EmbeddedExecutor` here |
| `crates/gents/src/eval/runner/embedded/home.rs` | 1 | `EmbeddedHome`, `boot_runtime`, `RunningRuntime` |
| `crates/gents/src/eval/runner/embedded/observe.rs` | 1 | `await_terminal`, `collect_request_evidence`, `classify_request_outcome` |
| `crates/gents/src/eval/runner/executor.rs` | 2 | `TrialExecutor`, `Isolation`, `TrialSpec`, `Capture`, `TrialEvidence`, `StageEvidence`, `TrialLocator`, `evidence_digest` |
| `crates/gents/src/eval/runner/scripted.rs` | 2 | `ScriptedExecutor` |
| `crates/gents/src/eval/runner/plan.rs` | 2 | `plan`, `PlannedTrial`, `trial_id_for` |
| `crates/gents/src/eval/runner/grade.rs` | 2 | `grade`, `VerdictRow` |
| `crates/gents/src/eval/checks/mod.rs` | 2 | `Check` trait, `CheckRegistry`, `CHECK_REGISTRY_VERSION` |
| `crates/gents/src/eval/checks/captured_rows_count.rs` | 2 | the seed check |
| `crates/gents/src/eval/runner/freeze.rs` | 3 | `RunRequest`, `CellSource`, `freeze` |
| `crates/gents/src/eval/runner/record.rs` | 3 | `Recorder`, `DocumentRecorder` |
| `crates/gents/src/eval/runner/embedded/executor.rs` | 4 | `EmbeddedExecutor` |
| `crates/gents/tests/eval_runner_canary.rs` | 4 | the canary and the live smoke |
| `crates/gents/tests/support/mod.rs`, `support/live_inference.rs`, `configurator_evals/runner.rs`, `configurator_evals/stages.rs` | 1 | thin wrappers over the library |

---

## PR 1: Move the #1512 support into the library

Branch: `eval/10-runner-support`, base `eval/05-contract` at `595fd8bc2`. No behavior change. The gate is that every existing test binary still compiles and `make live-configurator-eval` still runs (it needs a live provider; run `cargo test -p gents --test e2e_configurator --no-run` to prove it compiles).

### Task 1: `EmbeddedHome` and `boot_runtime`

**Files:**
- Create: `crates/gents/src/eval/mod.rs` gets `pub mod runner;` (rustfmt position); create `crates/gents/src/eval/runner/mod.rs`, `crates/gents/src/eval/runner/embedded/mod.rs`, `crates/gents/src/eval/runner/embedded/home.rs`
- Modify: `crates/gents/tests/support/mod.rs:1-46,110-135` (`TestDb`, `test_db`, `test_db_in`), `crates/gents/tests/support/live_inference.rs:206-217` (`boot_d4f_agent_with_options`), `crates/gents/tests/configurator_evals/runner.rs:878-886` (`retained_trial_db`)
- Modify: `crates/gents/Cargo.toml` if `tempfile` is a dev-dependency only: move it to `[dependencies]` (it is already in the tree).

**Interfaces:**
- Produces:
  ```rust
  pub struct EmbeddedHome {
      pub node: Arc<EmbeddedNode>,
      pub identity: Arc<dyn AgentIdentity>,
      did: String,
      path: PathBuf,
      tempdir: Option<TempDir>,   // Some for temp homes, None for retained
  }
  impl EmbeddedHome {
      pub async fn create_temp(prefix: &str) -> Result<Self>;
      pub async fn create_retained(dir: &Path) -> Result<Self>;   // dir must not exist; created here
      pub async fn open_retained(dir: &Path) -> Result<Self>;     // reopens node.key + data
      pub fn did(&self) -> &str;
      pub fn path(&self) -> &Path;
      pub async fn reopen(&mut self) -> Result<()>;               // rebuilds the node (crash simulation)
  }
  pub struct RunningRuntime { shutdown: watch::Sender<bool>, handle: JoinHandle<Result<()>>, pub agent_did: String }
  impl RunningRuntime { pub async fn shutdown(self) -> Result<()>; }
  pub async fn boot_runtime(home: &EmbeddedHome, identity: Arc<dyn AgentIdentity>, options: DocumentRuntimeOptions) -> Result<(RunningRuntime, Gents)>;
  ```

- [ ] **Step 1: Write the failing test** in `crates/gents/src/eval/runner/embedded/home.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_temp_home_has_a_node_an_identity_and_runtime_schemas() {
        let home = EmbeddedHome::create_temp("eval-home").await.unwrap();
        assert!(home.did().starts_with("did:"));
        assert!(home.path().join("node.key").exists());
        // ensure_runtime_schemas ran: AgentRequest is queryable.
        let response = home.node.execute("query { AgentRequest { _docID } }").await.unwrap();
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn a_retained_home_survives_reopen_and_keeps_its_did() {
        let dir = tempfile::tempdir().unwrap();
        let retained = dir.path().join("trial-home");
        let mut home = EmbeddedHome::create_retained(&retained).await.unwrap();
        let did = home.did().to_string();
        home.reopen().await.unwrap();
        assert_eq!(home.did(), did);
        drop(home);
        let reopened = EmbeddedHome::open_retained(&retained).await.unwrap();
        assert_eq!(reopened.did(), did);
    }

    #[tokio::test]
    async fn create_retained_refuses_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let error = EmbeddedHome::create_retained(dir.path()).await.unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
    }
}
```

- [ ] **Step 2: Run it to see it fail**: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::embedded::home` → compile error, `EmbeddedHome` not found.

- [ ] **Step 3: Implement `home.rs`**, moving the body of `test_db_in` (support/mod.rs:118-135) and `boot_d4f_agent_with_options` (live_inference.rs:206-217):

```rust
//! Embedded trial homes: one DefraDB data directory, one identity, one runtime.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::defra_node::EmbeddedNode;
use crate::{ensure_runtime_schemas, AgentIdentity, DocumentRuntimeOptions, Gents, KeyIdentity};

pub struct EmbeddedHome { /* as in Interfaces */ }

impl EmbeddedHome {
    pub async fn create_temp(prefix: &str) -> Result<Self> {
        let tempdir = tempfile::Builder::new().prefix(&format!("gents-{prefix}-")).tempdir()?;
        let mut home = Self::open_at(tempdir.path().to_path_buf()).await?;
        home.tempdir = Some(tempdir);
        Ok(home)
    }
    pub async fn create_retained(dir: &Path) -> Result<Self> {
        ensure!(!dir.exists(), "trial home {} already exists", dir.display());
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        Self::open_at(dir.to_path_buf()).await
    }
    pub async fn open_retained(dir: &Path) -> Result<Self> {
        ensure!(dir.join("node.key").exists(), "no trial home at {}", dir.display());
        Self::open_at(dir.to_path_buf()).await
    }
    async fn open_at(path: PathBuf) -> Result<Self> {
        let identity: Arc<dyn AgentIdentity> =
            Arc::new(KeyIdentity::load_or_create(path.join("node.key"), None).context("node identity")?);
        let did = identity.did().to_string();
        let node = Arc::new(
            EmbeddedNode::builder().data_path(&path).with_node_identity_did(&did).build().await.context("embedded node")?,
        );
        ensure_runtime_schemas(&node).await.context("runtime schemas")?;
        Ok(Self { node, identity, did, path, tempdir: None })
    }
    pub fn did(&self) -> &str { &self.did }
    pub fn path(&self) -> &Path { &self.path }
    pub async fn reopen(&mut self) -> Result<()> {
        let node = EmbeddedNode::builder().data_path(&self.path).with_node_identity_did(&self.did).build().await?;
        self.node = Arc::new(node);
        Ok(())
    }
}

pub struct RunningRuntime { /* as in Interfaces */ }
impl RunningRuntime {
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        self.handle.await.context("runtime task")?
    }
}

pub async fn boot_runtime(home: &EmbeddedHome, identity: Arc<dyn AgentIdentity>, options: DocumentRuntimeOptions) -> Result<(RunningRuntime, Gents)> {
    let agent = Gents::from_default_behavior_documents(home.node.clone(), identity, options).await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.clone().run(shutdown_rx));
    crate::eval::runner::embedded::observe::wait_for_runtime_ready(home.node.as_ref(), &agent_did).await;
    Ok((RunningRuntime { shutdown, handle, agent_did }, agent))
}
```

`wait_for_runtime_ready` is moved from `live_inference.rs` in Task 2; in this task, move it into `home.rs` as `pub(crate) async fn wait_for_runtime_ready` and have `live_inference.rs` call it. If `simulate_process_crash` in `TestDb` needs `process_generation`, keep that counter on `TestDb` and have it call `home.reopen()`.

- [ ] **Step 4: Rewire the wrappers.** `TestDb` becomes:

```rust
pub struct TestDb {
    home: gents::eval::runner::embedded::EmbeddedHome,
    pub node: Arc<EmbeddedNode>,
    pub node_identity: Arc<dyn AgentIdentity>,
    pub process_generation: u64,
}
pub async fn test_db(name: &str) -> TestDb { wrap(EmbeddedHome::create_temp(name).await.expect("embedded home")) }
pub async fn test_db_in(tempdir: TempDir) -> TestDb { /* open_retained on tempdir.path(), keep the TempDir alive inside TestDb: add a `_tempdir: Option<TempDir>` field */ }
```

`data_path()` returns `self.home.path()`; `simulate_process_crash` calls `self.home.reopen().await?`, sets `self.node = self.home.node.clone()`, bumps `process_generation`. `retained_trial_db` calls `EmbeddedHome::create_retained(&artifacts.join(format!("home-{}", uuid::Uuid::new_v4())))` and wraps. `boot_d4f_agent_with_options` calls `boot_runtime` and wraps the result in `BootedAgent::new(...)`; keep `BootedAgent` in the test file.

- [ ] **Step 5: Verify** `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner` → 3 passed; `cargo check -p gents --all-targets` → 0 errors; `cargo test -p gents --test misc` (a small binary that uses `test_db`) → passes; `cargo fmt --all --check`.

- [ ] **Step 6: Commit** `refactor(eval): EmbeddedHome and boot_runtime move trial-home support into the library`.

### Task 2: `await_terminal`, `collect_request_evidence`, `classify_request_outcome`

**Files:**
- Create: `crates/gents/src/eval/runner/embedded/observe.rs`
- Modify: `crates/gents/tests/configurator_evals/stages.rs:841-900` (`classify_request_outcome`), `:1037-1180` (`observe_request`), `:1181-1230` (`retain_request_evidence`, `retain_inference_evidence`); `crates/gents/tests/support/live_inference.rs` (`wait_for_request_terminal`, `wait_for_runtime_ready`)

**Interfaces:**
- Produces:
  ```rust
  pub struct TerminalObservation { pub terminal_state: RequestLifecycleState, pub session_id: Option<String>, pub interrupted_on_deadline: bool, pub elapsed: Duration }
  pub async fn await_terminal(node: &EmbeddedNode, request_id: &str, deadline: Duration, grace: Duration, poll: Duration) -> Result<TerminalObservation>;
  #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)] pub struct ToolCallEvidence { pub tool_name: String, pub status: Option<String>, pub lifecycle_state: Option<String>, pub tool_failure_class: Option<String>, pub started_at: Option<String>, pub completed_at: Option<String>, pub args: Value, pub result: Value }
  pub struct InferenceCallEvidence { pub call_seq: i64, pub call_state: Option<String>, pub failure_reason: Option<String>, pub prompt_tokens: Option<u64>, pub completion_tokens: Option<u64> }
  pub struct MessageEvidence { pub role: String, pub content: String, pub created_at: Option<String> }
  pub struct ResponseEvidence { pub status: Option<String>, pub error_message: Option<String> }
  pub struct RequestEvidence { pub messages: Vec<MessageEvidence>, pub tool_calls: Vec<ToolCallEvidence>, pub inference_calls: Vec<InferenceCallEvidence>, pub responses: Vec<ResponseEvidence>, pub failure_reason: Option<String> }
  pub async fn collect_request_evidence(node: &EmbeddedNode, request_id: &str) -> Result<RequestEvidence>;
  /// Returns the same string kinds stages.rs returns today: "deadline" | "tool" | "provider" | "runtime" | "unknown"; None means completed.
  pub fn classify_request_outcome(terminal_state: RequestLifecycleState, interrupted_on_deadline: bool, evidence: &RequestEvidence) -> Option<&'static str>;
  ```
  The `AgentMessage` query that `observe_request` runs inline today becomes part of `collect_request_evidence`.

- [ ] **Step 1: Write the failing tests** in `observe.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::embedded::EmbeddedHome;
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    fn evidence(inference_failed: bool, tool_failed: bool, failure_reason: Option<&str>) -> RequestEvidence {
        RequestEvidence {
            messages: vec![],
            tool_calls: if tool_failed { vec![ToolCallEvidence { tool_name: "t".into(), status: Some("failed".into()), lifecycle_state: None, tool_failure_class: None, started_at: None, completed_at: None, args: Value::Null, result: Value::Null }] } else { vec![] },
            inference_calls: if inference_failed { vec![InferenceCallEvidence { call_seq: 1, call_state: Some("failed".into()), failure_reason: None, prompt_tokens: None, completion_tokens: None }] } else { vec![] },
            responses: vec![],
            failure_reason: failure_reason.map(str::to_string),
        }
    }

    #[test]
    fn classification_matches_the_stages_table() {
        use RequestLifecycleState as S;
        assert_eq!(classify_request_outcome(S::Interrupted, true, &evidence(false, false, None)), Some("deadline"));
        assert_eq!(classify_request_outcome(S::Completed, false, &evidence(false, false, None)), None);
        assert_eq!(classify_request_outcome(S::Failed, false, &evidence(false, false, Some("invalid_tool_call_budget_exhausted"))), Some("tool"));
        assert_eq!(classify_request_outcome(S::Failed, false, &evidence(true, true, None)), Some("unknown"));
        assert_eq!(classify_request_outcome(S::Failed, false, &evidence(true, false, None)), Some("provider"));
        assert_eq!(classify_request_outcome(S::Failed, false, &evidence(false, true, None)), Some("tool"));
        assert_eq!(classify_request_outcome(S::Failed, false, &evidence(false, false, None)), Some("runtime"));
        assert_eq!(classify_request_outcome(S::Dead, false, &evidence(false, false, None)), Some("runtime"));
    }

    #[tokio::test]
    async fn await_terminal_returns_when_the_request_is_already_terminal() {
        let home = EmbeddedHome::create_temp("observe").await.unwrap();
        let request_id = "req-terminal";
        let mutation = format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "{}", agent_did: "{}", lifecycle_state: "completed", session_id: "s-1", content: "x", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(request_id), crate::graphql::escape_graphql_string(home.did())
        );
        // Add whatever non-null fields the AgentRequest SDL requires; read crates/gents-schemas/schemas/agent/agent_request.graphql.
        let response = home.node.execute(&mutation).await.unwrap();
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let observed = await_terminal(&home.node, request_id, Duration::from_secs(5), Duration::from_secs(1), Duration::from_millis(50)).await.unwrap();
        assert_eq!(observed.terminal_state, RequestLifecycleState::Completed);
        assert_eq!(observed.session_id.as_deref(), Some("s-1"));
        assert!(!observed.interrupted_on_deadline);
    }

    #[tokio::test]
    async fn await_terminal_interrupts_on_the_deadline_and_reports_it() {
        let home = EmbeddedHome::create_temp("observe-deadline").await.unwrap();
        // create a request left in "pending"; nothing will drive it
        // ... same mutation with lifecycle_state: "pending" and request_id "req-stuck"
        let observed = await_terminal(&home.node, "req-stuck", Duration::from_millis(200), Duration::from_millis(300), Duration::from_millis(50)).await.unwrap();
        assert!(observed.interrupted_on_deadline);
        // No runtime is running, so the observer never flips it; the state stays non-terminal and the grace expires.
        assert_ne!(observed.terminal_state, RequestLifecycleState::Completed);
        // interrupt_request latched the flag:
        let q = format!(r#"query {{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ interrupt_requested_at }} }}"#, crate::graphql::escape_graphql_string("req-stuck"));
        let response = home.node.execute(&q).await.unwrap();
        assert!(response.data.to_string().contains("interrupt_requested_at"));
    }
}
```

When the grace expires with the request still non-terminal, `await_terminal` returns `Ok` with `terminal_state` set to the last observed state parsed through `RequestLifecycleState::parse` (or `Interrupted` if the observed string is not terminal), and `interrupted_on_deadline: true`. State this in the doc comment; it mirrors `nonterminal_after_interrupt` today.

- [ ] **Step 2: Run to see it fail** (compile error).

- [ ] **Step 3: Implement `observe.rs`** by moving the bodies: `await_terminal` from `observe_request` (poll `AgentRequest { lifecycle_state session_id }` every `poll`; on `deadline` call `crate::interrupt::interrupt_request(node, request_id)` once, then keep polling until `deadline + grace`), `collect_request_evidence` from `retain_request_evidence` + `retain_inference_evidence` + the inline `AgentMessage` query (same filters and fields, into the structs above instead of files), `classify_request_outcome` verbatim from `stages.rs:841` with `terminal_state` as `RequestLifecycleState` instead of `&str`. Move `wait_for_runtime_ready` here as `pub async fn`.

- [ ] **Step 4: Rewire the wrappers.** `stages.rs::observe_request` calls `await_terminal` with `stage_timeout()?` and `Duration::from_secs(30)`, then `collect_request_evidence`, then `classify_request_outcome`, and builds the same `StageResult` it builds today. `retain_request_evidence` calls `collect_request_evidence` and writes the same two JSON files from the structs (`serde_json::to_vec_pretty`). `live_inference::wait_for_request_terminal` calls `await_terminal`. Delete the moved bodies.

- [ ] **Step 5: Verify** `cargo test -p gents --lib eval::runner` → 6 passed; `cargo check -p gents --all-targets`; `cargo test -p gents --test e2e_configurator --no-run` compiles; `cargo test -p gents --test e2e_lifecycle interruption` (exercises interrupt) passes; `cargo fmt --all --check`.

- [ ] **Step 6: Commit** `refactor(eval): await_terminal, request evidence and outcome classification move into the library`.

---

## PR 2: The pure core

Branch: `eval/11-runner-core`, base `eval/10-runner-support`.

### Task 3: Executor types, `ScriptedExecutor`, `plan`

**Files:**
- Create: `crates/gents/src/eval/runner/executor.rs`, `crates/gents/src/eval/runner/scripted.rs`, `crates/gents/src/eval/runner/plan.rs`
- Modify: `crates/gents/src/eval/runner/mod.rs` (module declarations and `pub use`)

**Interfaces:**
- Consumes: `gents::eval::{OutcomeKind, ProviderReason, TrialIdentity, TrialRecord, RunOrigin, CellSpec}`, `gents_protocol::request_lifecycle::RequestLifecycleState`, `crate::eval::runner::embedded::observe::{MessageEvidence, ToolCallEvidence, InferenceCallEvidence, ResponseEvidence}`.
- Produces (`executor.rs`):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation { Embedded, Process }

/// Everything a trial may contain. Never checks, tiers, splits, case ids or other cases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialSpec {
    pub trial_id: String,
    pub pack_dir: PathBuf,           // materialized pack for this cell
    pub pack_digest: String,
    pub behavior_id: String,
    pub inference: InferenceBinding, // profile + backend documents copied verbatim, plus seed
    pub fixtures: TrialFixtures,
    pub stages: Vec<StageSpec>,
    pub captures: Vec<Capture>,
    pub home_dir: PathBuf,           // <run dir>/trials/<trial_id>
    /// Populated only when `executor.wants_script_key()`; the one field that carries `case_id`.
    pub script_key: Option<crate::eval::runner::scripted::ScriptKey>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceBinding { pub profile: Value, pub backend: Value, pub seed: i64 }
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialFixtures { pub schemas: Vec<String>, pub documents: Vec<FixtureDocument>, pub files: Vec<FixtureFile> }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureDocument { pub collection: String, pub document: Value }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureFile { pub path: String, pub contents: Vec<u8> }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSpec { pub stage_id: String, pub prompt: String, pub deadline_secs: u64 }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Capture {
    Documents { name: String, collection: String, filter: Value, fields: Vec<String> },
    File { name: String, glob: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialLocator { pub trial_agent_did: String, pub session_id: String, pub home_hint: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageEvidence {
    pub stage_id: String,
    pub request_id: Option<String>,
    pub terminal_state: Option<RequestLifecycleState>,
    pub failure_kind: Option<OutcomeKind>,      // None == the stage ran to Completed
    pub provider_reason: Option<ProviderReason>,
    pub messages: Vec<MessageEvidence>,
    pub tool_calls: Vec<ToolCallEvidence>,
    pub inference_calls: Vec<InferenceCallEvidence>,
    pub captures: BTreeMap<String, CaptureResult>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureResult { Documents { rows: Vec<Value> }, Files { files: Vec<FileRef> } }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef { pub path: String, pub sha256: String, pub bytes: u64 }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialEvidence {
    pub locator: TrialLocator,
    pub stages: Vec<StageEvidence>,      // only stages that were submitted
    pub usage: TrialUsage,               // from gents::eval
    pub anchor: Anchor,                  // from gents::eval
    pub evidence_digest: String,
}
impl TrialEvidence {
    /// SHA-256 over canonical JSON of (stages, usage, anchor); locator excluded.
    pub fn digest(stages: &[StageEvidence], usage: &TrialUsage, anchor: &Anchor) -> String;
    pub fn infrastructure(locator: TrialLocator) -> Self;   // no stages, null usage, empty anchor
}

#[async_trait::async_trait]
pub trait TrialExecutor: Send + Sync {
    fn isolation(&self) -> Isolation;
    /// Only the scripted executor returns true; the loop then fills `TrialSpec.script_key`.
    fn wants_script_key(&self) -> bool { false }
    /// Creates the trial's identity (and, for embedded, its home) so the `EvalTrial` row can be
    /// written before execution. Never returns Err: a failed provisioning returns a locator whose
    /// `trial_agent_did` is "did:unprovisioned", and `execute` then reports `infrastructure`.
    async fn provision(&self, spec: &TrialSpec) -> TrialLocator;
    /// Never returns Err. Observes `cancel`: on cancellation, interrupts the current request and
    /// returns what it has.
    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence;
    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence>;
}
```

  Canonical JSON: `serde_json::to_vec` of a `serde_json::Value` produced by `serde_json::to_value` — `Value::Object` is a `BTreeMap` only when the `preserve_order` feature is off; check `Cargo.toml`; if it is on, sort keys recursively with a small helper before serializing. An `infrastructure` evidence has `stages: []`, `usage: TrialUsage::default()`, `anchor: Anchor { terminal_states: vec![], requests: 0, inference_calls: 0 }`, and its digest is the digest of those.

- Produces (`scripted.rs`):

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScriptKey { pub cell_label: String, pub case_id: String, pub trial_index: u32, pub attempt: u32 }
pub struct ScriptedExecutor { table: HashMap<ScriptKey, TrialEvidence>, default: Option<TrialEvidence>, pub calls: Mutex<Vec<ScriptKey>> }
impl ScriptedExecutor {
    pub fn new() -> Self;
    pub fn with(mut self, key: ScriptKey, evidence: TrialEvidence) -> Self;
    pub fn with_default(mut self, evidence: TrialEvidence) -> Self;
    /// Builders for tests: one completed stage with a documents capture of `rows`.
    pub fn passed_evidence(did: &str, stage_id: &str, capture_name: &str, rows: Vec<Value>) -> TrialEvidence;
    pub fn not_evidence(did: &str) -> TrialEvidence;   // infrastructure
    pub fn failed_evidence(did: &str, stage_id: &str, kind: OutcomeKind, reason: Option<ProviderReason>) -> TrialEvidence;
}
```

  `script_key` is the only field that carries `case_id`. The loop fills it only when `wants_script_key()` is true, which only `ScriptedExecutor` returns; `EmbeddedExecutor` never sees one.

- Produces (`plan.rs`):

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedTrial { pub trial_id: String, pub cell_id: String, pub cell_label: String, pub case_id: String, pub trial_index: u32, pub attempt: u32, pub seed: i64 }
pub fn trial_id_for(run_id: &str, cell_id: &str, case_id: &str, trial_index: u32, attempt: u32) -> String; // sha256 hex of the five joined by '\n'
pub fn plan(origin: &RunOrigin, run_id: &str, existing: &[TrialRecord]) -> Vec<PlannedTrial>;
```

- [ ] **Step 1: Write the failing tests** in `plan.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Anchor, CellSpec, DefinitionRef, RunOrigin, SubjectRef, TrialCompletion, TrialIdentity, TrialRecord, TrialUsage, DENOMINATOR_POLICY_V1, TAXONOMY_VERSION};
    use crate::document_config::eval_definition::EvalSplit;

    fn origin(cells: &[&str], cases: &[&str], trials_per_case: u32) -> RunOrigin {
        RunOrigin {
            definition: DefinitionRef { definition_id: "d".into(), comparability_version: 1, digest: "x".into() },
            split: EvalSplit::Validation,
            case_ids: cases.iter().map(|c| c.to_string()).collect(),
            cells: cells.iter().map(|c| CellSpec { cell_id: c.to_string(), label: c.to_string(), subject: SubjectRef { pack_digest: "p".into(), behavior_id: "b".into() }, inference_profile_id: "prof".into() }).collect(),
            trials_per_case, seed_base: 100, deadline_secs: None, concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(), taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 3, check_registry_version: "0".into(), source_commit: "c".into(), source_dirty: false, purpose: "eval".into(),
        }
    }
    fn record(cell: &str, case: &str, index: u32, attempt: u32, completed: bool) -> TrialRecord {
        TrialRecord {
            identity: TrialIdentity { trial_id: trial_id_for("r", cell, case, index, attempt), run_id: "r".into(), cell_id: cell.into(), case_id: case.into(), trial_index: index, attempt, trial_agent_did: "did:x".into(), session_id: "s".into(), seed: 100 + index as i64, home_hint: None },
            created_at: "t".into(),
            completion: completed.then(|| TrialCompletion { ended_at: "t".into(), stages: vec![], usage: TrialUsage::default(), anchor: Anchor { terminal_states: vec![], requests: 0, inference_calls: 0 } }),
        }
    }

    #[test]
    fn a_fresh_run_plans_the_full_matrix_pairs_adjacent() {
        let planned = plan(&origin(&["base", "cand"], &["b-case", "a-case"], 2), "r", &[]);
        let keys: Vec<(u32, &str, &str)> = planned.iter().map(|p| (p.trial_index, p.case_id.as_str(), p.cell_id.as_str())).collect();
        assert_eq!(keys, vec![(0,"a-case","base"),(0,"a-case","cand"),(0,"b-case","base"),(0,"b-case","cand"),(1,"a-case","base"),(1,"a-case","cand"),(1,"b-case","base"),(1,"b-case","cand")]);
        assert!(planned.iter().all(|p| p.attempt == 1));
        assert_eq!(planned[0].seed, 100); assert_eq!(planned[4].seed, 101);
    }

    #[test]
    fn completed_slots_are_skipped_and_abandoned_slots_get_the_next_attempt() {
        let existing = vec![record("base","a-case",0,1,true), record("cand","a-case",0,1,false), record("cand","a-case",0,2,false)];
        let planned = plan(&origin(&["base","cand"], &["a-case"], 1), "r", &existing);
        assert_eq!(planned.len(), 1);
        assert_eq!((planned[0].cell_id.as_str(), planned[0].attempt), ("cand", 3));
    }

    #[test]
    fn trial_ids_are_deterministic_and_distinct_per_attempt() {
        assert_eq!(trial_id_for("r","c","k",0,1), trial_id_for("r","c","k",0,1));
        assert_ne!(trial_id_for("r","c","k",0,1), trial_id_for("r","c","k",0,2));
        assert_eq!(trial_id_for("r","c","k",0,1).len(), 64);
    }

    #[test]
    fn a_completed_later_attempt_wins_over_an_abandoned_earlier_one() {
        let existing = vec![record("base","a-case",0,1,false), record("base","a-case",0,2,true)];
        assert!(plan(&origin(&["base"], &["a-case"], 1), "r", &existing).is_empty());
    }
}
```

  And in `executor.rs`:

```rust
#[test]
fn the_evidence_digest_ignores_the_locator_and_is_stable() {
    let a = ScriptedExecutor::passed_evidence("did:a", "s1", "items", vec![serde_json::json!({"k": 1})]);
    let mut b = a.clone(); b.locator.home_hint = Some("elsewhere".into()); b.locator.trial_agent_did = "did:b".into();
    assert_eq!(a.evidence_digest, b.evidence_digest);
    let c = ScriptedExecutor::passed_evidence("did:a", "s1", "items", vec![serde_json::json!({"k": 2})]);
    assert_ne!(a.evidence_digest, c.evidence_digest);
}
```

  And in `scripted.rs`:

```rust
#[tokio::test]
async fn the_scripted_executor_returns_the_table_entry_or_the_default_and_records_calls() {
    let key = ScriptKey { cell_label: "base".into(), case_id: "k".into(), trial_index: 0, attempt: 1 };
    let ex = ScriptedExecutor::new().with(key.clone(), ScriptedExecutor::not_evidence("did:x")).with_default(ScriptedExecutor::passed_evidence("did:x","s","items",vec![]));
    assert!(ex.wants_script_key());
    let mut spec = TrialSpec::empty_for_tests("t1"); spec.script_key = Some(key.clone());
    assert_eq!(ex.execute(&spec, CancellationToken::new()).await.stages.len(), 0);
    spec.script_key = Some(ScriptKey { attempt: 2, ..key.clone() });
    assert_eq!(ex.execute(&spec, CancellationToken::new()).await.stages.len(), 1);
    assert_eq!(ex.calls.lock().unwrap().len(), 2);
}
```

  `TrialSpec::empty_for_tests(trial_id)` is `#[cfg(test)]` or `#[doc(hidden)] pub` (it is needed by PR 3's tests in the same crate, so `pub(crate)`).

- [ ] **Step 2: Run to see it fail.**
- [ ] **Step 3: Implement.** `plan`: iterate `trial_index in 0..trials_per_case`, then `case_ids` sorted, then `cells` in origin order; for each slot, find existing records with that `(cell_id, case_id, trial_index)`; skip if any has `completion.is_some()`; else `attempt = max(existing attempts) + 1` (1 if none); `seed = seed_base + trial_index as i64`. `trial_id_for`: `sha2::Sha256` over `format!("{run_id}\n{cell_id}\n{case_id}\n{trial_index}\n{attempt}")`, hex. `ScriptedExecutor::provision` returns the table entry's (or default's) locator; `execute`: look up `spec.script_key`, push to `calls`, return the table entry cloned, else the default cloned, else `TrialEvidence::infrastructure(...)`. `isolation()` returns `Embedded`.
- [ ] **Step 4: Verify** `cargo test -p gents --lib eval::runner` → all pass; `cargo fmt --all --check`.
- [ ] **Step 5: Commit** `feat(eval): TrialExecutor seam, scripted executor and trial planning`.

### Task 4: `grade` and the seed check registry

**Files:**
- Create: `crates/gents/src/eval/checks/mod.rs`, `crates/gents/src/eval/checks/captured_rows_count.rs`, `crates/gents/src/eval/runner/grade.rs`
- Modify: `crates/gents/src/eval/mod.rs` (`pub mod checks;`), `runner/mod.rs`

**Interfaces:**
- Consumes: `EvalCase`, `EvalStage`, `EvalCheckRef`, `EvalTier` from `document_config::eval_definition`; `OutcomeKind`, `ProviderReason`, `VerdictDraft`; `StageEvidence`, `TrialEvidence`, `CaptureResult`.
- Produces (`checks/mod.rs`):

```rust
pub const CHECK_REGISTRY_VERSION: &str = "1";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckVerdict { pub kind: OutcomeKind, pub score_bp: Option<u32>, pub raw: Value, pub feedback: Option<String> }
pub trait Check: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    /// Pure. `params` is the check ref's params; `stage` is the stage's evidence.
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict;
}
pub struct CheckRegistry { checks: BTreeMap<&'static str, Box<dyn Check>> }
impl CheckRegistry {
    pub fn builtin() -> Self;                       // registers captured_rows_count
    pub fn get(&self, name: &str) -> Option<&dyn Check>;
    pub fn names(&self) -> Vec<&'static str>;
}
```

- Produces (`captured_rows_count.rs`): params `{ "name": "<capture name>", "min": <u64>, "max": <u64 | absent> }`. Verdict: `Passed` with `score_bp: Some(10000)` when `min <= rows.len() <= max`; `ModelAcceptance` with `Some(0)` otherwise; `Grader` with `None` and `raw.reason_code = "missing_capture"` when the capture name is absent or not a documents capture; `Grader` with `raw.reason_code = "bad_params"` when params do not parse. `raw` always carries `{ "reason_code", "detail", "count" }`.

- Produces (`grade.rs`):

```rust
pub struct VerdictRow { pub stage_id: String, pub check: String, pub check_version: String, pub tier: EvalTier, pub kind: OutcomeKind, pub provider_reason: Option<ProviderReason>, pub score_bp: Option<u32>, pub weight: u32, pub raw: Value, pub feedback: Option<String> }
/// One row per (stage, check) of the case. A stage the evidence lacks, or whose failure_kind is
/// Some, yields rows carrying that stage's kind (or SkippedPrerequisite when it never ran).
pub fn grade(case: &EvalCase, evidence: &TrialEvidence, registry: &CheckRegistry) -> Vec<VerdictRow>;
```

  Rules: for each `case.stages[i]`, find `evidence.stages` with the same `stage_id`. If absent → every check of that stage gets `SkippedPrerequisite`, `score_bp: Some(0)`, `raw: {"reason_code":"skipped_prerequisite"}`. If present with `failure_kind: Some(k)` → every check gets `k` and the stage's `provider_reason`, `score_bp: Some(0)` when `classify(k, reason) == Fail` else `None`. If present and completed → run each check; an unknown check name yields `Grader`, `None`, `raw.reason_code = "unknown_check"`. `feedback` is passed through only for `tier == Development` or when the caller later filters by split (the runner does: `append_verdict` refuses feedback off train). `weight` copies the check ref's weight. `check_version` is the check's `version()` or `"0"` for the synthetic rows. The `Provider` kind always carries the stage's `provider_reason`; if the stage has `Provider` with `None`, `grade` sets `kind: Unknown` and `raw.reason_code = "provider_without_reason"` — this is where the Global Constraint is enforced.

- [ ] **Step 1: Write the failing tests** in `grade.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::eval_definition::{EvalCase, EvalCheckRef, EvalReducer, EvalSplit, EvalStage, EvalTier};
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::{classify, EvidenceClass, OutcomeKind, ProviderReason};

    fn case(stages: &[(&str, &[(&str, Value)])]) -> EvalCase {
        EvalCase { case_id: "k".into(), split: EvalSplit::Train, reducer: EvalReducer::WeightedMean, fixtures: None,
            stages: stages.iter().map(|(id, checks)| EvalStage { stage_id: id.to_string(), prompt: "p".into(), deadline_secs: 60,
                checks: checks.iter().map(|(name, params)| EvalCheckRef { check: name.to_string(), params: params.clone(), tier: EvalTier::Acceptance, weight: 2 }).collect() }).collect() }
    }

    #[test]
    fn a_completed_stage_runs_its_checks_and_copies_weight_and_version() {
        let ev = ScriptedExecutor::passed_evidence("did:x", "s1", "items", vec![json!({}), json!({})]);
        let rows = grade(&case(&[("s1", &[("captured_rows_count", json!({"name":"items","min":2}))])]), &ev, &CheckRegistry::builtin());
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind, rows[0].score_bp, rows[0].weight, rows[0].check_version.as_str()), (OutcomeKind::Passed, Some(10000), 2, "1"));
    }

    #[test]
    fn a_stage_that_never_ran_yields_skipped_prerequisite_rows_for_every_check() {
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Deadline, None);
        let rows = grade(&case(&[("s1", &[("captured_rows_count", json!({"name":"items","min":1}))]), ("s2", &[("captured_rows_count", json!({"name":"items","min":1})), ("captured_rows_count", json!({"name":"other","min":1}))])]), &ev, &CheckRegistry::builtin());
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, OutcomeKind::Deadline);
        assert!(rows[1..].iter().all(|r| r.kind == OutcomeKind::SkippedPrerequisite && r.score_bp == Some(0)));
    }

    #[test]
    fn a_provider_failure_without_a_reason_is_downgraded_to_unknown() {
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Provider, None);
        let rows = grade(&case(&[("s1", &[("captured_rows_count", json!({"name":"items","min":1}))])]), &ev, &CheckRegistry::builtin());
        assert_eq!(rows[0].kind, OutcomeKind::Unknown);
        assert_eq!(rows[0].raw["reason_code"], "provider_without_reason");
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Provider, Some(ProviderReason::Unavailable));
        let rows = grade(&case(&[("s1", &[("captured_rows_count", json!({"name":"items","min":1}))])]), &ev, &CheckRegistry::builtin());
        assert_eq!((rows[0].kind, rows[0].provider_reason, rows[0].score_bp), (OutcomeKind::Provider, Some(ProviderReason::Unavailable), None));
        assert_eq!(classify(rows[0].kind, rows[0].provider_reason), EvidenceClass::NotEvidence);
    }

    #[test]
    fn an_unknown_check_name_is_a_grader_outcome_not_a_panic() {
        let ev = ScriptedExecutor::passed_evidence("did:x", "s1", "items", vec![]);
        let rows = grade(&case(&[("s1", &[("no_such_check", json!({}))])]), &ev, &CheckRegistry::builtin());
        assert_eq!((rows[0].kind, rows[0].score_bp), (OutcomeKind::Grader, None));
        assert_eq!(rows[0].raw["reason_code"], "unknown_check");
    }
}
```

  And in `captured_rows_count.rs`: min satisfied → Passed 10000; below min → ModelAcceptance 0 with `raw.count`; above max → ModelAcceptance 0; missing capture → Grader None `missing_capture`; `params: {"min": "two"}` → Grader `bad_params`.

- [ ] **Step 2: Run to see it fail.** **Step 3: Implement** as specified. **Step 4: Verify** `cargo test -p gents --lib eval` all pass; fmt. **Step 5: Commit** `feat(eval): grading over trial evidence and the seed check registry`.

---

## PR 3: The loop

Branch: `eval/12-runner-loop`, base `eval/11-runner-core`.

### Task 5: `freeze`

**Files:**
- Create: `crates/gents/src/eval/runner/freeze.rs`
- Modify: `runner/mod.rs`

**Interfaces:**
- Consumes: `load_run`, `create_run`, `RunOrigin`, `CellSpec`, `DefinitionRef`, `SubjectRef`, `DENOMINATOR_POLICY_V1`, `TAXONOMY_VERSION`, `CHECK_REGISTRY_VERSION`, `EvalDefinition` (loaded from the launching home by `definition_id`), `desired_state_document_digest` (crate-private, same crate), `pack::{resolve_pack, ResolvedPack}`, `document_config::{InferenceProfile, InferenceBackend, Tools}`.
- Produces:

```rust
#[derive(Clone, Debug)]
pub enum CellSource { InstalledPack { name: String }, Directory(PathBuf) }
#[derive(Clone, Debug)]
pub struct CellRequest { pub cell_id: String, pub label: String, pub source: CellSource, pub behavior_id: String, pub inference_profile_id: String }
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub run_id: String, pub owner: String, pub definition_id: String, pub split: EvalSplit,
    pub case_ids: Option<Vec<String>>,          // None = every case on the split
    pub cells: Vec<CellRequest>, pub trials_per_case: u32, pub seed_base: i64,
    pub deadline_secs: Option<u64>, pub concurrency: u32, pub max_infra_retries: u32, pub breaker_threshold: u32,
    pub purpose: String, pub source_commit: String, pub source_dirty: bool,
    pub runs_dir: PathBuf,                        // <launching home>/eval/runs
}
pub struct FrozenRun { pub record: RunRecord, pub run_dir: PathBuf, pub definition: EvalDefinition, pub cells: Vec<FrozenCell> }
pub struct FrozenCell { pub spec: CellSpec, pub pack_dir: PathBuf, pub inference: InferenceBinding, pub tools_unrestricted_bash: bool }
#[derive(Debug)] pub struct FreezeRefused(pub String);   // Display + Error; downcast helper `freeze_refused(&anyhow::Error) -> Option<&FreezeRefused>`
pub async fn freeze(access: &ConfigAccess, request: &RunRequest, isolation: Isolation) -> Result<FrozenRun>;
```

  `breaker_threshold` is stored in `origin` as an extra key: `RunOrigin` is frozen, so it rides in `purpose`? No. Ruling: `RunOrigin` has no slot, and M1 is frozen; store it as `origin.check_registry_version`? No. **Store it in the run directory**: `<run dir>/run.json` = `{ "breaker_threshold": N }`, written at freeze and read by resume. Record in the plan's final message that `RunOrigin.breaker_threshold` is a one-field M1 amendment to request from the orchestrator after M2 lands; until then `run.json` carries it. (The spec said "an additive key in the origin JSON"; the typed struct forbids it without an M1 change, and M1 is frozen.)

  Validation order, each refusal a `FreezeRefused` with the reason named: (1) definition exists for `owner` and `definition_id`; its digest is `desired_state_document_digest(&serde_json::to_value(&definition)?)` and becomes `DefinitionRef.digest`; (2) every requested `case_id` exists and is on `split`; the selected set is sorted; it is non-empty; (3) each cell's pack resolves: `InstalledPack` via `resolve_pack(name)` (digest = `resolved.digest`, dir = the pack's directory), `Directory` via the same loader over that directory (digest = `digest_declared_assets` over its files); (4) the pack's `AgentBehavior` with `behavior_id` exists in its `PackConfig`; (5) `inference_profile_id` resolves to an `InferenceProfile` document in the launching home and its backend to an `InferenceBackend`; both are loaded as `serde_json::Value` through the typed structs; (6) the backend's auth is not an OAuth subscription — find the auth variant in `document_config/inference_backend.rs` that names an `OAuthCredential`; refuse it by name; (7) when `isolation == Embedded`, no `Tools` document in the pack config grants unrestricted bash — find the field in `document_config/tools.rs` (`host.bash` mode; the scout named `BashMode::Unrestricted` in `tool_surface/modes.rs`); refuse naming the tool document id. Then: create `<runs_dir>/<run_id>/cells/<cell_id>/pack/` and copy the pack directory into it (`fs_extra`-free: walk and copy), write `run.json`, and `create_run` with the origin. Idempotence: if `load_run` finds the run, compare `origin` field-by-field (`==`); equal → reuse (do not re-copy packs); different → `FreezeRefused("run <id> exists with a different origin")`; `invalidated.is_some()` → refused.

- [ ] **Step 1: Write the failing tests** (against an embedded launching home built by `EmbeddedHome::create_temp` plus `ConfigAccess::Local(home.node.clone())`; install a minimal `EvalDefinition`, `InferenceProfile` and `InferenceBackend` with `DesiredStateApplyPlan::new` + `apply_desired_state_plan` inside `access.transact`, and point a `CellSource::Directory` at a fixture pack directory written by the test under a tempdir with a `manifest.json` and an `agent_behaviors/monitor.json` — copy the shape of `packs/pipeline`):

```rust
#[tokio::test] async fn freeze_writes_the_run_materializes_the_pack_and_is_idempotent() { /* freeze twice with the same request → same RunRecord, pack dir exists with the manifest, run.json has breaker_threshold */ }
#[tokio::test] async fn freeze_refuses_a_changed_origin_for_an_existing_run_id() { /* second request with trials_per_case 3 → FreezeRefused mentioning "different origin" */ }
#[tokio::test] async fn freeze_refuses_an_unknown_case_and_a_case_off_the_split() { /* case_ids: Some(["nope"]) → refused naming "nope"; a train case requested on Validation → refused */ }
#[tokio::test] async fn freeze_refuses_unrestricted_bash_under_embedded_but_not_under_process() { /* pack with a Tools doc granting unrestricted bash: Embedded → FreezeRefused naming the tools id; Process → Ok */ }
#[tokio::test] async fn freeze_refuses_an_oauth_backend() { /* backend auth = the OAuth variant → refused */ }
#[tokio::test] async fn freeze_refuses_an_invalidated_run() { /* create_run + invalidate_run, then freeze → refused */ }
```

- [ ] **Step 2: Run to see it fail. Step 3: Implement. Step 4: Verify** `cargo test -p gents --lib eval::runner::freeze`; fmt. **Step 5: Commit** `feat(eval): freeze a run: validation, pack materialization, frozen origin`.

### Task 6: `Recorder`, `run` and `resume`

**Files:**
- Create: `crates/gents/src/eval/runner/record.rs`
- Modify: `crates/gents/src/eval/runner/mod.rs` (the loop)

**Interfaces:**
- Produces (`record.rs`):

```rust
#[async_trait::async_trait]
pub trait Recorder: Send + Sync {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()>;
    async fn append_verdict(&self, owner: &str, split: EvalSplit, draft: &VerdictDraft) -> Result<()>;
    async fn complete_trial(&self, owner: &str, trial_id: &str, completion: &TrialCompletion) -> Result<()>;
    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>>;
}
pub struct DocumentRecorder<'a>(pub &'a ConfigAccess);   // delegates to gents::eval::documents
```

- Produces (`mod.rs`):

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutcome { pub run_id: String, pub completed: u32, pub abandoned: u32, pub not_evidence: u32, pub breaker_tripped: bool }
#[derive(Debug)] pub struct ProviderDown { pub run_id: String, pub consecutive_not_evidence: u32 }   // Display + Error; `provider_down(&anyhow::Error) -> Option<&ProviderDown>`
pub struct RunOptions { pub poll_backoff_base: Duration /* 5s */, pub poll_backoff_cap: Duration /* 60s */ }
pub async fn run(access: &ConfigAccess, request: &RunRequest, executor: &dyn TrialExecutor, registry: &CheckRegistry, cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;
pub async fn resume(access: &ConfigAccess, owner: &str, run_id: &str, runs_dir: &Path, executor: &dyn TrialExecutor, registry: &CheckRegistry, cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;
/// The loop both call; `recorder` is the seam the crash tests use.
pub(crate) async fn execute_frozen(frozen: &FrozenRun, recorder: &dyn Recorder, executor: &dyn TrialExecutor, registry: &CheckRegistry, cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;
```

  `CancellationToken` is `tokio_util::sync::CancellationToken` (add `tokio-util` with the `sync` feature if absent). `resume` reloads the run with `load_run`, refuses an invalidated run, rebuilds `FrozenRun` from `origin` and the run directory (no re-validation of the definition beyond digest equality; the packs are already materialized), then calls `execute_frozen`.

  `execute_frozen`, the loop:

  1. `existing = recorder.load_trials(owner, run_id)`; `planned = plan(origin, run_id, &existing)`; drop slots whose abandoned attempts already number `max_infra_retries + 1`; if nothing is planned, break.
  2. Run the planned batch through `buffer_unordered(concurrency)`, wrapped in `tokio::select!` with `cancel.cancelled()`. Per trial:
     a. Build `TrialSpec` from the frozen cell and the case (stages, fixtures, captures; `script_key` only if `executor.wants_script_key()`).
     b. `locator = executor.provision(&spec)`; write the `EvalTrial` row with `recorder.create_trial` from the locator (`trial_agent_did`, `session_id`, `home_hint`), `attempt`, `seed`.
     c. `evidence = executor.execute(&spec, cancel.child_token())`.
     d. `rows = grade(case, &evidence, registry)`; for each row `recorder.append_verdict` with `feedback` set to `None` unless `origin.split == Train`.
     e. `recorder.complete_trial` with `StageCompletion`s from `evidence.stages`, followed by one `SkippedPrerequisite` entry (with `request_id: None`, `terminal_state: None`) per case stage the evidence lacks; `usage`; `anchor`.
     f. Classify the trial: NotEvidence when the evidence has no stages, or when every acceptance-tier row classifies `NotEvidence`. On NotEvidence increment `consecutive`, else reset it to 0. If `consecutive >= breaker_threshold`, set `breaker_tripped`, stop launching, drain, and return `Err(ProviderDown { .. })`.
  3. Before re-planning a NotEvidence slot, back off `min(cap, base * 2^(attempt - 1))` with `tokio::time::sleep` (the one permitted sleep: it is inside the library, not a shell).
  4. On cancel: stop launching; in-flight `execute` calls observe the child token and return; their rows are not completed; count them as `abandoned`; return `Ok(outcome)`.
  5. Any `Err` from the recorder propagates unchanged (the in-flight row stays null; resume repairs it). `ProviderDown` and recorder errors are the only error returns.

- [ ] **Step 1: Write the failing tests** in `mod.rs` `tests`, on `ScriptedExecutor` and a `FaultingRecorder`:

```rust
struct FaultingRecorder<'a> { inner: DocumentRecorder<'a>, fail_on: Mutex<Option<(&'static str, u32)>>, calls: Mutex<u32> }
// fail_on = Some(("complete_trial", 1)) fails the first complete_trial call with anyhow!("injected"), then passes.

#[tokio::test] async fn a_two_cell_run_completes_every_slot_and_writes_verdicts_before_completion() { /* 2 cells × 2 cases × 2 trials on ScriptedExecutor with a default passed evidence → outcome.completed == 8; load_trials all completed; load_verdicts has 8 rows; verify ordering by inspecting recorder call log: for each trial, append_verdict precedes complete_trial */ }
#[tokio::test] async fn a_crash_after_create_trial_is_repaired_by_resume() { /* FaultingRecorder fails the first `append_verdict`... simpler: fail the first `execute`? No — fail first complete_trial → run returns Err; load_trials shows one row with null completion and verdicts attached to its trial_id; resume → the slot gets attempt 2, completes; the abandoned row still exists with null completion; total completed == matrix size */ }
#[tokio::test] async fn not_evidence_retries_up_to_the_cap_then_the_slot_is_left_as_not_evidence() { /* table: (base,k,0,1)=not_evidence, (base,k,0,2)=not_evidence, (base,k,0,3)=passed; max_infra_retries 3 → completed with attempt 3; with all attempts not_evidence → outcome.not_evidence == 1 and the slot stops at attempt max_infra_retries+1 */ }
#[tokio::test] async fn the_breaker_trips_on_consecutive_not_evidence_and_resume_continues() { /* default = not_evidence, breaker_threshold 2 → Err(ProviderDown{consecutive:2}); then swap the executor default to passed and resume → completes */ }
#[tokio::test] async fn cancel_stops_launching_and_leaves_in_flight_rows_null() { /* executor whose execute awaits the cancel token; cancel after first launch → Ok, abandoned == 1, no completion */ }
#[tokio::test] async fn an_invalidated_run_refuses_resume() { }
#[tokio::test] async fn feedback_is_dropped_off_train() { /* Validation split, a check that returns feedback → stored verdict has feedback None; Train → Some */ }
#[tokio::test] async fn concurrency_four_writes_the_same_documents_as_one() { /* run the same request under concurrency 1 and 4 into two homes; compare sorted (cell,case,index,attempt,completion) tuples */ }
```

  Use `RunOptions { poll_backoff_base: Duration::from_millis(1), poll_backoff_cap: Duration::from_millis(2) }` in tests.

- [ ] **Step 2: Run to see it fail. Step 3: Implement. Step 4: Verify** `cargo test -p gents --lib eval::runner` and `cargo check -p gents --all-targets`; fmt. **Step 5: Commit** `feat(eval): run and resume: the trial loop, the NotEvidence breaker, cancellation`.

---

## PR 4: `EmbeddedExecutor` and the canary

Branch: `eval/13-runner-embedded`, base `eval/12-runner-loop`.

### Task 7: `EmbeddedExecutor`

**Files:**
- Create: `crates/gents/src/eval/runner/embedded/executor.rs`
- Modify: `crates/gents/src/eval/runner/embedded/mod.rs` (`pub use executor::EmbeddedExecutor;`)

**Interfaces:**
- Consumes: `EmbeddedHome`, `boot_runtime`, `await_terminal`, `collect_request_evidence`, `classify_request_outcome`, `wait_for_runtime_ready` (PR 1); `TrialSpec`, `TrialLocator`, `TrialEvidence`, `StageEvidence`, `Capture`, `CaptureResult`, `FileRef` (PR 2); `DesiredStateApplyPlan::from_pack_config`, `apply_desired_state_plan`, `ConfigAccess::transact`; `pack::load_pack_config`; `crate::{RequestSpec, RequestIdentity, build_signed_request, RequestSigner}` (the path `gents-cli/src/request_helpers.rs::prepare_agent_request` uses: copy its `RequestSpec::new(RequestIdentity { request_id, agent_did, requester_did: None, behavior_id, session_id, content, execution_origin: Interactive, created_at }, admission)` construction; the implementer reads that file for the admission value and the `valid_until`/`retry` fields and reproduces them).
- Produces: `pub struct EmbeddedExecutor { pub runtime_options: DocumentRuntimeOptions }` implementing `TrialExecutor` with `isolation() == Isolation::Embedded`.

`provision(spec)`: `EmbeddedHome::create_retained(&spec.home_dir.join("home"))`; create `spec.home_dir.join("workspace")`; return `TrialLocator { trial_agent_did: home.did(), session_id: uuid v4, home_hint: Some(relative path of spec.home_dir from its runs dir) }`. Keep the created home in a `Mutex<HashMap<trial_id, EmbeddedHome>>` on the executor so `execute` finds it; on any failure return a locator with `trial_agent_did: "did:unprovisioned"` and let `execute` return `infrastructure` (never `Err`).

`execute(spec, cancel)`, every step mapped to Section 2 of the spec; any `Err` in steps 1 to 3 → `TrialEvidence::infrastructure(locator)` with `tracing::warn!` of the cause:
1. Take the home from the map (missing → infrastructure). Verify `spec.pack_digest` against `digest_declared_assets` over `spec.pack_dir` (mismatch → infrastructure, `raw` detail "pack digest mismatch").
2. `load_pack_config(manifest, &PackInstallOptions::default(), read_asset, env)` over `spec.pack_dir`; `DesiredStateApplyPlan::from_pack_config`; `ConfigAccess::Local(home.node.clone()).transact("eval.trial.install_pack", |txn| Box::pin(async move { apply_desired_state_plan(txn, &plan).await }))`. Then write `spec.inference.profile` and `spec.inference.backend` as `InferenceProfile`/`InferenceBackend` documents through a second `DesiredStateApplyPlan::new(vec![...])` (their `agent_did` rewritten to the trial DID), the `InferenceSampling.seed` set to `spec.inference.seed` on the profile (find the field in `document_config/inference_profile.rs`). Write the `WorkspaceRoot` exactly as `install_eval_workspace_root` does today (runner.rs:458-474), `root_path` = the workspace dir, `display_name` = `"Eval trial workspace"`. Fixtures: for each `schemas` entry `home.node.add_schema(sdl)`; for each `documents` entry a `create_<collection>` mutation with the document passed as a `$input` variable (never interpolated; see how `documents.rs` passes `$input`); for each `files` entry write `workspace/<path>` (refuse `..` components). Boot: `boot_runtime(&home, home.identity.clone(), self.runtime_options.clone())`.
3. Session: `session_id` from the locator; the first request creates it implicitly (that is how `gents request` works: no separate `AgentSession` write; confirm by reading `request_helpers.rs`, and if a session document is required for `behavior_id` selection, write it the way that file does).
4. For each stage in order: `request_id = uuid v4`; build the `RequestSpec` and `build_signed_request(spec, RequestSigner::RegisteredTarget).await` against `home.node`; `await_terminal(&home.node, &request_id, Duration::from_secs(stage.deadline_secs), Duration::from_secs(30), Duration::from_millis(250))`, racing `cancel.cancelled()`: on cancel call `interrupt_request` and await the grace once, then treat as `Runtime`. `evidence = collect_request_evidence(...)`; `failure_kind = classify_request_outcome(...)` mapped: `None → None`, `"deadline" → Deadline`, `"tool" → Tool`, `"provider" → Provider` with `provider_reason` derived from the failed `InferenceCall.failure_reason` (`Rejected` when it names an HTTP 4xx or "context"/"policy"; `Unavailable` when 5xx, "connect", "timeout", "rate"; this mapping is a named function `provider_reason_from_failure(&str) -> Option<ProviderReason>` with its own table test; `None` when unclassifiable, and `grade` then downgrades to Unknown), `"runtime" → Runtime`, `"unknown" → Unknown`. Run captures for the stage (step 5). If `failure_kind.is_some()`, stop submitting; remaining stages are absent from `evidence.stages` (grade writes `SkippedPrerequisite`).
5. Captures: `Documents { collection, filter }` → `query { <collection>(filter: <filter JSON as GraphQL input>) { ... } }`. Selecting all fields generically is not possible in GraphQL; use `_docID` plus the fields named in `filter`'s keys plus a `fields: Vec<String>` addition to `Capture::Documents` (add it in this task to `executor.rs`; default empty means `_docID` only). Filter values are interpolated through `escape_graphql_string` for strings and `serde_json` formatting for numbers/bools; objects nest. The one runtime-supplied variable is `"$trial"`, replaced by the trial DID. `File { glob }` → `glob::glob` under `workspace/` (add the `glob` crate if absent); `FileRef { path relative, sha256 hex, bytes }`.
6. Close: `runtime.shutdown().await`; `usage` = sum of `prompt_tokens`/`completion_tokens` over every stage's inference calls, `None` for either total if any call has `None`; `anchor` = terminal states in order, request count, inference-call count; `evidence_digest`. Drop the home from the map (the directory stays).

`recollect(at, captures)`: `EmbeddedHome::open_retained(<runs dir>/<home_hint>/home)` (the executor needs the runs dir: give it `pub runs_dir: PathBuf`); re-run `collect_request_evidence` for every request in the session (query `AgentRequest(filter: {session_id})` ordered by `created_at`), re-run captures, rebuild the evidence; `None` if the directory is missing.

- [ ] **Step 1: Write the failing unit tests** in `executor.rs`: `provider_reason_from_failure` table (`"HTTP 503"` → Unavailable, `"HTTP 429 rate limited"` → Unavailable, `"HTTP 400 context length"` → Rejected, `"content policy"` → Rejected, `"weird"` → None); `capture_query_renders_filter_and_escapes_strings` (a filter `{"requester_did": {"_eq": "$trial"}}` with DID `did:x"y` renders the escaped string); `file_capture_refuses_parent_components`; `provision_then_execute_on_a_bad_pack_digest_is_infrastructure` (real home, `pack_digest: "wrong"` → evidence with no stages and `anchor.requests == 0`).
- [ ] **Step 2: Run to see it fail. Step 3: Implement. Step 4: Verify** `cargo test -p gents --lib eval::runner::embedded`; `cargo check -p gents --all-targets`; fmt. **Step 5: Commit** `feat(eval): EmbeddedExecutor: fresh home, pack install, stages, capture`.

### Task 8: The canary and the live smoke

**Files:**
- Create: `crates/gents/tests/eval_runner_canary.rs` (a new test binary; `mod support;` at the top like the others)
- Create: `crates/gents/tests/fixtures/eval_runner/canary_pack/` — `manifest.json`, `agent_behaviors/canary.json`, `agent_contexts/canary.json`, `tools/canary.json` (no bash; file tools read-only), modeled on `packs/pipeline`'s document kinds. The implementer reads `packs/README.md` for the manifest fields.

**Interfaces:** consumes everything above plus `support::streaming_backend::{MockStreamingBackend, StreamPlan, StreamResponse}`.

- [ ] **Step 1: Write the canary** (it fails until PR 4 is wired, then passes; it is the acceptance test):

```rust
mod support;
use std::time::Duration;
use gents::eval::runner::{run, resume, RunOptions, RunRequest, CellRequest, CellSource, embedded::EmbeddedExecutor};
use gents::eval::checks::CheckRegistry;
use gents::eval::{load_trials, load_verdicts, OutcomeKind};
use gents::ConfigAccess;
use support::streaming_backend::{MockStreamingBackend, StreamPlan, StreamResponse};
use tokio_util::sync::CancellationToken;

// Launching home: EmbeddedHome::create_temp + ConfigAccess::Local; install an EvalDefinition with two
// cases: "one-stage" (stage "report", check captured_rows_count {name:"items", min:1}) and
// "two-stage" (stage "report" then "confirm", same check on both) — the definition, an
// InferenceProfile "canary" and an InferenceBackend pointing at backend.endpoint() with the mock model
// name, all applied through DesiredStateApplyPlan. The mock plan: for the marker in the stage prompt,
// StreamResponse::completes(marker, ["done"]). The subject's Tools grant a datastore surface on a
// fixture collection "CanaryItem" (schema installed as a fixture) so the model's reply cannot write a row;
// instead the fixture installs one CanaryItem document up front, so `items` captures 1 row and the check passes.

#[tokio::test]
async fn the_canary_runs_two_cases_end_to_end_on_an_embedded_home_with_a_scripted_model() {
    let backend = MockStreamingBackend::start_with_plans("canary-model", vec![StreamPlan::new("report".into(), vec![StreamResponse::completes("report", ["done"])]), StreamPlan::new("confirm".into(), vec![StreamResponse::completes("confirm", ["done"])])]).unwrap();
    let (access, runs_dir, request) = canary_request(&backend, /*failing_second_stage*/ false).await;
    let outcome = run(&access, &request, &EmbeddedExecutor { runtime_options: Default::default(), runs_dir: runs_dir.clone() }, &CheckRegistry::builtin(), CancellationToken::new(), &RunOptions::default()).await.unwrap();
    assert_eq!((outcome.completed, outcome.not_evidence, outcome.breaker_tripped), (2, 0, false));
    let trials = load_trials(&access, &request.owner, &request.run_id).await.unwrap();
    assert!(trials.iter().all(|t| t.completion.is_some()));
    let verdicts = load_verdicts(&access, &request.owner, &request.run_id).await.unwrap();
    assert_eq!(verdicts.len(), 3);
    assert!(verdicts.iter().all(|v| v.kind == OutcomeKind::Passed && v.score_bp == Some(10_000)));
    // the two-stage trial's completion anchors two requests
    let two = trials.iter().find(|t| t.identity.case_id == "two-stage").unwrap().completion.clone().unwrap();
    assert_eq!(two.anchor.requests, 2);
    assert_eq!(two.usage.output_tokens.is_some(), true, "the mock backend reports usage");
    // stable digest across an identical second run
    let (access2, runs_dir2, request2) = canary_request(&backend, false).await;
    run(&access2, &request2, &EmbeddedExecutor { runtime_options: Default::default(), runs_dir: runs_dir2 }, &CheckRegistry::builtin(), CancellationToken::new(), &RunOptions::default()).await.unwrap();
    let digest = |v: &Vec<gents::eval::TrialRecord>| v.iter().map(|t| t.completion.as_ref().unwrap().anchor.clone()).collect::<Vec<_>>();
    assert_eq!(digest(&trials), digest(&load_trials(&access2, &request2.owner, &request2.run_id).await.unwrap()));
}

#[tokio::test]
async fn a_failing_first_stage_skips_the_second_and_grades_it_skipped_prerequisite() {
    // mock plan for "report" answers with StreamResponse::service_unavailable(..) three times so the request fails as provider/unavailable;
    // expect: the "two-stage" trial has one submitted stage, its verdicts are Provider(Unavailable) for "report" and SkippedPrerequisite for "confirm";
    // outcome.not_evidence counts that trial after max_infra_retries attempts (set max_infra_retries: 0 for speed).
}

#[tokio::test]
async fn freeze_refuses_unrestricted_bash_and_an_oauth_backend_before_creating_any_home() {
    // two requests; each returns Err with freeze_refused(..).is_some(); runs_dir has no run directory afterwards.
}

#[tokio::test]
#[ignore = "needs GENTS_LIVE_CONFIG_PROVIDER and a real backend"]
async fn live_smoke_one_trial_on_a_real_provider_reports_whether_seed_was_honoured() {
    // build the same request against the provider from support::live_inference::LiveProvider::from_env();
    // run one case with trials_per_case 2 under one cell; print (tracing::info!) the two trials' first assistant message equality as the seed finding.
}
```

- [ ] **Step 2: Build the canary pack fixture and `canary_request`** (an async helper in the test file that builds the launching home, installs the definition/profile/backend, writes the fixture `CanaryItem` schema into the definition's `fixtures.schemas`, and returns `(ConfigAccess, runs_dir, RunRequest)`).
- [ ] **Step 3: Run** `CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary` → 3 passed, 1 ignored. If the mock backend's OpenAI-compatible stream does not report usage, relax the usage assertion to `usage.output_tokens.is_none()` **and** say so in the report (the spec's "null when absent" rule then applies).
- [ ] **Step 4: Verify the whole stack**: `cargo test -p gents --lib eval`; `cargo check --workspace --all-targets`; `cargo test -p gents --test e2e_configurator --no-run`; fmt.
- [ ] **Step 5: Commit** `test(eval): the runner canary on an embedded home with a scripted model`.

---

## Plan self-review

- **Spec coverage.** Section 1: Tasks 3 (seam, scripted, plan), 4 (grade), 1–2 (embedded module). Section 2: Task 7 steps 1–6, retention layout (Task 5 creates it, Task 7 fills it), `recollect`, the `tests/`→`src/` move (Tasks 1–2). Section 3: Task 5 (freeze, idempotence, refusals), Task 6 (plan order, write order, breaker, cancel, invalidation refusal, launching-home failure propagation). Section 4: layer 1 in Tasks 3–4, layer 2 in Task 6, canary and live smoke in Task 8, the seed check in Task 4. Gap: `RunOrigin.breaker_threshold` cannot be added without an M1 change; carried in `run.json` (Task 5) and listed as an M1 amendment request.
- **Type consistency.** `TrialExecutor` has four methods everywhere: `isolation`, `provision`, `execute(spec, cancel)`, `recollect(at, captures)`, plus the defaulted `wants_script_key`. `Capture::Documents` carries `fields`. `StageEvidence.failure_kind: Option<OutcomeKind>` with `None` meaning completed, matching `StageCompletion`.
- **Placeholders.** None. Where a value must be read from the repo (an auth variant name, a tools field, the admission value), the task names the file and says what to look for.

## Execution notes for the coordinator

- Task 1 and Task 2 change files eight test binaries compile; `cargo check -p gents --all-targets` is the fast gate, then one small binary per task.
- Tasks 3–4 and 5–6 are pure or embedded-only and fast. Task 7 is the largest; give its implementer Task 3's `executor.rs` and Task 6's `mod.rs` as interface files to read.
- Task 8 is the M2 acceptance test. A canary failure is a defect in Tasks 1–7, not in the test.
