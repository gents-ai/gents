# Backends

This page is the committed backend support matrix for defra-agent. It tracks
which providers are supported, which wire API each provider uses, what request
and response shaping defra-agent applies, and whether a provider has an offline
wire fixture replay fence.

The runtime owns provider-input assembly before any provider-specific client is
called. Provider-specific shaping should stay small, explicit, and tested at the
HTTP seam because live provider bugs tend to appear in headers, unsupported
parameters, response content types, and tool schema details rather than in the
agent loop itself.

## Support Matrix

| Provider kind | Wire API | Auth | Streaming | Tools | Reasoning | Request shaping | Response shaping | Fixture status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `OpenAiCompatible` | OpenAI Responses by default; Chat Completions fallback for compatible local servers | API key or local/no-auth | SSE | Function tools through rig | `reasoning.effort` plus local `chat_template_kwargs.enable_thinking` default | Adds cache-scope `user` when available; OpenAI-style params pass through | Standard rig OpenAI handling | Planned by #545 |
| `ChatGptCodex` | ChatGPT Codex Responses endpoint | `OAuthCredential` document, refreshed by owner runtime | SSE | Function tools, forced `strict: false` to match Codex CLI | `reasoning.effort` currently fixed at `medium` | Strips unsupported `max_output_tokens`, `temperature`, `top_p`; injects instructions/store/stream defaults; adds Codex `version` and `Accept` headers | Adds missing `Content-Type: text/event-stream` only when the backend omits it; synthesizes completion body from SSE for non-streaming probes | Unit-pinned in #530; replay corpus planned by #545 |
| `OpenRouter` | Chat Completions | API key | SSE | Function tools through rig | Provider-dependent | Adds OpenRouter provider preference `require_parameters: true` | Standard rig OpenRouter handling | Planned by #545 |
| local OpenAI-compatible servers | Responses or Chat Completions depending on server support | Usually none/local key | SSE varies by server | Function tools when server supports them | Reasoning parser support varies; `enable_thinking` is sent for vLLM-style servers | Same as `OpenAiCompatible`; operators may need Chat Completions fallback for servers without `/v1/responses` | Standard rig OpenAI handling | Planned by #545 |

## Probe lifecycle and health (#640)

Backend availability composes two signals, and they deliberately live in
different places:

- **Operator/bootstrap intent** — the fleet-replicated `InferenceBackend`
  document's `enabled` and `probe_status`. The startup ratchet promotes
  `unknown → healthy` for reachable backends, the scheduled prober keeps that
  promotion recurring (stamping `last_probe`), and
  `defra-agent config backend set --probe-status ...` remains the manual
  override. Nothing ever writes `unhealthy` here: reachability is
  observer-relative, and 16 runtimes stomping one document would replicate
  churn and conflicting opinions.
- **Measured health** — each runtime's scheduled prober (default: every 60s,
  10s timeout) probes the models endpoint of every enabled, probeable backend
  and keeps an in-memory `BackendHealthMap`. Hysteresis is K=3 consecutive
  failures to demote to `unhealthy`, one success to promote back (formal
  model: `crates/defra-agent/proofs/Proofs/BackendHealth/`). ChatGPT-Codex
  backends are never probed (OAuthCredential is agent-scoped) and therefore
  never demoted — the document status governs them.

Effective availability is `intent AND NOT measured-unhealthy`: a measured
demotion removes the backend from admission and marks dependent behaviors
unavailable within `probe_interval × K + reconcile debounce`, and one
successful probe restores routing. Measured state resets on restart (a dead
backend is doc-available again for up to K probe intervals until re-demoted).

The `defra_agent_backend_probe_status{backend_id,status}` metric reports the
MEASURED state with value 1 iff healthy — it genuinely reads 0 during an
outage — and `defra_agent_backend_last_probe_seconds` reports probe freshness.
Both fall back to document values for backends the prober has no opinion on.

## Wire Fixture Policy

Provider fixture replay is tracked in #545. Recorded fixtures live under
`crates/defra-agent/tests/fixtures/providers/` and must be safe to commit.

Rules:

- No access tokens, refresh tokens, API keys, account ids, or bearer strings in
  fixtures.
- Redaction happens before writing fixtures to disk.
- Fixture replay should assert every recorded request is consumed exactly once.
- Fixture refresh is a live/operator action; CI should replay committed fixtures
  offline.

The fixture directory has a regression test that scans committed fixture files
for common credential patterns. The scanner is intentionally conservative: if a
new provider introduces another credential shape, add it to the scanner before
committing fixtures.

## ChatGPT subscription (ChatGptCodex, OAuth)

Use a ChatGPT/Codex subscription instead of an API key. The credential is stored
as an `OAuthCredential` DefraDB document scoped by `agent_did` and provider
(`chatgpt-codex`), not in `~/.codex`.

### Setup

