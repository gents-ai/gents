# Native `lsp` tool (design)

**Date:** 2026-08-12
**Status:** Accepted. Implementation plan: `docs/superpowers/plans/2026-08-12-lsp-tools.md`
**Issue:** #1106
**Branch:** `feat/lsp-tools`
**Worktree:** `../gents-lsp-tools`

Capability port of oh-my-pi language-server **actions** into Gents. The
runtime wiring uses the abstractions that already exist: a lazy optional
tool assembled in `ToolSurface::build_tools`, one reconciled configuration
generation, one command-launch implementation, one file-mutation lock
registry, and no language-server process unless the model invokes `lsp`.

OMP is a behavior reference, not a client to copy:

- `packages/coding-agent/src/lsp/` and `docs/tools/lsp.md` — action set,
  catalog, numeric caps
- Do **not** import `lsp.json`, lspmux, workspace-local bin search,
  `workspace.applyEdit: true`, or OMP's single-shot process model

## Problem

Gents coding behaviors have `read_file` / `grep` / `edit_file` / `bash`.
Those tools cannot follow shadowing, re-exports, or cross-file callsites.

Earlier drafts of this spec bolted LSP onto the wrong seams:

- Treated `run_managed_exec` as if it could host a persistent stdio server.
  It cannot: it owns the pipes, waits for exit, and kills on the **request**
  deadline (`managed_exec/process.rs`).
- Treated `ToolSelection.lsp_config` as a CLI-tool equivalent. CLI binaries
  come from `ToolCeiling.cli_tools`; the selection only picks an approved
  name (`tool_surface/build.rs`). Default self-config category `tools` can
  patch ToolSelection.
- Reused bash's `CommandExecutionPolicy` while bash is `Off`. Parsing forces
  that policy to read-only and `build_host_tools` discards it entirely
  (`selection.rs`, `build.rs`).
- Had `reload "*"` re-read ToolSelection, opening a second config lane
  beside reconcile.
- Let `write_file` cold-start a server. Daemon sessions are not closed after
  every request; `hook.close` is principally the oneshot path.

## Decision

| Choice | Decision |
| --- | --- |
| Model surface | One tool named `lsp`, OMP action enum |
| Gate | `ToolSelection.enable_lsp`, default false, never backfilled |
| What the gate *is* | Advertisement + host-exec grant for the **built-in catalog** only, plus file-tier `FileCap` |
| Custom servers | **Not in v1.** Ceiling `lsp_servers` (CLI-tool shaped) is a later extension |
| `lsp_config` | Operator may set settings / init options. Self-config may not. Never `command` or `args` |
| Assembly | `LspToolConfig` on `ToolSurface`; `LspTool` built in `ToolSurface::build_tools` with `runtime.lsp_pool` — not `NativeTool` / `ToolSet::build_native_tools` |
| Spawn primitive | Extract `spawn_managed_process` from `managed_exec`; rebuild `run_managed_exec` on it |
| Command constraints | Project `CommandConstraints` from **effective** `static_policy` after meet; ignore only `bash.tool` and the read-only command allowlist |
| Config path | Reconcile only. `reload` uses the current `ToolSurface` snapshot |
| Start policy | Action start matrix. Writethrough never cold-starts. Biome/SwiftLint only on explicit `diagnostics` |
| Idle | Default 5 minutes. Leased clients; LRU only among zero-lease Ready entries |
| Mutations | `applyEdit: false`. Apply only foreground-returned edits |
| File IO | Shared file-mutation module; inbound / WorkspaceEdit / read-output URI rules are distinct |
| Network default | Omitted: `disabled` under seatbelt, `inherit` elsewhere. **Explicit `disabled` is never coerced** |
| Policy | `Surface.lsp : Bool` and `lspActionAuthorized` as `Prop` |
| Failures | Existing `FailureClass` via `ReportedFailure` |
| Lifecycle | Existing `nativeCommand`. No slot-drain callback. `session::close_session` stays persistence-only |

## Non-goals

