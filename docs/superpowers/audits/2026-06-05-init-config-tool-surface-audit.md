# Init/Config Tool Surface Audit

Date: 2026-06-05

This audit covers how recently added tool families can be initialized, configured,
and explained from the CLI. It is intentionally scoped to operator-facing init and
config surfaces, not the full runtime implementation.

The runtime should remain document-driven: durable behavior/tool policy lives in
DefraDB documents such as `ToolSelection`, `AgentBehavior`, and
`ToolServiceRegistry`. CLI affordances should help users seed or update those
documents during setup; they should not create a second policy layer that drifts
from the database.

## Current Coverage

| Tool family | Init path | Config path | Runtime/operator path | Notes |
| --- | --- | --- | --- | --- |
| File tools | `init --tool-package readonly/write --tool-root` | `config tools set --enable-file-tools --file-tools-mode --file-tool-root` | `server --tool-ceiling --tool-root` | `ToolCeiling` clamps host-native file tools. |
| Bash tools | `init --tool-package readonly/write` | `config tools set --enable-bash --bash-mode --command-*` | `server --tool-ceiling --tool-root` | Write package defaults to unrestricted bash plus workspace/root ceiling. |
| Background process tools | `init --tool-package write` enables `bash_unrestricted` backgrounding | `config tools set --backgroundable-tool-name ...` | `background list`, background cancellation paths | Background tools are wrappers over already-selected model-callable tools. |
| Memory | `init --enable-memory` | `config tools set --enable-memory true|false` | feature-gated at compile time | `tools explain` warns when selected but compiled out. |
| `defra_query` model tool | `init --disable-defra-query`, `init --defra-query-collection ...` | `config tools set --enable-defra-query true|false --defra-query-collection ...` | `query` command is separate operator surface | Empty collection list means all collections except hard-blocked sensitive fields. |
| Meta MCP tools | package defaults enable them except `minimal` | `config tools set --enable-meta-tools --allowed-mcp-service-id ...` | `mcp probe` inspects registered services | Empty MCP allowlist means all online `ToolServiceRegistry` rows. |
| ToolServiceRegistry/MCP services | manifest/provision/import managed | manifest/provision/import managed | `mcp probe` | No imperative `config tool-service set` command today. |
| External `/mcp` server | not persisted by init | not persisted by config | `server --enable-mcp --mcp-query-collection ...` | This is an operator/listener surface, not a per-behavior model tool. |
| Subagents | not bootstrapped by init | `config tools set --subagent-*` or manifests | `subagent list/cancel`, HTTP subagent tree endpoints | Targets require explicit `SubagentTarget` JSON, so manifests remain better for multi-behavior setup. |
| Codex shim/local control | not persisted by init | not persisted by config | `server --codex-shim ...` | Operator compatibility surface, not model-callable ToolSelection state. |

## Changes Made In This Pass

- Added init flags that seed the default `ToolSelection` document for the two
  safe per-behavior built-ins that can be configured without additional
  documents:
  - `--enable-memory`
  - `--disable-defra-query`
  - `--defra-query-collection <COLLECTION>`
- Added imperative subagent `ToolSelection` document flags while preserving
  existing apply-managed values when omitted:
  - `--subagent-target <SUBAGENT_TARGET_JSON>`
  - `--clear-subagent-targets`
  - `--subagent-spawn-enabled true|false`
  - `--subagent-steering-enabled true|false`
  - `--subagent-background-enabled true|false`
  - `--subagent-allow-cross-deployment true|false`
  - `--cross-deployment-spawn-timeout-seconds <SECONDS>`
- Converted `config tools set` to patch-style semantics for older fields too:
  omitted file/bash/meta/list/scalar fields now preserve existing document state,
  while paired clear flags explicitly clear nullable scalars or list fields.
- Kept `ToolServiceRegistry` setup document/manifest-managed. Adding an
  imperative registry writer would be a separate command family and needs its
  own update semantics.

## Remaining Inconsistencies

- Init still does not bootstrap subagent behaviors or targets. That is correct
  for now: subagents need target behavior IDs, DIDs, descriptions, and sometimes
  cross-deployment policy. Manifests remain the coherent setup path.
- External `/mcp`, Codex shim, and HTTP inspection routes are server/operator
  flags rather than per-behavior `ToolSelection` documents. `tools explain`
  should continue to keep them separate from the model-callable tool surface.
- Backend admission capacity is not the same as effective request concurrency.
  Current runtime wiring has one behavior executor draining each behavior queue;
  `InferenceBackend.max_concurrent > 1` can still be underutilized when multiple
  background subagent children route to the same behavior. This should be made
  explicit in operator/debug output or fixed by adding behavior executor fan-out.

## Recommended Next Step

Audit and simplify init/server flags around operator-only surfaces, especially
external `/mcp`, Codex shim/local control, daemon worker fan-out, and background
execution controls. Keep durable tool policy document-driven, but make runtime
capacity limits visible so users can distinguish selection/config issues from
scheduler bottlenecks.
