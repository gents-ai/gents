# SPEC: Claude A2c — tool bridging inside the owned loop

**Date:** 2026-09-02  
**Status:** Draft for review (architecture; landing is Track B). Shipped: single Messages wire 2026-09-03; auth on the agent-scoped `OAuthCredential` 2026-09-04 (PR 5) — see the C2 auth lock. Mentions of the text-only CLI Completer (`claude_completer`, `--tools ""`), `--claude-config-dir`, `.credentials.json`, and the Keychain below describe retired mechanisms and are kept as history.  
**Parent:** [`SPEC-claude-a2b-in-process.md`](./SPEC-claude-a2b-in-process.md)  
**Depends on:** A2b green + Track A P1–P4 on `spike/claude-p4-write-gate` (in-process `ClaudeCliSubscription`, proxy gone, refuse-closed write flag, honest stream-json).  
**This document:** architecture for tool bridging. Landing purposes: [`PR-STACK-claude-track-b-tools.md`](./PR-STACK-claude-track-b-tools.md). No Rust from this draft alone.  
**Branch:** own Track B family off `spike/claude-p4-write-gate` — not P5 on Track A.

## Problem

A2b makes Claude a first-class in-process provider (`BackendProviderKind::ClaudeCliSubscription`) and keeps it **text-only**: the Completer forces `--tools ""` and fails closed on any stdout `tool_use`. That is honest plumbing, not tool parity.

OpenAI and Grok already run inside the owned completion loop:

```text
behavior tool surface (reconcile)
  → CompletionRequest.tools
  → provider emits tool_call
  → hook persists AgentToolCall
  → dispatch_tool in gents
  → tool_result threaded into the next turn
```

Claude does not. A behavior with tools still reaches `run_loop_stream` with definitions; A2b **ignores** them, so Claude cannot call `bash`, MCP, skills, or `spawn_subagent`. The spike Path A contract (`--tools ""` + reject `tool_use`) was a viability fence, not the destination.

## Goal

Claude mirrors OpenAI/Grok **inside the owned loop**. Gents owns execution, audit, and the tool-call state machine. Claude is only the completer that may *request* tools.

```text
owned loop (unchanged chokepoint: run_loop_stream)
  1. Resolve the behavior tool surface (same reconcile as today).
  2. Expose that surface to Claude (CLI protocol or a future native API).
  3. Map Claude tool_use → gents AgentToolCall (native ToolCall / hook.on_tool_call).
  4. Execute in gents (dispatch_tool + existing policy / approval / subagent bridge).
  5. Feed results back into the next Claude turn (native tool_result threading).
  6. Fail closed / audit with the existing ToolCall lifecycle.
```

```text
owned loop
  ├─ XaiGrokOAuth           → Grok OAuth client  (tools today)
  ├─ OpenAiCompatible       → OpenAI HTTP client (tools today)
  └─ ClaudeCliSubscription  → claude_completer   (tools in A2c)
```

**Rule of thumb:** A2b = “Claude is a real provider.” A2c = “Claude may request gents tools; gents still runs them.”

## Foundation flow (start in Lean)

A2c changes **what the model feeds the provider** and **which tool_use events are legal to dispatch**. That is not provider-kind plumbing. Follow the repo rule: **Lean models → conformance tests → Rust.** Zero `sorry`s. If a proof obligation is hard to discharge, treat that as information — do not paper over it in the Completer.

A2b explicitly deferred this:

> A2c (tool bridging) *does* change what the model feeds the provider / tool loop and must start from Lean provider-input + tool-call lifecycles before Rust.

### What is a legal-transition / provider-input change (Lean first)

