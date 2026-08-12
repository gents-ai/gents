# Native `lsp` tool (design)

**Date:** 2026-08-12
**Status:** Draft, awaiting implementation
**Issue:** #1106
**Branch:** `feat/lsp-tools`
**Worktree:** `../gents-lsp-tools`

Capability port of oh-my-pi language-server support into Gents. The model-facing
behavior matches OMP's single `lsp` tool. The wiring is Gents-native:
ToolSelection documents, the existing file-tool sandbox, session-scoped
processes, and the Lean `ToolPolicy` / `ToolExecution` contracts.

OMP sources of truth (behavior only; do not import their file/config layout):

- `packages/coding-agent/src/lsp/` (client, config, defaults, actions, edits)
- `packages/coding-agent/src/prompts/tools/lsp.md`
- `docs/tools/lsp.md`, `docs/lsp-config.md`
- `packages/coding-agent/src/lsp/defaults.json`

## Problem

Gents coding behaviors have `read_file` / `grep` / `edit_file` / `bash`. Those
tools cannot follow shadowing, re-exports, or cross-file callsites. There is no
issue or native tool that talks to rust-analyzer, gopls, typescript-language-server,
or the rest of a project's language servers.

OMP already has this: one `lsp` tool, auto-detect from root markers plus
`$PATH`, and a full action set including rename and code actions. Dropping
OMP's `lsp.json` / lspmux / plugin merge into Gents would add a parallel
config world. Gents already gates optional tools on `ToolSelection`
(`enable_memory`, `enable_defra_query`, `enable_self_config`) and already
scopes host IO through `ToolContext`.

## Decision

One optional native `lsp` tool.

| Choice | Decision |
| --- | --- |
| Model surface | One tool named `lsp`, OMP action enum |
| Gate | `ToolSelection.enable_lsp`, default false, never backfilled |
| Config | `ToolSelection.lsp_config` JSON string + compiled-in catalog |
| Workspace | Existing file-tool workspace (`file_tool_root`, request `workspace_cwd`) |
| Process lifetime | Per `AgentSession`; start on first use; tear down on session close |
| Writes | `WorkspaceEdit` / rename apply through `ToolContext` |
| Policy | Lean `ToolPolicy.Surface.lsp : Bool` |
| Failures | Existing `FailureClass` via `ReportedFailure` |
| Lifecycle | Existing `nativeCommand` — no new tool-call states |

Not chosen: a new `LspConfig` collection, filesystem `lsp.json`, host-wide
or lspmux sharing, or per-server / per-capability tool names.

## Non-goals

- Desktop editor for the new fields. Preserve-on-absent through the existing
  tool-selection bridge; UI is #580.
- Sharing one language-server process across sessions (OMP `lsp.shared` /
  lspmux / broker mux).
- Filesystem or plugin LSP config (`~/lsp.json`, `<cwd>/.omp/lsp.json`,
  Claude/Codex/Gemini config dirs).
- Advertising `lsp` when file tools are `Off` (no workspace to bind).
- Backgrounding `lsp` (OMP is single-shot; v1 stays foreground).
- New `ToolExecution` states or a new `FailureClass`.
- Pointing a language server at Gents `--home`. That directory is the node
  data dir, not the project under edit.
- Hidden compiler subprocesses for workspace diagnostics (`cargo check`,
  `tsc --noEmit`, `go build`, `pyright`). Those stay on `bash`.

## Architecture

```text
ToolSelection.enable_lsp + lsp_config
        │
        ▼
ToolPolicy.Surface.lsp  ⊓  file ≠ Off  ⊓  ceiling
        │
        ▼
advertise native tool `lsp`     (toolset/lsp/)
        │
        ▼
session LspPool[(session_id, workspace, server)]
        │  stdio JSON-RPC
        ▼
language server (rust-analyzer, gopls, …)
        │
        ├─ read actions → formatted text, completed
        └─ write actions → WorkspaceEdit via ToolContext
```

`write_file` / `edit_file` optionally run format + diagnostics after a
successful mutation when `enable_lsp` is on and the corresponding
`lsp_config` flags are set. That is a post-step on existing tools, not a
new hook subsystem.

## Components

### 1. ToolSelection

