# SPEC: Claude spike Phase 6 — Path A packaging

**Historical (superseded 2026-09-04).** The proxy / process-seat design described here no longer exists; the shipped design is `docs/backends.md` § Claude subscription (agent-scoped `OAuthCredential`, Messages HTTP).

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)  
Unlocked by: Phase 5 verdict **Go** (`.scratch/claude-spike/logs/phase5-verdict.md`)

## Objective

Package the proven Option 1 stack so an operator can run Claude Max as a
**gents-owned text completer** without harvesting Anthropic OAuth access tokens
into DefraDB and without a native Anthropic Messages provider.

Success looks like:

1. Operator runs `gents claude-login` (official Claude CLI login) against a
   chosen `CLAUDE_CONFIG_DIR`.
2. Operator starts a loopback OpenAI Chat Completions proxy that shells to the
   Claude completer under sanitized env.
3. A gents home points stock `OpenAiCompatible` + `ChatCompletions` at that
   proxy with model `claude-plan` and a dummy API key.
4. One gated text turn completes; documents persist; no oat / no `tool_use`.

## Capability map

| Module id | Responsibility | Depends on |
| --- | --- | --- |
| `claude-completer-lib` | Rust-ify Phase 1 adapter: env sanitize, argv, stream-json parse, fail-closed on `tool_use` | — |
| `claude-loopback-proxy` | Productize Phase 2 proxy (loopback Chat Completions SSE, tool strip, request log) calling completer-lib | `claude-completer-lib` |
| `claude-login-probe` | CLI `claude-login` (+ a seat probe command, retired 2026-09-03; `claude-login` now prints the `seat` field itself); seat status only; **no oat persistence** | — |
| `claude-operator-preset` | Docs + init/config recipe: OpenAiCompatible → proxy `/v1`, model `claude-plan`, text-only behavior | `claude-loopback-proxy`, `claude-login-probe` |
| `claude-backends-doc` | `docs/backends.md` row + spike design-note status update | `claude-operator-preset` |

Build order: `claude-completer-lib` → `claude-loopback-proxy` → `claude-login-probe` → `claude-operator-preset` → `claude-backends-doc`.

Login/probe can start in parallel with completer-lib (no code dependency), but
preset/docs wait for both.

## Locked Path A decisions

| # | Question | Decision |
| --- | --- | --- |
| 1 | Transport | Keep Option 1: gents → OpenAI Chat Completions loopback → Claude CLI completer |
| 2 | `BackendProviderKind` | **No new kind in v1.** Stock `OpenAiCompatible` + `ChatCompletions` |
| 3 | DefraDB credentials | **No oat.** Do **not** upsert `access_token` / `refresh_token` / `id_token` for Claude |
| 4 | `OAuthCredential` row | **Omit in v1.** Seat truth lives in Claude CLI config (`CLAUDE_CONFIG_DIR`); probe reads CLI/`claude auth status`, not DefraDB |
| 5 | Login UX | Wrap official `claude auth login --claudeai` (or current equivalent). No gents-hosted claude.ai login page |
| 6 | Tool surface | Text-only default for this backend path; `--tools ""` + reject stdout `tool_use` |
| 7 | Model slug | Client-facing `claude-plan` |
| 8 | Secrets / env | Child processes strip `ANTHROPIC_*` and cloud Anthropic provider env; never use `claude --bare` |
| 9 | Write gate | Numbered human Claude write gate remains for billable smoke during packaging |
| 10 | Scope vs Grok | Mirror Grok *operator UX* lightly; do **not** copy Grok’s bearer-in-DefraDB model |

## Explicitly deferred (not Path A v1)

- `BackendProviderKind::AnthropicClaudeSubscription` (or similar) that spawns proxy in-process
- Metadata-only `OAuthCredential` for desktop accounts panel
- Harvesting `sk-ant-oat01` / transport B Messages API
- Tool bridging (Claude tools ↔ gents tools)
- Desktop wizard card
- Lean / schema changes
- Auto-start proxy from `gents serve`

## Tech stack

- Rust in `crates/gents` / `crates/gents-cli` for completer parse, env sanitize,
  loopback OpenAI adapter (`gents claude-proxy`), and login/probe commands
- Spike Python proxy is **reference only** — Path A ships Rust
- Claude Code CLI 2.1.x+ on PATH (operator dependency)
- Stock gents `OpenAiCompatible` completion path (rig Chat Completions)

## Stabilization posture

Path A is an **experimental operator path**, not the final product shape.
Keep using the loopback adapter under the write gate, observe billing/auth/tool
behavior, and only open A2 (native provider kind / in-process / desktop) after
the Path A contract is stable.

## Commands

