# Harbor Foreground Command Watchdog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decouple the foreground Bash command default timeout from the model-requestable maximum, and configure the Harbor Terminal-Bench adapter at 600s default / 3600s max so a pathological command can no longer occupy a 24-hour benchmark slot.

**Architecture:** The #985 machinery (timeout → normal tool outcome, process-group kill, `min()` against the request deadline) already exists; this change threads a second `foreground_max` value from a new `gents server --command-timeout-max-secs` flag through `ToolCeiling` → `NativeTool` → the bash tools' schema and `resolve_command_timeout`. Unset max = today's exact behavior (max = default). The Harbor adapter (`scripts/harbor/`) then sets 600/3600 and documents the pair separately from the 86400s request deadline.

**Tech Stack:** Rust (crates `gents`, `gents-cli`), POSIX sh (`scripts/harbor/run_gents.sh`), Markdown docs.

**Spec:** `docs/superpowers/specs/2026-08-04-harbor-command-watchdog-design.md`

## Global Constraints

- Not Lean-first: pure configuration resolution, no lifecycle transition or invariant changes. Fence is `toolset/tests.rs` + `tool_surface/tests.rs` + CLI args tests.
- Invariant everywhere: effective max = `max(default, configured_max)` — a misconfigured pair can never push the cap below the default.
- Unset max ⇒ max = default ⇒ byte-for-byte #985 behavior (backward compatible).
- Background path (`spawn_process`, `BACKGROUND_COMMAND_TIMEOUT_SECS` = 36_000) is unchanged.
- Gate each Rust task with `cargo test -p gents` (never `--lib` alone); final task gates with `cargo check --workspace --all-targets`.
- `tracing`, never `println`.
- Each task compiles and passes tests on its own; commit at the end of every task.

---

### Task 1: Toolset core — decoupled timeout resolution, schema, and `NativeTool` pair

**Files:**
- Modify: `crates/gents/src/toolset/shared.rs` (lines 27–59)
- Modify: `crates/gents/src/toolset/bash_tools.rs`
- Modify: `crates/gents/src/toolset.rs` (constant comment ~line 64; `ToolSet::readonly`/`readwrite` ~lines 120–158; tool build match ~lines 225–240; `NativeTool` enum ~lines 279–288; builder methods ~lines 365–439)
- Modify: `crates/gents/src/tool_surface/build.rs` (lines 87–109, mechanical: pass the same value twice; Task 2 threads the real max)
- Modify: `crates/gents/tests/e2e_subagent/r6_background_tools.rs` (~line 405)
- Test: `crates/gents/src/toolset/tests.rs` (existing test at ~line 2187; new test near the schema test at ~line 37)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces (Task 2 and 3 rely on these exact names):
  - `resolve_command_timeout(requested_secs: Option<u64>, foreground_default: Duration, foreground_max: Duration, background: bool) -> Duration`
  - `resolve_command_timeout_in_scope(requested_secs: Option<u64>, foreground_default: Duration, foreground_max: Duration) -> Duration`
  - `NativeTool::BashReadOnly { timeout, timeout_max, allowlist, policy }` and `NativeTool::BashUnrestricted { timeout, timeout_max, root, policy }`
  - `ToolSetBuilder::bash_read_only_with_timeouts(timeout, timeout_max)`, `bash_read_only_with_policy_and_timeouts(policy, timeout, timeout_max)`, `bash_unrestricted_with_timeouts(root, timeout, timeout_max)`, `bash_unrestricted_with_policy_and_timeouts(root, policy, timeout, timeout_max)` (the old `_with_timeout` / `_with_policy_and_timeout` names are renamed; zero-timeout conveniences `bash_read_only()`, `bash_read_only_with_policy()`, `bash_unrestricted()`, `bash_unrestricted_with_policy()` keep their signatures)
  - `ReadOnlyBashTool::with_policy(context, default_timeout, max_timeout, policy)` / `UnrestrictedBashTool::with_policy(context, default_timeout, max_timeout, policy)`; `#[cfg(test)] new()` constructors keep their current signatures and set `max_timeout = default_timeout`.

