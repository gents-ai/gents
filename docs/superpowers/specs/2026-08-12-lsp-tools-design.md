# Native `lsp` tool (design)

**Date:** 2026-08-12
**Status:** Draft, review findings 1–10 incorporated
**Issue:** #1106
**Branch:** `feat/lsp-tools`
**Worktree:** `../gents-lsp-tools`

Capability port of oh-my-pi language-server **actions** into Gents. Wiring is
Gents-native: ToolSelection documents, `CommandExecutionPolicy` +
`managed_exec` for spawn, `ToolContext` + `file_mutation_lock_for` +
`content_hash` for edits, session-scoped processes, and the Lean
`ToolPolicy` / `CommandPolicy` / `ToolExecution` contracts.

OMP is a behavior reference, not a client to copy:

- `packages/coding-agent/src/lsp/` and `docs/tools/lsp.md` — action set,
  catalog, numeric caps
- Do **not** import `lsp.json`, lspmux, workspace-local bin search, or
  `workspace.applyEdit: true`

## Problem

Gents coding behaviors have `read_file` / `grep` / `edit_file` / `bash`. Those
tools cannot follow shadowing, re-exports, or cross-file callsites. There is no
issue or native tool that talks to rust-analyzer, gopls, typescript-language-server,
or the rest of a project's language servers.

The first draft of this spec treated `enable_lsp` like a file-read add-on and
said "port OMP client semantics." That is wrong in this codebase: starting a
language server is host execution, and OMP's client applies server-initiated
edits. Gents already has the patterns this needs (CLI-tool admission,
`CommandExecutionPolicy`, `managed_exec`, `FileCap`, `expected_content_hash`).

## Decision

One optional native `lsp` tool.

| Choice | Decision |
| --- | --- |
| Model surface | One tool named `lsp`, OMP action enum |
| Gate | `ToolSelection.enable_lsp`, default false, never backfilled |
| What the gate *is* | Host-exec grant for **operator-admitted** language-server binaries (CLI-tool class), **plus** file-tier authorization from existing `FileCap` |
| Config | `ToolSelection.lsp_config` JSON string + compiled-in **ordered** catalog |
| Workspace | Existing file-tool workspace (`file_tool_root`, request `workspace_cwd`) |
| Spawn | `managed_exec` + `CommandExecutionPolicy` + `build_shell_env()` — never workspace-local bins |
| Process lifetime | Per `AgentSession`; start on first use; tear down on session close / idle / digest change |
| Client mutations | `workspace.applyEdit: false`. Apply only edits returned by the foreground call |
| Writes | Preflight every URI through `ToolContext`, then apply under `file_mutation_lock_for` |
| Policy | Lean `Surface.lsp : Bool` **and** an `LspAction` authorization model against `FileCap` |
| Failures | Existing `FailureClass` via `ReportedFailure` |
| Lifecycle | Existing `nativeCommand` — no new tool-call states |

Not chosen: a new `LspConfig` collection, filesystem `lsp.json`, host-wide
or lspmux sharing, per-server tool names, or treating `enable_lsp` as
file-read authority.

## Non-goals

- Desktop editor for the new fields. Preserve-on-absent through the existing
  tool-selection bridge; UI is #580.
- Sharing one language-server process across sessions (OMP `lsp.shared` /
  lspmux / broker mux).
- Filesystem or plugin LSP config (`~/lsp.json`, `<cwd>/.omp/lsp.json`).
- Advertising `lsp` when file tools are `Off` (no workspace to bind).
- Backgrounding `lsp` (v1 stays foreground).
- New `ToolExecution` states or a new `FailureClass`.
- Pointing a language server at Gents `--home`.
- Hidden compiler subprocesses for workspace diagnostics (`cargo check`,
  `tsc --noEmit`, `go build`, `pyright`). Those stay on `bash`.
- Resolving `node_modules/.bin`, `.venv/bin`, or other workspace-controlled
  executables.
- `workspace/executeCommand` and server-initiated `workspace/applyEdit`.
- A POSIX multi-file transaction. Preflight + ordered locks + hash check +
  stop-on-first-failure is the same honesty as #724.

## Architecture

