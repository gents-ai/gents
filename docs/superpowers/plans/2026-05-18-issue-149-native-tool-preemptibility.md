# Issue #149 Native Tool Preemptibility Implementation Plan

Goal: implement the approved design in
`docs/superpowers/specs/2026-05-18-issue-149-native-tool-preemptibility-design.md`.

Do not start implementation until the design PR is approved. This plan assumes
the design recommendation is accepted: keep the current `spawn_blocking`
filesystem boundary as the interim mitigation, then migrate native filesystem
traversal tools to a managed subprocess boundary with process-group kill.
Controller decisions from 2026-05-18 are incorporated below.

Each task below is intended to be one PR unless the implementation reveals a
smaller safe split. Every task ends with formatting, focused tests, broader
smoke tests where appropriate, and a commit.

Filed sub-issues:

- #230: ManagedExec Lean liveness model.
- #231: ManagedExec Rust boundary and process ownership.
- #232: Native filesystem runner protocol.
- #233: ManagedExec conformance witness rows.
- #234: Health/status liveness reporting.
- #235: Soak closeout gate.
- #236: Windows process termination support.

Controller decisions:

- Option A is confirmed; keep
  `native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue`
  green throughout migration.
- ManagedExec is a module at `crates/defra-agent/src/managed_exec/`, not a new
  crate.
- The runner binary should live in its own crate because it is a separate
  executable.
- `read_file` stays on `tokio::fs::read`; `grep`'s synchronous
  `std::fs::read_to_string` migrates with grep.
- First implementation is Unix-only with an explicit non-Unix stub pointing at
  #236.
- Executor metadata is memory-only plus health/status/logging, not persisted on
  `AgentToolCall`.

Implementation closeout, 2026-05-18:

- Tasks 1-4 landed the Lean `ManagedExec` state machine, liveness theorems,
  tool-execution composition, conformance JSON, and Rust consumers.
- Tasks 5-8 landed Unix ManagedExec process-group ownership, the
  `defra-native-fs-runner` binary crate, and migrated `list_files`, `glob`, and
  `grep` to `managedExecProcessGroupBoundary`. `read_file` remains in-process on
  `tokio::fs::read` by design. Production runner resolution requires
  `DEFRA_NATIVE_FS_RUNNER` or an adjacent runner binary; it does not invoke
  `cargo run` at request time.
- Task 9 is satisfied through request-scoped runtime context handoff into the
  native runner and ManagedExec timeout/cancel outcomes returning the existing
  lifecycle markers consumed by the hook.
- Task 10 landed active native executor snapshots in `/healthz`, `/status`,
  Prometheus, and CLI status liveness when the live HTTP server is reachable.
- Task 11 did not add `crates/amygdala-evals/` coverage because that crate is
  not present in this worktree. The closeout regression is the deterministic
  `native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue`
  test plus the final gate below.

Final verification completed:

```text
cargo fmt --all --check
cargo test -p defra-agent --lib runtime_status::tests::
cargo test -p defra-agent --lib toolset::tests::read_only_bash
cargo test -p defra-agent-cli --test cli_server server_startup_with_iroh_p2p_reports_runtime_connectivity -- --nocapture --test-threads=1
cargo test -p defra-agent-cli --test cli_status -- --nocapture --test-threads=1
cargo test -p defra-agent-cli --test cli_reconciliation reconciled_runtime_sends_generation_two_tools_and_completes_tool_loop -- --nocapture --test-threads=1
cargo test -p defra-agent --test state_machine_conformance
cd crates/defra-agent/proofs && lake build
cargo test -p defra-agent --lib managed_exec::
cargo test -p defra-agent --lib toolset::tests::native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue
cargo run -p defra-native-fs-runner -- --self-test
```

## Task 1: Add ManagedExec Lean State Skeleton

Purpose: introduce the executor state machine without connecting it to tool
execution yet.

Files touched:

- Add: `crates/defra-agent/proofs/Proofs/ManagedExec.lean`
- Add: `crates/defra-agent/proofs/Proofs/ManagedExec/State.lean`
- Add: `crates/defra-agent/proofs/Proofs/ManagedExec/Transition.lean`
- Add: `crates/defra-agent/proofs/Proofs/ManagedExec/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`
- Modify: `crates/defra-agent/proofs/README.md`