- [ ] **Step 1: Rewrite the resolution test for the new signature (red)**

In `crates/gents/src/toolset/tests.rs`, replace the whole `command_timeout_resolution_matches_advertised_schema` test (currently at lines 2184–2231, including its `// #985:` comment) with:

```rust
// #985/#1018: the timeout applied when the model omits timeout_secs must equal
// the schema-advertised default; explicit foreground requests may raise it up
// to the operator's foreground ceiling; backgrounded runs get their own
// lifetime budget instead of either foreground value.
#[test]
fn command_timeout_resolution_matches_advertised_schema() {
    use super::shared::resolve_command_timeout;

    let foreground_default = Duration::from_secs(120);
    let foreground_max = Duration::from_secs(3_600);
    assert_eq!(
        resolve_command_timeout(None, foreground_default, foreground_max, false),
        foreground_default,
        "omission must apply the advertised default, not the ceiling"
    );
    assert_eq!(
        resolve_command_timeout(Some(600), foreground_default, foreground_max, false),
        Duration::from_secs(600),
        "explicit foreground requests may exceed the default up to the ceiling"
    );
    assert_eq!(
        resolve_command_timeout(Some(7_200), foreground_default, foreground_max, false),
        foreground_max,
        "explicit foreground requests are capped at the ceiling"
    );
    assert_eq!(
        resolve_command_timeout(Some(5), foreground_default, foreground_max, false),
        Duration::from_secs(5)
    );
    assert_eq!(
        resolve_command_timeout(Some(0), foreground_default, foreground_max, false),
        Duration::from_secs(1)
    );
    assert_eq!(
        resolve_command_timeout(Some(600), foreground_default, foreground_default, false),
        foreground_default,
        "max equal to default reproduces the coupled #985 ceiling"
    );
    assert_eq!(
        resolve_command_timeout(Some(600), foreground_default, Duration::from_secs(1), false),
        foreground_default,
        "a misconfigured max below the default is raised to the default"
    );

    let budget = Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS);
    assert_eq!(
        resolve_command_timeout(None, foreground_default, foreground_max, true),
        budget,
        "background omission uses the background lifetime budget"
    );
    assert_eq!(
        resolve_command_timeout(Some(7_200), foreground_default, foreground_max, true),
        Duration::from_secs(7_200),
        "background requests are exempt from the foreground ceiling"
    );
    assert_eq!(
        resolve_command_timeout(Some(999_999), foreground_default, foreground_max, true),
        budget,
        "background requests are still capped at the background budget"
    );
    assert_eq!(
        resolve_command_timeout(Some(0), foreground_default, foreground_max, true),
        Duration::from_secs(1)
    );
}
```

Also add this new test immediately after the existing `native_tool_definitions_include_model_facing_defaults_and_constraints` test (it ends at ~line 112; the `temp_root(name: &str) -> PathBuf` helper already exists in this file):

```rust
// #1018: when the operator raises the foreground cap above the default, the
// model-visible schema advertises the default and the cap as distinct values.
#[tokio::test]
async fn bash_schema_advertises_decoupled_default_and_max() {
    let root = temp_root("gents-decoupled-timeout");
    let tool = UnrestrictedBashTool::with_policy(
        ToolContext::new(root, false).unwrap(),
        Duration::from_secs(600),
        Duration::from_secs(3_600),
        CommandExecutionPolicy::write_capable(),
    );
    let def = crate::llm::tool::Tool::definition(&tool, String::new()).await;
    assert_eq!(def.parameters["properties"]["timeout_secs"]["default"], 600);
    assert_eq!(def.parameters["properties"]["timeout_secs"]["maximum"], 3_600);
    let description = def.parameters["properties"]["timeout_secs"]["description"]
        .as_str()
        .unwrap();
    assert!(description.contains("600"), "{description}");
    assert!(description.contains("3600"), "{description}");
}
```

