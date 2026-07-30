# Spike: Grok / xAI subscription OAuth (#973)

Pinned facts for the Grok OAuth vertical. Sources: live OIDC discovery at
`https://auth.x.ai/.well-known/openid-configuration` (fetched 2026-07-30),
public Grok CLI / peer-CLI docs (shunt M6, Hermes, OpenClaw, pi-xai-oauth),
and issue #973. **No secrets.** The only client identifier below is the
documented public Grok CLI OAuth client (no client secret; `token_endpoint_auth_methods_supported` includes `none`).

## Resolved open questions

| # | Question | Decision |
| --- | --- | --- |
| 1 | Provider kind name | **`XaiGrokOAuth`** with serde aliases `xai-oauth`, `grok-oauth`, `xai-grok-oauth`. Credential `provider` string: **`xai-oauth`**. |
| 2 | Subscription base URL | **`https://cli-chat-proxy.grok.com/v1`**. Same SuperGrok bearer against `https://api.x.ai/v1` is commonly **402** (`personal-team-blocked:spending-limit`) / **403** tier gate. API-key path remains `OpenAiCompatible` → `api.x.ai`. |
| 3 | Public `client_id` | **`b1a00492-073a-47ea-816f-4c329264a828`** — public Grok CLI client used across Hermes / OpenCode / shunt / peer tools. **Provenance for review:** community-documented public OAuth client for Grok Build CLI; no secret. Flag in PR if legal wants a first-party registered client later. |
| 4 | Default model | **`grok-4.5`** (subscription coding default; catalog also has `grok-build-0.1`). Probe lists live `/models-v2` ids. |
| 5 | Desktop events | **Parallel** `desktop_grok_login` / `desktop://grok-login-url` (mirror Codex; no shared event rename in v1). |
| 6 | Schema | **No migration.** Leave `chatgpt_plan_type` / `is_fedramp` null/false for Grok. |
| 7 | API-key preset | **Out of v1.** Document custom `OpenAiCompatible` + `https://api.x.ai/v1` + `XAI_API_KEY`. Optional later one-liner. |

## Auth endpoints (OIDC discovery)

| Constant | Value |
| --- | --- |
| Issuer | `https://auth.x.ai` |
| Authorization | `https://auth.x.ai/oauth2/authorize` |
| Device authorization | `https://auth.x.ai/oauth2/device/code` |
| Token | `https://auth.x.ai/oauth2/token` |
| Userinfo | `https://auth.x.ai/oauth2/userinfo` |
| JWKS | `https://auth.x.ai/.well-known/jwks.json` |
| Code challenge | `S256` |
| Grants | `authorization_code`, `refresh_token`, `urn:ietf:params:oauth:grant-type:device_code` |

### Scopes (request)

```text
openid profile email offline_access grok-cli:access api:access conversations:read conversations:write
```

Discovery also lists `team:read`, `org:read`, `grok-plugins:access`, `workspaces:read`, `workspaces:write` — not required for v1 chat.

### Device-code flow (redacted shapes)

**1. Request device code**

```http
POST /oauth2/device/code HTTP/1.1
Host: auth.x.ai
Content-Type: application/x-www-form-urlencoded
Accept: application/json

client_id=b1a00492-073a-47ea-816f-4c329264a828&scope=openid+profile+email+offline_access+...
```

**2. Response (fields)**

```json
{
  "device_code": "<opaque>",
  "user_code": "XXXX-XXXX",
  "verification_uri": "https://accounts.x.ai/...",
  "verification_uri_complete": "https://accounts.x.ai/...?user_code=...",
  "expires_in": 1800,
  "interval": 5
}
```

**3. Poll token**

```http
POST /oauth2/token HTTP/1.1
Host: auth.x.ai
Content-Type: application/x-www-form-urlencoded
Accept: application/json

grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code
&client_id=b1a00492-073a-47ea-816f-4c329264a828
&device_code=<opaque>
```

Pending: `error=authorization_pending` / `slow_down`. Terminal: `access_denied`, `expired_token`, etc.

**4. Success token response (fields)**

```json
{
  "access_token": "<jwt>",
  "refresh_token": "<opaque>",
  "id_token": "<jwt optional>",
  "token_type": "Bearer",
  "expires_in": 900
}
```

Access-token lifetime is often short (~15m). Expiry is taken from JWT `exp` (unverified decode; same trust model as Codex — TLS token endpoint).

### Refresh (redacted)

```http
POST /oauth2/token HTTP/1.1
Host: auth.x.ai
Content-Type: application/x-www-form-urlencoded
Accept: application/json

grant_type=refresh_token
&client_id=b1a00492-073a-47ea-816f-4c329264a828
&refresh_token=<opaque>
```

**Refresh token rotates** on every successful refresh. Owner-only single-writer refresh is required (same as Codex). If the response omits `refresh_token`, treat as invalid and do not persist a half-rotated pair.