```bash
# Fixture / unit (no Claude)
cargo test -p gents claude_completer -- --nocapture
cargo test -p gents-cli --lib claude_ -- --nocapture

# Login / probe (may hit Anthropic auth — WRITE GATE if network login)
gents claude-login --config-dir "$CLAUDE_CONFIG_DIR" --dry-run
gents claude-login --config-dir "$CLAUDE_CONFIG_DIR"   # needs CLAUDE_WRITE_APPROVED=1; prints seat: {ok, detail}

# Local OpenAI adapter (Rust)
# Default = canned (no Claude). Live needs PROXY_USE_CLAUDE=1 + CLAUDE_WRITE_APPROVED=1.
gents claude-proxy --config-dir "$CLAUDE_CONFIG_DIR" --host 127.0.0.1 --port 8787
PROXY_USE_CLAUDE=1 CLAUDE_WRITE_APPROVED=1 \
  gents claude-proxy --config-dir "$CLAUDE_CONFIG_DIR" --host 127.0.0.1 --port 8787

# Operator gents home (isolated during packaging smokes)
gents init --home "$GENTS_HOME" --agent-name claude-plan \
  --inference-url "http://127.0.0.1:8787/v1" \
  --provider-kind OpenAiCompatible \
  --openai-wire-api chat-completions \
  --api-key not-used \
  --model-name claude-plan
```

(Exact CLI flags verified against `crates/gents-cli` on branch
`spike/claude-subscription-plan`. Prefer `./target/debug/gents` until the
installed `~/.local/bin/gents` is rebuilt with Path A commands.)

## Operator recipe (Path A, experimental)

Use **isolated** homes and Claude config dirs. Do **not** point these commands
at prod `~/.gents` or a personal `~/.claude` during packaging smokes.

### 0. Isolate paths

```bash
REPO_ROOT="$(pwd)"   # gents checkout
SPIKE="$REPO_ROOT/.scratch/claude-spike"
export CLAUDE_CONFIG_DIR="$SPIKE/claude-config"
export GENTS_HOME="$SPIKE/gents-home-path-a"
mkdir -p "$CLAUDE_CONFIG_DIR" "$GENTS_HOME" "$SPIKE/logs" "$SPIKE/workdir"
```

### 1. Seat login + probe (no DefraDB oat)

```bash
# Dry-run prints planned argv; never contacts Anthropic.
gents claude-login --config-dir "$CLAUDE_CONFIG_DIR" --dry-run

# Live login wraps `claude auth login --claudeai` and requires a numbered
# Claude write approval + CLAUDE_WRITE_APPROVED=1. Prefer a human-interactive
# shell if the Claude CLI needs keychain / browser.
CLAUDE_WRITE_APPROVED=1 gents claude-login --config-dir "$CLAUDE_CONFIG_DIR"

# Seat probe command retired 2026-09-03: `claude-login` prints `seat: {ok, detail}` after login.
```

### 2. Start loopback OpenAI adapter

```bash
# Canned / offline wiring check (default; no Claude traffic):
gents claude-proxy --config-dir "$CLAUDE_CONFIG_DIR" --host 127.0.0.1 --port 8787

# Live Claude path (WRITE GATE):
PROXY_USE_CLAUDE=1 CLAUDE_WRITE_APPROVED=1 \
  gents claude-proxy --config-dir "$CLAUDE_CONFIG_DIR" --host 127.0.0.1 --port 8787
```

Adapter contract:

- Loopback bind only (`127.0.0.1` / `localhost` / `::1`)
- Client model slug `claude-plan`
- Strips `tools` / `tool_choice`; completer uses `--tools ""` and fails closed on `tool_use`
- Child env strips `ANTHROPIC_*` / cloud Anthropic vars; never `claude --bare`

### 3. Init isolated gents home → stock OpenAiCompatible

No new `BackendProviderKind`. Point Chat Completions at the proxy with a dummy key:

```bash
gents init --home "$GENTS_HOME" --agent-name claude-plan \
  --inference-url "http://127.0.0.1:8787/v1" \
  --provider-kind OpenAiCompatible \
  --openai-wire-api chat-completions \
  --api-key not-used \
  --model-name claude-plan
# Default tool package is readonly (omit --write / --yolo for text-only Path A).
```

### 4. Serve + one text turn (live = WRITE GATE)

```bash
gents server --home "$GENTS_HOME"
# In another pane, after write approval #N for a live Claude turn:
gents chat --home "$GENTS_HOME"   # or the repo's preferred interactive chat entry
```

Abort if: `tool_use` appears, probe shows API-key auth, any `OAuthCredential`
upsert for Claude, or traffic leaves loopback.

### 5. Verify no oat

