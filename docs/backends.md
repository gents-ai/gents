# Backends

> This page is the home for the committed backend support matrix (#509). It
> starts with the ChatGPT-subscription (OAuth) backend; provider rows are added
> as #509 lands each one.

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
- When replicating credentials, use an agent-scoped filter such as
  `OAuthCredential:agent_did=did:key:...`; do not include `OAuthCredential` in an
  unfiltered config replicator.
- The single-node/local demo path treats the local runtime as the owner.
- A remote frontend (`defra-agent codex`) does not need local ChatGPT credentials;
  the server-side runtime uses the replicated `OAuthCredential` document.

### Token refresh

- The ChatGPT HTTP client asks a `DbCredentialBearer` for a bearer before every
  request.
- If the access token is near expiry and this runtime is the owner, it posts the
  refresh token to OpenAI's token endpoint, writes the rotated tokens back to
  `OAuthCredential`, then sends the request.
- If the provider rejects a token with 401/403, `codex-auth-probe` and runtime
  errors tell the operator to rerun `defra-agent codex-login`.

### Diagnostics

- `defra-agent codex-auth-probe` reads the credential document, refreshes it if
  needed, probes `/models`, and prints account, plan, expiry, and reachable
  models.
- `defra-agent diagnose` reports `checks.chatgpt_auth` as structured JSON with
  `credential_id` and `expires_at`, or an actionable `defra-agent codex-login`
  guidance string when the document is missing or expired.
