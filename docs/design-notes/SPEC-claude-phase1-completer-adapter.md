# SPEC: Claude spike Phase 1 — Completer adapter

**Historical (superseded 2026-09-04).** The proxy / process-seat design described here no longer exists; the shipped design is `docs/backends.md` § Claude subscription (agent-scoped `OAuthCredential`, Messages HTTP).

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)

## Goal

Prove Claude Code can be driven as a **text-only completer** under an isolated
config dir, with fail-closed behavior on tool use.

## Non-goals

- OpenAI wire compatibility (Phase 2)
- gents integration (Phase 3)
- Billing proof (Phase 5)
- Using `--bare` or API keys

## Constraints

- **Claude write gate:** every adapter run that invokes `claude` needs an approved write request.
- `cwd` = `.scratch/claude-spike/workdir` (empty; not the gents repo).
- `CLAUDE_CONFIG_DIR` = `.scratch/claude-spike/claude-config`.
- Child env must unset at least: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_API_KEY_OLD`, `CLAUDE_CODE_OAUTH_TOKEN`, and Bedrock/Vertex/Foundry Anthropic provider vars if present.
- Do **not** pass `--bare`.

## Completer argv (contract)

```bash
claude -p \
  --output-format stream-json \
  --verbose \
  --tools "" \
  --permission-mode dontAsk \
  --no-session-persistence \
  --system-prompt "You are a text-only completer. Reply with plain text only." \
  "$PROMPT"
```

Optional later: `--model <mapped>` behind slug `claude-plan`.

## Acceptance criteria

- [ ] Script lives at `.scratch/claude-spike/bin/claude-completer.sh` (or equivalent)
- [ ] Script supports `--dry-run` that prints env + argv and **does not** exec `claude`
- [ ] Without an approved write request, default path is dry-run / refuse
- [ ] On approved run: JSONL stdout parsed to assistant text
- [ ] Any `tool_use` in JSONL → non-zero exit + clear error
- [ ] Missing result / empty assistant text → non-zero exit
- [ ] Logs under `.scratch/claude-spike/logs/` (redact secrets if any appear)
- [ ] Documented one-shot smoke prompt for the first approved write request

## Exit

One successful **approved** text completion via the adapter with no `tool_use`.
Billing glance optional; not a gate.