Add two columns to `ToolSelection` (`crates/gents-schemas/schemas/agent/tool_selection.graphql`)
and thread them through document_config, apply/desired-state, CLI validate,
`tools explain`, protocol rows, and desktop-core **preserve-on-absent**.

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `enable_lsp` | `Boolean` | `false` / unset | Same opt-in rule as `enable_self_config`: never backfill true |
| `lsp_config` | `String` | unset | JSON object; missing/empty/null means compiled-in defaults |

`lsp` is a reserved builtin tool name next to `read_file` / `bash` / `memory`.

`from_selection` sets `surface.lsp = enable_lsp && file != Off`. Ceiling and
runtime meet with AND, identical to `memory` / `defraQuery`.

Write actions additionally require `file == ReadWrite` at dispatch. A
ReadOnly file-tool selection may use diagnostics, navigation, hover, symbols,
status, and capabilities only.

### 2. `lsp_config` JSON

One object. Unknown keys are ignored. Object-valued server fields replace
wholesale (OMP shallow merge).

```json
{
  "idle_timeout_ms": 300000,
  "format_on_write": false,
  "diagnostics_on_write": true,
  "diagnostics_on_edit": false,
  "diagnostics_deduplicate": true,
  "servers": {
    "rust-analyzer": { "disabled": true },
    "my-lsp": {
      "command": "my-lsp-server",
      "args": ["--stdio"],
      "file_types": [".xyz"],
      "root_markers": [".xyz-project"]
    }
  }
}
```

| Field | Default when omitted | Meaning |
| --- | --- | --- |
| `idle_timeout_ms` | unset / 0 / negative = disabled | Shut down an idle client in this session. Session close always tears down. |
| `format_on_write` | `false` | After `write_file`, apply LSP formatting when a server matches |
| `diagnostics_on_write` | `true` | After `write_file`, append diagnostics |
| `diagnostics_on_edit` | `false` | After `edit_file`, append diagnostics |
| `diagnostics_deduplicate` | `true` | Suppress diagnostics already shown for that file |
| `servers` | `{}` | Per-server override map, merged onto the compiled-in catalog |

Server override / catalog fields (snake_case), matching OMP `ServerConfig`:

`command`, `args`, `file_types`, `language_id`, `root_markers`,
`init_options`, `settings`, `disabled`, `warmup_timeout_ms`, `is_linter`,
`capabilities`, `workspace_ready_timings`.

A new server is valid only when `command`, `file_types`, and `root_markers`
are all non-empty after merge. Invalid entries are dropped with a warning
(`tools explain` surfaces them). Runtime-owned OMP fields (`resolvedCommand`,
`createClient`) are not configurable.

### 3. Built-in catalog

Port `defaults.json` into the runtime as `include_str!` JSON under
`crates/gents/src/toolset/lsp/defaults.json`. Detection is Gents-native:

1. Workspace root = `ToolContext` effective base (request `workspace_cwd` if
   set, otherwise `file_tool_root` / the tool base). Never Gents `--home`.
2. A server is eligible when at least one `root_markers` entry exists **in
   that workspace root** (one-level wildcard such as `*.cabal` allowed; no
   parent walk, no recursive scan) **and** `command` resolves.
3. Resolve `command` in this order: absolute path; workspace-local bins
   (`node_modules/.bin`, `.venv/bin`, `venv/bin`, `bin`); then `$PATH`.
4. Drop `disabled` servers after merge.

Custom linter adapters that OMP implements out of process (Biome CLI,
SwiftLint CLI) ship as Rust adapters behind the same `is_linter` catalog
entries. They participate in `diagnostics` (and format-on-write when they
can format). They do not handle navigation or rename.

Catalog `capabilities` (rust-analyzer `flycheck`, `ssr`, `expandMacro`,
`runnables`, `relatedTests`, plus `workspace_ready_timings`) are honored
inside this same tool — readiness waits and extra requests — not as new
tool names. They remain off unless the merged server entry sets them.

### 4. Native tool

`crates/gents/src/toolset/lsp/` — one `NativeTool::Lsp` / tool name `lsp`.

Actions (OMP set):