- Desktop editor for the new fields (#580). Preserve-on-absent only.
- Custom / user-defined language-server executables in v1.
- Sharing one server across sessions (lspmux / broker).
- Filesystem LSP config.
- Advertising `lsp` when file tools are `Off`.
- Backgrounding `lsp`.
- New `ToolExecution` states or a new `FailureClass`.
- Pointing a language server at Gents `--home`.
- A separate compiler-based workspace-diagnostics fallback (`cargo check`,
  `tsc`, `go build`, `pyright`). Language servers **may** spawn compilers
  and other descendants; Gents does not add a second diagnostics path.
- Resolving the **initial** language-server executable from workspace-local
  bins (`node_modules/.bin`, `.venv/bin`, …). Descendants are not similarly
  fenced.
- `workspace/executeCommand` and server-initiated `workspace/applyEdit`.
- Re-reading ToolSelection (or any other document) from `reload`.
- Invoking `GlobTool` and parsing its model-facing text.
- A POSIX multi-file transaction. Same honesty as #724.
- Claiming `network: disabled` is enforceable under `Unrestricted`.
- Silently coercing an explicit `network_mode: disabled` to `inherit`.
- A reconcile “slot drained” callback, or putting process teardown in
  `session::close_session`.

## Architecture

```text
reconcile → ToolSurface (immutable generation)
                │
                ├─ enable_lsp ∩ file ≠ Off  → advertise lsp
                ├─ LspToolConfig (catalog ∩ lsp_config flags, digest)
                └─ WriteFileTool / EditFileTool + optional LspWritethrough
                         │
ToolSurface::build_tools(runtime)
                │
                └─ LspTool { config, pool: runtime.lsp_pool }

explicit lsp action  (see start matrix)
                │
                ▼
LspPool.get_or_start
  (MCP no-lock-across-I/O + explicit per-key Starting/singleflight)
                │
                ▼
spawn_managed_process  (pool-owned cancel token, persistent stdio)
  spawn the admitted canonical path, not the bare catalog name
                │
run_managed_exec  = spawn + write stdin + wait + terminate
  (bash and the fs-runner stay callers; CLI tools do not)

reload   → retire entries matching this
           (session_id, behavior_id, workspace_root, config_digest)
reconcile → new surface + new digest; old clients become Retiring
            and die via zero-lease LRU / 5-minute TTL
```

## Components

### 1. ToolSelection

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `enable_lsp` | `Boolean` | `false` / unset | Never backfill true |
| `lsp_config` | `String` | unset | JSON object. **No executable identity.** |

`lsp` is a reserved builtin name.

`from_selection` sets `surface.lsp = enable_lsp && file != Off`. Meet is AND.

`lsp_config` is a JSON **string** column, not a GraphQL struct, so
excluding `command`/`args` is a decoder rule, not a schema guarantee.
Server `settings` / `init_options` can still name interpreters, plugins,
or extra commands. Treat those objects as opaque executable-adjacent
config.

**Operator-authored** `lsp_config` (apply / CLI / desktop preserve) may
carry:

- scalars: `idle_timeout_ms` (default **300000**), `format_on_write`,
  `diagnostics_on_write`, `diagnostics_on_edit`,
  `diagnostics_deduplicate`, `network_mode`
- per catalog name: `disabled`, `priority`, `warmup_timeout_ms`,
  `capabilities`, `workspace_ready_timings`, `language_id`,
  **`settings`, `init_options`**

Always rejected: `command`, `args`, `resolvedCommand`, `createClient`,
and any server name not in the built-in catalog.

**SelfConfig** (`configure_tools`) may patch only:

- `enable_lsp`
- the scalar controls listed above
- per-server `disabled`, `priority`, `warmup_timeout_ms`

It must **not** patch `settings`, `init_options`, `command`, `args`, or
any other object-valued server field. Lean `writableFields` for
`.toolSelection` gains `enable_lsp` and a constrained `lsp_config`; the
Rust patch decoder enforces the key allowlist. Tests must reject a
self-config patch that smuggles `settings.rust-analyzer.server.path`
(or equivalent).

If `enable_lsp` is self-configurable — and the default `tools` category
makes it so whenever self-config is on — **enabling that category
delegates authority to activate any detected built-in catalog server**.
It is no longer solely a direct operator grant. `tools explain` must say
this. Operators who want the grant to stay exclusive leave
`enable_self_config` off, or omit `tools` from
`self_config_categories`.

### 2. No custom servers in v1

Built-in catalog commands are admitted by `enable_lsp`. That is the v1
host-exec grant.

A later extension, if needed, copies the CLI-tool pattern exactly:

- `ToolCeiling.lsp_servers: Vec<LspServerConfig>` holds `name`,
  `command`, `args` (operator / host owned).
- ToolSelection may only *select* an approved name (and still cannot
  change `command`/`args`).
- `build` drops names that are not in the ceiling, same warning as
  `dropping CLI tool not present in tool ceiling`.

Do not add that ceiling vector until there is a concrete operator need.

### 3. Built-in catalog

`crates/gents/src/toolset/lsp/defaults.json` is an **array** (ordered).
Each entry has `name`, `command`, `args`, `file_types`, `root_markers`,
`priority`, and the rest of the OMP server fields except runtime-owned
ones.

Detection: a `root_markers` entry exists in the `ToolContext` workspace
root (one-level wildcard; no parent walk) **and** `command` resolves on
the **host PATH** to a canonical path that is **not** under the tool
root. After admission, spawn that **canonical path**, not the original
bare name. `config_digest` hashes the canonical path.

Rustup proxies are not an exception to that rule. When a host
`rust-analyzer` entry canonicalizes to `rustup`, resolve the selected
toolchain from rustup's on-disk selection state (`RUSTUP_TOOLCHAIN`, directory
override, nearest `rust-toolchain[.toml]`, then the default toolchain) and
admit/spawn/hash the selected `toolchains/.../bin/rust-analyzer`. Do not run
`rustup which` outside managed execution and do not spawn the proxy as the
`rustup` CLI.

