# SPEC: Claude A2b — first-class in-process provider

**Date:** 2026-09-01  
**Status:** Locked for implementation (human decisions 2026-09-01)  
**Parent:** [`claude-subscription-spike.md`](./claude-subscription-spike.md)  
**Depends on:** A2a complete (`SPEC-claude-a2a-unified-suite.md`)  
**Follow-on:** A2c tool bridging (separate SPEC after A2b green)  
**Branch:** continue on `spike/claude-subscription-plan`  
**Local handoff (not in git):** `.scratch/claude-spike/handoff/claude-a2b-handoff.md`  
**Interrupted session (do not resume):** `1d36dc49-6d24-4cc1-9d4e-67892b52e37e` — A2b-1 was requested (`proceed`) and never started (Grok stream failure).

## Problem

A2a hit the operator success bar, but Claude is still a fake OpenAI endpoint:

```text
owned loop → OpenAiCompatible HTTP → managed claude-proxy :8787 → Claude CLI
```

Claude is a different provider. Modeling it as `OpenAiCompatible` was a spike convenience, not the destination.

## Goal

```text
owned loop
  ├─ XaiGrokOAuth        → Grok OAuth client
  ├─ OpenAiCompatible    → OpenAI HTTP client
  └─ ClaudeCliSubscription → claude_completer (CLI seat in --config-dir)
```

Operator surface:

```bash
./target/debug/gents server \
  --claude-config-dir "$CLAUDE_CONFIG_DIR" \
  --claude-write-approved   # only after numbered human write approval
```

No `:8787` proxy process. Codex `/model` still lists Grok + Claude. Claude remains subscription-backed and oat-free.

**Rule of thumb:** A2a = “server babysits the proxy.” A2b = “Claude is a real provider.”

## Locked decisions (A2b-0)

| # | Decision | Lock |
|---|---|---|
| 1–2 | Provider seam | **B2** — new `BackendProviderKind` (working name `ClaudeCliSubscription`). Rejected B1 short-circuit and B3 two-step. |
| 3 | Server flag | **`--claude-config-dir`** is the enablement surface. Remove `--claude-proxy` / host / port after cutover. |
| 4 | Standalone proxy | **Delete** `gents claude-proxy` and managed-proxy path once A2b green. |
| 5 | Tools | **A2b stays text-only.** Tool bridging that mirrors OpenAI/Grok is **A2c** after A2b provider path works. |
| 6 | Write gate | **CLI/server flag**, refuse-closed by default: `--claude-write-approved`. Drop env double-gate (`PROXY_USE_CLAUDE` / `CLAUDE_WRITE_APPROVED`) from the normal path. Numbered human approval still required before setting the flag for live calls. |
| 7 | Branch | Forge on `spike/claude-subscription-plan` (no A2a merge gate). |

### Naming

Prefer `ClaudeCliSubscription` (or `ClaudeMaxCli` if shorter wins in review). Must serialize as a stable `provider_kind` string on `InferenceBackend`. Not an oat/OAuthCredential provider — `is_agent_scoped_oauth()` stays **false** for Claude.

### Why B2 (not B1)

- Claude auth, transport, and failure modes differ from OpenAI HTTP.
- Grok already has a first-class kind (`XaiGrokOAuth`); Claude should match that honesty.
- Fake `endpoint=http://127.0.0.1:8787/v1` was scaffolding.

## Non-goals for A2b

- Claude↔gents tool bridging / MCP passthrough (**A2c**)
- Harvesting `sk-ant-oat01` or storing Claude tokens in `OAuthCredential`
- Desktop UI
- Cross-home model federation
- Changing Grok / Codex provider paths
- Keeping the HTTP proxy “for debug” after cutover (explicitly deleted)

## Carry-forwards

| # | Contract | A2b |
|---|---|---|
| 1 | Seat in explicit `--config-dir` | Yes — via `--claude-config-dir` |
| 2 | No Claude oat / `OAuthCredential` | Yes |
| 3 | Text-only CLI (`--tools ""`, fail closed on `tool_use`) | Yes for A2b |
| 4 | Numbered human write approval before live Claude | Yes |
| 5 | No `claude --bare`; strip `ANTHROPIC_*` in child | Yes |
| 6 | Full Claude model IDs only | Yes |
| 7 | Prod `~/.gents` unification | Yes |
| 8 | Prefer `./target/debug/gents` | Yes |

