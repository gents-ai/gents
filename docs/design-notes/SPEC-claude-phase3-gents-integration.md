# SPEC: Claude spike Phase 3 — Gents throwaway integration

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)

## Goal

Drive **one** owned-loop text turn: gents → OpenAI-compatible backend → spike
proxy → Claude completer, under an isolated gents home.

## Non-goals

- Tool round-trips
- Production home changes
- New `BackendProviderKind`
- Billing Go/No-Go (Phase 5)

## Constraints

- `$GENTS_SPIKE_HOME` = `.scratch/claude-spike/gents-home` (or absolute path under `.scratch/claude-spike/`)
- Backend: `OpenAiCompatible` + `openai-wire-api chat-completions` + dummy `--api-key not-used`
- `--inference-url` / endpoint = proxy `/v1`
- `--model-name` / models = `claude-plan`
- Behavior: **text-only / no tools** for the spike agent
- Entire agent turn is a **single Claude write request** (or N explicit requests if multiple completions occur — each must be approved if gated at proxy)

## Suggested init shape

```bash
gents init --home "$GENTS_SPIKE_HOME" --agent-name claude-spike \
  --inference-url "http://127.0.0.1:${PROXY_PORT}/v1" \
  --provider-kind OpenAiCompatible \
  --openai-wire-api chat-completions \
  --api-key not-used \
  --model-name claude-plan
```

(Adjust to whatever `gents init` flags the installed CLI exposes; record the
exact command used in the spike log.)

## Acceptance criteria

- [ ] Throwaway home created; prod gents home untouched
- [ ] Backend document points at loopback proxy with ChatCompletions wire
- [ ] Spike agent behavior has **no tool surface** (or tools disabled)
- [ ] Before the turn: Claude write request approved by human
- [ ] One agent turn completes with assistant text
- [ ] Proxy log shows the corresponding request
- [ ] No Claude built-in tools executed (adapter fail-closed still holds)

## Exit

Owned-loop text turn succeeded under write gate. Phase 4 verifies documents
(preferably without new Claude calls).