| Action | Kind | Notes |
| --- | --- | --- |
| `diagnostics` | read | File, glob, or `file: "*"` (every eligible server). Workspace mode is LSP-only — do not spawn `cargo check` / `tsc` / `go build` / `pyright`; the agent already has bash for that. |
| `definition` | read | `file` + `line` + `symbol` |
| `type_definition` | read | same position rules |
| `implementation` | read | same position rules |
| `references` | read | include declaration; project-aware retry |
| `hover` | read | |
| `symbols` | read | document, or `file: "*"` + `query` for workspace |
| `status` | read | configured vs started |
| `capabilities` | read | dump server capabilities |
| `rename` | write | apply unless `apply: false` |
| `rename_file` | write | filesystem rename + `will/didRenameFiles` |
| `code_actions` | write | list by default; apply one with `apply: true` + `query` |
| `reload` | write | restart one server or all (`file: "*"`); `*` also reloads config |
| `request` | write | raw method + optional JSON `payload` |

Position convention matches OMP: `line` is 1-indexed; `symbol` is a
substring on that line; `name#N` selects the Nth match. For
`definition` / `references` / `rename` against project-aware servers,
`line` without `symbol` is `argumentInvalid` — no silent first-column
fallback.

Output is a single text block, OMP-shaped (locations with context,
grouped diagnostics, preview vs applied edits). Empty navigation
(`No definition found`) is a **successful** `completed` call, same as
grep with no matches.

### 5. Session-scoped `LspPool`

Host-side cache, not a document.

- Key: `(session_id, workspace_root, server_name)`.
- Value: one stdio JSON-RPC client (initialize once, reuse).
- Create on first use for that key.
- `idle_timeout_ms` stops an idle client inside the session; the next call
  cold-starts it.
- `close_session` / session teardown sends `shutdown` + `exit`, then kills
  the process if it does not leave.
- A request that changes `workspace_cwd` uses a different key; it does not
  reuse another workspace's client.
- Child behaviors get `lsp` only if **their** `ToolSelection.enable_lsp` is
  true. Their session is a different key.

Implementation sits next to the existing host-level `McpPool` but is keyed
by session, not shared across the host. Do not put process handles in
DefraDB.

JSON-RPC framing, `didOpen` / `didChange` / `didClose`,
`publishDiagnostics` cache, `$/cancelRequest` on abort, initialize
backoff after a failed handshake: port OMP `client.ts` semantics.

### 6. File-tool writethrough

After a successful `write_file` / `edit_file`, if `surface.lsp` and the
matching flag:

1. `didChange` (or reopen) the file on matching servers.
2. If `format_on_write` and the mutator was `write_file`, request formatting
   and apply the text edits through the same `file_mutation_lock_for` path
   `write_file` already uses.
3. If diagnostics-on-write/edit, wait briefly for `publishDiagnostics` and
   append the formatted diagnostics to the tool result. Dedup when
   `diagnostics_deduplicate` is true.

Writethrough failures never fail the original write. They append a note.

## Data flow

1. Reconcile builds the behavior tool surface. `enable_lsp && file != Off`
   after policy meet advertises `lsp`.
2. First `lsp` call in a session: load catalog, merge `lsp_config`, select
   eligible servers for the workspace, start the routed server if needed.
3. Route by `file_types` (extension or exact basename). Primary
   (non-`is_linter`) servers handle navigation and refactor. `diagnostics`
   queries matching primaries **and** linter adapters.
4. Position actions `didOpen` the file, resolve the column, send the LSP
   request, format the result.
5. Write actions produce a `WorkspaceEdit` or filesystem rename, apply
   through `ToolContext` (path must stay under the tool root), then notify
   the server.
6. Session close drains that session's clients.

## Error handling (Lean + existing classes)

Foundation flow: this **does** change `ToolPolicy.Surface`, so Lean goes
first. It does **not** change legal `ToolExecution` transitions.

### ToolPolicy

Add `lsp : Bool` to `Proofs.ToolPolicy.Types.Surface`.

- `Surface.meet`: `a.lsp && b.lsp`
- `effective_lsp_le_ceiling` / `effective_lsp_le_behavior` next to the
  `memory` lemmas
- `SurfaceView.lsp` in `Cases.lean` and
  `Conformance.Contracts.Json.ToolPolicy`
