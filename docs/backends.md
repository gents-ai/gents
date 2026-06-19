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