Code shape:

```lean
inductive ManagedExecState where
  | pendingSpawn
  | running
  | exited
  | killSignaled
  | killed
  | spawnFailed
  | reapFailed
  deriving DecidableEq, Repr

structure ManagedExecContext where
  state : ManagedExecState
  deadline : Time
  now : Time
  killSignaledAt : Option Time
  exitCode : Option Int
  deriving Repr
```

Verify:

```bash
cd crates/defra-agent/proofs && lake build
```

Expected output:

```text
Build completed successfully
```

Commit:

```text
Add ManagedExec Lean state skeleton
```

## Task 2: Prove ManagedExec Deadline And Cancel Liveness

Purpose: prove the executor-level transition facts needed before composition.

Files touched:

- Modify: `crates/defra-agent/proofs/Proofs/ManagedExec/Transition.lean`
- Add: `crates/defra-agent/proofs/Proofs/ManagedExec/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ManagedExec.lean`
- Modify: `crates/defra-agent/proofs/README.md`

Code shape:

```lean
theorem deadline_running_exec_reaches_kill_signaled
    (pre : ManagedExecContext)
    (h_running : pre.state = .running)
    (h_deadline : pre.deadline < pre.now) :
    exists post,
      BoundedTrace pre post maxKillSignalSteps
      /\ post.state = .killSignaled
      /\ post.killSignaledAt = some pre.now
```

Add the matching cancel theorem:

```lean
theorem cancel_running_exec_reaches_kill_signaled ...
```

Verify:

```bash
cd crates/defra-agent/proofs && lake build Proofs.ManagedExec
cd crates/defra-agent/proofs && lake build
```

Expected output:

```text
Build completed successfully
```

Commit:

```text
Prove ManagedExec liveness properties
```

## Task 3: Compose ManagedExec With ToolExecution

Purpose: prove the #159 R3 operational theorem over request, tool, and
executor state.

Files touched:

- Add: `crates/defra-agent/proofs/Proofs/ManagedExec/Composed.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Composed.lean`
- Modify: `crates/defra-agent/proofs/Proofs/ManagedExec.lean`
- Modify: `crates/defra-agent/proofs/README.md`

Code shape:

```lean
structure ManagedExecComposedState where
  request : RequestContext
  tool : ToolExecution.ToolCallContext
  exec : ManagedExecContext
  now : Time

theorem running_tool_times_out_after_deadline_bounded
    (pre : ManagedExecComposedState)
    (h_tool : pre.tool.state = .running)
    (h_exec : pre.exec.state = .running)
    (h_deadline : pre.request.deadline < pre.now) :
    exists post,
      BoundedTrace pre post maxTimeoutSteps
      /\ post.tool.state = .timedOut
      /\ post.exec.state = .killSignaled
```

Verify:

```bash
cd crates/defra-agent/proofs && lake build Proofs.ManagedExec
cd crates/defra-agent/proofs && lake build Proofs.Composed
```

Expected output:

```text
Build completed successfully
```

Commit:

```text
Compose ManagedExec liveness with tool execution
```

## Task 4: Emit ManagedExec Conformance Contracts

Purpose: make the new Lean machine visible to Rust tests before runtime code
lands.

Files touched:

- Add: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/ManagedExec.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Types.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/src/lean_vocab_test.rs`
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`

Code shape:

```lean
structure ManagedExecLivenessCase where
  name : String
  trigger : String
  preExecState : String
  preToolState : String
  expectedExecState : String
  expectedToolState : String
  maxSteps : Nat
  killSignalRequired : Bool
  deriving Repr
```

Verify:

```bash
cd crates/defra-agent/proofs && lake build Proofs.Conformance.Contracts
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean >/tmp/managed-exec-contracts.json
cargo test -p defra-agent --test state_machine_conformance managed_exec
```

Expected output:

```text
test result: ok
```

Commit:

```text
Emit ManagedExec conformance contracts
```

## Task 5: Add ManagedExec Rust Process Boundary

Purpose: add the process supervisor as Rust infrastructure, without migrating
any tool yet.

Files touched:

- Add: `crates/defra-agent/src/managed_exec/mod.rs`
- Add: `crates/defra-agent/src/managed_exec/process.rs`
- Add: `crates/defra-agent/src/managed_exec/output.rs`
- Add: `crates/defra-agent/src/managed_exec/tests.rs`
- Modify: `crates/defra-agent/src/lib.rs` or module root
- Modify: `Cargo.toml` only if a new dependency is required

Code shape:

```rust
// crates/defra-agent/src/managed_exec/process.rs
pub(crate) struct ManagedExecRequest {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub deadline_at: Option<DateTime<Utc>>,
    pub cancellation_token: CancellationToken,
    pub max_output_bytes: usize,
}

pub(crate) enum ManagedExecOutcome {
    Exited { code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8> },
    TimedOut { stdout: Vec<u8>, stderr: Vec<u8>, kill: KillReport },
    Cancelled { stdout: Vec<u8>, stderr: Vec<u8>, kill: KillReport },
}

#[cfg(not(unix))]
pub(crate) async fn run_managed_exec(_request: ManagedExecRequest) -> ManagedExecOutcome {
    unimplemented!("Windows ManagedExec process termination: see #236")
}
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib managed_exec::
```

Expected output:

```text
test result: ok
```

Commit:

```text
Add managed exec process boundary
```

## Task 6: Add Native Filesystem Runner Protocol

Purpose: define the runner request/response envelope and a runner binary that
can execute current filesystem operations out of process.

Files touched:

- Add: `crates/defra-native-fs-runner/Cargo.toml`
- Add: `crates/defra-native-fs-runner/src/main.rs`
- Add: `crates/defra-native-fs-runner/src/protocol.rs`
- Modify: workspace `Cargo.toml`
- Add: `crates/defra-agent/src/toolset/native_runner.rs`
- Modify: `crates/defra-agent/src/toolset/shared/filesystem.rs`
- Modify: `crates/defra-agent/src/toolset/shared.rs`
- Add tests under `crates/defra-agent/tests/` or `src/toolset/tests.rs`

Code shape:

```rust
#[derive(Serialize, Deserialize)]
pub(crate) enum NativeFsRunnerRequest {
    ListFiles(ListFilesArgs),
    Glob(GlobArgs),
    Grep(GrepArgs),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct NativeFsRunnerResponse {
    pub ok: bool,
    pub output: Option<String>,
    pub error: Option<String>,
}
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib toolset::tests::native_runner
cargo run -p defra-native-fs-runner -- --self-test
```

Expected output:

```text
test result: ok
self-test ok
```

Commit:

```text
Add native filesystem runner protocol
```

## Task 7: Migrate Glob And ListFiles To ManagedExec

Purpose: move the highest-risk recursive traversal tools to the subprocess
boundary while preserving output format.

Files touched:

- Modify: `crates/defra-agent/src/toolset/file_tools.rs`
- Modify: `crates/defra-agent/src/toolset/tests.rs`
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs` if witness
  names change from `spawnBlockingRuntimeBoundary`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/BoundaryRuntime.lean`
  only if the witness family is updated in the same PR

Code shape:

```rust
async fn call(&self, args: GlobArgs) -> Result<String, ToolError> {
    self.native_runner
        .run(NativeFsRunnerRequest::Glob(args), current_tool_runtime_context())
        .await
        .map_err(Into::into)
}
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib toolset::tests::glob_
cargo test -p defra-agent --lib toolset::tests::list_files_
cargo test -p defra-agent --lib toolset::tests::native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue
```

Expected output:

```text
test result: ok
```

Commit:

```text
Migrate glob and list_files to managed exec
```

## Task 8: Migrate Grep

Purpose: move the remaining traversal-heavy native filesystem tool. `read_file`
stays in process on `tokio::fs::read`; this task only verifies that decision
does not regress its tests.

Files touched:

- Modify: `crates/defra-agent/src/toolset/file_tools.rs`
- Modify: `crates/defra-agent/src/toolset/tests.rs`

Code shape:

```rust
async fn call(&self, args: GrepArgs) -> Result<String, ToolError> {
    self.native_runner
        .run(NativeFsRunnerRequest::Grep(args), current_tool_runtime_context())
        .await
        .map_err(Into::into)
}
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib toolset::tests::grep_
cargo test -p defra-agent --lib toolset::tests::read_file_
cargo test -p defra-agent --test state_machine_conformance managed_exec
```

Expected output:

```text
test result: ok
```