If `CommandExecutionPolicy` is not already imported in tests.rs, add it to the existing `use` block that imports the other toolset items.

- [ ] **Step 2: Run the tests to verify they fail to compile (red)**

Run: `cargo test -p gents --lib toolset::tests::command_timeout_resolution_matches_advertised_schema`
Expected: compile error — `resolve_command_timeout` takes 3 arguments but 4 were supplied / `with_policy` takes 3 arguments but 4 were supplied. (`--lib` is fine for the red step only; the green gate below uses the full package suite.)

- [ ] **Step 3: Implement the decoupled resolution in `shared.rs`**

Replace lines 27–59 of `crates/gents/src/toolset/shared.rs` (both functions and the doc comment) with:

```rust
/// Resolves the effective command timeout for a bash tool call (#985, #1018).
///
/// Foreground: an omitted `timeout_secs` applies the tool's configured
/// default — the same value the schema advertises — and explicit requests are
/// clamped to the operator's foreground ceiling, which is never below the
/// default. Background (spawn_process): neither foreground value applies; the
/// run gets the `BACKGROUND_COMMAND_TIMEOUT_SECS` lifetime budget instead.
pub(super) fn resolve_command_timeout(
    requested_secs: Option<u64>,
    foreground_default: std::time::Duration,
    foreground_max: std::time::Duration,
    background: bool,
) -> std::time::Duration {
    if background {
        let budget = std::time::Duration::from_secs(super::BACKGROUND_COMMAND_TIMEOUT_SECS);
        return match requested_secs {
            Some(secs) => std::time::Duration::from_secs(secs.max(1)).min(budget),
            None => budget,
        };
    }
    let ceiling = foreground_max.max(foreground_default);
    match requested_secs {
        Some(secs) => std::time::Duration::from_secs(secs.max(1)).min(ceiling),
        None => foreground_default,
    }
}

/// Scope-aware wrapper: reads whether the current execution was backgrounded
/// from the task-local tool runtime scope.
pub(super) fn resolve_command_timeout_in_scope(
    requested_secs: Option<u64>,
    foreground_default: std::time::Duration,
    foreground_max: std::time::Duration,
) -> std::time::Duration {
    let background = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .is_some_and(|context| context.background);
    resolve_command_timeout(requested_secs, foreground_default, foreground_max, background)
}
```

- [ ] **Step 4: Thread the pair through `bash_tools.rs`**

In `crates/gents/src/toolset/bash_tools.rs`:

Replace `timeout_secs_schema` (lines 14–26) with:

```rust
fn timeout_secs_schema(default_timeout: Duration, max_timeout: Duration) -> serde_json::Value {
    let max_secs = max_timeout.as_secs().max(default_timeout.as_secs());
    serde_json::json!({
        "type": "integer",
        "default": default_timeout.as_secs(),
        "minimum": 1,
        "maximum": max_secs,
        "description": format!(
            "Timeout in seconds; omit for the default ({}s). Explicit values are capped at the foreground ceiling ({}s). Backgrounded runs (spawn_process) instead get a {}s lifetime budget.",
            default_timeout.as_secs(),
            max_secs,
            BACKGROUND_COMMAND_TIMEOUT_SECS,
        )
    })
}
```

Add a `max_timeout: Duration` field to both `ReadOnlyBashTool` and `UnrestrictedBashTool` structs (after `default_timeout`). Update the constructors:

```rust
impl ReadOnlyBashTool {
    #[cfg(test)]
    pub(super) fn new(
        context: ToolContext,
        default_timeout: Duration,
        allowlist: Vec<String>,
    ) -> Self {
        Self {
            context,
            default_timeout,
            max_timeout: default_timeout,
            policy: CommandExecutionPolicy::read_only(allowlist),
        }
    }

    pub(super) fn with_policy(
        context: ToolContext,
        default_timeout: Duration,
        max_timeout: Duration,
        policy: CommandExecutionPolicy,
    ) -> Self {
        Self {
            context,
            default_timeout,
            max_timeout: max_timeout.max(default_timeout),
            policy,
        }
    }
}
```