Family rules (first eligible by `priority`, then catalog order):

| Family | Rule |
| --- | --- |
| TypeScript / JS | `denols` if `deno.json` / `deno.jsonc` / `deno.lock`; else `typescript-language-server` |
| Python | `basedpyright`, then `pyright`, then `pylsp` |
| Ruby | `ruby-lsp`, then `solargraph` |
| Elixir | `expert`, then `elixirls` |
| Nix | `nixd`, then `nil` |
| PHP | `intelephense`, then `phpactor` |

Linters never win primary routing.

Biome and SwiftLint are **single-shot adapters**, not pooled language
servers. They may run only during an explicit `lsp` `diagnostics` call
(admitted PATH binary, `prepare_managed_command` + `run_managed_exec`).
They do **not** participate in writethrough: there is no already-Ready
pooled client, and invoking them would violate no-cold-start.

### 4. Extract `spawn_managed_process`

`run_managed_exec` is single-shot. LSP needs persistent bidirectional
stdio after the initiating tool call returns. Do not pretend otherwise.

Refactor `crates/gents/src/managed_exec` into:

```text
spawn_managed_process(request) -> Result<ManagedProcess>
  ManagedProcess { stdin, stdout, stderr, wait, terminate }

run_managed_exec(request) =
    spawn_managed_process
    + optional stdin write
    + wait (request cancel / deadline → terminate)
    + collect capped stdout/stderr
```

`ManagedProcess` keeps the existing process-group / job-object teardown,
`env_clear`, `ActiveExecGuard`, and Lean `ManagedExec` states. The Lean
machine still describes one child (`pendingSpawn → running → terminal`).
This is a Rust extract, not a new lifecycle.

`ActiveExecGuard` stays, but the snapshot must distinguish kinds:

- `ForegroundCommand` — bash / fs-runner; long age is a wedged tool call
- `PersistentService` — pooled LSP (and later anything like it); a
  five-minute rust-analyzer is healthy, not wedged

`active_executor_snapshots` (and any status UI that consumes it) must
surface that kind. Do not report a live language server as a stuck
foreground execution.

**Ownership of cancellation:**

| Token | Owner | Kills the server? |
| --- | --- | --- |
| Process-lifetime token | `LspPool` entry | Yes — idle, LRU, digest retire, session/runtime shutdown, failed initialize |
| Request token | current tool call | **No.** Sends `$/cancelRequest` only |
| Initialize | the get-or-start attempt | Yes, **before** the entry is published to the pool |

A request deadline must not tear down a healthy pooled server.

### 5. Extract command constraints from `BashMode`

Today command policy is bash-shaped. With bash `Off`,
`command_policy_from_document` forces `ReadOnly` and `build_host_tools`
returns `None` and never attaches the policy.

Extract a bash-independent `CommandConstraints`:

- `allowed_argv_prefixes`
- `forbidden_argv_prefixes`
- `network_mode`
- sandbox selection (`workspace_write` seatbelt vs none)
- environment filter (`build_shell_env`)

The runtime already computes

`static_policy = behavior ∩ ceiling ∩ runtime`

(`behavior_config.rs`: `ToolPolicySurface::effective`). Bash then
projects `ToolPolicyBash` off that meet. LSP must project
`CommandConstraints` from the **same effective policy**, not from raw
ToolSelection fields. Otherwise a ceiling's prefixes, network, or
sandbox never reach the spawn and a tighter ceiling does not change the
digest.

Ignore only:

- `bash.tool` (`BashMode`) — LSP is not bash
- `read_only_allowlist` — `enable_lsp` is the grant, not
  `default_read_only_commands`

Take from the effective policy: `allowed_argv_prefixes`,
`forbidden_argv_prefixes`, `network_mode`, `sandbox` /
`execution_mode` (for seatbelt vs unrestricted). Environment is always
`build_shell_env()`.