Commit:

```text
Migrate grep native filesystem execution
```

## Task 9: Wire ManagedExec Outcomes Into Tool Lifecycle

Purpose: ensure timeout and cancel outcomes are persisted through the existing
`ToolCallLifecycle` path and startup recovery remains coherent.

Files touched:

- Modify: `crates/defra-agent/src/tool_call_lifecycle/runtime.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/transition.rs`
- Modify: `crates/defra-agent/src/tool_call_lifecycle/recovery.rs`
- Modify: `crates/defra-agent/src/hook.rs`
- Modify: `crates/defra-agent/src/hook/persistence.rs`
- Add or modify lifecycle tests

Code shape:

```rust
match outcome {
    ManagedExecOutcome::TimedOut { .. } => Ok(timeout_result(deadline_at)),
    ManagedExecOutcome::Cancelled { .. } => Ok(cancelled_result()),
    ManagedExecOutcome::Exited { code: Some(0), stdout, .. } => render(stdout),
    ManagedExecOutcome::Exited { .. } => Err(ToolError::from(...)),
}
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib tool_call_lifecycle::
cargo test -p defra-agent --lib hook::tests::
cargo test -p defra-agent --test state_machine_conformance
```

Expected output:

```text
test result: ok
```

Commit:

```text
Wire managed exec outcomes into tool lifecycle
```

## Task 10: Add Health And Status Liveness Reporting

Purpose: close the #149 operator blind spot with active request/tool/executor
age and expired-processing counters.

Files touched:

- Modify: `crates/defra-agent/src/runtime_status.rs` or current health module
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs`
- Modify: `crates/defra-agent/src/hook.rs`
- Modify: CLI/status tests if health output is user-visible
- Modify: docs if `/healthz` schema is documented

Code shape:

```rust
pub(crate) struct RuntimeLivenessStatus {
    pub active_request_id: Option<String>,
    pub active_tool_name: Option<String>,
    pub active_tool_started_at: Option<DateTime<Utc>>,
    pub active_executor_pid: Option<i32>,
    pub age_since_last_progress_ms: i64,
    pub expired_processing_count: i64,
}
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib runtime_status::tests::
cargo test -p defra-agent-cli --test cli_status -- --nocapture --test-threads=1
```

Expected output:

```text
test result: ok
```

Commit:

```text
Expose native tool liveness status
```

## Task 11: Add Soak Regression Gate And Closure Notes

Purpose: prove the original #149 shape no longer requires restart recovery and
document the release/soak bar for closing the operational thread.

Files touched:

- Add or modify soak regression under `crates/amygdala-evals/` if present in
  the implementation worktree
- Modify: `docs/superpowers/audits/2026-05-12-deadline-plumbing-audit.md` or
  add a follow-up closeout audit
- Modify: `docs/superpowers/specs/2026-05-18-issue-149-native-tool-preemptibility-design.md`
  with final accepted decisions

Code shape:

```text
assert zero AgentToolCall rows remain lifecycle_state="running" past deadline
assert zero AgentRequest rows remain processing past deadline
assert queue advances without daemon restart
assert health/status reports active executor age during the blocker window
```

Verify:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib toolset::tests::native_filesystem_deadline_preempts_single_poll_blocker_and_advances_queue
cargo test -p defra-agent --test state_machine_conformance
```

Expected output:

```text
test result: ok
soak replay: 70/70 terminal without restart
```

Commit:

```text
Document issue 149 managed exec closeout
```

## Final PR Gate

Before marking the implementation sequence complete, run:

```bash
cargo fmt --all --check
cargo test -p defra-agent --lib runtime_status::tests::
cargo test -p defra-agent --lib toolset::tests::read_only_bash
cargo test -p defra-agent-cli --test cli_server server_startup_with_iroh_p2p_reports_runtime_connectivity -- --nocapture --test-threads=1
cargo test -p defra-agent-cli --test cli_status -- --nocapture --test-threads=1
cargo test -p defra-agent-cli --test cli_reconciliation reconciled_runtime_sends_generation_two_tools_and_completes_tool_loop -- --nocapture --test-threads=1
cargo test -p defra-agent --test state_machine_conformance
cd crates/defra-agent/proofs && lake build
```

Expected output:

```text
test result: ok
Build completed successfully
```