```rust
impl UnrestrictedBashTool {
    #[cfg(test)]
    pub(super) fn new(context: ToolContext, default_timeout: Duration) -> Self {
        Self {
            context,
            default_timeout,
            max_timeout: default_timeout,
            policy: CommandExecutionPolicy::write_capable(),
        }
    }

    pub(super) fn with_policy(
        context: ToolContext,
        default_timeout: Duration,
        max_timeout: Duration,
        policy: CommandExecutionPolicy,
    ) -> Self {
        Self {
            context,
            default_timeout,
            max_timeout: max_timeout.max(default_timeout),
            policy,
        }
    }
}
```

(Note: the Step 1 test constructs `UnrestrictedBashTool::with_policy(context, 600, 3600, policy)` — 600 default, 3600 max — matching this order.)

In both `definition()` methods change the schema call to:

```rust
"timeout_secs": timeout_secs_schema(self.default_timeout, self.max_timeout),
```

In both `call()` methods change the resolution call to:

```rust
resolve_command_timeout_in_scope(args.timeout_secs, self.default_timeout, self.max_timeout),
```

- [ ] **Step 5: Carry `timeout_max` on `NativeTool` and the builder**

In `crates/gents/src/toolset.rs`:

Update the constant comment (lines 64–66) to:

```rust
// Foreground default aligned with other agent frameworks (Claude Code and
// grok-build both default to 120s); deployments raise or lower it with
// `--command-timeout-secs`. Explicit model requests may exceed it up to the
// separately configured `--command-timeout-max-secs` ceiling (#985, #1018).
```

Add `timeout_max: Duration,` to both bash variants of the `NativeTool` enum (after `timeout`):

```rust
    BashReadOnly {
        timeout: Duration,
        timeout_max: Duration,
        allowlist: Vec<String>,
        policy: CommandExecutionPolicy,
    },
    BashUnrestricted {
        timeout: Duration,
        timeout_max: Duration,
        root: PathBuf,
        policy: CommandExecutionPolicy,
    },
```

In `ToolSet::readonly()` and `ToolSet::readwrite()` add `timeout_max: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),` right after each `timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),` line (three literal sites: lines ~120, ~146, ~153).

In the tool build match (lines ~225–240), destructure and pass the new field:

```rust
                NativeTool::BashReadOnly {
                    timeout,
                    timeout_max,
                    policy,
                    ..
                } => built.push(Box::new(ReadOnlyBashTool::with_policy(
                    read_context.clone(),
                    *timeout,
                    *timeout_max,
                    policy.clone(),
                ))),
                NativeTool::BashUnrestricted {
                    timeout,
                    timeout_max,
                    root,
                    policy,
                } => built.push(Box::new(UnrestrictedBashTool::with_policy(
                    ToolContext::new(root.clone(), true)?,
                    *timeout,
                    *timeout_max,
                    policy.clone(),
                ))),
```

Replace the four timeout-taking builder methods (lines ~365–439) with `_with_timeouts` spellings; the zero-argument conveniences keep their signatures:

```rust
    pub fn bash_read_only(self) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_read_only_with_timeouts(timeout, timeout)
    }

    pub fn bash_read_only_with_timeouts(mut self, timeout: Duration, timeout_max: Duration) -> Self {
        self.tools.push(NativeTool::BashReadOnly {
            timeout,
            timeout_max: timeout_max.max(timeout),
            allowlist: default_read_only_commands(),
            policy: CommandExecutionPolicy::read_only(default_read_only_commands()),
        });
        self
    }

    pub fn bash_read_only_with_policy(self, policy: CommandExecutionPolicy) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_read_only_with_policy_and_timeouts(policy, timeout, timeout)
    }

    pub fn bash_read_only_with_policy_and_timeouts(
        mut self,
        policy: CommandExecutionPolicy,
        timeout: Duration,
        timeout_max: Duration,
    ) -> Self {
        self.tools.push(NativeTool::BashReadOnly {
            timeout,
            timeout_max: timeout_max.max(timeout),
            allowlist: default_read_only_commands(),
            policy,
        });
        self
    }

    pub fn bash_unrestricted(self, root: impl Into<PathBuf>) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_unrestricted_with_timeouts(root, timeout, timeout)
    }

    pub fn bash_unrestricted_with_timeouts(
        mut self,
        root: impl Into<PathBuf>,
        timeout: Duration,
        timeout_max: Duration,
    ) -> Self {
        self.tools.push(NativeTool::BashUnrestricted {
            timeout,
            timeout_max: timeout_max.max(timeout),
            root: root.into(),
            policy: CommandExecutionPolicy::write_capable(),
        });
        self
    }

    pub fn bash_unrestricted_with_policy(
        self,
        root: impl Into<PathBuf>,
        policy: CommandExecutionPolicy,
    ) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_unrestricted_with_policy_and_timeouts(root, policy, timeout, timeout)
    }

    pub fn bash_unrestricted_with_policy_and_timeouts(
        mut self,
        root: impl Into<PathBuf>,
        policy: CommandExecutionPolicy,
        timeout: Duration,
        timeout_max: Duration,
    ) -> Self {
        self.tools.push(NativeTool::BashUnrestricted {
            timeout,
            timeout_max: timeout_max.max(timeout),
            root: root.into(),
            policy,
        });
        self
    }
```

- [ ] **Step 6: Mechanically fix the two remaining callers**

`crates/gents/src/tool_surface/build.rs` lines 87–109 — rename the four calls and pass the ceiling value twice (Task 2 replaces the second argument with the real max):

```rust
    match bash {
        BashMode::Off => {}
        BashMode::ReadOnly => {
            builder = match command_policy.clone() {
                Some(policy) => builder.bash_read_only_with_policy_and_timeouts(
                    policy,
                    ceiling.command_timeout(),
                    ceiling.command_timeout(),
                ),
                None => builder.bash_read_only_with_timeouts(
                    ceiling.command_timeout(),
                    ceiling.command_timeout(),
                ),
            };
        }
        BashMode::Unrestricted => {
            let root = effective_root
                .clone()
                .ok_or_else(|| anyhow!("unrestricted bash requires a configured tool root"))?;
            builder = match command_policy.clone() {
                Some(policy) => builder.bash_unrestricted_with_policy_and_timeouts(
                    root,
                    policy,
                    ceiling.command_timeout(),
                    ceiling.command_timeout(),
                ),
                None => builder.bash_unrestricted_with_timeouts(
                    root,
                    ceiling.command_timeout(),
                    ceiling.command_timeout(),
                ),
            };
        }
    }
```

`crates/gents/tests/e2e_subagent/r6_background_tools.rs` line ~405 — rename and add the second timeout argument with the same value:

```rust
    let bash_tools = gents::ToolSet::builder()
        .bash_read_only_with_policy_and_timeouts(
            gents::CommandExecutionPolicy::read_only(vec!["sleep".to_string()]),
            std::time::Duration::from_secs(120),
            std::time::Duration::from_secs(120),
        )
        .build()
        .build_native_tools()
        .unwrap();
```

- [ ] **Step 7: Run the full package suite (green)**

Run: `cargo test -p gents`
Expected: PASS, including `command_timeout_resolution_matches_advertised_schema`, `bash_schema_advertises_decoupled_default_and_max`, and the untouched schema test `native_tool_definitions_include_model_facing_defaults_and_constraints` (its `maximum == default == 120` assertions still hold because `new()` sets max = default). If any other test constructs the renamed builder methods or destructures the bash `NativeTool` variants without `..`, fix those sites the same mechanical way.

