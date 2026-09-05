# PR stack: Claude tool bridging (Track B)

**Date:** 2026-09-02
**Parent SPEC:** [`SPEC-claude-a2c-tool-bridging.md`](./SPEC-claude-a2c-tool-bridging.md)
**Base:** `spike/claude-p4-write-gate` (Track A complete). Own branches; not P5 on the P1–P4 Graphite stack.
**Not this track:** fork-retry; restoring Grok as prod default (ask first); renaming `ClaudeCliSubscription`; desktop UI; letting Claude Code execute tools.

**Status (2026-09-04):** B3 is done — write requests #7–#11 (`.scratch/claude-spike/logs/b3-live-args-evidence.md`, `b3-live-http-text-evidence.md`). #10 failed closed on an expired seat (`b3-live-single-wire-evidence.md`, environmental FAIL) and was re-run as #10b: PASS on 2026-09-04, `b3-live-single-wire-b-evidence.md` — single Messages wire observed live (`system[0]` identity, behavior System row at `system[1]`, no `system:` user block, `cache_control` on the last system and content blocks, `tools` only on `inference.1`, `cached_input_tokens=9727` on the second inference, `AgentToolCall.args.path == "."`, response `listed`, zero 4xx/429). #11 PASS on 2026-09-04: expired seat measured unhealthy after K=3 with the `gents claude-login --config-dir` hint, no spawn, no HTTP (`b3-live-health-expired-evidence.md`); a row hand-set to `unknown` was promoted to `healthy` by the next probe cycle (`b3-live-health-restored-evidence.md`). The two-wire Completer (process CLI for empty surfaces, Messages HTTP for tools) was retired on 2026-09-03 in favour of the single Messages wire — see `docs/superpowers/specs/2026-09-03-claude-single-wire-design.md` (never landed in-tree; the shipped wire is recorded in the C2 auth lock of `SPEC-claude-a2c-tool-bridging.md`). B4 (spawn / subagent) is still later.

**Status (2026-09-04, PR 5 — OAuth credential parity):** the host-local seat is deleted. Claude authenticates with an agent-scoped `OAuthCredential` (`provider = "claude-subscription"`, `credential_id = "claude-subscription:<agent_did>"`) written by `gents claude-login` — a first-party PKCE loopback flow with `--manual` / `--no-browser` fallbacks; the `claude` binary is not a dependency. Gents refreshes the token itself (single-flight, owner-only, within 5 minutes of expiry) and a `401` invalidates the bearer once. Health is the credential-expiry probe shared with Grok/Codex (never refreshes, never spawns); a behavior with no enabled credential is not runnable and names `gents claude-login --agent-did <did>`; `gents diagnose` reports `checks.claude_auth`. `gents server --claude-config-dir`, the status JSON `claude_subscription` object, the `.credentials.json` / Keychain readers, the seat probe, and the in-process CLI completer are gone. Branch `claude/pr5-oauth-parity` on `claude/pr4-cli-docs`; live probes #13–#16 are its verify gate. Operator guide: `docs/backends.md`.

**Status (2026-09-05):** re-ported onto main as `claude/track-b-on-main` (no seat era; T1–T5).

Track A made Claude an honest **text** provider (stream, usage, seat health, write-gate). That is still text-only: `--tools ""`, fail-closed on any `tool_use`. [Historical: Track A as of 2026-09-02; superseded by the single wire (2026-09-03) and the credential model (2026-09-04).] Track B is a different product: Claude may **request** gents tools; gents **executes** them on the same owned loop as Grok/Codex.

## Why this is its own track

The old A2c-0…4 list was one feature cut into methodology stages (lock → Lean → conformance → code → live) and hung under P4. Track A was purpose-sliced: each PR was independently valuable if you stopped. Track B must be the same.

Lean → conformance → Rust still applies **inside** any slice that changes legal transitions or provider-input. Those are how a purpose lands, not the purposes themselves. A gated live turn is a verify gate on a purpose, not a PR whose purpose is “go live.”

## What the old steps were actually for