- Rust `ToolPolicySurface.lsp` + `lean_vocab_test` stay byte-aligned

`secure_minimal` / default-false selections have `lsp = false`.
`runtime_all` and the current `legacy_non_host_wide` ceiling stay
permissive (`true`) so an explicit selection can enable it, matching
`defra_query`.

### ToolExecution

`lsp` is `ToolOperation.nativeCommand`. Allowed transitions are the ones
already proven: dispatch, spawnFailed, complete, fail, timeout, cancel,
approval hold/deny.

Do not invent a parallel "text result with success=false" channel. The
tool uses `ToolError::ReportedFailure` like file and bash tools.

| Situation | Class | Terminal |
| --- | --- | --- |
| Missing/invalid `action`, `file`, `query`, `new_name`, `payload` JSON, or required `symbol` | `argumentInvalid` | `failed` |
| Path outside tool root | `policyDenied` | `failed` |
| Write action while `file != ReadWrite` | `policyDenied` | `failed` |
| Policy meet has `lsp = false` (should not be advertised; defense in depth) | `policyDenied` | `failed` |
| No matching server, binary missing, initialize failure | `serviceUnavailable` | `failed` |
| Stdio/process death mid-request | `transport` | `failed` |
| LSP error response (`method not found`, server-side rename error) | `toolReturnedError` | `failed` |
| Wall-clock / request deadline | `external` (or existing timeout transition) | `timedOut` / `failed` |
| `approval_required_tools` contains `lsp` and operator denies | `approvalDenied` | `failed` (existing path) |
| Empty definition/references/hover/diagnostics-clean | none | `completed` |

Initialize failures cache a backoff on `(session_id, server)` so a dead
binary does not respawn every call. Workspace `reload` clears that cache.

One server failing inside a multi-server `diagnostics` call does not fail
the others; those errors are notes in a still-`completed` result.

Session teardown is best-effort `shutdown`/`exit`, then kill. Teardown
errors are logged, not persisted as tool calls.

## Testing

### Lean

- `lake build` after the `Surface.lsp` field and meet lemmas.
- New / updated ToolPolicy conformance JSON cases: enable ∩ ceiling,
  enable ∩ file Off, disable wins.
- Zero `sorry`s.

### Rust

- Reserved name `lsp`; `enable_lsp` default false; no true backfill.
- `lsp_config` parse, shallow server merge, invalid custom server dropped.
- Policy meet and advertisement (`tools explain`).
- `FailureClass` mapping table above.
- `ToolContext` sandbox on reads and on every `WorkspaceEdit` URI.
- Session pool: start, reuse same key, different workspace → different
  client, session close drops the process, idle timeout drops the process.
- Fake stdio language-server fixture covering the full action set
  (diagnostics, navigation, symbols, rename preview/apply, rename_file,
  code_actions list/apply, status, capabilities, request, reload).
- File-tool writethrough: format-on-write applies, diagnostics append,
  writethrough failure does not fail the write.

No live rust-analyzer (or other real server) in CI. An `#[ignore]` live
test may be added later if useful; it is not a gate.

Gates: `cargo test -p gents`, then
`cargo check --workspace --all-targets` for every crate that grows a
required field (desktop-core, protocol rows, CLI apply, fixtures).

## Implementation sketch (for the plan, not this PR)

Foundation order:

1. Lean `ToolPolicy.Surface.lsp` + lemmas + conformance JSON.
2. Schema + document_config + apply/validate + reserved name + policy
   rust + vocab tests.
3. Catalog + merge + detection (no process).
4. JSON-RPC client + session pool + fake server fixture.
5. Action dispatcher (read, then write).
6. File-tool writethrough.
7. Prompt text + `tools explain`.
8. Desktop/protocol preserve-on-absent (no editor UI).

Keep modules small: `toolset/lsp/{mod,catalog,config,client,pool,actions,edits,fixture}`.

## Related

- #1106 — this work
- #580 — desktop tool-selection panel (out of scope)
- #937 — native long-running tool backgrounding (lsp is not backgrounded)
- #728 — AGENTS.md at the tool root (workspace binding is the same root)
- #732 — ripgrep-backed search (orthogonal text search)
- #739 — structural edit_file mode (orthogonal to LSP rename)