`BashMode` plus those same effective fields still derive bash's
`CommandExecutionPolicy`. Two consumers of one meet.

One helper prepares a spawn:

`prepare_managed_command(root, command, args, constraints) -> (canonical_program, argv, env, sandbox)`

PATH lookup, canonicalize, `workspaceExecutable`, sandbox wrap, and
`CORE_ENV_VARS` live only there. The returned program is the canonical
path. Bash `run_command` and LSP spawn both call it.

### 6. Network and platform honesty

Existing `validate_network_mode` rejects `network: disabled` under
`Unrestricted` (`DisabledNetworkUnenforceable`). macOS `workspace_write`
can enforce disabled network via seatbelt. Off macOS, write-capable bash
is already `Unrestricted`.

**When `network_mode` is omitted:**

| Platform | Spawn sandbox | Network |
| --- | --- | --- |
| macOS with `sandbox-exec` | `workspace_write` | `disabled` |
| anything else | unsandboxed (`Unrestricted`) | **`inherit`** |

Linux stays available. That is the product call.

**When `network_mode` is explicitly `disabled`:** do **not** coerce it
to `inherit`. Use the existing command-policy idiom
`DisabledNetworkUnenforceable` under `Unrestricted`. The tool is then
unavailable / `policyDenied` on that host, and `tools explain` reports
that the requested restriction cannot be enforced.

`inherit` remains a valid explicit setting everywhere.

`tools explain` and `RuntimeToolAvailability` must also state, off
macOS, that the **server process and its descendants** can write
outside the tool root and can use the inherited network. The macOS
seatbelt profile allows `process-exec` / `process-fork`; it does not
stop rust-analyzer from launching `cargo` / `rustc`.

macOS without `sandbox-exec`: existing `workspaceWriteSandboxUnavailable`.

### 7. `LspTool` assembly and file-tool seam

`ToolSet::build_native_tools()` has no `ToolRuntimeContext`. Runtime
tools (memory, `defra_query`, meta, self-config) are assembled in
`ToolSurface::build_tools`.

- `LspToolConfig` lives on `ToolSurface` (catalog ∩ `lsp_config`, digest,
  writethrough flags, spawn constraints, workspace root).
- `LspTool` is built in `build_tools` as
  `LspTool::new(config, runtime.lsp_pool.clone())`.
- Do **not** add `NativeTool::Lsp`.
- `ToolRuntimeContext` grows `lsp_pool: LspPool` next to `mcp_pool`.

Writethrough is an optional `LspWritethrough` handle passed into
`WriteFileTool` and `EditFileTool` **before they are boxed**.
`build_native_tools` (or a `build_tools`-only sibling) takes the handle
and forwards it to those constructors. Do not try to recover a concrete
file tool from `Box<dyn ToolDyn>`. No general hook subsystem.

Move out of `file_tools.rs` into `toolset/file_mutation.rs` (or similar):

- `content_hash`
- `file_mutation_lock_for`
- workspace-root / `ToolContext` resolution used by both file tools and LSP

`WorkspaceEdit` apply is three stages, because format-on-write already
holds the path lock (non-reentrant `tokio::sync::Mutex`):

1. `prepare` — parse, reject non-`file:`, resolve URIs through
   `ToolContext`, validate ranges, compute lock keys and new bytes.
2. `acquire` — take `file_mutation_lock_for` in sorted order.
3. `apply_with_held_locks` — hash/version check **every prepared file
   before the first write**, then write, with no further acquire.

Format-on-write calls `prepare` then `apply_with_held_locks` on the
locks it already owns. The ordinary rename path calls all three.

### 8. On-demand start, idle, leases, caps

- File writethrough uses **already-Ready** clients only. If none match,
  skip format/diagnostics and do not start anything.
- Default `idle_timeout_ms` is **300_000** (5 minutes).
- Caps: **4** live clients per `(session_id, behavior_id)`; **16** live
  clients per host `LspPool`. Starting entries **count**.

Pool entry states: `Starting`, `Ready`, `Retiring`.

A borrow returns a **lease** that covers the foreground JSON-RPC
request. Dropping the lease may complete retirement.

**Non-destructive eviction** (reload, idle sweep, cap eviction,
generation retirement — one rule):

- Retirement removes the entry from future lookup and marks `Retiring`.
- The process is terminated only when the last lease drops.
- LRU considers **zero-lease `Ready` entries only**.
- If every candidate is `Starting` or leased, return
  `serviceUnavailable`. Do not kill an active request.
- EOF or a reader failure makes a `Ready` client ineligible immediately;
  the next server-backed action evicts it and starts a replacement.