## Architecture

### Runtime dispatch

Extend `BackendProviderKind` and the match in `agent/runtime/context.rs`:

```text
OpenAiCompatible / OpenRouter / ChatGptCodex → existing HTTP / OAuth clients
XaiGrokOAuth                                 → Grok client
ClaudeCliSubscription                        → in-process claude_completer
```

Completer already knows how to build argv/env, force `--tools ""`, parse stream-json, and fail closed on `tool_use`. A2b wires that into the owned-loop completion seam instead of HTTP.

### Backend document shape

Prod Claude `InferenceBackend` migrates from:

```text
provider_kind: OpenAiCompatible
endpoint:      http://127.0.0.1:8787/v1
openai_wire_api: ChatCompletions
models: [claude-opus-5, claude-sonnet-5, claude-haiku-4-5-20251001, claude-fable-5]
```

to:

```text
provider_kind: ClaudeCliSubscription
endpoint:      null / unused (or reserved for future native Anthropic HTTP — not required)
openai_wire_api: null / unused
models: same full IDs
# seat path comes from server --claude-config-dir (process-local), not DefraDB secrets
```

No Claude tokens in DefraDB. Config dir is process/operator state, like today’s proxy seat.

### Server flags

```bash
gents server \
  --claude-config-dir <dir> \      # enables Claude provider seat for this process
  --claude-write-approved \        # refuse-closed without this; billable spawn allowed with it
  [--claude-bin <path>] \
  [--claude-workdir <dir>] \
  [--claude-log-dir <dir>]
```

Remove after cutover:
- `--claude-proxy`
- `--claude-proxy-host` / `--claude-proxy-port`
- `gents claude-proxy` command
- `PROXY_USE_CLAUDE` requirement

Keep:
- `gents claude-login --config-dir … --claude-write-approved` (flag, not env)
- `gents claude-auth-probe --config-dir …`

### Lean / schema impact (foundation-flow checkpoint)

Evidence today:
- GraphQL `InferenceBackend.provider_kind` is a **String**, not a closed enum.
- Lean `SelfConfig` lists `provider_kind` as a field name only; it does not enumerate provider kinds.
- Adding `ClaudeCliSubscription` is primarily a **Rust provider-dispatch** change.

**A2b rule:** treat provider-kind addition as plumbing unless we change provider-input assembly, legal request transitions, or transcript→provider sanitization. Those stay unchanged while Claude is text-only.

**A2c** (tool bridging) *does* change what the model feeds the provider / tool loop and must start from Lean provider-input + tool-call lifecycles before Rust.

If implementation discovers a Lean obligation (e.g. sampling validation per provider), stop and do the Lean→conformance→Rust pass explicitly.

## A2c preview (not in A2b scope)

After A2b green, tool bridging should make Claude mirror OpenAI/Grok inside the owned loop:

1. Resolve behavior tool surface (same as today).
2. Expose tools to Claude CLI in whatever protocol the CLI supports (or a future native API).
3. Map Claude `tool_use` → gents `AgentToolCall`.
4. Execute tools in gents; feed results back into the next Claude turn.
5. Keep fail-closed / audit semantics consistent with existing tool-call lifecycle.

That needs its own SPEC and Lean starting point. Do not sneak it into A2b.

## Testing strategy

- Unit: `BackendProviderKind` parse/display for Claude kind.
- Unit: runtime/completion dispatch to completer with fake completer (no network, no CLI).
- Unit: refuse-closed without `--claude-write-approved`; allow with flag.
- Unit: existing claude_completer fixtures remain green (text-only / tool_use fail-closed).
- CLI: `server` accepts `--claude-config-dir` without `--claude-proxy`; rejects live Claude without write flag.
- Migration: prod/spike recipe updates Claude backend `provider_kind`.
- Deletion: `gents claude-proxy` removed; managed-proxy tests removed or replaced.
- Gated live: numbered write approval → server with `--claude-config-dir` + `--claude-write-approved` → Codex/chat Claude pong → no listener required on `:8787` → `OAuthCredential` Claude=0 → `AgentToolCall=0`.