- [ ] **Step 8: Commit**

```bash
git add crates/gents/src/toolset/shared.rs crates/gents/src/toolset/bash_tools.rs crates/gents/src/toolset.rs crates/gents/src/toolset/tests.rs crates/gents/src/tool_surface/build.rs crates/gents/tests/e2e_subagent/r6_background_tools.rs
git commit -m "feat(toolset): decouple foreground command default from requestable max (#1018)"
```

---

### Task 2: `ToolCeiling` carries the foreground max and threads it to the bash tools

**Files:**
- Modify: `crates/gents/src/tool_surface/modes.rs` (struct ~line 64, four constructors ~lines 73–133, builder ~line 150, accessor ~line 175)
- Modify: `crates/gents/src/tool_surface/build.rs` (the four call sites from Task 1 Step 6)
- Test: `crates/gents/src/tool_surface/tests.rs` (existing test `command_timeout_ceiling_reaches_selected_bash_tool` at ~line 77)

**Interfaces:**
- Consumes (Task 1): `bash_*_with_timeouts` builder methods; `NativeTool::BashReadOnly { timeout, timeout_max, .. }`.
- Produces (Task 3 relies on): `ToolCeiling::with_command_timeout_max_secs(timeout_secs: u64) -> Self` (public), `ToolCeiling::command_timeout_max(&self) -> Duration` (pub(crate)).

- [ ] **Step 1: Write the failing test**

In `crates/gents/src/tool_surface/tests.rs`, the existing test `command_timeout_ceiling_reaches_selected_bash_tool` builds a `ToolCeiling::readonly_at(&operator_root).with_command_timeout_secs(120)` and asserts the built `NativeTool::BashReadOnly { timeout, .. }` has `timeout.as_secs() == 120`. Duplicate that entire test (including the full `ToolSelection { ... }` literal — copy it verbatim from the existing test) as a new test directly below it, with these two differences:

```rust
#[test]
fn command_timeout_max_ceiling_reaches_selected_bash_tool() {
    let operator_root = temp_root("gents-command-timeout-max-root");
    let ceiling = ToolCeiling::readonly_at(&operator_root)
        .with_command_timeout_secs(600)
        .with_command_timeout_max_secs(3_600);
    // ... identical BehaviorToolConfig::from_selection(...) body copied from
    // command_timeout_ceiling_reaches_selected_bash_tool ...

    assert!(matches!(
        config.host_tools().native_tools(),
        [crate::toolset::NativeTool::BashReadOnly { timeout, timeout_max, .. }]
            if timeout.as_secs() == 600 && timeout_max.as_secs() == 3_600
    ));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p gents --lib tool_surface::tests::command_timeout_max_ceiling_reaches_selected_bash_tool`
Expected: compile error — no method named `with_command_timeout_max_secs`.

- [ ] **Step 3: Implement on `ToolCeiling`**

In `crates/gents/src/tool_surface/modes.rs`:

Add the field to the struct (after `command_timeout`):

```rust
    command_timeout_max: Option<std::time::Duration>,
```

Add `command_timeout_max: None,` after the `command_timeout: ...` initializer in all four constructors (`meta_only`, `readonly`, `readonly_at`, `readwrite`).

Add the builder directly after `with_command_timeout_secs` (~line 153):

```rust
    /// Foreground cap for explicit `timeout_secs` requests (#1018). Unset ⇒
    /// the cap equals the default, i.e. the coupled #985 behavior.
    pub fn with_command_timeout_max_secs(mut self, timeout_secs: u64) -> Self {
        self.command_timeout_max = Some(std::time::Duration::from_secs(timeout_secs.max(1)));
        self
    }
```

Add the accessor directly after `command_timeout()` (~line 177):

```rust
    pub(crate) fn command_timeout_max(&self) -> std::time::Duration {
        self.command_timeout_max
            .unwrap_or(self.command_timeout)
            .max(self.command_timeout)
    }
```

