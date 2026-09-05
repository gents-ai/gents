# #1337 CliTool Through managed_exec Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every tool-invoked subprocess is spawned, deadlined, cancelled, and killed by `managed_exec`; `CliTool` stops running its own `tokio::process` lifecycle.

**Architecture:** `toolset/cli_tool.rs::run_cli_command` builds a `ManagedExecRequest` the same way `toolset/shared/command.rs::run_command` does (request deadline from `current_tool_runtime_context()` min'd with the tool's own `timeout_secs`, the request cancellation token, the shared live-output writer) and maps `ManagedExecOutcome` to the same text report it produces today. The local `tokio::time::timeout` and `kill_on_drop` path is deleted.

**Tech Stack:** Rust; `crate::managed_exec::{run_managed_exec, ManagedExecRequest, ManagedExecOutcome}` (`pub(crate)`), `crate::tool_call_lifecycle::runtime::current_tool_runtime_context`.

**Spec:** GitHub issue #1337.

## Global Constraints

- One spawn path for tool subprocesses: `managed_exec`. No `tokio::process::Command` in `toolset/` outside `managed_exec`.
- Behavior preserved: the tool's `timeout_secs` still bounds the run (as the min of request deadline and tool timeout); the output report format (`cwd:`, `command:`, `exit_code:`, `stdout:`, `stderr:`) is unchanged; `env_vars` still override, `PAGER`/`NO_COLOR`/`TERM` still set; `working_dir` validation unchanged.
- Timeout and cancellation are reported distinctly in the error text (`timed out after Ns` stays for timeout; cancellation says `cancelled`).
- Net code deletion.

---

### Task 1: Route `run_cli_command` through `managed_exec`

**Files:**
- Modify: `crates/gents/src/toolset/cli_tool.rs:85-155`
- Read for the pattern: `crates/gents/src/toolset/shared/command.rs:560-640` (`run_command`), `crates/gents/src/managed_exec/process.rs:34-46` (`ManagedExecRequest`), `crates/gents/src/managed_exec/output.rs:10-40` (`ManagedExecOutcome`)
- Test: `crates/gents/src/toolset/cli_tool.rs` (new `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ManagedExecRequest { argv, cwd, deadline_at: Option<DateTime<Utc>>, cancellation_token: CancellationToken, max_output_bytes, stdin, environment: Option<HashMap<String,String>>, tool_name, live_output }`; `ManagedExecOutcome::{Exited{code,stdout,stderr,..}, TimedOut{..}, Cancelled{..}, SpawnFailed{error}}`; `current_tool_runtime_context() -> Option<ToolRuntimeContext>` with `deadline_at`, `cancellation_token`, `live_output`.
- Produces: `run_cli_command(config, argv) -> Result<String>` unchanged signature.

- [ ] **Step 1: Write the failing tests** (unix-only, use `/bin/sh`):

```rust
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(timeout_secs: u64) -> CliToolConfig {
        CliToolConfig {
            name: "sh".into(), binary_path: "/bin/sh".into(), description: String::new(),
            allowed_argv_prefixes: vec![], env_vars: HashMap::from([("GENTS_T".to_string(), "1".to_string())]),
            working_dir: None, timeout_secs,
        }
    }

    #[tokio::test]
    async fn reports_exit_code_and_output_and_env() {
        let out = run_cli_command(&config(5), &["-c".into(), "echo $GENTS_T; echo err 1>&2; exit 3".into()]).await.unwrap();
        assert!(out.contains("exit_code: 3"), "{out}");
        assert!(out.contains("stdout:\n1"), "{out}");
        assert!(out.contains("stderr:\nerr"), "{out}");
    }

    #[tokio::test]
    async fn tool_timeout_kills_the_process_group() {
        let err = run_cli_command(&config(1), &["-c".into(), "sleep 30".into()]).await.unwrap_err();
        assert!(err.to_string().contains("timed out after 1s"), "{err}");
    }

    #[tokio::test]
    async fn request_cancellation_stops_the_command() {
        use crate::tool_call_lifecycle::runtime::{ToolRuntimeContext, with_tool_runtime_context};
        let token = tokio_util::sync::CancellationToken::new();
        let ctx = ToolRuntimeContext { cancellation_token: token.clone(), ..ToolRuntimeContext::default() };
        let cancel = token.clone();
        tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_millis(200)).await; cancel.cancel(); });
        let err = with_tool_runtime_context(ctx, run_cli_command(&config(30), &["-c".into(), "sleep 30".into()])).await.unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
    }
}
```

Adjust the runtime-context constructor/scoping helper names to what `crates/gents/src/tool_call_lifecycle/runtime.rs` actually exports (look for the function `run_command`'s tests or `hook/` tests use to install a context); if no test helper exists, add the smallest `#[cfg(test)]` scoping helper there.

- [ ] **Step 2: Run** — `cargo test -p gents --lib toolset::cli_tool` — the first test may pass already; the cancellation test must FAIL (today's path ignores the token).

- [ ] **Step 3: Implement** — replace the body of `run_cli_command` after `working_dir` validation with a `ManagedExecRequest`: `argv = [binary_path, argv...]`, `cwd`, `deadline_at = Some(min(request deadline, now + timeout_secs.max(1)))`, `cancellation_token` from the context (default token if none), `max_output_bytes: usize::MAX`, `stdin: vec![]`, `environment: Some(current process env + the fixed pager/color/TERM vars + config.env_vars)`, `tool_name: Some(config.name.clone())`, `live_output` from the context. Map: `Exited` → the existing report with `code.unwrap_or(-1)`; `TimedOut` → `bail!("timed out after {}s", timeout_secs.max(1))`; `Cancelled` → `bail!("command cancelled by the owning request")`; `SpawnFailed{error}` → `bail!(error)`. Keep `cap_output` on both streams. Delete the `tokio::process`, `Stdio`, `Duration`, and `kill_on_drop` imports.

- [ ] **Step 4: Run** — `cargo test -p gents --lib toolset::cli_tool` and `cargo test -p gents --lib managed_exec` green; `cargo check -p gents --all-targets` clean; `grep -n 'tokio::process\|kill_on_drop\|tokio::time::timeout' crates/gents/src/toolset/cli_tool.rs` empty.

- [ ] **Step 5: Commit** — `fix(toolset): CliTool subprocesses run under managed_exec (#1337)`.

### Task 2: Gate

- [ ] `cargo test -p gents --lib toolset::` green; `cargo test -p gents --test e2e_runtime` green (covers `--cli-tool` wiring if a test exists; if none exercises `CliTool` end to end, say so in the report, do not add one).
- [ ] `cargo check --workspace --all-targets`, `cargo fmt --all --check` clean.
- [ ] `grep -rn 'tokio::process::Command' crates/gents/src/toolset` returns only `managed_exec` (none in `toolset/`).