| Area | Why A2c touches it | Starting files |
|---|---|---|
| Provider-input assembly | After mapping, the owned loop must still see a sanitizer-valid history: paired `assistantToolCalls` + `toolResult`, no orphans, unique call ids, content-order normalize. If the Claude on-the-wire view is *not* the native transcript (CLI prompt flattening, session resume, content-block ids), that view is a **new projection** that must refine `sanitizeForProvider`. | `Proofs/PromptAssembly/Provider.lean`, `Executable.lean`, `State.lean`, `Properties.lean` |
| Tool-argument repair | Claude `tool_use.input` is a JSON object; OpenAI `function.arguments` is often a string. Mapping must land in the proven `ToolArgs` object normal form before dispatch. | `Proofs/PromptAssembly/ToolArgs.lean` |
| Per-turn / aggregate budget | Tool turns already grow input; A2c makes Claude capable of those turns. Existing `input + effectiveOutput ≤ context` and request-wide ledger still apply. Missing Claude usage reports must keep the fail-closed charge classifications, not a new silent-success path. | `Proofs/PromptAssembly/Budget.lean`, `AggregateBudget.lean` |
| Loop-threading chokepoint | Entry sanitization at `run_loop_stream` stays the only provider-bound gate. Completer-side flattening must not bypass it. Prove the Claude map is a homomorphism into the existing row model, or extend the model. | `Proofs/PromptAssembly` + `run_loop_stream` contract cases |
| Tool-call lifecycle | Claude-originated calls must use the **same** `ToolCallState` machine (`pending` → `awaitingApproval` \| `running` → terminal). New states are a last resort. A new `FailureClass` (e.g. unmapped provider tool) is a legal-transition change and needs Lean + the ToolCall conformance machine. | `Proofs/ToolExecution/{State,Transition,Policy,Properties,Executable}.lean`, `Proofs/Conformance/Contracts/Machines/ToolCall.lean` |
| Unique call ids | Claude ids look like `toolu_*`. Gents already has `call_id` + `internal_call_id`. The map must be injective for the life of the request; reuse across turns is the defect class `UniqueCallIds` / per-turn resolution already fence. | `Proofs/CrossMachineComposed/UniqueCallIds.lean`, `PromptAssembly.Provider` (call-occurrence multiplicity boundary) |
| Subagent lifecycle | **Only if** the behavior surface includes spawn tools (`spawn_subagent`, …). A Claude `tool_use` of those names must take the **bridge** path (`childRequestId = some`, `bridge_complete` / `bridge_failure` / `bridge_cancel_cascade`), never `complete_native`. Claude Code’s own `Task` tool is not a gents child request. | `Proofs/Conformance/Contracts/Machines/Subagent.lean`, ToolCall named `bridge_*` rows, `Proofs/Recovery/Sweeps/ToolCalls.lean` |
| Tool / command policy | Exposure is the **gents** surface (ToolPolicy meet + CommandPolicy argv/sandbox/env), not Claude Code’s built-in Bash/Read/Write. Enabling Claude-native tools is a policy bypass. | `Proofs/ToolPolicy`, `Proofs/CommandPolicy`, `Proofs/ToolExecution/Policy.lean` (`preflight`) |
| Recovery | Claude-originated `AgentToolCall` rows must be sweepable with the existing causes (deadline, parent interrupt/terminal, child terminal, unclaimed spawn). No Claude-only zombie path. | `Proofs/Recovery/Sweeps/ToolCalls.lean` |
| RenderedCapture | Persist-before-send stays mandatory. A2b-4 requires persist-before-send on the process-CLI Completer seam; A2c must keep that fence when tools exist. Canonical body for a tool turn includes the exposed surface, not A2b’s `tools: []`. `CanonicalRequest` is opaque, so body shape is plumbing **unless** capture can be skipped or rebound. | `Proofs/RenderedCapture.lean` |

### What is plumbing (Rust after Lean/conformance, not a new machine)

- Completer argv: stop forcing `--tools ""` on tool-capable turns; pass the **locked** protocol flags.
- Parse stream-json `tool_use` / `tool_result` blocks instead of `CompleterParseError::ToolUse`. [Superseded 2026-09-03 by the single Messages HTTP wire: `claude_messages::MessagesSseState` parses Messages SSE; there is no stream-json.]
- Map into existing `gents_protocol::message::ToolCall` / `StreamedAssistantContent::ToolCall` so `loop_stream` / `dispatch_tool` / `hook.on_tool_call` do not grow a Claude-only dispatch path.
- Fake-completer fixtures (JSONL with `tool_use`, paired results, unmapped names). [Superseded 2026-09-03 by the single wire: test-only SSE fixtures queued through `claude_messages` (`#[cfg(test)]`).]
- Name translation tables (if any) and argv/env construction.
- Server flags already exist (`--claude-config-dir`, `--claude-write-approved`, `--claude-bin`, workdir, log-dir). [Superseded 2026-09-03 by the single wire: only `--claude-config-dir` and `--claude-write-approved` remain; `--claude-bin`, workdir and log-dir went with the process completer.] [Retired 2026-09-04: the server write gate was removed; `--claude-config-dir` was the opt-in until PR 5 (same day) replaced the seat with the agent-scoped `OAuthCredential`.]

**A2c rule:** if implementation discovers that Claude cannot emit a homomorphism into the native tool-call/result row model, **stop and extend Lean** (likely a Claude provider-view in `PromptAssembly`). Do not flatten unpaired `tool_use` into ordinary assistant text to “make the CLI work.”

## Carry-forwards (Path A / A2b contracts that still apply)