In `crates/gents/src/tool_surface/build.rs`, in the four calls written in Task 1 Step 6, replace the second `ceiling.command_timeout()` argument (the duplicated one) with `ceiling.command_timeout_max()` — the first argument stays `ceiling.command_timeout()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gents`
Expected: PASS, including both `command_timeout_ceiling_reaches_selected_bash_tool` (unchanged: unset max ⇒ `timeout` still 120) and the new `command_timeout_max_ceiling_reaches_selected_bash_tool`.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src/tool_surface/modes.rs crates/gents/src/tool_surface/build.rs crates/gents/src/tool_surface/tests.rs
git commit -m "feat(tool-surface): thread foreground command max through ToolCeiling (#1018)"
```

---

### Task 3: CLI — `gents server --command-timeout-max-secs` plus evidence logging

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (~line 554, after `command_timeout_secs`)
- Modify: `crates/gents-cli/src/commands/serve.rs` (lines 306–310)
- Test: `crates/gents-cli/src/cli/args/tests.rs` (existing test `server_command_timeout_defaults_and_parses` at ~line 216)

**Interfaces:**
- Consumes (Task 2): `ToolCeiling::with_command_timeout_max_secs(u64)`.
- Produces (Task 4 relies on): the `gents server --command-timeout-max-secs <secs>` flag.

- [ ] **Step 1: Extend the failing args test**

In `crates/gents-cli/src/cli/args/tests.rs`, replace `server_command_timeout_defaults_and_parses` with:

```rust
#[test]
fn server_command_timeout_defaults_and_parses() {
    assert_eq!(parse_server(&[]).command_timeout_secs, 120);
    assert_eq!(parse_server(&[]).command_timeout_max_secs, None);
    assert_eq!(
        parse_server(&["--command-timeout-secs", "300"]).command_timeout_secs,
        300
    );
    assert_eq!(
        parse_server(&["--command-timeout-max-secs", "3600"]).command_timeout_max_secs,
        Some(3600)
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p gents-cli server_command_timeout_defaults_and_parses`
Expected: compile error — no field `command_timeout_max_secs`.

- [ ] **Step 3: Add the flag and wire it**

In `crates/gents-cli/src/cli/args.rs`, directly after the `command_timeout_secs` field (~line 554), add:

```rust
    #[arg(
        long,
        help = "Foreground cap in seconds for explicit Bash timeout_secs requests. Defaults to --command-timeout-secs; values below it are raised to it (#1018)"
    )]
    pub(crate) command_timeout_max_secs: Option<u64>,
```

In `crates/gents-cli/src/commands/serve.rs`, replace lines 306–310 (`tool_ceiling = ...` through the `tracing::info!` block) with:

```rust
    tool_ceiling = tool_ceiling.with_command_timeout_secs(args.command_timeout_secs);
    if let Some(max_secs) = args.command_timeout_max_secs {
        tool_ceiling = tool_ceiling.with_command_timeout_max_secs(max_secs);
    }
    let effective_command_timeout_max_secs = args
        .command_timeout_max_secs
        .unwrap_or(args.command_timeout_secs)
        .max(args.command_timeout_secs)
        .max(1);
    tracing::info!(
        command_timeout_secs = args.command_timeout_secs.max(1),
        command_timeout_max_secs = effective_command_timeout_max_secs,
        "configured foreground command timeout default and ceiling"
    );
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p gents-cli`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gents-cli/src/cli/args.rs crates/gents-cli/src/cli/args/tests.rs crates/gents-cli/src/commands/serve.rs
git commit -m "feat(cli): add gents server --command-timeout-max-secs (#1018)"
```

---

### Task 4: Harbor adapter — 600s default / 3600s max, documented separately

**Files:**
- Modify: `scripts/harbor/run_gents.sh` (line 20 and the `start_server` block at lines 68–78)
- Modify: `scripts/harbor/README.md` (example command ~lines 47–71, the paragraph at ~lines 73–81, overrides table ~lines 107–124)