### Browser (authorization code + PKCE)

Supported by discovery (`authorization_endpoint` + `S256`). v1 CLI/desktop can ship **device-code first** (works on SSH/VPS without loopback); browser loopback is optional parity with `codex-login` when a registered redirect is available for the public client. Device-code is the headless path (`--device-auth` / default for remote).

## Inference for OAuth bearers

| Surface | URL | Notes |
| --- | --- | --- |
| Subscription (this feature) | `https://cli-chat-proxy.grok.com/v1` | Serves **both** Responses (`POST …/responses`) and Chat Completions (`POST …/chat/completions`); the official client selects per model via the `/models-v2` `apiBackend` field (its compiled default is chat_completions; the newer pager client uses responses). Gents defaults to Responses and honors `openai_wire: chat_completions`. |
| API key (existing) | `https://api.x.ai/v1` | Metered; not the OAuth default. |

### Required subscription proxy headers

Source-verified against `xai-org/grok-build` (`inject_proxy_headers`, storage
client identity docs):

```http
Authorization: Bearer <access_token>
Accept: text/event-stream, application/json
x-xai-token-auth: xai-grok-cli
x-authenticateresponse: authenticate-response
x-grok-client-identifier: grok-shell
x-grok-client-version: 0.2.93
User-Agent: xai-grok-cli
```

`x-xai-token-auth` and `x-authenticateresponse` are required by the proxy's
auth middleware; `x-grok-client-version` feeds a version-gate check (env
override: `GENTS_XAI_GROK_CLIENT_VERSION`). `grok-shell` is one of the
documented client identifiers (`grok-shell` / `grok-pager` / `grok-desktop` /
`grok-extension`). Without these the proxy often answers like an unentitled
API client (**402** / **426**).

Prefer **minimal** wire shaping vs Codex: no `ChatGPT-Account-ID`, no FedRAMP header, no Codex `version` header, no forced `strict: false` tool rewrite. Set `store: false` when absent if the proxy requires it (peer tools do).

### Model discovery

- `GET {endpoint}/models-v2` — the official Grok CLI catalog path. Shape:
  `{"data":[{"model":…,"modelId":…,"name":…,"contextWindow":…,"apiBackend":…}]}`
  (`id`, when present, is a catalog row id — identify models by `model` /
  `modelId`). Gents' discovery and `grok-auth-probe` parse this shape and
  tolerate OpenAI-style `data[].id` / slug-style entries as fallbacks.
- Default preset model: `grok-4.5`.

## Source verification (2026-07-30)

Checked against local checkouts of the official Grok CLI
(`xai-org/grok-build`) and `nousresearch/hermes-agent`:

- **client_id `b1a00492-…`** appears in grok-build's own test fixtures paired
  with issuer `https://auth.x.ai` — first-party confirmation of the public
  client.
- **Scopes** match grok-build's `default_oauth2_scopes()` verbatim.
- **Device-code flow** is first-party (`auth/device_code.rs`); servers can
  disable it per-deployment (`DeviceCodeError::NotEnabled`).
- Hermes takes a different route (browser PKCE loopback against `api.x.ai`
  with fewer scopes) — not the model for this provider; grok-build is.

## Eligibility / error surface

| Signal | Meaning | Operator guidance |
| --- | --- | --- |
| Refresh / inference **401** | Expired / revoked grant | Re-run `gents grok-login` |
| Refresh **400** `invalid_grant` | Consumed/rotated refresh race or revoke | Re-login |
| Refresh / inference **403** (permission) | Tier not entitled to OAuth API | Not fixed by re-login; use `XAI_API_KEY` + `api.x.ai` or upgrade SuperGrok |
| Inference **402** on `api.x.ai` with subscription bearer | Wrong base URL for subscription | Use `cli-chat-proxy.grok.com` |
| Inference **402/426** on proxy without identity headers | Client not recognized as CLI | Send Grok-CLI headers |

## Credential document

Reuse `OAuthCredential` as-is:

- `provider = "xai-oauth"`
- `credential_id = "xai-oauth:{agent_did}"`
- `chatgpt_plan_type` / `is_fedramp` unused (null / false)
- `account_id` optional if claims ever expose a team/account id; v1 may leave null

## Implementation map (post-spike)

1. Shared shell: `gents::oauth_credential` (CRUD + `DbCredentialBearer` + pluggable refresh).
2. `xai_oauth_refresh` + `xai_oauth_login` (device-code; browser if low-cost).
3. `xai_grok_oauth` + `BackendProviderKind::XaiGrokOAuth`.
4. CLI `grok-login` / `grok-auth-probe`, preset, diagnose, init.
5. Desktop parallel login + wizard card.
6. `docs/backends.md` matrix row.

## Non-goals (reaffirmed)

No secrets in repo; no Grok Build binary dependency; no media stack; no encrypted token fields; no multi-node refresh election.