```text
ToolSelection.enable_lsp + lsp_config
        │
        ▼
ToolPolicy.Surface.lsp  ⊓  file ≠ Off  ⊓  ceiling     (advertise)
        │
        ├─ read  LspAction  ⇔  file ∈ {readOnly, readWrite}
        └─ write LspAction  ⇔  file = readWrite
        │
        ▼
admit server argv  (CommandPolicy + PATH/absolute-outside-root)
        │
        ▼
managed_exec spawn  (process group, build_shell_env, sandbox, network)
        │
        ▼
session LspPool[session_id, workspace, server, config_digest]
        │
        ├─ read actions → formatted text, completed
        └─ write actions → foreground WorkspaceEdit only
              preflight ToolContext → lock sorted paths → hash check → apply
```

`enable_lsp` does **not** mean "this is a read tool." It means the operator
admitted language-server binaries for this behavior. File-tier read vs write
is still `FileCap`, proven in Lean, enforced at dispatch.

`write_file` / `edit_file` optionally format + report diagnostics **inside
the existing per-path lock**, after the mutation, before release.

## Components

### 1. ToolSelection

Add two columns to `ToolSelection` and thread them through document_config,
apply/desired-state, CLI validate, `tools explain`, protocol rows, and
desktop-core **preserve-on-absent**.

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `enable_lsp` | `Boolean` | `false` / unset | Same opt-in rule as `enable_self_config`: never backfill true |
| `lsp_config` | `String` | unset | JSON object; missing/empty/null means compiled-in defaults |

`lsp` is a reserved builtin tool name next to `read_file` / `bash` / `memory`.

`from_selection` sets `surface.lsp = enable_lsp && file != Off`. Ceiling and
runtime meet with AND, identical to `memory` / `defraQuery`.

That boolean is **advertisement only**. Action authorization is
`lspActionAuthorized(file, action)` in Lean (section Formal model).

Custom `command` / `args` in `lsp_config` are operator document fields, the
same class as `CliToolConfig.binary_path`. The model cannot change them
unless `enable_self_config` allows the `tools` category.

### 2. `lsp_config` JSON

One object. Unknown keys are ignored. Object-valued server fields replace
wholesale.

```json
{
  "idle_timeout_ms": 300000,
  "format_on_write": false,
  "diagnostics_on_write": true,
  "diagnostics_on_edit": false,
  "diagnostics_deduplicate": true,
  "network_mode": "disabled",
  "servers": {
    "rust-analyzer": { "disabled": true },
    "my-lsp": {
      "command": "my-lsp-server",
      "args": ["--stdio"],
      "file_types": [".xyz"],
      "root_markers": [".xyz-project"],
      "priority": 100
    }
  }
}
```

| Field | Default when omitted | Meaning |
| --- | --- | --- |
| `idle_timeout_ms` | unset / 0 / negative = disabled | Shut down an idle client in this session. Session close always tears down. |
| `format_on_write` | `false` | After `write_file`, format **while still holding** that path's mutation lock |
| `diagnostics_on_write` | `true` | After `write_file`, append diagnostics (still inside the lock for the didChange; the write itself has already landed) |
| `diagnostics_on_edit` | `false` | After `edit_file`, append diagnostics |
| `diagnostics_deduplicate` | `true` | Suppress diagnostics already shown for that file |
| `network_mode` | `disabled` | Meet with `CommandExecutionPolicy.network_mode` (more restrictive wins) |
| `servers` | `{}` | Per-name override map, merged onto the compiled-in **ordered** catalog |

Server fields: `command`, `args`, `file_types`, `language_id`, `root_markers`,
`init_options`, `settings`, `disabled`, `warmup_timeout_ms`, `is_linter`,
`priority`, `capabilities`, `workspace_ready_timings`.

A new server is valid only when `command`, `file_types`, and `root_markers`
are all non-empty after merge. Invalid entries are dropped with a warning.
Runtime-owned OMP fields (`resolvedCommand`, `createClient`) are not
configurable.

### 3. Built-in catalog

Port `defaults.json` into `crates/gents/src/toolset/lsp/defaults.json` as an
**array** (ordered). JSON object iteration is not a routing contract.

Each entry has a stable `name` plus the server fields above. `priority` is a
`u16`; lower wins among non-linter matches. Equal priority keeps catalog
order.

Detection:

1. Workspace root = `ToolContext` effective base (request `workspace_cwd` if
   set, otherwise `file_tool_root`). Never Gents `--home`.
2. Eligible when a `root_markers` entry exists **in that workspace root**
   (one-level wildcard such as `*.cabal`; no parent walk, no recursive scan)
   **and** the command is **admitted** (next section).