`reload` (file or `*`) retires entries whose key matches the current
`(session_id, behavior_id, workspace_root, config_digest)` — not every
host entry that happens to share a digest.

**Connect discipline:** reuse MCP's *no lock across I/O* (`get_or_connect`
drops the map lock before dialing). Do **not** call that singleflight.
MCP only serializes keys already in the parking map; a fresh key can
still start two connections. LSP adds an explicit per-key `Starting`
state / waiters. Tests: two simultaneous first calls spawn one process;
failed initialize wakes all waiters; a later call after backoff retries.

### 9. Snapshot ownership

`reload` uses the current `LspToolConfig` digest. It does not read
DefraDB.

Document changes flow only through reconcile. A new surface gets a new
digest and a new `LspTool`. Old-generation clients are marked `Retiring`
and die via the lease / LRU / 5-minute TTL rule above.

There is **no** “after old slot drained” hook today:
`retire_slot` detaches a join, and `on_slot_retired` runs separately
before that join finishes (`reconcile.rs`, `reconcile/slot.rs`). Do not
invent a reconcile-specific LSP callback in v1.

`session::close_session` stays persistence-only. The runtime or oneshot
owner that has **both** the session id and the pool calls
`LspPool::close_session`. `run_agent` explicitly awaits
`lsp_pool.shutdown()` after behavior slots finish
(`shutdown_slots` in `reconcile.rs`, then pool shutdown). Oneshot calls
`LspPool::close_session` after `hook.close()` (which only writes the
session row).

### 10. Pool identity

Key:

`(session_id, behavior_id, workspace_root, server_name, config_digest)`

`config_digest` covers **authority and client state**, not just argv:

- effective tool root / writable root
- full **effective** `CommandConstraints` (post-meet prefixes, network,
  sandbox)
- resolved **canonical** command path
- fixed catalog args
- `language_id`
- `init_options`, `settings`, `capabilities`, readiness timings
- writethrough flags do **not** need to be in the digest (they do not
  change an already-running initialize)

A tighter ceiling or policy produces a new digest. A process admitted
under a looser generation cannot be reused.

### 11. Native tool actions

| Action | Mutates files? | May cold-start? | Notes |
| --- | --- | --- | --- |
| `status` | no | **no** | Report each configured server as starting/indexing, ready, retiring, failed/backoff, unavailable (with the failed catalog check), or not started with the server-backed action needed to start it. |
| `reload` | no | **no** | Retire matching current-key clients. Does not start a replacement. |
| `capabilities` | no | yes | |
| `diagnostics` | no | yes (pooled servers). Biome/SwiftLint single-shot only here. | File, typed glob, or `file: "*"`. |
| `definition` / `type_definition` / `implementation` / `references` / `hover` / `symbols` | no | yes | Returned **structured** locations go through `ToolContext`. |
| `rename` / `rename_file` | yes | yes | Foreground-returned edits only. |
| `code_actions` | list no; apply yes | yes | Apply `CodeAction.edit` only. Bare `Command` → `argumentInvalid`. |
| `request` | see below | yes | |

`request` on ReadOnly: only the read-method allowlist. Deserialize the
**known** parameter shape for that method (`textDocument`, `position`,
…). Every URI-shaped field in those params goes through `ToolContext`;
bare paths are treated as paths and non-`file:` schemes are rejected. Do
**not** forward an arbitrary `payload` verbatim on a ReadOnly surface.

`request` on ReadWrite: `payload` is still parsed JSON, and every
URI-shaped field in it is validated the same way. Unknown methods are
`argumentInvalid`.

Cold-start semantic navigation retries `null` and empty-array responses
at most three times during the indexing window. The user-supplied tool
timeout bounds the whole retry loop, not each individual attempt. This
keeps a genuinely empty result from consuming the full 20–300 second
budget. Empty diagnostics remain a successful clean result.

### 12. Client capabilities

- `workspace.applyEdit = false`. Incoming `workspace/applyEdit` replies
  `{ applied: false }` and performs no IO.
- No `workspace/executeCommand` in v1.
- `positionEncodings = ["utf-8", "utf-16"]`; prefer utf-8.
- `didOpen` / `didChange` / `didClose`, diagnostics cache, `$/cancelRequest`.

### 13. File-boundary URI rules

Three distinct rules:

1. **Inbound / model-supplied** path or `file:` URI (action `file`, glob
   root, `request` params) that escapes `ToolContext` → terminal
   `policyDenied`.
2. **WorkspaceEdit** URI that escapes → abort preflight with
   `policyDenied`, no writes.
3. **Read-output** structured location (definition, references,
   diagnostic range, workspace symbol, `Location` / `LocationLink`) that
   escapes → omit/redact that result, append a note, **complete** the
   call. Do not fail the whole lookup.