## Success criteria

- [ ] `BackendProviderKind::ClaudeCliSubscription` (final name) exists and dispatches in-process
- [ ] Prod Claude backend no longer depends on OpenAiCompatible/`http://127.0.0.1:8787`
- [ ] `gents server --claude-config-dir …` enables seat; no managed proxy task
- [ ] Live Claude requires `--claude-write-approved` (flag), refuse-closed otherwise
- [x] Standalone `gents claude-proxy` and `--claude-proxy*` flags deleted
- [ ] Codex `/model` still lists Grok + Claude full IDs
- [ ] Text-only Path A contracts hold; tools deferred to A2c
- [ ] Docs (`backends.md`, spike notes) updated
- [ ] Focused tests green; one gated live smoke filed

## Tasks

### A2b-0 Locks — DONE

Human locks recorded above.

### A2b-1 Provider kind + refuse-closed dispatch (no live Claude)

- Add `ClaudeCliSubscription` to `BackendProviderKind` (parse/display/tests).
- Wire runtime/completion path to `claude_completer` with injectable/fake command.
- Implement `--claude-write-approved` refuse-closed gate at the spawn boundary.
- Server: `--claude-config-dir` enables Claude seat without starting HTTP proxy.
- Keep old proxy path temporarily behind tests until A2b-3 deletion, or feature-flag removal in same PR if safer.

**Verify:** unit/CLI tests only; no billable Claude.

### A2b-2 Migrate backend docs + operator recipe

- Update prod Claude `InferenceBackend` to new `provider_kind`.
- Update `docs/backends.md` and spike notes.
- Ensure Codex `/model` projection still works from `models[]`.
- Skip fleet HTTP probes for `ClaudeCliSubscription` (placeholder endpoint is not `/models`).
- `config backend set` to `ClaudeCliSubscription` must clear sticky `openai_wire_api`.
- `list_backend_records` must warn+skip unknown/unparseable `provider_kind` instead of failing the whole list.

**Verify:** GraphQL shows new kind; `/model` lists four Claude IDs + Grok.

### A2b-3 Delete proxy scaffolding

- Remove `gents claude-proxy` command, managed proxy in `serve.rs`, host/port flags, proxy-only tests.
- Remove `PROXY_USE_CLAUDE` from normal operator path.
- Align `claude-login` write gate to `--claude-write-approved` flag.

**Verify:** `cargo test` focused suites; `gents --help` no longer shows `claude-proxy`.

### A2b-4 GATED live verification

Requires numbered Claude write approval:

- Start server with `--claude-config-dir` + `--claude-write-approved`.
- Claude text pong via chat and/or Codex.
- Confirm no dependency on `:8787`.
- Harvest: Claude `OAuthCredential=0`, `AgentToolCall=0`.
- File evidence under `.scratch/claude-spike/logs/`.

### A2b-5 Open A2c SPEC (docs only)

Draft tool-bridging SPEC from Lean provider-input + tool-call lifecycles; no implementation.

## Boundaries

**Always**
- Numbered human approval before any live Claude write.
- Prefer focused tests; use Herdr for long cargo builds.
- Foundation flow: if provider-input/tool legality changes, start in Lean.

**Ask first**
- Final provider kind string if rename from `ClaudeCliSubscription`.
- Any oat / `OAuthCredential` design.
- Starting A2c implementation.
- Changing prod default behavior model.

**Never**
- Silent billable Claude without approval + write flag.
- `claude --bare`.
- Writing Claude tokens into DefraDB.
- Shipping tool bridging inside A2b by stealth.

## Exit

A2b is done when Claude is a first-class in-process provider on the spike branch, the HTTP proxy is gone, write gating is a refuse-closed flag, and docs match. Tool parity with OpenAI/Grok is A2c.