3. Drop `disabled` servers after merge.

Mutually exclusive families — first eligible by `priority` then catalog
order, after the family rule:

| Family | Rule |
| --- | --- |
| TypeScript / JS | `denols` if `deno.json` / `deno.jsonc` / `deno.lock` is in the workspace root; otherwise `typescript-language-server` |
| Python | `basedpyright`, then `pyright`, then `pylsp` |
| Ruby | `ruby-lsp`, then `solargraph` |
| Elixir | `expert`, then `elixirls` |
| Nix | `nixd`, then `nil` |
| PHP | `intelephense`, then `phpactor` |

Linters (`is_linter`) never win primary routing. They participate only in
`diagnostics` and format-on-write.

Custom linter adapters (Biome CLI, SwiftLint CLI) are Rust adapters behind
the same catalog entries. They are still admitted as executables (PATH /
absolute-outside-root) and spawned through `managed_exec`.

Catalog `capabilities` (rust-analyzer `flycheck`, `ssr`, `expandMacro`,
`runnables`, `relatedTests`, plus `workspace_ready_timings`) are honored
inside this tool for readiness waits and extra **read** requests. They do
not add tool names and do not authorize writes.

### 4. Executable admission and spawn

Language-server startup is **command execution**, not file IO. Reuse the
existing stack:

| Concern | Existing owner |
| --- | --- |
| Argv admission | `CommandExecutionPolicy` / Lean `CommandPolicy` (`forbidden` prefixes, optional `allowed` prefixes) |
| Process group / cancel / deadline | `managed_exec` (`setsid` + `terminate_process_group` / job object) |
| Environment | `build_shell_env()` — `CORE_ENV_VARS` only, `env_clear` |
| Sandbox | Same `CommandExecutionMode` seatbelt as bash (`workspace_write` on macOS) |
| Network | `CommandNetworkMode`, default **disabled** for LSP, meet with selection |
| Binary pinning | CLI-tool pattern: operator-chosen name/path, never workspace discovery |

**Admission** of `command`:

1. If `command` is absolute or contains a path separator: canonicalize. If
   the path is under the tool root → `policyDenied` (workspace-controlled
   binary). If it does not exist → `serviceUnavailable`. Otherwise admit
   (operator-installed path, same as `CliToolConfig.binary_path`).
2. If `command` is a bare name: resolve on the **host PATH only** (the same
   PATH bash uses). Never `node_modules/.bin`, `.venv/bin`, `venv/bin`, or
   workspace `bin/`.
3. Build `CommandRequest { command, lookupCommand: resolved, args }` and
   run the existing CommandPolicy checks that apply to a non-interactive
   spawn (forbidden prefixes always; allowed prefixes when the selection
   has a non-empty allowlist). Do **not** require the binary to be in
   `default_read_only_commands` — `enable_lsp` is the grant, like adding a
   CLI tool.
4. `lsp_config.servers.*.command` / `args` are part of that argv. They are
   not a model-facing escape hatch.

**Spawn policy** is independent of `FileCap` (file-tier still blocks
tool-applied edits). Default matches `CommandExecutionPolicy::write_capable()`:
`WorkspaceWrite` + seatbelt on macOS, `Unrestricted` elsewhere. Network is
`lsp_config.network_mode` (default disabled) meet the selection's
`command_network_mode`. On macOS, missing `sandbox-exec` is the existing
`workspaceWriteSandboxUnavailable` denial.

Do **not** run `default_read_only_commands` against rust-analyzer. Use
forbidden/allowed argv prefixes from the selection when those lists are
set. Bash `Off` still spawns (CLI tools already do).

The language server will fork compilers (`rustc`, `cargo`, `go`) and will
write analysis caches. Children sit in the managed process group and die
with it. Seatbelt, when active, restricts those writes to `WRITABLE_ROOT`.
That can mutate the workspace even when `FileCap` is `readOnly` — file-tier
only blocks **foreground WorkspaceEdit application**. `tools explain` must
say this; it is why `enable_lsp` is a host-exec grant, not a read tool.
Servers may still **read** toolchains and caches outside the root.

### 5. Native tool

`crates/gents/src/toolset/lsp/` — one `NativeTool::Lsp` / tool name `lsp`.