The hard no-escape guarantee applies to **structured URI/location
fields** only. Free-form hover Markdown and diagnostic `message` text
can still mention host paths. If we need “no absolute host path reaches
the model,” that is a **separate text redactor** over those strings, not
the location preflight. v1 ships the structured-field rules; the text
redactor is optional and called out in `tools explain` if omitted.

Model-facing document symbols retain qualified container names, symbol
kind, and 1-indexed line. The 200-symbol cap is global across nested
children, and normal tool truncation keeps the head (including the result
header). Push, pull, and workspace diagnostic envelopes are normalized to
the same capped message-bearing representation before location redaction.

### 14. WorkspaceEdit

`prepare` / `acquire` / `apply_with_held_locks` as above.

- Reject non-`file:` URIs.
- Overlapping ranges on one file → `argumentInvalid`.
- Tracked documents (we have a `didOpen` version): the edit's `version`
  must match.
- Before a file action, compare the disk content hash to the last
  `didOpen` / `didChange` payload and send a full-content `didChange` when
  an out-of-band edit made the tracked buffer stale.
- Unversioned files (server never opened them): validate ranges against
  **current** bytes under the lock; then write.
- The `content_hash` check protects the interval between preflight and
  write, **not** freshness since the LSP request was sent, and **not**
  files the server never versioned. External writers remain the #724
  race: last writer wins outside this process.

Stop on first write failure; report applied vs pending.

### 15. Writethrough

Keep **didChange, formatting, re-hash, and format apply** under the
existing mutation lock. Release the lock **before** waiting for
diagnostics. A multi-second diagnostic wait does not protect a mutation
and must not block concurrent `write_file` / `edit_file`.

1. If no **already-Ready** pooled client matches, return. No start.
   Biome/SwiftLint are not consulted here.
2. Under the lock: `didChange`; if `format_on_write` on `write_file`,
   `prepare` + `apply_with_held_locks` (re-hash; mismatch → skip +
   note). Capture the document version / `content_hash`.
3. **Release the lock.**
4. If diagnostics-on-write/edit: wait briefly off the lock. Render only
   diagnostics whose document version is **at least** the captured
   version. Dedup when configured.

Failures never fail the original write.

### 16. Position encoding

Negotiate utf-8 / utf-16. Convert in `toolset/lsp/encoding.rs`. Tests:
ASCII, combining marks, CJK, non-BMP (`😀`) for navigation and edits.

### 17. Workspace diagnostics and typed glob

`file: "*"`: if a matching server advertises `workspace/diagnostic`, use
it. Otherwise do not walk the tree. Completed note: pass a file or glob.

Glob: do **not** call `GlobTool` or parse its text. Extract a typed
internal helper that issues `NativeFsRunnerRequest::Glob` (same ignore,
sandbox, traversal budget) and returns `Vec<PathBuf>` (or the structured
match list), capped at 20. `raw_json` on the public tool is not an API
to scrape.

### 18. Session scope

Add `session_id` (and, for the pool key, `behavior_id`) to
`ToolRuntimeScope` — the existing task-local. Dispatchers already have
both. Background dispatch captures `session_id` before `tokio::spawn` and
passes it explicitly into the spawned task's runtime scope.

`LspPool` is host-owned on `ToolRuntimeContext`. The owner that holds
the pool calls `LspPool::close_session` / `shutdown` (section 9). Idle /
zero-lease LRU / TTL are the steady-state control.

## Formal model

No new `ToolExecution` states. `lsp` is `nativeCommand`.

### ToolPolicy

`Surface.lsp : Bool`, meet AND, `effective_lsp_le_*`, conformance JSON,
Rust vocab aligned.

### LspAction

```lean
def lspAdvertised (lsp : Bool) (file : FileCap) : Prop :=
  lsp = true ∧ file ≠ FileCap.off

def lspActionAuthorized (lsp : Bool) (file : FileCap) (action : LspAction) : Prop :=
  lspAdvertised lsp file ∧
    (¬action.mutates ∨ file = FileCap.readWrite)

def lspApplyAuthorized (lsp : Bool) (file : FileCap) (src : LspMutationSource) : Prop :=
  lspAdvertised lsp file ∧
    file = FileCap.readWrite ∧
    src = LspMutationSource.foregroundReturnedEdit
```

Do not mix `Bool` and `Prop`. If a `Bool` is needed for JSON contracts,
define `decide` instances; the theorems quantify over `Prop`.

The first draft hardcoded `lspAdvertised true file`. That did not require
the effective gate. Do not.