| # | Contract | A2c |
|---|---|---|
| 1 | Seat in explicit `--claude-config-dir` | Yes — process-local, not DefraDB secrets [Retired 2026-09-04 (PR 5): no seat; the credential is an `OAuthCredential` document] |
| 2 | No Claude oat / `OAuthCredential` | Yes — `is_agent_scoped_oauth()` stays **false** [Retired 2026-09-04 (PR 5): now **true**; see the C2 auth lock] |
| 3 | Numbered human write approval before live Claude | Yes — `--claude-write-approved` refuse-closed [Retired 2026-09-04: the server write gate was removed; `--claude-config-dir` was the opt-in until PR 5 (same day) replaced the seat with the agent-scoped `OAuthCredential`.] |
| 4 | No `claude --bare`; strip `ANTHROPIC_*` (and cloud Anthropic provider vars) in the child | Yes |
| 5 | Full Claude model IDs only | Yes |
| 6 | Prod `~/.gents` unification / `./target/debug/gents` | Yes |
| 7 | Gents owns the loop + documents | Yes — **strengthened**: gents also owns tool execution |
| 8 | Text-only CLI (`--tools ""`, fail closed on `tool_use`) | **A2b only.** A2c replaces this on tool-capable turns; empty surface may keep the A2b fence |

A2b lock #5 (“tools deferred to A2c”) is the parent of this SPEC. It does **not** lock a Claude tool wire protocol.

## Architecture (target shape; protocol not locked)

### Owned-loop path (locked as the destination)

This is the same path OpenAI/Grok already take. A2c must not add a Claude-owned inner agent loop.

```text
reconcile → ToolDyn[] on the behavior
run_loop_stream
  sanitize_history_for_provider          // PromptAssembly
  build_request(.tools(tool_defs))       // native definitions
  Completer (ClaudeCliSubscription)
    persist-before-send                  // RenderedCapture
    expose defs via locked protocol
    stream native ToolCall items
  hook.on_tool_call → AgentToolCall row  // ToolCall machine
  dispatch_tool / subagent bridge
  hook.on_tool_result
  close_streaming_turn threads results
  next completion turn
```

`loop_stream.rs` is **not** the place to special-case Claude. Mapping lives at the Completer seam (`claude_completer` / `claude_subscription`), then the owned loop sees ordinary `ToolCall` / `ToolResult` messages.

### Completer today (A2b, fail closed)

```text
completer_argv:  claude -p --output-format stream-json --tools "" …
parse_stream_jsonl: any content-block type=tool_use → CompleterParseError::ToolUse
complete_text: if request.tools nonempty → warn and ignore
capture body: tools: []
```

Fixture `crates/gents/src/claude_completer/fixtures/tool_use.jsonl` is the current fail-closed witness (`Bash` / `toolu_1`). A2c keeps that fixture as the **unmapped / Claude-native** reject case; it must not become a silent execute of Claude `Bash`.

### What “expose tools to Claude” must mean

The surface is **gents’** resolved behavior tools (`tool_surface` at reconcile: native, MCP, skills, datastore, spawn, …), not Claude Code’s built-in catalog.

Claude-native names (`Bash`, `Read`, `Write`, `Edit`, `Glob`, `Grep`, `Task`, `WebSearch`, …) are **not** enabled on the CLI. If the model emits one that is not in the gents surface, fail closed — do not let the CLI execute it, and do not alias it onto gents `bash` / `read_file` without an explicit, tested name map (open question).

## Tool protocol options (propose; do not lock)

A2b did not lock how Claude learns about gents tools. **B1 (wire evidence) must**, from CLI/docs evidence, not a meeting. B2 (fake owned-loop round-trip) proves at the native row boundary and does not wait on B1. Until B1 fills the table, treat the following as options.

### C1 — Claude CLI stream-json as a function-calling wire (extend Path A)

Keep the in-process CLI Completer. On tool-capable turns, stop passing `--tools ""`. Parse `tool_use` content blocks into native `ToolCall`s; execute in gents; re-invoke the CLI for the next turn with results in the provider input.

**Attractions:** stays on the subscription CLI seat; no oat harvest; matches A2b process topology.

**Risks (must be answered before lock):**

- Claude Code `--tools` historically names **built-in** tools the CLI will **execute**. That is the opposite of A2c. Need evidence of a “declare but do not execute” mode (custom schemas, `--max-turns 1` without execution, `--input-format stream-json` continuation, etc.).
- `--permission-mode dontAsk` plus enabled tools may mean the CLI runs Bash before gents sees the block.
- `--no-session-persistence` vs `--resume`: how are `tool_result`s fed back? First-class content blocks, or prompt flattening that PromptAssembly cannot see?
- Does `--tools` accept JSON schemas for gents names, or only `Bash,Read,…`?