| Action | Mutates files? | Notes |
| --- | --- | --- |
| `diagnostics` | no | File, glob (existing glob + cap), or `file: "*"`. See workspace diagnostics. |
| `definition` | no | `file` + `line` + `symbol` |
| `type_definition` | no | |
| `implementation` | no | |
| `references` | no | include declaration; project-aware retry |
| `hover` | no | |
| `symbols` | no | document, or `file: "*"` + `query` |
| `status` | no | configured vs started |
| `capabilities` | no | dump server capabilities (capped) |
| `reload` | no | process control; allowed whenever `lsp` is advertised |
| `rename` | yes | apply unless `apply: false` |
| `rename_file` | yes | filesystem rename + `will/didRenameFiles` |
| `code_actions` | list = no; apply = yes | list by default; apply one with `apply: true` + `query`, **edit field only** |
| `request` | yes unless method is on the read-method allowlist | raw method + optional JSON `payload` |

Read-method allowlist for `request` (ReadOnly-legal):
`textDocument/hover`, `textDocument/definition`, `textDocument/typeDefinition`,
`textDocument/implementation`, `textDocument/references`,
`textDocument/documentSymbol`, `textDocument/diagnostic`,
`workspace/symbol`, `workspace/diagnostic`, `shutdown` is not here
(`reload` owns restart). Anything else on a ReadOnly surface is
`policyDenied`.

Position convention: `line` is 1-indexed; `symbol` is a substring on that
line; `name#N` selects the Nth match. For `definition` / `references` /
`rename` against project-aware servers, `line` without `symbol` is
`argumentInvalid`.

Positions and ranges are **LSP-encoded**, not Rust `char`/`byte` indexes.
See Position encoding.

Empty navigation is a successful `completed` call (grep-with-no-matches).

### 6. Client capabilities (do not port OMP here)

Initialize with:

- `workspace.applyEdit = false`
- `workspace.workspaceEdit.documentChanges = true` (we understand the
  shape for **returned** edits)
- `workspace.workspaceEdit.resourceOperations = ["create", "rename", "delete"]`
- `general.positionEncodings = ["utf-8", "utf-16"]`, prefer `utf-8` if the
  server agrees, else UTF-16

The client **does not** implement `workspace/applyEdit`. If a server sends
it anyway, reply `{ applied: false }` and log. No file IO.