Theorems: ReadOnly ∩ mutating = false; `serverApplyEdit` never authorized;
`lsp = false` never authorized; ReadWrite ∩ advertised ∩ foreground edit
authorized.

### CommandPolicy

`DenialReason.workspaceExecutable` for a resolved path under the tool
root. Thread through `toContract` and `CommandPolicyDenial`.

`CommandConstraints` is a Rust extract of fields already in Lean
`BashPolicy` / `CommandRequest`. Do not fork a second Lean policy
machine.

### ManagedExec

No new states. `spawn_managed_process` is how Rust enters `running`;
`run_managed_exec` still waits to a terminal. Pool-held children stay
`running` until `terminate`.

### SelfConfig

Add `enable_lsp` and `lsp_config` to `allFields` / `writableFields`.
The writable `lsp_config` decoder accepts only the SelfConfig key
allowlist (scalars + per-server `disabled` / `priority` /
`warmup_timeout_ms`). `settings` and `init_options` are not writable.

## Failures

| Situation | Class | Terminal |
| --- | --- | --- |
| Bad args, overlapping ranges, version mismatch, unknown `request` method, bare `Command` code action | `argumentInvalid` | `failed` |
| Inbound URI escape; WorkspaceEdit URI escape; under-root executable; mutating action without ReadWrite; `lsp = false`; explicit `network_mode: disabled` under `Unrestricted` | `policyDenied` | `failed` |
| No matching server, binary missing, initialize failure, cap hit with no zero-lease candidate | `serviceUnavailable` | `failed` |
| Stdio / process death mid-request | `transport` | `failed` |
| LSP error response | `toolReturnedError` | `failed` |
| Request deadline / tool timeout | `external` / `timedOut` | does **not** kill the pooled server |
| Approval deny | `approvalDenied` | `failed` |
| Empty navigation / clean diagnostics / no Ready client for writethrough / read-output location redacted | none | `completed` |

## Limits

| Cap | Value |
| --- | --- |
| Tool timeout default / min / max | 20s / 5s / 300s |
| JSON-RPC request timeout | 30s |
| Initialize | 5s |
| Project-load wait | 15s |
| Idle TTL default | **5 min** |
| Idle sweep | 60s |
| Init-failure backoff | 3 min |
| Clients per session+behavior | 4 |
| Clients per host pool | 16 |
| JSON-RPC `Content-Length` | 8 MiB |
| Server stderr ring | 16_000 chars |
| Pending requests per client | 32 |
| Diagnostic messages | 50 |
| Glob diagnostic targets | 20 |
| Workspace symbols | 200 |
| Reference context | 50 |
| Rename pairs | 1_000 |
| Model-facing dump | `TruncationLimits` (2000 lines / 50 KiB) |

## Testing

### Lean

- `Surface.lsp`, `lspActionAuthorized` / `lspApplyAuthorized` with the
  `lsp` parameter, `workspaceExecutable`, SelfConfig field list.
- Cases: `lsp = false`; file Off; ReadOnly ∩ mutate; `serverApplyEdit`;
  advertised ReadWrite foreground edit.
- `lake build`, zero `sorry`s.

### Rust

- `spawn_managed_process`: bash `run_managed_exec` still passes; a
  long-lived process survives request cancel; initialize failure is not
  published; request cancel sends `$/cancelRequest` only.
- `prepare_managed_command` shared by bash and LSP; PATH-only; under-root
  denied.
- Constraints come from `static_policy` after meet; a ceiling prefix /
  network / sandbox change flips the digest. Spawn uses the canonical
  path. A rustup proxy resolves to the selected canonical toolchain binary
  without a helper subprocess.
- `enable_lsp` default false; operator `lsp_config` may include
  `settings`; self-config patch of `settings` / `command` / `args`
  rejected.
- `LspTool` built from `build_tools` with `runtime.lsp_pool`.
- `status` / `reload` do not start a server. Writethrough does not.
  Biome only on explicit `diagnostics`.
- Simultaneous first calls: one process. A waiter registers its
  notification before releasing the entry lock, so Ready cannot be a
  lost wakeup. Failed initialize wakes waiters. Retry after backoff starts
  again.
- Lease: reload / LRU do not terminate a leased client. All-busy cap
  returns `serviceUnavailable`.
- `reload` scopes to the current session/behavior/workspace/digest.
- `session::close_session` does not touch the pool; the owner calls
  `LspPool::close_session`.
- `applyEdit: false`; fake `workspace/applyEdit` is a no-op.
- Inbound bare path / URI → validate through `ToolContext` or reject the
  scheme. Edit URI → no writes. Read-output URI → redact + complete.