**Lean impact if chosen:** prove either (a) the Completer map is a homomorphism into existing `MessageKind`, or (b) a `ClaudeCliView` projection of assembled history is sound, idempotent, and split-stable.

### C2 — Future native Anthropic Messages API (tools JSON)

Standard `tools` / `tool_use` / `tool_result` over HTTP, same shape the PromptAssembly row model already assumes for OpenAI-like providers.

**Attractions:** cleanest homomorphism; streaming tool_calls match `loop_stream` with less argv archaeology.

**Risks:** Path A / A2b forbid storing Claude tokens in `OAuthCredential` and harvesting `sk-ant-oat01`. A native API that reads the CLI seat **without** persisting oat is a new human lock, not implied by A2b. Endpoint-on-`InferenceBackend` was reserved in A2b as unused.

**Lean impact if chosen:** likely smaller PromptAssembly change; still need injective id map + fail-closed unmapped names. Transport capture moves from process-CLI to HTTP persist-before-send (already modeled); do not drop the fence.

### C3 — Prompt-stuffed JSON (keep `--tools ""`)

Leave the CLI text-only. Stuff tool schemas into the system prompt; parse a JSON convention out of assistant text; synthesize `ToolCall`s.

**Attractions:** CLI never executes tools.

**Risks:** not `tool_use`; pairing/ids are synthetic; models drift; sanitizer theorems would describe a protocol we invented. **Last resort** if C1 cannot declare-without-execute and C2 cannot be done without oat.

### C4 — MCP / Claude-owned loop (rejected by goal)

Point Claude Code at gents tools as MCP servers (or enable Claude `Task`) so **Claude** runs the agent loop. That inverts ownership: no `run_loop_stream` dispatch, no `AgentToolCall` machine, no PromptAssembly chokepoint. **Out of A2c.** MCP tools that already live on the **gents** behavior surface still execute through gents, as they do for OpenAI/Grok.

### Comparison

| | C1 CLI stream-json | C2 native Messages | C3 prompt JSON | C4 Claude-owned MCP |
|---|---|---|---|---|
| Gents owns execution | Only if CLI cannot execute | Yes | Yes | No |
| Homomorphism into PromptAssembly | Unknown (open) | Likely | Synthetic | N/A |
| Seat / no oat | Yes | **Locked:** process-local seat read, no DefraDB oat | Yes | Yes, but wrong owner |
| A2c default candidate | Dead (`--tools`) | **Locked for B3** | Last resort (not taken) | Rejected |

### B1 evidence (2026-09-02) — no live write

