# SPEC: Claude spike Phase 2 — OpenAI loopback proxy

**Historical (superseded 2026-09-04).** The proxy / process-seat design described here no longer exists; the shipped design is `docs/backends.md` § Claude subscription (agent-scoped `OAuthCredential`, Messages HTTP).

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)

## Goal

Present the Phase 1 completer as stock **OpenAI Chat Completions** so gents’
`OpenAiCompatible` + `ChatCompletions` path can call it unchanged.

## Non-goals

- Responses API wire
- Forwarding tools to Claude
- Real auth validation
- Billing proof

## Constraints

- Bind **`127.0.0.1` only**
- Ignore `Authorization`
- Never forward or inherit Anthropic secrets to child processes
- Strip / ignore `tools` and `tool_choice` before calling the completer
- Advertise model id **`claude-plan`**
- Claude invocations only through Phase 1 adapter and only with write-gate approval
- Proxy source under `.scratch/claude-spike/proxy/`

## Wire contract

### `GET /v1/models`

Returns JSON including at least:

```json
{ "data": [ { "id": "claude-plan", "object": "model" } ] }
```

### `POST /v1/chat/completions`

- Accept OpenAI chat messages body
- Flatten `messages[]` → single prompt for the completer (system + user sufficient for spike)
- If `stream: true` (rig default): respond with OpenAI SSE
  - `data: chat.completion.chunk` lines
  - terminate with `data: [DONE]`
- If `stream: false`: optional non-stream `chat.completion` JSON for curl smoke
- Must tolerate `stream_options.include_usage` without error (usage fields may be zero/null)

## Acceptance criteria

- [ ] Proxy starts on loopback; port recorded in logs
- [ ] `GET /v1/models` returns `claude-plan`
- [ ] Non-stream curl smoke works **only after** approved Claude write (or documented as requiring approval)
- [ ] Streaming SSE smoke satisfies a minimal OpenAI client / curl SSE check under write gate
- [ ] Request log written (timestamp, model, message count, stream flag) — no secrets
- [ ] Sending `tools` / `tool_choice` does not reach Claude as enabled tools; completer still text-only
- [ ] Build/unit of proxy possible without any Claude call

## Exit

Streaming Chat Completions against loopback works with an **approved** completer
invocation. Phase 3 may begin.