**Interfaces:**
- Consumes (Task 3): `gents server --command-timeout-max-secs`.
- Produces: env vars `GENTS_COMMAND_TIMEOUT_SECS` (default 600) and `GENTS_COMMAND_TIMEOUT_MAX_SECS` (default 3600).

- [ ] **Step 1: Update `run_gents.sh`**

Change line 20 from `: "${GENTS_COMMAND_TIMEOUT_SECS:=86400}"` to:

```sh
: "${GENTS_COMMAND_TIMEOUT_SECS:=600}"
: "${GENTS_COMMAND_TIMEOUT_MAX_SECS:=3600}"
```

In `start_server()`, after the `--command-timeout-secs "${GENTS_COMMAND_TIMEOUT_SECS}" \` line, add:

```sh
    --command-timeout-max-secs "${GENTS_COMMAND_TIMEOUT_MAX_SECS}" \
```

- [ ] **Step 2: Syntax-check the script**

Run: `sh -n scripts/harbor/run_gents.sh`
Expected: exit 0, no output.

- [ ] **Step 3: Update `README.md`**

Three edits:

1. In the example command, delete the line `  --ae GENTS_COMMAND_TIMEOUT_SECS=86400` and remove the now-trailing ` \` from the preceding `  --ae GENTS_REQUEST_TIMEOUT_SECS=86400 \` line so it becomes the final argument.

2. Replace the sentence `The explicit 24-hour Gents limits serve the same purpose inside the runtime.` (in the paragraph following the example) with:

```markdown
The explicit 24-hour request deadline serves the same purpose inside the
runtime. Foreground commands are deliberately bounded far below it (#1018):
each command defaults to a 600-second timeout and the model may explicitly
request up to 3,600 seconds per command. A command that hits its timeout is
killed as a process group and returns a normal `status: "timeout"` tool
outcome with partial output, so the model can recover or narrow the command
instead of silently occupying the benchmark slot; longer work belongs in
`spawn_process`, which has a 10-hour background lifetime budget. Both values
are advertised to the model in the bash tool schema, logged at server startup
in `gents-server.log`, and recorded per call as `timeout_ms` in the persisted
command result.
```

3. In the overrides table, replace the row

`| \`GENTS_COMMAND_TIMEOUT_SECS\` | \`86400\` | Foreground shell command ceiling |`

with these two rows:

```markdown
| `GENTS_COMMAND_TIMEOUT_SECS` | `600` | Foreground command timeout applied when the model omits `timeout_secs` |
| `GENTS_COMMAND_TIMEOUT_MAX_SECS` | `3600` | Foreground cap for explicit `timeout_secs` requests; kept far below `GENTS_REQUEST_TIMEOUT_SECS` so a pathological command returns control to the model (#1018) |
```

- [ ] **Step 4: Commit**

```bash
git add scripts/harbor/run_gents.sh scripts/harbor/README.md
git commit -m "feat(harbor): bound foreground commands at 600s/3600s below the request deadline (#1018)"
```

---

### Task 5: Full workspace gates

**Files:** none new — verification only.

- [ ] **Step 1: Full package suite**

Run: `cargo test -p gents && cargo test -p gents-cli`
Expected: PASS.

- [ ] **Step 2: Whole-workspace compile check**

Run: `cargo check --workspace --all-targets`
Expected: clean — this catches construction sites in examples, desktop crates, and test targets that `cargo test -p gents` does not compile (e.g. any other user of the renamed builder methods or the widened `NativeTool` variants). Fix any breakage mechanically (add the second timeout argument with the same value as the first) and re-run both gates.

- [ ] **Step 3: Commit any fixes**

```bash
git add -A
git commit -m "fix: repair workspace construction sites for decoupled command timeouts (#1018)"
```

Only commit if Step 2 required fixes; otherwise nothing to do.