Pinned against local CLI **2.1.251** (`claude --help`) and official [CLI reference](https://code.claude.com/docs/en/cli-reference) / [headless](https://code.claude.com/docs/en/headless). No `claude -p` model call.

| Question | Finding |
|---|---|
| Does `--tools` accept gents JSON schemas? | **No.** Help: “from the built-in set.” Docs: restrict built-ins; `""` disables all, `"default"` all, or names like `"Bash,Edit,Read"`. Does **not** affect MCP tools. |
| `--allowedTools` / `--disallowedTools` | Permission / deny rules, not schema declaration. |
| `--mcp-config` custom tools | Claude Code **executes** MCP tools (`mcp__server__tool`). That is C4 (Claude-owned loop). Out. |
| `--tools ""` | CLI executes nothing. Model is not given gents tools. |
| `--max-turns` | Limits agentic turns in print mode. Does **not** declare-without-execute; enabled tools still run. |
| `--input-format stream-json` | Exists (print mode). Can feed later messages / `tool_result` **if** we already have a function-calling wire. Not a declaration mechanism. |
| `--permission-mode dontAsk` | Denies anything not pre-allowed; does not stop execution of allowed built-ins. |
| PreToolUse hooks as a fence | Documented to deny before execute, but GitHub #36071 reports `-p` races (hook after execute). Not a gents-owned loop. |
| Declare-without-execute flag | **Not in help or official reference.** A live probe cannot invent a missing flag; it could only confirm that enabled `--tools` execute. |

**C1 as originally proposed** (stop `--tools ""`, pass gents names on `--tools`) is **dead**: that flag cannot name gents tools, and naming built-ins means the CLI executes them.

**Locked 2026-09-02 (human):** live B3 wire is **C2** (Anthropic Messages HTTP). C3 is not the path. C1 `--tools` declaration stays dead. C4 stays rejected.

### C2 auth lock (oat-free)

The Messages client authenticates from the **process seat** (`--claude-config-dir`), never from DefraDB [Superseded 2026-09-04 (PR 5) — see the paragraph at the end of this section; the bullets below are the 2026-09-02 lock as written]:

- Read `$CLAUDE_CONFIG_DIR/.credentials.json` → `claudeAiOauth.accessToken` (same file the CLI uses on Linux and as the macOS Keychain fallback). Do not silently fall back to `~/.claude`.
- Send `Authorization: Bearer <accessToken>` on `POST /v1/messages`. Do **not** send the oat as `x-api-key`.
- **Never** upsert `OAuthCredential`, never harvest `sk-ant-oat01` into DefraDB, never log or print the token.
- `is_agent_scoped_oauth()` stays **false**. Health (superseded 2026-09-03; originally the CLI auth-status subcommand, P3) is the seat-token read the wire performs: `probe_process_seat_health` → `read_seat_access_token`, no spawn; the detail carries source + expiry, with the `claude-login` hint on Expired/MissingFile. A document born `unknown` is promoted to `healthy` on the first passing cycle, like HTTP backends.
- Expired / missing token fail closed; operator re-runs `gents claude-login` (write-gated). Token refresh that would write the seat is **out of the first B3 slice**.
- Live Messages send still requires `--claude-write-approved`. [Retired 2026-09-04: the server write gate was removed; `--claude-config-dir` was the opt-in until PR 5 (same day) replaced the seat with the agent-scoped `OAuthCredential`.]
- Empty `ToolDyn[]` stays on the A2b process-CLI Completer (`--tools ""`). Tool-capable turns use Messages HTTP so the CLI never executes tools.
- macOS Keychain (`Claude Code-credentials`, keyed to `CLAUDE_CONFIG_DIR`) is the CLI’s primary store; file read is the testable/Linux path. Keychain read is a follow-on if the file is absent — not a DefraDB lookup.

Do **not** enable MCP so Claude Code runs gents tools.

**Recorded 2026-09-03 (single wire, as shipped):**

- The Claude Code identity block is `system[0]` on every request (Lean `ClaudeMap.systemBlocks_head`); the assembled preamble and `Message::System` rows follow verbatim.
- macOS Keychain is read via `security(1)` when `.credentials.json` is absent; the file stays the Linux/testable path. [Retired 2026-09-04 (PR 5): both readers deleted.]
- One wire: every Claude turn, tool-capable or text-only, goes over Messages HTTP. The process-CLI Completer is gone; the `claude` binary is a login-time dependency only. [Superseded 2026-09-04 (PR 5): not a dependency at all.]
- `tools` is omitted from the body when the surface is empty (Lean `ClaudeMap.toolsField_empty`), never sent as `[]`.
- Two `cache_control` breakpoints: the last `system` block and the last content block.
- Live evidence (2026-09-04): #10 failed closed on an expired seat and was re-run as #10b, PASS — `.scratch/claude-spike/logs/b3-live-single-wire-b-evidence.md` (single wire, `cached_input_tokens=9727` on the second inference, `tools` only on `inference.1`, 0 4xx/429). #11 PASS — `b3-live-health-expired-evidence.md` (expired seat → unhealthy after K=3, login hint, no spawn/HTTP) and `b3-live-health-restored-evidence.md` (`unknown` promoted to `healthy` by the next probe cycle).

**Superseded 2026-09-04 (PR 5, OAuth credential parity):** the process seat is gone. The Messages client authenticates with an agent-scoped `OAuthCredential` (`provider = "claude-subscription"`, `credential_id = "claude-subscription:<agent_did>"`, `enabled = true`) written by `gents claude-login`, which runs the PKCE loopback flow itself (`--manual` for paste-the-code, `--no-browser` to print the URL); the `claude` binary is not involved. `is_agent_scoped_oauth()` is now **true** for `ClaudeCliSubscription`. The bearer is the shared single-flight `DbCredentialBearer` (`OAuthRefreshKind::Claude`): gents refreshes against Anthropic's token endpoint (JSON body) within 5 minutes of expiry, owner-only, and persists the rotated row; a `401` invalidates the bearer once and the next request refreshes. Health is the credential-expiry read shared with Grok/Codex (`probe_oauth_credential`): fresh promotes `unknown`, missing fails with the `gents claude-login --agent-did <did>` hint, and (since 444a532a, same day) a stale access token with a refresh token still passes so the next request can refresh it; the probe never refreshes and never spawns. A behavior with no enabled credential is not runnable. `gents diagnose` reports `checks.claude_auth`. Deleted: `gents server --claude-config-dir`, the status JSON `claude_subscription` object, the `.credentials.json` and Keychain readers, the seat probe, and the in-process CLI completer. Carry-forwards 1 and 2 are retired; the "oat-free" premise of this lock no longer holds — the Claude oat lives in DefraDB under the same agent-scoped replication rule as the other OAuth kinds. Operator guide: `docs/backends.md`.

## Fail-closed / audit semantics

Match the existing tool-call lifecycle. Do not add a Claude-only “best effort” path.

**Must fail closed (no dispatch, no CLI-side execution):**

- `tool_use` whose `name` is not on this turn’s gents surface (unknown tool, Claude-native `Bash`/`Task`/…, stale name after reconcile).
- Unparseable `tool_use` (missing id, missing name, non-object input that `ToolArgs` cannot repair into an object).
- Duplicate `toolu_*` / gents `call_id` in one turn (Provider multiplicity boundary; production already drops duplicate keys).
- `tool_use` on a behavior with an empty tool surface (empty `ToolDyn[]`) — keep A2b reject.
- Persist-before-send failure — no spawn (RenderedCapture `capture_failure_blocks_send`).
- Live path without `--claude-write-approved`. [Retired 2026-09-04: the server write gate was removed; `--claude-config-dir` was the opt-in until PR 5 (same day) replaced the seat with the agent-scoped `OAuthCredential`.]
- Preflight block (`ToolExecution.preflight`: unreachable MCP → `serviceUnavailable`; invalid schema → `argumentInvalid`; policy hold/deny unchanged).

**Must still persist and audit:**

- Every dispatched call writes `AgentToolCall` through `hook.on_tool_call` **before** `dispatch_tool` (pending → running / awaitingApproval).
- Results go through `hook.on_tool_result` with the same truncation / model-facing text rules.
- Approval holds (`awaitingApproval` → approve/deny/timeout/cancel) apply to Claude-originated calls.
- Subagent-typed tools use bridge transitions, not native complete/fail.
- Recovery sweeps see Claude-originated rows like any other provider’s.

**Unknown-tool already has a typed outcome** in `dispatch_tool` (`error: unknown tool '{name}'`). That is a **completed tool result** fed back to the model. A2c’s fail-closed for **Claude-native** names should be stricter at the Completer parse: do not turn “Claude tried to `Bash`” into a gents tool result unless an explicit alias table is locked later. Prefer turn failure (`FailureClass` TBD — likely `policyDenied` or a new class, Lean first) over quietly teaching Claude that gents will catch CLI tools.

## Non-goals

- Implementing A2c in this draft (no Rust, no Lean proofs, no live Claude).
- Desktop UI / accounts panel for Claude.
- Storing Claude tokens in `OAuthCredential` / harvesting `sk-ant-oat01`.
- Reintroducing `gents claude-proxy` or HTTP `:8787`.
- Changing Grok / OpenAI / Codex tool paths.
- Letting Claude Code execute tools (Bash, filesystem, Task, MCP) on gents’ behalf.
- Cross-home model federation.
- Shipping a protocol lock inside this SPEC — that is **B1** (evidence into this lock table).

## Testing strategy (after locks + Lean)

- **Lean:** no `sorry`; PromptAssembly soundness/fixpoint/idempotence/split-stability still hold on mapped traces; ToolCall machine unchanged or explicitly extended; UniqueCallIds preserved under the id map.
- **Conformance:** generated witnesses for the map (paired `tool_use`→call+result, unpaired drop, orphan drop, duplicate id, unmapped name). Drive `tests/conformance/prompt_assembly.rs` and ToolCall matrix tests from the spec, not a hand oracle.
- **Fake completer (no network, no CLI):** JSONL fixtures — text-only still works; mapped `tool_use` becomes `AgentToolCall` + gents execute + next-turn prompt contains the result; Claude `Bash` / unknown name fail closed; empty surface + `tool_use` fail closed; persist-before-send still required when `tools` is nonempty.
- **Unit:** argv no longer forces `--tools ""` on tool-capable turns; empty surface keeps the A2b fence; env strip and no `--bare` unchanged.
- **Gated live:** numbered write approval only; one behavior with a harmless gents tool (not Claude `Bash`); `AgentToolCall ≥ 1` with legal transitions; `OAuthCredential` Claude = 0; no `:8787`.

## Success criteria

- [x] B1 evidence recorded (2026-09-02): C1 `--tools` custom schemas dead; MCP = C4.
- [x] Live wire **C2** locked (2026-09-02): Messages HTTP; oat-free seat read; empty surface stays process CLI.
- [ ] Lean starting models updated or explicitly proven unchanged; zero `sorry`s.
- [ ] Conformance witnesses generated from Lean, consumed by Rust.
- [ ] Claude tool_use maps to `AgentToolCall` and executes in gents, not in Claude Code.
- [ ] Results thread into the next Claude turn through the owned loop.
- [ ] Unmapped / Claude-native / empty-surface `tool_use` fail closed.
- [ ] Path A/A2b seat, write gate, no-oat, no `--bare` contracts hold.
- [ ] Persist-before-send still holds on the process-CLI (or native HTTP) seam with tools present.
- [ ] Fake-completer suite green; one gated live smoke filed.

## Tasks

Landing purposes (not methodology stages): [`PR-STACK-claude-track-b-tools.md`](./PR-STACK-claude-track-b-tools.md).

**Invariants, not tasks:** no aliases (`Bash` ↛ `bash`); empty surface keeps A2b `--tools ""`; C4 rejected; spawn is B4 later (first product = native+MCP only).

### B1 Wire evidence — evidence recorded 2026-09-02

Fill this SPEC’s lock table from CLI/docs evidence (live probe only with numbered write approval if help cannot answer):

1. Protocol: **C2** native Messages locked for live B3. C1 `--tools` declaration dead. C3 not taken. C4 rejected.
2. C1 flags: N/A (dead). `--input-format stream-json` is feedback-only.
3. C2 auth: process-local `--claude-config-dir/.credentials.json` `claudeAiOauth.accessToken` as `Authorization: Bearer`. No `OAuthCredential`. No token logs. Expired → fail closed + re-login. Keychain follow-on if file absent. [Superseded 2026-09-04 (PR 5): agent-scoped `OAuthCredential` via `gents claude-login`; see the C2 auth lock.]

**Verify:** lock table filled; live empty-surface argv still `--tools ""`.

### B2 Owned-loop round-trip (fake)

One purpose. Lean → conformance → Completer map are how it lands:

- Prove the content-block map is a homomorphism into existing `MessageKind` **or** add a Claude view that is sound/idempotent/split-stable. UniqueCallIds on `toolu_*`. Reuse ToolCall states. Persist-before-send and budget fail-closed classes unchanged. Zero `sorry`s.
- Lean-computed witnesses (paired round-trip, unpaired/orphan drop, duplicate id, unmapped name). Fences go red against A2b Completer until the map lands.
- Map at `claude_completer` / `claude_subscription` only. Fake JSONL paired round-trip; keep `tool_use.jsonl` as Bash-unmapped reject. Empty surface and unmapped/native names fail closed. Do not special-case Claude inside `loop_stream.rs` unless Lean changed the chokepoint.

**Verify:** focused `claude_` / conformance / loop-stream tool tests; no billable Claude; live argv still `--tools ""`.

B2 does not wait on B1 (native row boundary is shared by C1 and C2). If B1 later forces C3, stop and extend Lean.

### B3 Live tool-capable seat

Depends on B1 + B2. Stop forcing `--tools ""` on tool-capable turns using the locked wire. Gated live with numbered write approval: gents-owned tool (not Claude `Bash`); `AgentToolCall ≥ 1`; oat Claude = 0; no `:8787`; no CLI Bash in the workdir. Evidence under `.scratch/claude-spike/logs/`.

### B4 Spawn / subagent (later)

Out of first product. Claude `tool_use` of gents spawn tools must take the bridge path, never Claude `Task`.

## Boundaries

**Always**

- Numbered human approval before any live Claude write.
- Lean → conformance → Rust for anything that changes provider-input or tool legality.
- Gents executes tools; Claude does not.
- Prefer focused tests; Herdr for long cargo builds.

**Ask first**

- Starting B3 without a B1 wire lock.
- Any oat / `OAuthCredential` / reading CLI credentials for a native API.
- Aliasing Claude-native tool names onto gents tools.
- Changing `loop_stream` control flow rather than the Completer seam.
- New `ToolCallState` values.
- Starting B4 (spawn) in the first product.

**Never**

- Silent billable Claude without approval + write flag.
- `claude --bare`.
- Writing Claude tokens into DefraDB.
- Enabling Claude Code built-in tools so the CLI runs Bash/files/Task.
- Sneaking tool bridging into Track A / P1–P4, or landing B2 without Lean, or landing B3 without a B1 wire.

## Exit

A2c is done when a Claude-backed behavior with a gents tool surface runs the **same** owned-loop tool path as OpenAI/Grok: surface in, `AgentToolCall` audit in the middle, results back, fail-closed on anything that is not that surface — and the Lean provider-input + tool-call models say that path is legal.

This SPEC is done when it is specific enough to run Track B (B1 evidence + B2 Lean/map), with open questions listed rather than silently locked. B1 answers the protocol question from evidence.

## Open questions (human; do not imply answers)

1. **Protocol lock:** C1 (CLI stream-json) vs C2 (native Messages API)? Is there evidence Claude Code can *declare* custom tools without *executing* them? — **Closed 2026-09-03:** C2 on a single Messages wire; `.scratch/claude-spike/logs/b3-live-args-evidence.md`, `b3-live-single-wire-b-evidence.md` (#10b PASS 2026-09-04; #10 failed closed on an expired seat and was re-run as #10b).
2. **Result feedback (C1):** `--resume` / session continuation vs a fresh `-p` with flattened history vs `--input-format stream-json`? Which of those is a PromptAssembly homomorphism?
3. **C2 auth:** may gents call Anthropic HTTP using the CLI seat on disk without persisting oat in `OAuthCredential`? Or is any native API out of Path A forever? — **Closed 2026-09-03:** yes; seat file or Keychain via `security(1)`, `OAuthCredential` count stays 0; `.scratch/claude-spike/logs/b3-live-http-text-evidence.md`. **Reopened and re-closed 2026-09-04 (PR 5):** the seat read was a stopgap; Claude now uses an agent-scoped `OAuthCredential` like Codex/Grok (see the C2 auth lock).
4. **Name map:** fail closed on Claude-native names always, or lock an explicit alias table (`Bash`/`bash`, `Read`/`read_file`)? Default proposal: **no aliases**.
5. **Empty surface:** keep A2b `--tools ""` + reject `tool_use` when `ToolDyn[]` is empty? — **Closed 2026-09-03:** empty surface goes over the same Messages wire with `tools` omitted and `tool_use` rejected fail-closed; `.scratch/claude-spike/logs/b3-live-http-text-evidence.md`.
6. **Subagent v1:** include `spawn_subagent` / bridge tools in the first A2c slice, or native+MCP only?
7. **`FailureClass`:** reuse `policyDenied` / `external` for unmapped Claude tools, or add a class? (Lean if new.)
8. **CLI version floor:** still Claude Code 2.1.x, or does C1 need a newer flag set?
9. **`--system-prompt`:** A2b overwrites with a text-only instruction. A2c must not clobber the gents preamble/skills assembly (`PromptAssembly.Template` layer order). How does the CLI `--system-prompt` interact with the assembled request? — **Closed 2026-09-03:** no CLI in the wire; the assembled preamble rides as `system[1..]` behind the identity block; `.scratch/claude-spike/logs/b3-live-identity-evidence.md`, `b3-live-single-wire-b-evidence.md` (#10b PASS 2026-09-04: `system[0]` identity, behavior System row at `system[1]`, no `system:` user block).
10. **Usage reporting:** stream-json `result` usage vs aggregate-budget fail-closed `Missing` charge — acceptable for v1?
11. **Partial/streaming tool_use:** wait for a complete content block before `on_tool_call`, or map deltas? Owned loop currently ignores `ToolCallDelta`.
12. **Parallel `tool_use` blocks in one assistant message:** map 1:1 to parallel `dispatch_tool` like OpenAI, or serialize? (Lifecycle allows multiple calls; UniqueCallIds still applies.)
13. **Permission mode:** keep `dontAsk`? If C1 cannot disable execution, `dontAsk` is unsafe.
14. **Workdir:** A2b empty spike workdir vs gents workspace cwd for a tool-capable CLI child (even if the CLI must not execute tools).

## Appendix: Lean / Rust map (implementation starting points; do not edit in A2b-5)

| Concern | Lean | Rust (later) |
|---|---|---|
| Sanitize / pairing | `Proofs/PromptAssembly/{Provider,Executable,Properties}.lean` | `compaction::sanitize_history_for_provider`, `loop_stream` entry |
| Args object normal form | `Proofs/PromptAssembly/ToolArgs.lean` | Completer map of `input` → `ToolFunction.arguments` |
| Layer order / preamble | `Proofs/PromptAssembly/Template.lean` | `build_request` + CLI `--system-prompt` (open #9) |
| Budgets | `Proofs/PromptAssembly/{Budget,AggregateBudget}.lean` | `loop_stream` clamp + charge |
| ToolCall machine | `Proofs/ToolExecution/*`, `Conformance/Contracts/Machines/ToolCall.lean` | `hook.on_tool_call` / `dispatch_tool` / `tool_call_lifecycle` |
| Subagent bridge | `Machines/Subagent.lean`, ToolCall `bridge_*` | spawn tools on the behavior surface |
| Unique ids | `CrossMachineComposed/UniqueCallIds.lean` | `toolu_*` → `ToolCall.id` / `internal_call_id` |
| Capture order | `Proofs/RenderedCapture.lean` | process-CLI persist-before-send (keep when tools exist) |
| Recovery | `Proofs/Recovery/Sweeps/ToolCalls.lean` | existing sweeps |
| Completer fail-closed today | — | `claude_completer::parse_stream_jsonl`, `completer_argv`, `claude_subscription::complete_text` |
