# Native `lsp` tool (design)

**Date:** 2026-08-12
**Status:** Draft, architecture aligned to existing runtime abstractions
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
| `lsp_config` | Disable / settings / priority / timeouts / flags. Never `command` or `args` |
| Assembly | `LspToolConfig` on `ToolSurface`; `LspTool` built in `ToolSurface::build_tools` with `runtime.lsp_pool` — not `NativeTool` / `ToolSet::build_native_tools` |
| Spawn primitive | Extract `spawn_managed_process` from `managed_exec`; rebuild `run_managed_exec` on it |
| Command constraints | Extract from `BashMode`; bash and LSP each derive an execution policy; one prepare helper |
| Config path | Reconcile only. `reload` uses the current `ToolSurface` snapshot |
| Start policy | Only an explicit `lsp` action starts a server. Writethrough never cold-starts |
| Idle | Default 5 minutes. Per-session and global LRU caps |
| Mutations | `applyEdit: false`. Apply only foreground-returned edits |
| File IO | Shared file-mutation module; `ToolContext` on **inbound URIs and outbound locations** |
| Network default | `disabled` under enforceable `workspace_write` (macOS); `inherit` elsewhere |
| Policy | `Surface.lsp : Bool` and `lspActionAuthorized lsp file action` |
| Failures | Existing `FailureClass` via `ReportedFailure` |
| Lifecycle | Existing `nativeCommand`. No new tool-call states |

## Non-goals