The client **does not** send `workspace/executeCommand` in v1.
`code_actions` apply uses only `CodeAction.edit`. A bare `Command` action
is `argumentInvalid` ("action has no workspace edit; executeCommand is not
supported").

`didOpen` / `didChange` / `didClose`, `publishDiagnostics` cache, and
`$/cancelRequest` on abort are kept. Those are protocol, not OMP's mutation
policy.

### 7. Session-scoped `LspPool`

Host-side cache, not a document. Same ownership idea as `McpPool` on
`ToolRuntimeContext`, but keyed by session.

**Key:** `(session_id, workspace_root, server_name, config_digest)`

`config_digest` is a hash of the normalized spawn identity: resolved
command path, args, `init_options`, `settings`, `capabilities`,
`is_linter`, spawn mode, network mode. A ToolSelection / `lsp_config`
change that alters any of those is a different key. Reconcile evicts
entries whose digest is no longer in the effective surface.

**Backoff key:** the same 4-tuple. The first draft omitted workspace and
config; do not.

- Create on first use.
- `idle_timeout_ms` stops an idle client; next call cold-starts.
- `close_session`, runtime shutdown, and digest eviction send
  `shutdown`/`exit` then `managed_exec` process-group kill. In-flight
  initialize uses the same `CancellationToken` as the tool scope.
- Different `workspace_cwd` → different key.
- Child behaviors use their own `enable_lsp` and their own `session_id`.

`session_id` is **not** on today's `ToolRuntimeScope`
(`tool_call_lifecycle/runtime.rs`). Add it there — the same task-local
that already carries `deadline_at`, `cancellation_token`, `workspace_cwd`,
and `live_output`. The dispatcher (`loop_stream`, `daemon/inference`)
already has `session_id` in hand. `close_session` gains an `LspPool`
teardown call; the document mutation stays as it is.

Do not put process handles in DefraDB.

### 8. WorkspaceEdit apply

Applies only to edits **returned** by a write-authorized foreground action
(`rename`, `rename_file`, `code_actions` apply, format-on-write).

Pipeline, using existing file-tool primitives:

1. Accept `documentChanges` or legacy `changes`. Reject mixed unsupported
   shapes.
2. Reject any URI whose scheme is not `file:`.
3. Resolve every source and destination through `ToolContext`
   (`resolve_path` / `resolve_path_allow_create`). Escape → `policyDenied`.
4. Validate text-edit ranges (start ≤ end). Overlapping ranges on one file
   → `argumentInvalid` (do not apply in declared order and hope).
5. If `version` is present, it must match the version we last sent in
   `didOpen`/`didChange`. Mismatch → `argumentInvalid` (stale edit).
6. Sort canonical lock keys and acquire `file_mutation_lock_for` in that
   order (same lock `write_file` / `edit_file` use).
7. Under the locks, re-read each existing file and check `content_hash`
   against the snapshot used to interpret the edit (the #724
   `expected_content_hash` pattern). Mismatch → abort **all** paths, no
   writes.
8. Compute every new byte image in memory. Then write. If a write fails
   after preflight, **stop**, report applied vs pending, do not continue.
   There is no cross-file atomic rename in this runtime.

`rename_file` is the same preflight for every `willRenameFiles` pair plus
the filesystem rename itself, still under the sorted locks.

### 9. File-tool writethrough

Runs **inside** `write_file` / `edit_file`'s existing lock, after the
bytes hit disk, before the lock is released. Do not return from
`write_file` and then re-acquire.

1. Record `content_hash` of the bytes just written (already on the
   metadata).
2. `didChange` matching servers.
3. If `format_on_write` and the mutator was `write_file`: request
   formatting with a short timeout. Before applying, re-hash under the
   **still-held** lock. Hash mismatch or version mismatch → skip format,
   append a non-fatal note. Apply format edits through the same
   WorkspaceEdit preflight (single path).
4. If diagnostics-on-write/edit: wait briefly for `publishDiagnostics`,
   append formatted diagnostics. Dedup when configured.

Writethrough failures never fail the original write.

### 10. Position encoding

LSP positions default to UTF-16 code units. Rust `String` indexes are UTF-8
bytes; `chars()` are Unicode scalars. Either will mis-hit emoji and
non-BMP text.

On initialize, advertise `utf-8` and `utf-16`; use the server's chosen
`positionEncoding`. Convert in both directions in one module
(`toolset/lsp/encoding.rs`):

- UTF-8: `line` + byte offset into that line (LSP utf-8)
- UTF-16: `line` + UTF-16 code units into that line

Symbol search on a line uses the same encoding to compute `character`.
Tests must cover ASCII, combining marks, CJK, and non-BMP (e.g. `😀`) for
navigation **and** for applied edits.

### 11. Workspace diagnostics and routing

`file: "*"` diagnostics:

1. If the routed primary (or any eligible server) advertises
   `workspace/diagnostic`, send that and format the result (capped).
2. Otherwise do **not** enumerate the tree (that is #729). Return a
   completed note: pass a file or a glob. Glob expansion uses the existing
   glob tool + `MAX_GLOB_DIAGNOSTIC_TARGETS` (20).

Primary server for a concrete file: family rule, then lowest `priority`,
then catalog order, excluding `is_linter`. If none remain,
`serviceUnavailable`.

## Data flow

1. Reconcile builds the surface. `enable_lsp && file != Off` after meet
   advertises `lsp`. Digest-evict stale pool entries for live sessions on
   that behavior.
2. First call: merge catalog, admit servers, spawn via `managed_exec`.
3. Route by `file_types` + family/priority. Linters only on diagnostics /
   format.
4. Position actions convert line/symbol through the negotiated encoding,
   `didOpen` if needed, send, format, truncate.
5. Write actions require `file = readWrite`, take only returned edits,
   preflight, lock, hash-check, apply.
6. Session close / shutdown drains that `session_id`.

## Formal model

Foundation flow: Lean first. No new `ToolExecution` states. `lsp` stays
`nativeCommand`.

### ToolPolicy advertisement

Add `lsp : Bool` to `Proofs.ToolPolicy.Types.Surface`.

- `Surface.meet`: `a.lsp && b.lsp`
- `effective_lsp_le_ceiling` / `effective_lsp_le_behavior`
- `SurfaceView.lsp` in Cases + conformance JSON
- Rust `ToolPolicySurface.lsp` + `lean_vocab_test` stay aligned

`secure_minimal` is `lsp = false`. `runtime_all` and
`legacy_non_host_wide` ceilings stay permissive so an explicit selection
can enable it.

### LspAction authorization (`Proofs/Lsp/` or `ToolPolicy/Lsp.lean`)

This is the missing formal piece from the first draft. A boolean cannot
express "read actions on ReadOnly, writes only on ReadWrite."

```lean
inductive LspAction
  | diagnostics | definition | typeDefinition | implementation
  | references | hover | symbols | status | capabilities | reload
  | rename | renameFile | codeActionsList | codeActionsApply
  | requestRead | requestWrite

def LspAction.mutates : LspAction → Bool
  -- rename, renameFile, codeActionsApply, requestWrite = true; else false

def lspAdvertised (lsp : Bool) (file : FileCap) : Bool :=
  lsp && file ≠ .off

def lspActionAuthorized (file : FileCap) (action : LspAction) : Bool :=
  lspAdvertised true file &&
    (!action.mutates || file = .readWrite)

inductive LspMutationSource
  | foregroundReturnedEdit
  | serverApplyEdit

def lspApplyAuthorized (file : FileCap) (src : LspMutationSource) : Bool :=
  file = .readWrite && src = .foregroundReturnedEdit
```

Theorems (zero `sorry`s):

- `¬lspActionAuthorized .readOnly a` when `a.mutates`
- `lspActionAuthorized .readWrite a` when advertised
- `¬lspAdvertised true .off`
- `¬lspApplyAuthorized file .serverApplyEdit` for every `file`
- `lspApplyAuthorized .readWrite .foregroundReturnedEdit`

Conformance JSON cases for those rows, consumed like ToolPolicy cases.

Client `applyEdit = false` is a Rust fixture assertion on the initialize
payload (not a new Lean machine).

### CommandPolicy

Spawn argv is a `CommandRequest`. Existing denial reasons apply
(`forbiddenPrefix`, `allowedPrefixRequired`). Add
`DenialReason.workspaceExecutable` in Lean `CommandPolicy` (and the Rust
mirror) for a command path that resolves under the tool root. Thread it
like the other reasons through `toContract`, conformance cases, and
`CommandPolicyDenial`.

ReadOnly `requestWrite` is `policyDenied` via `lspActionAuthorized`, not
CommandPolicy.

### ToolExecution

Unchanged transitions. Failures are `ReportedFailure` with existing
classes.

| Situation | Class | Terminal |
| --- | --- | --- |
| Missing/invalid args, payload JSON, required `symbol`, overlapping ranges, version mismatch | `argumentInvalid` | `failed` |
| Path outside tool root; under-root executable; write action / `serverApplyEdit` / non-allowlisted `request` on ReadOnly | `policyDenied` | `failed` |
| `surface.lsp = false` | `policyDenied` | `failed` |
| No matching server, binary missing, initialize failure | `serviceUnavailable` | `failed` |
| Stdio / process death mid-request | `transport` | `failed` |
| LSP error response | `toolReturnedError` | `failed` |
| Wall-clock / request deadline | `external` / timeout transition | `timedOut` / `failed` |
| `approval_required_tools` contains `lsp` and deny | `approvalDenied` | `failed` |
| Empty definition / clean diagnostics | none | `completed` |

## Limits

Reuse `truncate()` / `TruncationLimits` for the model-facing text. Named
caps in `toolset/lsp` (OMP numbers, Gents names):

| Cap | Value |
| --- | --- |
| Tool timeout default / min / max | 20s / 5s / 300s |
| JSON-RPC request timeout | 30s |
| Initialize / warmup | 5s |
| Project-load wait | 15s |
| Idle sweep | 60s when idle timeout set |
| Init-failure backoff | 3 min |
| JSON-RPC `Content-Length` max | 8 MiB (reject / `toolReturnedError`) |
| Server stderr ring | `DEFAULT_MAX_COMMAND_CHARS` (16_000) |
| Pending requests per client | 32 |
| Diagnostic messages | 50 |
| Single-file diagnostics wait | 3s |
| Glob diagnostic wait / targets | 400ms / 20 |
| Workspace symbols | 200 |
| Reference context lines | 50 |
| References retries | 2 × 250ms |
| Rename pairs | 1_000 |
| Capabilities / raw `request` JSON | `TruncationLimits` default (2000 lines / 50 KiB) |

An untrusted subprocess does not get unbounded allocation into the owned
loop.

## Error handling notes

One server failing inside multi-server `diagnostics` does not fail the
others; those errors are notes on a `completed` result.

Session teardown errors are logged, not persisted as tool calls.

`reload` with `file: "*"` drops the session's pool entries and re-reads
the **ToolSelection document** via the existing config client. There is no
config file to re-stat.

## Testing

### Lean

- `lake build` after `Surface.lsp`, `LspAction` lemmas, and
  `DenialReason.workspaceExecutable` if added.
- ToolPolicy + LspAction conformance JSON cases: advertise ∩ file Off;
  ReadOnly ∩ mutating action; ReadWrite ∩ mutate; `serverApplyEdit`
  never authorized; disable wins.
- Zero `sorry`s.

### Rust

- Reserved name; `enable_lsp` default false; no true backfill.
- `lsp_config` parse, ordered catalog merge, family/priority routing.
- PATH-only resolution; under-root absolute path denied; workspace
  `node_modules/.bin` ignored even if it exists.
- Spawn uses `managed_exec` + filtered env; process-group kill on
  session close and on digest change.
- Initialize payload has `applyEdit: false`; a fake server
  `workspace/applyEdit` does not touch the filesystem.
- `FailureClass` table above.
- WorkspaceEdit preflight: non-`file:` URI, overlap, version mismatch,
  hash mismatch, escape — no writes. Happy path applies under sorted
  locks.
- Format-on-write: concurrent `edit_file` cannot land between write and
  format (lock held); stale format after a hash change is skipped.
- Position encoding: UTF-8 and UTF-16 fixtures with `😀` and CJK.
- Pool key: config change evicts; backoff key includes workspace + digest.
- `ToolRuntimeScope` carries `session_id`; close_session drains the pool.
- Fake stdio fixture for the action set, including `file: "*"`
  workspace/diagnostic vs unsupported note.
- `cargo test -p gents` then `cargo check --workspace --all-targets`.

No live rust-analyzer in CI.

## Implementation sketch

1. Lean: `Surface.lsp`, `LspAction` authorization, CommandPolicy
   `workspaceExecutable`, conformance JSON.
2. Schema + document_config + apply/validate + reserved name + Rust
   policy + vocab tests.
3. Catalog (ordered) + admission (PATH / outside-root) + merge. No
   process yet.
4. `session_id` on `ToolRuntimeScope`; `LspPool` on `ToolRuntimeContext`;
   close_session / shutdown drain.
5. `managed_exec` client + encoding + fake server fixture.
6. Read actions, then write actions with WorkspaceEdit preflight.
7. Writethrough **inside** `write_file` / `edit_file` locks.
8. Prompt text + `tools explain`.
9. Desktop/protocol preserve-on-absent (no editor UI).

Modules: `toolset/lsp/{mod,catalog,config,admit,encoding,client,pool,actions,edits,fixture}`.

## Review findings (this pass)

| # | Resolution |
| --- | --- |
| 1 | `enable_lsp` is a host-exec grant. Spawn goes through CommandPolicy + `managed_exec` + `build_shell_env` + sandbox/network. Workspace-local bins are forbidden. |
| 2 | `LspAction` + `lspActionAuthorized` / `lspApplyAuthorized` in Lean with conformance cases. `Surface.lsp` stays the advertise bit. |
| 3 | `applyEdit: false`. No `executeCommand`. Apply only foreground-returned edits. |
| 4 | Preflight every URI/range/version; sorted `file_mutation_lock_for`; `content_hash` check; stop-on-first-failure. No fake multi-file atomicity. |
| 5 | Negotiate utf-8/utf-16; convert both ways; non-ASCII tests. |
| 6 | Format-on-write stays inside the existing write lock; re-hash before apply. |
| 7 | Pool and backoff keys include `config_digest`. Reconcile evicts. |
| 8 | Add `session_id` to `ToolRuntimeScope`. Pool lives next to `McpPool`. `close_session` drains it. |
| 9 | OMP numeric caps + `truncate()` / `TruncationLimits` + Content-Length / pending-request bounds. |
| 10 | `file: "*"` uses `workspace/diagnostic` or a glob/note. Ordered catalog + family/priority routing. |

## Related

- #1106 — this work
- #580 — desktop tool-selection panel (out of scope)
- #724 — content-addressed stale-write rejection (hash/lock pattern)
- #729 / #732 — unbounded search; do not reintroduce via workspace diagnostics
- #937 — native long-running tool backgrounding (lsp is not backgrounded)
- #728 — AGENTS.md at the tool root (same workspace binding)
- #739 — structural `edit_file` (orthogonal to LSP rename)