```bash
# Seat probe command retired 2026-09-03; read the `seat` field printed by `gents claude-login`.
# Confirm oauth_credential_written=false and credential_store=claude_config_dir.
# On the smoke home, OAuthCredential count for any Claude/Anthropic provider must stay 0.
```

## Project structure (expected touch points)

```text
crates/gents/src/claude_completer/     # parse + argv + env sanitize (new)
crates/gents-cli/src/commands/claude_login.rs
# proxy: either keep script under tools/ or .scratch promotion path documented
docs/backends.md
docs/design-notes/claude-subscription-spike.md
docs/design-notes/SPEC-claude-phase6-packaging.md
tasks/plan.md
tasks/todo.md
```

Spike artifacts under `.scratch/claude-spike/` remain the reference implementation
until code lands; do not delete them in Phase 6.

## Code style

Follow existing Grok/Codex command patterns for CLI surface, but **credential
handling diverges**:

```rust
// Good (Path A): probe reports CLI seat; no token upsert
pub struct ClaudeAuthProbeResult {
    pub logged_in: bool,
    pub auth_method: Option<String>,      // e.g. claude.ai
    pub subscription_type: Option<String>, // e.g. max
    pub config_dir: PathBuf,
    pub api_key_source: Option<String>,   // expect "none" for subscription seat
}

// Bad (out of scope): copy oat into OAuthCredential.access_token
```

Completer parser stays fail-closed: any `tool_use` → error; empty assistant → error.

## Testing strategy

| Level | What |
| --- | --- |
| Unit | stream-json fixtures: success / `tool_use` / missing result |
| Unit | env sanitize removes Anthropic/cloud vars from child command |
| Unit | CLI help / arg parse for login + probe |
| Integration (no Claude) | proxy canned/fake-completer SSE + models list |
| Gated live | one approved login (if needed), one approved completer/proxy/gents smoke |

Coverage bar: parser + env sanitize covered by unit tests before any live Claude.

## Boundaries

**Always**

- Keep Claude write gate for billable calls
- Strip Anthropic/cloud env from completer children
- Fail closed on `tool_use`
- Leave prod `~/.gents` and prod Claude config untouched during packaging smokes
- Prefer isolated `CLAUDE_CONFIG_DIR` for tests

**Ask first**

- Adding `BackendProviderKind`
- Any `OAuthCredential` upsert (even metadata-only)
- Rewriting proxy from Python to Rust
- Desktop UI work
- Changing default tool policy beyond the Claude-plan operator recipe
- Shipping proxy as a required subprocess of `gents serve`

**Never**

- `claude --bare`
- Harvest / store `sk-ant-oat01` (or any Anthropic oat) in DefraDB
- Native Anthropic Messages provider in this phase
- Tool bridging
- Committing secrets or live `CLAUDE_CONFIG_DIR` contents
- Treating CLI `total_cost_usd` as Console API billing proof

## Success criteria

- [x] Completer library parses fixtures fail-closed without invoking Claude
- [x] Documented operator path: login → proxy → gents OpenAiCompatible turn
- [x] `gents claude-login` (and the since-retired seat probe command) exist and do not write oat docs
- [x] `OAuthCredential` count for Claude provider remains 0 on packaging smoke home
- [x] `docs/backends.md` documents Claude Max subscription via loopback completer
- [x] Parent spike design note marks Phase 6 Path A status (packaged / partial)
- [x] No Lean or schema changes
- [x] Any live Claude traffic used numbered write approvals

Evidence: `.scratch/claude-spike/logs/task20-packaging-evidence.md` (write request #4). A2 remains deferred.

## Open questions — locked 2026-08-30

1. **Proxy / adapter language:** **Rust** (spike Python is reference only).
2. **Command names:** `gents claude-login` (+ a seat probe command, retired 2026-09-03).
3. **Config dir default:** **require explicit `--config-dir`** (no silent `~/.claude`).
4. **Adapter packaging location:** **`gents claude-proxy`** CLI subcommand
   (local OpenAI Chat Completions adapter in front of Claude CLI).
5. **A2 (native provider / in-process / desktop):** unlocked 2026-08-31 as
   **A2a managed-proxy unified suite** (complete 2026-09-01; see
   [`SPEC-claude-a2a-unified-suite.md`](./SPEC-claude-a2a-unified-suite.md)).
   In-process completer is **A2b** — draft SPEC:
   [`SPEC-claude-a2b-in-process.md`](./SPEC-claude-a2b-in-process.md).

## Exit

Path A operator recipe is implemented and documented enough that a new engineer
can reproduce the Phase 3 text turn without reading spike archaeology. Follow-on
A2 (native provider kind / desktop) needs a separate SPEC.