| Old | What it was | Purpose it was trying to serve | Where it goes now |
|---|---|---|---|
| A2c-0 (1) C1 vs C2 | Human lock before all work | Know a wire that can *declare* gents tools without the CLI *executing* them | **B1** (evidence, not a meeting) |
| A2c-0 (2) C1 flags / result feedback | Human lock | Same as B1 — exact argv and how `tool_result` returns | **B1** |
| A2c-0 (3) C2 oat-free auth | Human lock | Only if B1 kills C1 | **B1** fallback, or a later C2 slice |
| A2c-0 (4) name map | Human lock | Gents names are the surface; `Bash` is not `bash` | Invariant of **B2/B3**, default **no aliases**. Not a PR |
| A2c-0 (5) subagent in v1 | Human lock | Claude `spawn_subagent` is a gents child request, never Claude `Task` | **B4** later. First product is native+MCP only |
| A2c-0 (6) empty surface | Human lock | Text-only behaviors stay on the A2b fence | Invariant of **B2/B3**. Not a PR |
| A2c-1 Lean | Methodology | Claude `tool_use`/`tool_result` are legal PromptAssembly + ToolCall traces | Method inside **B2** |
| A2c-2 conformance | Methodology | Generated witnesses fence the map | Method inside **B2** |
| A2c-3 Completer map | The product increment | Fake completer round-trip through `run_loop_stream` | Product of **B2** |
| A2c-4 live tool turn | Methodology / smoke | Real seat, gents executes, CLI does not Bash, oat=0 | Verify gate of **B3** |

## Purposes (the stack)

```text
Track B — Claude requests gents tools; gents executes
B1  wire evidence
B2  owned-loop round-trip (fake completer)
B3  live tool-capable seat
B4  spawn / subagent          (optional, later; not v1)
```

B1 and B2 do **not** block each other. B3 needs both. B4 is a later purpose, not a lock that stalls B1–B3.

C4 (Claude-owned MCP / Claude `Task` as the agent loop) stays **rejected**. C3 (prompt-stuffed JSON) stays last resort if B1 kills C1 and C2 cannot auth without oat.

### B1 — Wire evidence

**Purpose:** know how live Claude learns about gents tools without Claude Code running them.

This is archaeology plus a written lock, not a vote. `--tools` on Claude Code has historically named **built-ins the CLI executes**. If that is still true and there is no declare-without-execute mode, C1 is dead.

**Change.**

- Capture evidence: CLI help/docs/flags for custom tool schemas, permission modes, `--max-turns`, `--input-format stream-json`, `--resume` vs fresh `-p`, whether the CLI executes enabled tools before gents sees `tool_use`.
- Live probe only with numbered write approval, and only if help/docs cannot answer. Prefer a throwaway `--claude-config-dir`, not prod `~/.gents`. [Retired 2026-09-04 (PR 5): no config dir; use a throwaway `--home` with `gents claude-login`.]
- Fill the SPEC lock table from that evidence: C1, or C1 dead → C2 (oat-free auth must be stated), or both dead → C3 last resort.
- No Completer behavior change. Keep `--tools ""` on the live path.

**Stop after:** the SPEC records a wire. We do not guess in B3 argv.

**Evidence 2026-09-02 (CLI 2.1.251 + official cli-reference, no live `-p`):** `--tools` is the built-in set only. C1 “pass gents names on `--tools`” is dead. MCP is C4 (CLI executes). `--input-format stream-json` is feedback-only.

**Locked 2026-09-02:** live B3 is **C2** (Messages HTTP). Auth: read `claudeAiOauth.accessToken` from `--claude-config-dir/.credentials.json`; `Authorization: Bearer`; never DefraDB oat; never log the token. Empty surface stays process CLI. C3 not taken. [Superseded 2026-09-04 (PR 5): auth is the agent-scoped `OAuthCredential` written by `gents claude-login`; see the status above.]

**Not in B1:** Lean, fake-completer map, changing `loop_stream`, aliases, spawn.

### B2 — Owned-loop round-trip (fake)

**Purpose:** a Claude-shaped `tool_use` of a **gents** name becomes a normal `AgentToolCall`, gents runs it, and the next provider turn contains the `tool_result`. Production CLI stays text-only. [Superseded 2026-09-03 by the single Messages wire: no CLI in the wire.]

This is the product increment that makes “Claude is on the same loop as Grok” true in tests. The homomorphism is at the **native row** boundary (`tool_use` / `tool_result` content blocks → `MessageKind` ToolCall/ToolResult). That shape is shared by C1 stream-json and C2 Messages, which is why B2 does not wait on B1. If B1 later forces C3, B2 Lean would need a different projection — stop and extend the model; do not flatten unpaired `tool_use` into assistant text.

**How it lands** (foundation flow, one purpose):