- Rename destinations use create-target resolution, including
  server-returned rename document changes. Create-target resolution uses
  `symlink_metadata` for the nearest entry so a dangling symlink is never
  mistaken for a missing creatable leaf.
- ReadOnly `request` rejects verbatim `payload` and unknown shapes.
- Format-on-write does not deadlock; diagnostics wait is off the lock.
- Hash check: documents the preflight-to-write window only.
- UTF-8 / UTF-16 with `😀`.
- UTF-8 positions in the middle of a code point return an error, never
  panic.
- Semantic indexing retry covers `null` and `[]` under one total timeout;
  linter diagnostics honor the request deadline and cancellation.
- Push, pull, and workspace diagnostic envelopes preserve capped messages.
- Nested document-symbol output has qualified names/kinds and a global cap;
  truncation retains its header.
- Typed glob helper, not `GlobTool` text.
- Explicit `network_mode: disabled` off macOS is
  `DisabledNetworkUnenforceable`, not coerced.
- `ActiveExecGuard` kind distinguishes persistent LSP from foreground
  commands.
- `cargo test -p gents` then `cargo check --workspace --all-targets`.

No live rust-analyzer in required CI. Operator-run proof is
`packs/lsp_rust` plus the ignored `e2e_live::lsp_live` test
(`GENTS_LIVE_LSP=1`). The deterministic arm asserts useful, file-specific
document-symbol results and corresponding hovers. A second unscripted arm
asks only a semantic question, asserts at least one useful LSP call, and
checks the factual answer. Neither arm accepts status, a completed-but-empty
call, or a scripted call sequence by itself as proof that the server works.

## Implementation sketch

1. Lean: `Surface.lsp`, `LspAction` auth with `lsp` parameter,
   `workspaceExecutable`, SelfConfig fields.
2. `spawn_managed_process` + rebuild `run_managed_exec`. Existing bash /
   fs-runner tests stay green.
3. `CommandConstraints` + `prepare_managed_command`; bash switched onto
   the helper.
4. `file_mutation` module; WorkspaceEdit `prepare` / `acquire` /
   `apply_with_held_locks`.
5. Schema: `enable_lsp`, `lsp_config` (no command/args). Catalog +
   admission. Policy + vocab + explain.
6. `LspPool` (Starting/Ready/Retiring, leases, per-key singleflight,
   digest). `session_id` / `behavior_id` on `ToolRuntimeScope`.
   `lsp_pool` on `ToolRuntimeContext`. `run_agent` awaits
   `lsp_pool.shutdown()`.
7. `LspToolConfig` + `LspTool` in `build_tools`. Read actions.
8. Write actions + location redaction + encoding.
9. `LspWritethrough` injection; never cold-start.
10. Prompt + `tools explain` (including platform honesty).
11. Desktop/protocol preserve-on-absent.

Modules: `managed_exec` (split), `toolset/shared/command` (constraints
helper), `toolset/file_mutation`, `toolset/lsp/{mod,catalog,config,admit,encoding,client,pool,actions,edits,fixture}`.

## Review findings

| Pass | Resolution |
| --- | --- |
| First 1–10 | Host-exec grant, `LspAction`, `applyEdit: false`, preflight, encoding |
| Second 1–11 | Persistent process primitive, no custom servers, constraints extract, snapshot reload, on-demand start, `build_tools`, URI preflight, platform network default, digest, Lean gate, typed glob |
| This 1 | Constraints from effective `static_policy` after meet; spawn canonical path |
| This 2 | Self-config cannot patch `settings` / `init_options`; enabling `tools` delegates catalog activation |
| This 3 | Starting/Ready/Retiring + leases; LRU only zero-lease Ready; reload scoped to current key |
| This 4 | MCP no-lock-across-I/O plus explicit per-key Starting/singleflight; not “MCP already singleflights” |
| This 5 | No slot-drain callback; `close_session` stays persistence; owner/`run_agent` shut the pool |
| This 6 | Explicit `disabled` → `DisabledNetworkUnenforceable`; omitted default stays inherit off macOS |
| This 7 | Inbound / edit / read-output URI rules split; structured fields only |
| This 8 | Diagnostics wait released from the file lock |
| This 9 | Descendants honesty; Biome/SwiftLint explicit-diagnostics only |
| Small | Start matrix; CLI off the `run_managed_exec` diagram; Lean `Prop`; `ActiveExecGuard` kind |

## Related

- #1106 — this work
- #580 — desktop tool-selection panel
- #724 — stale-write hash / lock
- #729 / #732 — unbounded search
- #937 — native long-running tools (lsp stays foreground)
- #654 — self-config; executable identity stays off that surface
