# ChatGPT OAuth credentials as DefraDB documents

**Issue:** reworks #509 slice 1 (#339 / PR #530); supersedes the filesystem-auth approach
**Date:** 2026-06-19
**Branch:** `feat/responses-api-finalize-509` (rework #530 in place)
**Status:** Design — pending user review

## Summary

PR #530 finished the ChatGptCodex backend by reading OAuth credentials from the **filesystem**
(`~/.codex` / `DEFRA_CODEX_HOME`) via `codex_login::AuthManager`. That was a "borrow the
developer's laptop creds" shortcut. It is the wrong home for **agent-side** state: the agent
runtime — and every fleet node running it — is what must present the bearer, so the credential
belongs where the agent's control plane lives — **DefraDB**, behind DID identity, document ACL,
and P2P replication. "The database is the control plane" applies to secrets too (the
`InferenceBackend.api_key` field already stores its secret in a document; ChatGptCodex was the
outlier reading from disk).

This rework moves the credential into an `OAuthCredential` **document**. A new
`defra-agent codex-login` command performs the OAuth handshake and writes the doc. The runtime
reads the doc, serves the bearer per request, refreshes near-expiry, and writes the rotated
token **back** to the doc. Filtered replication distributes it to authorized agent nodes.

#530's reusable parts carry over unchanged: the per-request bearer-injection HTTP client
(`ChatGptCodexHttpClient<S: BearerSource>`) and the auth-error classification
(`ChatGptAuthProblem` / `classify_chatgpt_auth_error`). Only the **token source** swaps from
filesystem to DB — the `BearerSource` impl changes from `AuthManagerBearer` to
`DbCredentialBearer`.

## Decisions (locked in brainstorm)

1. **Own the refresh loop (DB-native).** Store `TokenData` in the doc; on near-expiry call the
   raw OAuth token endpoint ourselves and write the rotated token back to the doc. Use
   `codex_login` ONLY for the interactive login flow. No codex fork; aligns with the
   codex/rig-removal direction (#438).
2. **Plaintext doc for v1, encryption as a named fast-follow.** Match the existing `api_key`
   precedent (plaintext field), ship behind filtered-replication scoping, and schedule the
   native KMS/ACP DEK-release-by-DID encryption as the very next slice (§Fast-follow).
3. **Rework PR #530's branch in place.** Keep the reusable plumbing; rebuild the token source.

## Goals

- ChatGPT/Codex OAuth credentials live as `OAuthCredential` DefraDB documents, owned by an
  `agent_did`, never written to the filesystem by defra-agent.
- A native `defra-agent codex-login` command acquires creds (no external Codex CLI required).
- The runtime serves a fresh bearer per request, refreshing near-expiry and persisting the
  rotated token back to the doc.
- Credentials replicate only to authorized nodes (filtered replication by `agent_did` now;
  ACP/encryption next).
- `codex-auth-probe` and `diagnose` report the **document** credential.

## Non-goals (this slice)

- Encryption at rest / native KMS/ACP DEK-release. Explicitly the next slice (§Fast-follow).
- Multi-node concurrent refresh coordination beyond the owner-only rule (§Sharp edge A).
- Non-ChatGPT OAuth providers (the schema is provider-tagged to allow them later).
- Importing an existing `~/.codex` credential (optional convenience, deferred).

## Architecture

### A. Schema — `OAuthCredential` collection

New SDL under `crates/defra-agent-protocol/schemas/` (e.g. `inference/oauth_credential.graphql`),
registered in the schema apply path. Plaintext v1, mirroring `InferenceBackend.api_key`:

```graphql
type OAuthCredential {
    credential_id: String @index(unique: true)
    agent_did: String @index           # owner; @immutable scope key for filtered replication
    provider: String @index            # "chatgpt-codex" (future: other OAuth providers)
    access_token: String               # current bearer (rotates on refresh)
    refresh_token: String              # long-lived; rotates on refresh
    id_token: String                   # raw JWT (source of account/plan/fedramp claims)
    account_id: String                 # denormalized for the ChatGPT-Account-ID header
    chatgpt_plan_type: String          # display
    is_fedramp: Boolean                # denormalized for the X-OpenAI-Fedramp header
    access_token_expires_at: DateTime  # drives near-expiry refresh
    last_refresh: DateTime
    enabled: Boolean @index
}
```

- **One credential per `(agent_did, provider)`.** `credential_id` is derived deterministically
  (e.g. `sha256(agent_did + ":" + provider)` or `"<provider>:<agent_did>"`) so the login
  command upserts rather than duplicates.
- `agent_did` is the `@immutable` filtered-replication scope key, consistent with
  `AgentRequest`/`AgentToolCall` (`tool_call_lifecycle.rs`).
- Protocol row type `OAuthCredentialRow` added to `defra-agent-protocol/src/row.rs` (alongside
  `InferenceBackendRow`).

### B. `defra-agent codex-login` command (acquisition)

New CLI command (`crates/defra-agent-cli/src/commands/codex_login.rs`), dispatched in `main.rs`
alongside `CodexAuthProbe`, with `CodexLoginArgs` (mirroring `CodexAuthProbeArgs`):
`--device-auth` (headless device-code), `--provider` (default `chatgpt-codex`),
`--agent-did` (default the local principal's DID), `--graphql` (target node), `--issuer` /
`--client-id` (optional overrides).

Flow — login WITHOUT touching the filesystem:
1. Build `ServerOptions::new(synthetic_home, CLIENT_ID.to_string(), None,
   AuthCredentialsStoreMode::Ephemeral)` — Ephemeral keeps tokens in an in-memory store, never
   on disk. `synthetic_home` is a unique in-memory key, not a real path.
2. Run the **public** login flow: `run_login_server(opts)` (browser/PKCE loopback, prints
   `auth_url`, blocks on `block_until_done()`), or `run_device_code_login(opts)` with
   `open_browser=false` when `--device-auth`.
3. Read the result back: construct `AuthManager` over the same Ephemeral home, `auth().await`,
   then `CodexAuth::get_token_data()` → full `TokenData` (access/refresh/id token + account_id).
4. Derive `access_token_expires_at`, `chatgpt_plan_type`, `is_fedramp` from the id-token JWT
   (the shared decode helper from §C).
5. Upsert the `OAuthCredential` doc (GraphQL mutation; `graphql::escape_graphql_string()` for
   every interpolated value; emit `null`, never `[]`).
6. Drop the Ephemeral store (process memory) — nothing persists to disk.

### C. Own the refresh primitive

New module `crates/defra-agent/src/chatgpt_oauth_refresh.rs` (~50 LOC, no codex fork):

```rust
pub struct RefreshedTokens {
    pub access_token: String,
    pub refresh_token: String,   // rotated
    pub id_token: Option<String>,
    pub account_id: Option<String>,
    pub is_fedramp: bool,
    pub plan_type: Option<String>,
    pub access_token_expires_at: chrono::DateTime<chrono::Utc>,
}

/// POST {refresh endpoint} { client_id, grant_type: "refresh_token", refresh_token }.
pub async fn refresh_chatgpt_token(refresh_token: &str, http: &reqwest::Client)
    -> Result<RefreshedTokens, ChatGptAuthProblem>;

/// Decode a ChatGPT id-token JWT payload for exp / account_id / fedramp / plan.
pub fn decode_id_token_claims(id_token: &str) -> IdTokenClaims;
```

- Endpoint: `https://auth.openai.com/oauth/token`, honoring
  `codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR` (`CODEX_REFRESH_TOKEN_URL_OVERRIDE`).
- `client_id`: `codex_login::CLIENT_ID` (public const — do not hardcode the literal).
- Error mapping: HTTP 401 → `ChatGptAuthProblem::Expired`; other non-success → `Other`;
  transient/network → `Other` (retryable by the caller's next request).
- JWT decode: base64url-decode the payload segment, read `exp`, `chatgpt_account_id`,
  `chatgpt_account_is_fedramp`, `chatgpt_plan_type`. No signature verification (the token is
  already provider-issued; we only read claims we then send back to the same provider).

### D. DB-backed `BearerSource` (runtime consumption)

Keep `ChatGptCodexHttpClient<S: BearerSource>`, `BearerSource`, the per-request injection on
`send`/`send_streaming`/`send_multipart`, and the fail-closed `prepare()` from #530 — all
unchanged. Replace the `AuthManagerBearer` impl with:

```rust
pub struct DbCredentialBearer {
    db: <graphql query+mutation handle>,
    credential_id: String,
    http: reqwest::Client,
    cache: tokio::sync::Mutex<Option<CachedToken>>, // access_token + expiry, lazily loaded
    refresh_lock: tokio::sync::Mutex<()>,           // serialize refresh within this process
    is_owner: bool,                                 // see Sharp edge A
}

impl BearerSource for DbCredentialBearer {
    async fn current_bearer(&self) -> Result<String> {
        // 1. ensure cache loaded from the OAuthCredential doc
        // 2. if access_token not within REFRESH_SKEW of expiry -> return it
        // 3. else take refresh_lock:
        //    - re-read the doc (another node/turn may have refreshed)
        //    - if still near-expiry AND is_owner -> refresh_chatgpt_token(refresh_token),
        //      then mutate the doc with rotated tokens + new expiry/last_refresh, update cache
        //    - if near-expiry AND NOT is_owner -> return current token; if the provider 401s,
        //      surface "credential is refreshing on its owner node; retry"
        //    - on Expired -> error: "re-run `defra-agent codex-login`"
    }
}
```

- `build_responses_client` changes: instead of `resolve_chatgpt_auth` (filesystem), resolve the
  `OAuthCredential` doc for the behavior's `agent_did` + `provider="chatgpt-codex"`, build
  `DbCredentialBearer`, and build the headers from the doc fields.
- `build_chatgpt_codex_headers` refactors to take plain inputs (`account_id: Option<&str>`,
  `is_fedramp: bool`) instead of a `CodexAuth` — sourced from the doc.
- Header injection still overwrites `authorization` per request (the placeholder-key-never-on-
  wire property #530's review verified stays intact).

### E. `codex-auth-probe` / `diagnose` read the doc

- `codex-auth-probe` resolves the `OAuthCredential` doc (by `--agent-did`/principal + provider)
  instead of `resolve_chatgpt_auth`; reports account/plan/expiry from the doc and probes
  `/models` with the doc's access token (refreshing first if near-expiry).
- `diagnose`'s `checks.chatgpt_auth` reads the doc (no filesystem `AuthManager`), removing #530's
  unconditional-filesystem-refresh side-effect; reports `{ok, credential_id, expires_at}` or
  `{ok:false, guidance}` when absent/expired.

### F. What #530's filesystem code becomes

- **Removed:** `resolve_chatgpt_auth`, `AuthManagerBearer`, `load_chatgpt_auth`,
  `load_default_chatgpt_auth`, the `codex_login::AuthManager` runtime dependency for
  serving/refreshing. (`codex_login` is still depended on — only for the login flow in §B.)
- **Kept:** `ChatGptCodexHttpClient<S>` + `BearerSource` + send-path injection + `prepare()` +
  body-patching (`patch_instructions_body`) + `ChatGptAuthProblem`/`classify_chatgpt_auth_error`
  (Missing/WrongMode/Expired guidance now points at `defra-agent codex-login`).
- `backend_provider.rs` model-discovery (`/models`) path: uses the doc's token instead of
  `load_default_chatgpt_auth`.
- `docs/backends.md`: rewritten for the DB model (login → doc → server → chat; no `~/.codex`).

## Sharp edges (must be honored)

### A. Refresh-token rotation is a single-writer problem

OAuth refresh **rotates** the refresh token. If two nodes refresh concurrently with the same old
refresh token, the provider's **reuse detection revokes the credential** (codex classifies this
as `refresh_token_reused`). Therefore:

- **Only the credential's owning node refreshes and writes the doc back.** Replicas serve the
  current token and rely on replication to receive the rotated one. `DbCredentialBearer.is_owner`
  gates the refresh+mutate branch.
- Ownership = the deployment that owns this `(agent_did, behavior)` (consistent with the
  one-deployment-per-(did,behavior) routing model). For the single-node demo, the local node is
  always owner.
- Within one process, `refresh_lock` serializes concurrent turns; across nodes, the owner rule
  prevents reuse-revocation. This constraint is stated in `docs/backends.md` and asserted in the
  refresh path (a non-owner never calls `refresh_chatgpt_token`).

### B. The exposure delta is replication, not plaintext-at-rest

Plaintext-at-rest is **parity with the status quo**: `~/.codex/auth.json` already stores the same
access + refresh tokens unencrypted (just `0600`, single-machine). So storing them plaintext in
the doc is not a regression in *at-rest* posture. The genuinely new surface is that the credential
now lives in a **replicating** store and can leave the originating machine. The v1 bar is therefore
"no worse than `~/.codex`":
- **Don't broadcast it.** `OAuthCredential` replicates ONLY to its `agent_did` via filtered
  replication from day one — never an open broadcast. On a single-node demo it never leaves the box.
- **Don't let it escape sideways.** The token must not reach logs, traces, the rendered-request
  projection, config export/import, or unguarded GraphQL reads (the `defra_query` field guard).
  Those are leaks *beyond* the accepted at-rest plaintext and are in scope to prevent now.

Encryption is a **named next slice** (§Fast-follow) precisely because it addresses the *distribution*
surface — on-wire confidentiality + at-rest-on-peer + DID-gated decryption — which is what changes
once the secret replicates. It is not fixing a plaintext regression (there isn't one). The field set
is designed so the token columns can become ciphertext without a schema reshape.

## Fast-follow (next slice, not this one)

Native at-rest + on-wire encryption: store the token columns as DefraDB-encrypted fields with
KMS **DEK-release gated by the requester's DID through an ACP policy** (the binding's KMS/ACP
already implements ECIES key-release-by-DID; defra-agent has no precedent yet, and the
`@encryptedIndex` directive is parse-only — so this is a spike with its own spec). Outcome: the
credential replicates but only ACP-authorized DIDs can decrypt it.

## Testing

- **`chatgpt_oauth_refresh`:** unit-test request shaping (client_id/grant_type/refresh_token body,
  endpoint + override env) and `decode_id_token_claims` against a hand-built JWT (exp/account/
  fedramp/plan) — mock HTTP, no network.
- **`DbCredentialBearer`:** against a test node / mock DB — (1) fresh token returned without
  refresh; (2) near-expiry triggers refresh, doc mutated, cache updated; (3) non-owner never
  refreshes; (4) permanent 401 → actionable "re-run codex-login" error. Reuse #530's
  `injects_fresh_bearer_on_each_request` for the injection layer (now over `DbCredentialBearer`).
- **`codex-login`:** the interactive OAuth itself is not unit-testable; test the
  TokenData→doc-upsert step with an injected `TokenData` (Ephemeral) and assert the resulting
  `OAuthCredential` fields (including derived expiry/fedramp).
- **`diagnose`/`probe`:** doc-present and doc-absent cases; `diagnose` stays valid JSON / exit 0.
- The #509 replay-fixture harness covers the live wire shape in its own slice.

## Foundation note (Lean)

This changes the credential **resolution path** (filesystem → document) but not what transitions
are legal, what invariants hold, or what the model is fed — it is auth plumbing. v1 defers
encryption, so there is no crypto invariant to discharge yet. No new theorems. (The owner-only-
refresh property and, later, the ACP decryption-authorization property are candidates to model
when the encryption slice lands.)

## Open questions / risks

- **Ownership signal.** How a `DbCredentialBearer` learns it is the owning node needs a concrete
  source (deployment doc for the `(agent_did, behavior)` vs a simpler "local node created the
  credential" flag). Resolve in planning; default for single-node demo = owner.
- **Refresh write-back vs replication lag.** A replica may serve a soon-to-rotate token briefly
  after the owner refreshes; the per-request 401 path + retry covers the window. Acceptable for
  v1; revisit with the encryption/fleet slice.
- **`get_token_data()` availability.** Confirm `CodexAuth::get_token_data() -> Result<TokenData>`
  is `pub` in the pinned codex_login (observed at `auth/manager.rs:313`); if not, extract via the
  Ephemeral store's `AuthDotJson` directly.