- Desktop editor for the new fields (#580). Preserve-on-absent only.
- Custom / user-defined language-server executables in v1.
- Sharing one server across sessions (lspmux / broker).
- Filesystem LSP config.
- Advertising `lsp` when file tools are `Off`.
- Backgrounding `lsp`.
- New `ToolExecution` states or a new `FailureClass`.
- Pointing a language server at Gents `--home`.
- Hidden compiler subprocesses for workspace diagnostics.
- Workspace-local binaries (`node_modules/.bin`, `.venv/bin`, …).
- `workspace/executeCommand` and server-initiated `workspace/applyEdit`.
- Re-reading ToolSelection (or any other document) from `reload`.
- Invoking `GlobTool` and parsing its model-facing text.
- A POSIX multi-file transaction. Same honesty as #724.
- Claiming `network: disabled` is enforceable under `Unrestricted`.

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

explicit lsp action
                │
                ▼
LspPool.get_or_start  (MCP-style: no map lock across initialize)
                │
                ▼
spawn_managed_process  (pool-owned cancel token, persistent stdio)
                │
run_managed_exec  = spawn + write stdin + wait + terminate
  (bash / CLI / fs-runner unchanged at the call site)

reload          → terminate clients of *this* ToolSurface digest
reconcile       → new surface + new digest; old clients idle/LRU
                  after the old behavior slot drains
```

## Components

### 1. ToolSelection

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `enable_lsp` | `Boolean` | `false` / unset | Never backfill true |
| `lsp_config` | `String` | unset | JSON object. **No executable identity.** |

`lsp` is a reserved builtin name.

`from_selection` sets `surface.lsp = enable_lsp && file != Off`. Meet is AND.

`lsp_config` may contain:

- `idle_timeout_ms` (default **300000**)
- `format_on_write` (default false)
- `diagnostics_on_write` (default true)
- `diagnostics_on_edit` (default false)
- `diagnostics_deduplicate` (default true)
- `network_mode` (`disabled` or `inherit` — see Network)
- `servers`: map of **catalog name** → `{ disabled, settings, priority, warmup_timeout_ms, capabilities, workspace_ready_timings, language_id }`

Rejected keys (dropped with `tools explain` warning; never honored):
`command`, `args`, `resolvedCommand`, `createClient`. A `servers` entry
whose name is not in the built-in catalog is dropped.

Self-config category `tools` (default on when self-config is enabled) may
patch `enable_lsp` and the allowed `lsp_config` keys. It must not be able
to introduce or change an executable. That is automatic if `command`/`args`
are not in the schema. Lean `SelfConfig` field lists gain `enable_lsp` and
`lsp_config`; the `lsp_config` decoder still strips executable keys.

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
the **host PATH** to a path that is **not** under the tool root.

Family rules (first eligible by `priority`, then catalog order):

| Family | Rule |
| --- | --- |
| TypeScript / JS | `denols` if `deno.json` / `deno.jsonc` / `deno.lock`; else `typescript-language-server` |
| Python | `basedpyright`, then `pyright`, then `pylsp` |
| Ruby | `ruby-lsp`, then `solargraph` |
| Elixir | `expert`, then `elixirls` |
| Nix | `nixd`, then `nil` |
| PHP | `intelephense`, then `phpactor` |

Linters never win primary routing. Biome / SwiftLint adapters are still
catalog binaries on PATH, spawned through the same process primitive.

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

`BashMode` plus the selection's command-policy fields **derive** a
`CommandExecutionPolicy` for bash. `enable_lsp` plus the same constraint
fields **derive** a spawn policy for language servers. They are two
consumers, not one policy object reused by pretending LSP is bash.

One helper prepares a spawn:

`prepare_managed_command(root, command, args, constraints) -> (program, argv, env, sandbox)`

Sandbox wrapping, `CORE_ENV_VARS`, PATH lookup, and
`DenialReason.workspaceExecutable` (path under the tool root) live only
there. Bash `run_command` and LSP spawn both call it. No drift.

LSP spawn constraints in v1:

- Forbidden / allowed prefixes from the selection, when set (those fields
  exist independently of whether bash is on).
- Not the `default_read_only_commands` allowlist.
- Sandbox / network: see Network below.
- Environment: `build_shell_env()` only.

### 6. Network and platform honesty

Existing `validate_network_mode` rejects `network: disabled` under
`Unrestricted` (`DisabledNetworkUnenforceable`). macOS `workspace_write`
can enforce disabled network via seatbelt. Off macOS, write-capable bash
is already `Unrestricted`.

**Default:**

| Platform | Spawn sandbox | Network |
| --- | --- | --- |
| macOS with `sandbox-exec` | `workspace_write` | `disabled` (unless `lsp_config.network_mode = inherit`) |
| anything else | unsandboxed (`Unrestricted`) | **`inherit`** |

`tools explain` and `RuntimeToolAvailability` must state, off macOS:

- network cannot be disabled
- the server can **write outside the tool root**, not merely analysis
  caches under it

Do not fail-closed and hide `lsp` on Linux. That would make the tool
useless on the hosts that already run Unrestricted bash. An explicit
`lsp_config.network_mode: disabled` off macOS is coerced to `inherit`
and called out in `tools explain`, not turned into a hard denial.

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
3. `apply_with_held_locks` — hash/version check, write, no further
   acquire.

Format-on-write calls `prepare` then `apply_with_held_locks` on the
locks it already owns. The ordinary rename path calls all three.

### 8. On-demand start, idle, caps

- **Only** an explicit `lsp` action may `get_or_start` a server.
- File writethrough uses **already-active** clients only. If none match,
  skip format/diagnostics and do not start anything.
- Default `idle_timeout_ms` is **300_000** (5 minutes), not “disabled”.
- Caps: **4** live clients per `(session_id, behavior_id)`; **16** live
  clients per host `LspPool`. Evict idle first, then LRU.
- MCP pattern (`mcp_pool.rs` `get_or_connect`): look up under the map
  lock, **drop the lock**, initialize outside it, publish only on
  success. Singleflight per key so two concurrent `lsp` calls do not
  spawn two rust-analyzers. A failed initialize never publishes.

`close_session` / runtime shutdown still drain. Do not rely on them for
steady-state hygiene; daemon sessions stay open.

### 9. Snapshot ownership

`reload` (file or `*`) restarts clients that belong to the **current**
`LspToolConfig` digest. It does not read DefraDB.

Document changes flow only through reconcile:

1. Reconcile builds a new `ToolSurface` / `LspToolConfig` with a new digest.
2. New requests get the new `LspTool` instance.
3. Old-generation clients are **not** killed under an in-flight request
   still using that surface. They retire when the old behavior slot
   drains, or by idle / LRU.

`reload "*"` on the new surface does not shoot the old generation.

### 10. Pool identity

Key:

`(session_id, behavior_id, workspace_root, server_name, config_digest)`

`config_digest` covers **authority and client state**, not just argv:

- effective tool root / writable root
- full `CommandConstraints` (prefixes, network, sandbox)
- resolved command path
- fixed catalog args
- `language_id`
- `init_options`, `settings`, `capabilities`, readiness timings
- writethrough flags do **not** need to be in the digest (they do not
  change an already-running initialize)

A tighter ceiling or policy produces a new digest. A process admitted
under a looser generation cannot be reused.

### 11. Native tool actions

Same action set as the previous revision, with these bindings:

| Action | Mutates files? | Notes |
| --- | --- | --- |
| `diagnostics` | no | File, typed glob, or `file: "*"`. |
| `definition` / `type_definition` / `implementation` / `references` / `hover` / `symbols` | no | Returned locations go through `ToolContext` before context read or display. |
| `status` / `capabilities` / `reload` | no | `reload` uses the current snapshot only. |
| `rename` / `rename_file` | yes | Foreground-returned edits only. |
| `code_actions` | list no; apply yes | Apply `CodeAction.edit` only. Bare `Command` → `argumentInvalid`. |
| `request` | see below | |

`request` on ReadOnly: only the read-method allowlist. Deserialize the
**known** parameter shape for that method (`textDocument`, `position`,
…). Every `file:` URI in those params goes through `ToolContext`. Do
**not** forward an arbitrary `payload` verbatim on a ReadOnly surface.

`request` on ReadWrite: `payload` is still parsed JSON, and every `file:`
URI in it is validated the same way. Unknown methods are
`argumentInvalid`.

### 12. Client capabilities

- `workspace.applyEdit = false`. Incoming `workspace/applyEdit` replies
  `{ applied: false }` and performs no IO.
- No `workspace/executeCommand` in v1.
- `positionEncodings = ["utf-8", "utf-16"]`; prefer utf-8.
- `didOpen` / `didChange` / `didClose`, diagnostics cache, `$/cancelRequest`.

### 13. Reads and outputs stay inside the file boundary

Every model-supplied path or `file:` URI — action `file`, glob root,
`request` params — is resolved with `ToolContext` first.

Every **returned** location (definition, references, diagnostics,
workspace symbol, hover contents that embed URIs) is resolved the same
way **before** reading source context or rendering a host path.
Outside-root hits are reported as outside the allowed workspace (no
absolute host path, no file read). They do not fail the whole call.

### 14. WorkspaceEdit

`prepare` / `acquire` / `apply_with_held_locks` as above.

- Reject non-`file:` URIs.
- Overlapping ranges on one file → `argumentInvalid`.
- Tracked documents (we have a `didOpen` version): the edit's `version`
  must match.
- Unversioned files (server never opened them): validate ranges against
  **current** bytes under the lock; then write.
- The `content_hash` check protects the interval between preflight and
  write, **not** freshness since the LSP request was sent, and **not**
  files the server never versioned. External writers remain the #724
  race: last writer wins outside this process.

Stop on first write failure; report applied vs pending.

### 15. Writethrough

Inside the existing `write_file` / `edit_file` lock, after bytes hit
disk, before release:

1. If no **already-active** client matches the file, return. No start.
2. `didChange` on those clients.
3. Format-on-write (`write_file` only): `prepare` + `apply_with_held_locks`.
   Re-hash before apply; mismatch → skip + note.
4. Diagnostics flags: wait briefly, append, dedup.

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
both.

`LspPool` is host-owned on `ToolRuntimeContext`. `close_session` and
runtime shutdown drain. Idle / LRU are the steady-state control.

## Formal model

No new `ToolExecution` states. `lsp` is `nativeCommand`.

### ToolPolicy

`Surface.lsp : Bool`, meet AND, `effective_lsp_le_*`, conformance JSON,
Rust vocab aligned.

### LspAction

```lean
def lspAdvertised (lsp : Bool) (file : FileCap) : Bool :=
  lsp && file ≠ .off

def lspActionAuthorized (lsp : Bool) (file : FileCap) (action : LspAction) : Bool :=
  lspAdvertised lsp file &&
    (!action.mutates || file = .readWrite)

def lspApplyAuthorized (lsp : Bool) (file : FileCap) (src : LspMutationSource) : Bool :=
  lspAdvertised lsp file &&
    file = .readWrite &&
    src = .foregroundReturnedEdit
```

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

Add `enable_lsp` and `lsp_config` to the ToolSelection patch field list.
Decoder rejects `command` / `args` inside `lsp_config`.

## Failures

| Situation | Class | Terminal |
| --- | --- | --- |
| Bad args, overlapping ranges, version mismatch, unknown `request` method, bare `Command` code action | `argumentInvalid` | `failed` |
| Path / URI / returned-location escape; under-root executable; mutating action without ReadWrite; `lsp = false` | `policyDenied` | `failed` |
| No matching server, binary missing, initialize failure | `serviceUnavailable` | `failed` |
| Stdio / process death mid-request | `transport` | `failed` |
| LSP error response | `toolReturnedError` | `failed` |
| Request deadline / tool timeout | `external` / `timedOut` | does **not** kill the pooled server |
| Approval deny | `approvalDenied` | `failed` |
| Empty navigation / clean diagnostics / no active client for writethrough | none | `completed` |

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
- `enable_lsp` default false; `lsp_config` cannot set `command`/`args`;
  unknown server names dropped; self-config patch of `command` rejected.
- `LspTool` built from `build_tools` with `runtime.lsp_pool`.
- Writethrough does not start a server; explicit `lsp` does.
- Idle 5 min + LRU at 4 / 16.
- `reload` does not touch DefraDB; digest change does not kill an
  in-flight old-generation call.
- `applyEdit: false`; fake `workspace/applyEdit` is a no-op.
- Inbound URI + outbound location redaction.
- ReadOnly `request` rejects verbatim `payload` and unknown shapes.
- Format-on-write does not deadlock (held-lock apply).
- Hash check: documents the preflight-to-write window only.
- UTF-8 / UTF-16 with `😀`.
- Typed glob helper, not `GlobTool` text.
- Off-macOS explain text: inherit network, writes may escape the root.
- `cargo test -p gents` then `cargo check --workspace --all-targets`.

No live rust-analyzer in CI.

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
6. `LspPool` (MCP get-or-start, idle, LRU, digest). `session_id` /
   `behavior_id` on `ToolRuntimeScope`. `lsp_pool` on
   `ToolRuntimeContext`.
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
| First 1–10 | Host-exec grant, `LspAction`, `applyEdit: false`, preflight, encoding, lock-held format, digest, session scope, caps, routing |
| This 1 | Extract `spawn_managed_process`; pool owns process cancel; request cancel is `$/cancelRequest` only |
| This 2 | No custom servers in v1; `lsp_config` cannot carry `command`/`args`; self-config cannot introduce executables |
| This 3 | `CommandConstraints` extracted from `BashMode`; one prepare helper |
| This 4 | `reload` uses the current `ToolSurface`; reconcile is the only document path |
| This 5 | Writethrough never starts a server; 5 min idle; 4 / 16 LRU; MCP singleflight |
| This 6 | `LspTool` in `build_tools`; shared file-mutation module; three-stage apply |
| This 7 | `ToolContext` on inbound URIs and outbound locations; no verbatim ReadOnly payloads |
| This 8 | `disabled` network only under seatbelt; `inherit` elsewhere; explain writes-escape off macOS |
| This 9 | Digest includes root, constraints, language_id, readiness; key includes `behavior_id` |
| This 10 | `lspActionAuthorized` / `lspApplyAuthorized` take `lsp`; hash-check window stated |
| This 11 | Typed fs-runner glob helper, not `GlobTool` |

## Related

- #1106 — this work
- #580 — desktop tool-selection panel
- #724 — stale-write hash / lock
- #729 / #732 — unbounded search
- #937 — native long-running tools (lsp stays foreground)
- #654 — self-config; executable identity stays off that surface