1. Configure a backend with `provider_kind = ChatGptCodex`.
2. Sign in and write the credential document:

   ```sh
   defra-agent codex-login --agent-did did:key:...
   ```

   Add `--device-auth` for headless login, and `--graphql` to write to a running
   node instead of the local home.
3. Verify:

   ```sh
   defra-agent codex-auth-probe --agent-did did:key:...
   ```

### Models

- **Default:** the `chatgpt-codex` preset defaults the model to **`gpt-5.5`**, so
  `defra-agent init --backend-preset chatgpt-codex` works without `--model-name`.
- **Use plain `gpt-5.x` slugs, not `-codex` variants.** A ChatGPT subscription serves
  models like `gpt-5.5`; the `-codex` variants (`gpt-5.2-codex`, …) return
  *"not supported when using Codex with a ChatGPT account"*.
- **List your account's models:**

  ```sh
  defra-agent config backend discover-models \
    --graphql <url> --backend-id <id> --agent-did did:key:...
  ```

  The returned set is what the account can actually use — it is gated server-side by
  plan and by the advertised Codex client version (see below). An empty list usually
  means a stale client version.
- **Change the model:** pass `--model-name <slug>` to `init`, or update the behavior
  with `defra-agent config behavior set --backend-id <id> --model-name <slug>`.
- **Client version gate.** The backend gates model availability on the Codex client
  version defra-agent advertises (currently `0.138.0`, on both the request `version`
  header and the `/models` `client_version` query param). If a newer floor is required,
  set `DEFRA_CHATGPT_CODEX_CLIENT_VERSION` — one knob moves it everywhere.
- **Reasoning effort** is currently fixed at `medium`; per-behavior effort selection
  (e.g. `xhigh`) is tracked in #540.

### Wire-shaping guarantees

The ChatGPT Codex path is stricter than hosted OpenAI Responses in several
places. Regression tests pin these details:

- unsupported top-level params are stripped: `max_output_tokens`, `temperature`,
  `top_p`
- function tools are sent as `strict: false`
- the Codex client version is sourced from one accessor and used for both the
  request `version` header and `/models?client_version=...`
- `Accept: text/event-stream, application/json` is sent
- a missing SSE `Content-Type` is filled as `text/event-stream`, while a
  backend-supplied content type is preserved

### Credential storage

- `defra-agent codex-login` uses Codex's OAuth flow with an ephemeral in-memory
  store, then writes the resulting access token, refresh token, id token, account
  id, plan, FedRAMP flag, and expiry into `OAuthCredential`.
- The runtime reads `OAuthCredential` for the behavior's `agent_did`; it does not
  read `CODEX_HOME`, `DEFRA_CODEX_HOME`, or `~/.codex` for ChatGPT backend auth.
- v1 stores token fields as plaintext document fields, matching the current
  `InferenceBackend.api_key` precedent. Filtered replication must scope the
  credential to the owning `agent_did`; encrypted token fields are the next slice.

### Fleet / remote

- OAuth refresh rotates the refresh token, so only the owner node for the
  `(agent_did, behavior)` should refresh and write the document. Replicas can use
  the current access token and receive the rotated document through replication.
- Owner election across nodes is not yet wired: every runtime currently builds
  the bearer as the owner, so the single-writer guarantee relies on the routing
  model placing each `(agent_did, behavior)` on exactly one deployment. Do not
  replicate an `OAuthCredential` to a second node that also runs the same
  `(agent_did, behavior)` until owner derivation lands (a later slice).
- When replicating credentials, use an agent-scoped filter such as
  `OAuthCredential:agent_did=did:key:...`; do not include `OAuthCredential` in an
  unfiltered config replicator.
- The single-node/local demo path treats the local runtime as the owner.
- A remote frontend (`defra-agent codex`) does not need local ChatGPT credentials;
  the server-side runtime uses the replicated `OAuthCredential` document.

### Token refresh

- The ChatGPT HTTP client asks a `DbCredentialBearer` for a bearer before every
  request. All clients for one `credential_id` share a single bearer (cache and
  refresh lock) per process, so the rotating refresh token has exactly one
  in-process writer.
- If the access token is near expiry and this runtime is the owner, it posts the
  refresh token to OpenAI's token endpoint, writes the rotated tokens back to
  `OAuthCredential`, then sends the request.
- If the provider rejects a live request with 401/403, the bearer is invalidated
  so the next request forces a refresh rather than replaying a clock-fresh but
  server-revoked token. Runtime errors still tell the operator to rerun
  `defra-agent codex-login` when a refresh cannot recover.

### Diagnostics

- `defra-agent codex-auth-probe` reads the credential document (read-only; it
  never refreshes — the owning runtime is the single refresh writer, so a second
  writer would trip the provider's reuse-detection), probes `/models`, and prints
  account, plan, expiry, and reachable models.
- `defra-agent diagnose` reports `checks.chatgpt_auth` as structured JSON with
  `credential_id` and `expires_at`, or an actionable `defra-agent codex-login`
  guidance string when the document is missing or expired.
