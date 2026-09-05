# Spike: Claude subscription as OpenAI-compatible completer

Pinned decisions for proving whether a Claude Code / Claude subscription seat
can back gents' **owned completion loop** without a native Anthropic provider
crate. Companion to `xai-grok-oauth-spike.md` (different seat, same thesis:
gents owns the loop + documents; the vendor CLI is only a billed completer).

**Status:** Phases 0–5 complete; Phase 5 verdict **Go** (2026-08-30). Phase 6
Path A packaging is **complete** under write request #4. A2a unified prod suite
(managed Claude proxy under `gents server`) is **complete** (human-confirmed
2026-09-01). **A2b** first-class in-process Claude provider is **in progress**
on `spike/claude-subscription-plan`: A2b-0/A2b-1 done; A2b-2 migrates backends
off `OpenAiCompatible`/`:8787` onto `ClaudeCliSubscription` +
`claude-cli://subscription` (see
[`SPEC-claude-a2b-in-process.md`](./SPEC-claude-a2b-in-process.md)). Tool bridging
that mirrors OpenAI/Grok is **A2c** after A2b. A2b itself is provider-dispatch
plumbing (text-only); A2c must start in Lean.

## Hard operating rule — Claude write gate

**No Claude-backed invocation without explicit human approval for that specific
call.** This includes:

- `claude` / `claude -p`
- spike proxy forwarding to Claude
- any gents agent turn whose backend points at that proxy
- Claude auth/login that hits Anthropic servers

Agents may prepare scripts, configs, curl templates, and checklists. Before any
billable or potentially billable Claude traffic, stop and present:

```text
CLAUDE WRITE REQUEST #<n>
Phase: <0–6>
Purpose: <one line>
Command/path: <exact>
Env: CLAUDE_CONFIG_DIR=…; ANTHROPIC_*=unset; cwd=…
Model/slug: <…>
Expected usage: ~1 completion, text-only, no tools
Abort if: tool_use | non-zero | unexpected auth path
Proceed? (yes / edit / no)
```

Opportunistic “just one quick test” is forbidden.

## Locked defaults

| # | Question | Decision |
| --- | --- | --- |
| 1 | Phase 3/4 tool surface | **Text-only / no-tools behavior.** Tool bridging is out of spike scope. |
| 2 | Model slug | Advertise **`claude-plan`**. Proxy may map internally to a Claude `--model` later; client always sees `claude-plan`. |
| 3 | Artifact root | Worktree-local **`.scratch/claude-spike/`** (gitignored). Prod `~/.gents` / prod Claude home untouched. |
| 4 | Phase 6 packaging | **Unlocked by Phase 5 Go.** Path A only for v1: CLI-login + loopback completer; **no oat / no `OAuthCredential`**. See `SPEC-claude-phase6-packaging.md`. |
| 5 | Billing gate placement | **Phase 5** (after gents-shaped traffic exists). Not a Phase 0/1 blocker. |
| 6 | Provider shape | **A2b:** `ClaudeCliSubscription` + process seat (`--claude-config-dir`); placeholder endpoint `claude-cli://subscription`. Historical Path A used stock `OpenAiCompatible` + Chat Completions + dummy API key → loopback proxy. |
| 7 | Completer-only enforcement | Claude Code **2.1.x**: `--tools ""` + reject stdout `tool_use`. Do **not** use `--bare` (forces API-key auth). |

## Capability map / phases

| Phase | Module | Claude traffic? |
| --- | --- | --- |
| 0 | Toolchain / isolation | No (local `--help` / path checks only) |
| 1 | Completer adapter | Yes — **gated** |
| 2 | OpenAI loopback proxy | Build no; Claude smoke — **gated** |
| 3 | Gents throwaway integration | Yes — **gated** |
| 4 | Document verification | Prefer no new Claude calls |
| 5 | Billing confirmation | Correlate meter vs logs; extra calls only if approved |
| 6 | Native packaging (optional) | Auth/login — **gated**; only after Phase 5 Go |

Build order is strict: each phase’s exit criteria unlock the next.

## Out of scope

- Native Anthropic Messages provider crate
- Claude→gents tool-call bridging
- Harvesting `sk-ant-oat01` / direct Messages API on plan tokens
- Schema change to optional OAuth token fields
- Lean / conformance changes
- Touching production gents home or production Claude config

## Spec index

| Spec | Phase |
| --- | --- |
| [`SPEC-claude-phase0-toolchain.md`](./SPEC-claude-phase0-toolchain.md) | 0 |
| [`SPEC-claude-phase1-completer-adapter.md`](./SPEC-claude-phase1-completer-adapter.md) | 1 |
| [`SPEC-claude-phase2-openai-proxy.md`](./SPEC-claude-phase2-openai-proxy.md) | 2 |
| [`SPEC-claude-phase3-gents-integration.md`](./SPEC-claude-phase3-gents-integration.md) | 3 |
| [`SPEC-claude-phase4-document-verify.md`](./SPEC-claude-phase4-document-verify.md) | 4 |
| [`SPEC-claude-phase5-billing-confirmation.md`](./SPEC-claude-phase5-billing-confirmation.md) | 5 |
| [`SPEC-claude-phase6-packaging.md`](./SPEC-claude-phase6-packaging.md) | 6 (Path A) |
| [`SPEC-claude-a2a-unified-suite.md`](./SPEC-claude-a2a-unified-suite.md) | A2a (complete) |
| [`SPEC-claude-a2b-in-process.md`](./SPEC-claude-a2b-in-process.md) | A2b (in progress; A2b-0/1 done) |

## Spike-level success / fail

**Done when:** Phases 0–4 pass and Phase 5 yields an explicit Go/No-Go on
subscription-as-completer viability.

**Failed when:** completer emits `tool_use` under `--tools ""`, proxy cannot
satisfy rig SSE, gents turn does not persist expected documents, or Phase 5
shows Console/API billing instead of plan meter (No-Go).

## Local layout (Phase 0)

```text
.scratch/claude-spike/
  claude-config/     # CLAUDE_CONFIG_DIR
  workdir/           # empty cwd for claude -p
  logs/              # adapter + proxy request logs
  bin/               # dry-run stubs / scripts
  proxy/             # proxy source (Phase 2)
  gents-home/        # $GENTS_SPIKE_HOME (created in Phase 3)
```