1. Lean: map is a homomorphism into existing `sanitizeForProvider` / ToolCall states, **or** a `ClaudeCliView` that is sound, idempotent, and split-stable. UniqueCallIds under `toolu_*` → gents `call_id`. No new `ToolCallState` unless a proof says we must. Empty surface and unmapped/Claude-native names fail closed (prefer turn failure over teaching Claude that gents will catch `Bash`). Persist-before-send still blocks send. Budget/aggregate fail-closed classes unchanged. Zero `sorry`s.
2. Conformance: Lean-computed witnesses (paired round-trip, unpaired drop, orphan drop, duplicate id, unmapped name). Rust fences go red against today’s A2b Completer.
3. Completer map at `claude_completer` / `claude_subscription` only. Stop ignoring `request.tools` on the **fake** path. Stop fail-closed on **mapped** `tool_use`. Keep fail-closed on unmapped / native Claude / empty surface. Fake JSONL: one paired round-trip; keep `tool_use.jsonl` as the Bash-unmapped reject. Do not special-case Claude in `loop_stream.rs` unless Lean changed the chokepoint. [Superseded 2026-09-03: `claude_completer` went with the process completer; the map lives in `claude_messages` / `claude_subscription`.]

If Lean is large, land (1)+(2) then (3) as stacked commits/PRs **of B2**, not as a new track. Same purpose.

**Stop after:** fake-completer tool round-trip is green; live argv still `--tools ""`.

**Not in B2:** live CLI flags; spawn/subagent; oat; aliases; HTTP Messages client (that is C2 transport, B3 or a C2 follow-on).

### B3 — Live tool-capable seat (C2 Messages)

**Purpose:** on a real seat, a tool-capable Claude behavior requests a gents tool; the CLI does not execute; gents does; the loop continues.

**Depends on:** B1 (C2 lock) and B2 (map).

**Change.**

- Empty surface: keep A2b process CLI (`--tools ""`).
- Tool-capable turns: Anthropic `POST /v1/messages` with gents `tools` JSON. Auth from the seat file (C2 lock). Capture as HTTP persist-before-send (do not drop the fence). Map `tool_use` with B2 allow-list. Next turn is native `tool_result` content, not CLI flatten.
- Gated live: numbered `--claude-write-approved`; harmless gents tool (not Claude `Bash`); `AgentToolCall ≥ 1`; oat Claude `OAuthCredential` = 0; no `:8787`; no CLI Bash in the workdir. Evidence under `.scratch/claude-spike/logs/`. [Retired 2026-09-04: the server write gate was removed; `--claude-config-dir` was the opt-in until PR 5 (same day) replaced the seat with the agent-scoped `OAuthCredential`.]

**Stop after:** live tool parity for native+MCP (and skills already on the gents surface). Still no Claude-owned tools. Still no spawn unless B4.

**Not in B3:** C3; Keychain (follow-on if `.credentials.json` absent); token refresh that writes the seat; spawn. [Closed 2026-09-04 (PR 5): the Keychain follow-on is moot — the credential is a document; refresh now lives in gents and writes the `OAuthCredential` row.]

### B4 — Spawn / subagent (later)

**Purpose:** a Claude `tool_use` of gents spawn tools takes the **bridge** path (`childRequestId = some`), never `complete_native`, never Claude `Task`.

Out of the first product. First slice is native + MCP (+ skills already on the behavior). Do not block B1–B3 on this.

## Invariants (not PRs)

Record in the A2c SPEC. Do not open a slice to decide them unless we need to change them.

- Gents executes; Claude only requests.
- No aliases: `Bash` ↛ `bash`, `Read` ↛ `read_file`, … unless explicitly locked later.
- Empty `ToolDyn[]` keeps A2b `--tools ""` + fail-closed on `tool_use`.
- No oat in `OAuthCredential`. No `--bare`. Persist-before-send before spawn. Write gate refuse-closed. [Retired 2026-09-04 (PR 5): the Claude oat is an agent-scoped `OAuthCredential` like Codex/Grok; the write gate was removed the same day.]
- C4 rejected.
- Mapping lives at the Completer seam.

## Always / ask / never

**Always:** numbered write approval before live Claude; fake-completer tests before a live B3; Lean first inside B2 (and inside any later slice that changes legal transitions).

**Ask first:** starting B3 without a B1 wire lock; C2 that reads CLI credentials; any alias table; changing `loop_stream` control flow; new `ToolCallState`; starting B4.

**Never:** silent billable Claude; enabling Claude Code built-ins so the CLI runs Bash/files/Task; sneaking this into Track A / P1–P4.

## Landing

Own branch family off `spike/claude-p4-write-gate`, e.g. `spike/claude-b1-wire` / `b2-round-trip` / `b3-live-tools`. Do not Graphite-stack these as P5–P8 on Track A.

B1 and B2 may proceed in parallel. B3 stacks on both. Focused `claude_` + PromptAssembly/ToolCall conformance tests. Live Claude only on B1 (if evidence needs it) and B3, each with numbered write approval.
