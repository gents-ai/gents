# #1533 design note — MCP OAuth discovery + headless consent (analysis and test plan)

Status: **analysis only** — no production source was modified. This note was authored
against `23434bfffc47`, then reviewed and rebased onto `047a4ab9918c` (`origin/main` on
2026-09-18). The rebase did not turn any proposed interface below into implemented
behavior.
Companion work: #1532 (`feat/1532-mcp-bearer-auth` worktree) is concurrently building the
operator-managed Bearer credential foundation; this note identifies the shared seams so
neither issue builds a competing credential system.

Turn 2 (§8–§9): full prior-art reuse audit of `oauth_http.rs`, `xai_oauth_login.rs`,
`chatgpt_oauth_refresh.rs`, `claude_oauth_refresh.rs`, both login crates, CLI/desktop
login paths, DID/ACP/P2P filtering, with a generalizable-vs-provider-specific split and
the exact shared-interface recommendation for #1532. Still design-only until coordinated.

Evidence base: repo checkout at `23434bffc`, rechecked after rebasing to `047a4ab99`;
rmcp 1.3.0 source under
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rmcp-1.3.0/` (cited inline as
`rmcp:…`); issue bodies #1532 and #1533 read via `gh issue view`.

---

## 1. Current state at `23434bffc` (evidence-checked)

### 1.1 Canonical MCP registry — no auth surface

`crates/gents/src/document_config/installation.rs:7-56` — `ToolServiceRegistry` carries
`service_id`, `agent_did`, display fields, hostname/tailscale_ip/lan_ip, `mcp_port`,
`mcp_path`, `send_agent_did`, `enabled`, `tags`. **No auth selection, no credential
reference.** The persisted schema
(`crates/gents-protocol/schemas/services/tool_service_registry.graphql`) additionally has
`status`/`version`/`updated_at` fields the canonical Lean vocabulary does not model
(`ConfigDocuments.lean:66` lists only the 13 canonical fields).

Validation owner: `crates/gents/src/document_config/installation_validation.rs` requires
positive `mcp_port` and one address field. Nothing validates auth.

### 1.2 Transport — Bearer header only, no challenge handling

`crates/gents/src/mcp_pool.rs:106-124` — `streamable_http_transport_config` injects
optional `x-agent-did` (L24) + trace-context custom headers. `rmcp 1.3.0`
`StreamableHttpClientTransportConfig` additionally exposes `auth_header: Option<String>`
("bearer token without the `Bearer ` prefix", `rmcp:streamable_http_client.rs:1061-1092`),
which mcp_pool does not set today.

Important rmcp behavior verified in the pinned source
(`rmcp:transport/common/reqwest/streamable_http_client.rs:130-167`):
- HTTP 401 **with** a parseable `WWW-Authenticate` header → `StreamableHttpError::AuthRequired(AuthRequiredError { www_authenticate_header })`.
- HTTP 403 with `WWW-Authenticate` → `StreamableHttpError::InsufficientScope { required_scope }`.
- 401 **without** the header, or unparseable header values, fall through to
  `error_for_status()` — a bare 401 surfaces as an opaque reqwest status error, not
  `AuthRequired`.

So the challenge hook already exists at the transport layer; the gap is that Gents
(a) never sets `auth_header`, and (b) never classifies `AuthRequired` /
`InsufficientScope` in `mcp_pool.rs` or `health_checker.rs` (a 401 today produces a
generic connect failure → parking strikes, not an actionable auth diagnosis).

### 1.3 Credential owner — `OAuthCredential` documents (shared with #1532's Bearer plan)

- Schema: `crates/gents-protocol/schemas/inference/oauth_credential.graphql` — keyed by
  `credential_id` (unique), `agent_did` (`@index @immutable`), `provider`, raw
  `access_token`/`refresh_token`/`id_token`, expiry, `enabled`.
- Row + decode: `crates/gents-protocol/src/row.rs:476` (`OAuthCredentialRow`),
  `gents/src/oauth_credential.rs:490-513` (`oauth_credential_from_value`).
- CRUD: `oauth_credential.rs:202-395` — `lookup_oauth_credential(node, agent_did, provider)`,
  `lookup_oauth_credential_by_id`, `list_oauth_credentials[_on]`, `upsert_oauth_credential[_on]`,
  all through `crate::config_client::ConfigAccess::write_local` / `access.write` and
  `gents_protocol::graphql::extract_mutation_doc_id`. `agent_did` is immutable post-add
  (update branch omits it, L236-258).
- Secret-safe JSON: `claude_login_result_json` / `codex_login_result_json` redact tokens
  as `"<redacted>"` (`claude_login.rs:117-122`, `codex_login.rs:119-121`); the debug
  redaction pattern is `inference_backend.rs:122` (`"[redacted]"`).
- Refresh single-flight: `DbCredentialBearer` (`oauth_credential.rs:596-826`) — cache +
  `refresh_lock` + 60 s failure cooldown + `force_refresh` invalidation; refresh-kind
  dispatch `OAuthRefreshKind::{ChatGpt,Claude,Xai}` at L722-737. BearerSource trait at
  L588-594. Shared registry `shared_bearer` (L573-586) keys by `credential_id`.
- Bearer-injection wrapper: `oauth_http.rs::BearerAuthHttpClient` (L117+) with
  `OAuthHttpPolicy::REJECTION_STATUSES` — the existing owner of "which status means the
  bearer is dead" (Codex 401+403, xAI 401-only, with the documented rotation rationale
  at L37-46).
- Bootstrap: `bootstrap_oauth_client(node, agent_did, provider, kind, product)` →
  `(Arc<DbCredentialBearer>, OAuthCredential)` (`oauth_http.rs:294+`), used by
  `chatgpt_codex.rs:401`, `claude_subscription.rs:63`, `xai_grok_oauth.rs:307`,
  `backend_health.rs:244`.

**Gaps vs. MCP OAuth (this issue):**
1. `provider` is a free string; there is no per-`agent_did`+`service` scoping —
   `oauth_credential_id(agent_did, provider)` is `(provider, agent_did)`-keyed only. Two
   different MCP services with the same issuer would collide unless provider carries the
   service binding (e.g. `mcp:<service_id>`).
2. No resource/audience (`aud`) or issuer binding is persisted; `refresh_token` binding
   to the intended AS is implicit in the provider enum.
3. No `resource` parameter persistence for RFC 8707 audience binding.
4. `OAuthRefreshKind` is a closed enum — MCP services need a dynamic, discovered-refresh
   kind (issuer-specific token endpoint from metadata), not a fourth static variant.
5. No consent/pending-authorization state; a credential row is created only after a
   completed exchange.
6. Redaction exists ad hoc per command (`<redacted>` literals) rather than as a shared
   helper the new MCP surfaces can reuse (rows flowing through desktop views and config
   exports need one canonical redaction owner).

### 1.4 Existing browser-login surfaces (loopback-only, first-party only)

- `gents-chatgpt-login` (`lib.rs:92-260`): loopback callback server, hard-bound to
  `127.0.0.1:{1455,1457}` (`bind_server`, L270-284), fixed `/auth/callback` path, PKCE
  S256 (`generate_pkce` L292-302), random `state` (`generate_state` L304+), state
  comparison then code exchange (`handle_callback_request` L187-264), shared
  `gents-login-ui::response` CSP'd HTML response (`gents-login-ui/src/lib.rs:81-96`,
  `no-store`/`no-referrer`/`default-src 'none'`, HTML-escaped messages, tested).
- `gents-claude-login` (`lib.rs:100+`): same shape at `/callback`, plus
  `run_manual_login` (paste-code flow against the pinned manual redirect page).
- `xai_oauth_login.rs`: device-code flow (`DEVICE_CODE_URL` L18) — the existing SSH/VPS
  precedent for non-loopback login.
- CLI: `codex_login.rs`, `claude_login.rs`, `grok_login.rs` each resolve
  `(ConfigAccess, home)`, resolve `agent_did`, run the flow, persist via
  `access.write("cli.<flow>.credential", mutation)`, print redacted JSON.
- Desktop: `bridge/tauri_commands/inference_setup.rs:391+` and `:820` run the same
  servers in-process, emit the URL to the UI, block with timeout + cancel handle, then
  persist via `core.operator_access(agent_did)` → `upsert_oauth_credential_on`.

All of these are **first-party fixed issuer** flows. None of them: discover issuer
metadata, perform dynamic client registration, handle a resource parameter, or support a
redirect URI other than the machine-local loopback. The MCP consent problem is a new
shape: issuer is per-service (discovered), redirect URI must be operator-owned
(SSH-forwarded or desktop-bound), and the runtime doing the exchange may not be the
machine where the browser runs.

### 1.5 Principal/service binding and health

- Pool scoping: `McpPool::for_agent(agent_did)` (`mcp_pool.rs:291-295`) + `ParkKey`
  including `owner_agent_did`, `service_id`, `endpoint`, `agent_did_header`
  (L208-231). Connections are already partitioned per principal × service × endpoint —
  the natural binding point for authenticated connections (a credential change must
  partition or evict like `agent_did_changed`/`endpoint_changed` do today, L631-655).
- Registry read: `registry.rs:30-80 configured_mcp_services(node, agent_did)` —
  principal-scoped, fail-closed on duplicates/missing owner.
- Health: `health_checker.rs:28-293` (`McpHealthCheckService`), K-model in Lean
  `Proofs/MCPHealth/{State,Transition}.lean` (`healthy/degraded/evicted/reconnecting`,
  probeSuccess/probeFail/backoffExpiry/registryAbsent events), persisted as
  `ToolServiceHealthState` (`tool_service_health_state.graphql`), projected by
  `ToolServiceHealthState::project` (`gents-protocol/src/tool_service_health.rs:35`,
  healthy/stale/unreachable) — **the single classification owner**, consumed verbatim by
  the desktop (`mcpHealthModel.ts` classifies against `displayState` only).
- Diagnostics: `meta_tools/shared.rs:15-27` (`MetaToolContext`, allowed-service filter),
  `meta_tools/discover.rs:58` surfaces configured services to agents; CLI
  `gents mcp probe` (`commands/mcp.rs:116+`) and desktop
  `desktop_list_mcp_services_with_health` / `desktop_probe_mcp_service`
  (`bridge/commands/mcp_health.rs:28,125`, routes at
  `apps/gents-desktop/src-tauri/src/bin/bridge_runner/http/routes.rs:331-343`).

**Gap:** health states carry no *cause vocabulary for authentication*. A service that
exists but requires consent, or whose token expired, is indistinguishable from a
network-down service today (`apply_probe_failure` with a free-text reason). #1533's
acceptance item 6 ("a service requiring OAuth is never reported as working solely
because its registry document exists") needs the auth state to enter this pipeline as a
first-class observation.

### 1.6 Self-config guard (already permits OAuth *references*, never secrets)

Lean `Proofs/SelfConfig/Auth.lean`: `authPatchAllowed` permits `.environment` and
`.principalOAuth` edits, blocks any new `.apiKey` (`backend_step_cannot_set_new_raw_key`).
Rust mirror: `document_config/inference_backend.rs:95-116` `BackendAuth` tagged enum with
`Unauthenticated | ApiKey{key} | Environment{variable} | PrincipalOAuth` and a redacted
Debug impl. **The canonical pattern for MCP:** an auth *selection* on the service
document that references a credential, never holding the secret. `PrincipalOAuth` is
resolved at execution via `bootstrap_oauth_client`-style lookup
(`completion_factory.rs:439` maps it to the no-key path;
`backend_health.rs:215-260` refreshes through the credential owner during probes).

### 1.7 Existing test infrastructure to reuse

- Mock MCP server: `mcp_pool/tests.rs:432-520` raw-TCP header-capture servers
  (`spawn_header_capture_mcp_server`, `spawn_session_tracking_mcp_server`); support
  axum `mock_endpoint.rs` (recorded `HttpRequestData`).
- Credential fixtures: `sample_credential()` (`oauth_credential.rs:834-850`).
- Redaction fence: `e2e_runtime/provider_fixture_redaction.rs` scans committed fixtures
  for token patterns and unredacted query params — a ready-made enforcement point to
  extend to MCP OAuth fixtures.
- Conformance fence: `tests/conformance/docs.rs` (`grounding_doc_paths_resolve`,
  `proofs_contain_no_sorrys`, rig-allowlist); `conformance/mcp_health.rs` pins
  Lean-emitted transition rows to the Rust projection.
- Live fixture host: `apps/fixture-host/src-tauri` exists but serves fixture replay, not
  an OAuth-protected MCP server; a new in-repo fixture is needed for acceptance item 1.

---

## 2. Proposed canonical interfaces (for parent review — not yet implemented)

Everything below extends existing owners; no parallel identity/auth system.

### 2.1 Service auth selection on `ToolServiceRegistry` (owner: document_config)

Mirror `BackendAuth` tagging discipline as a nested value on the service document
(nested settings within Tools/installation documents, per AGENTS.md "tool groups are
nested settings"):

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolServiceAuth {
    /// Default today; preserves backward compatibility (absent field == no auth).
    None,
    /// Operator-managed static token — #1532's Bearer foundation lands here.
    Bearer,
    /// MCP OAuth (this issue). All fields are references/metadata, never secrets.
    OAuth {
        /// Requested scopes (advisory; server may grant fewer — record granted set).
        scopes: Vec<String>,
    },
}
```

Schema (`tool_service_registry.graphql`): `auth: Json` (or flattened typed columns if
Lean's ConfigDocuments vocabulary prefers flat fields — resolve against #1430's
canonical field lists before implementation; the Lean catalog currently enumerates 13
fields and would gain the auth selection in the same change).

`ToolServiceRegistry::validate()` treats both auth variants as markers/references, not
authority to choose arbitrary credential or audience values. For OAuth, the credential
provider is derived as `mcp:<service_id>` and the RFC 8707 resource is the canonical
selected service URI; protected-resource metadata must report that same resource. A
static Bearer selection likewise carries no arbitrary credential id: the
credential owner derives `mcp-bearer:<service_id>` from the validated service id and
the current principal, then verifies that the configured service endpoint still matches
the credential's approved destination before releasing a token. This prevents a caller
from selecting another principal's or service's credential by reference.

### 2.2 Credential provider key convention (owner: oauth_credential)

`oauth_credential_id(agent_did, provider)` already provides uniqueness; MCP adds the
provider-key convention `mcp:<service_id>` so `(agent_did, "mcp:web")` is exactly one
credential row per principal per service. `OAuthCredential` gains optional columns:

| field | purpose |
| --- | --- |
| `issuer` | Discovered AS issuer; refresh binds to it (re-verified at refresh time). |
| `resource` | RFC 8707 resource value used at authorization; recorded for audience audit. |
| `granted_scopes` | Scopes the AS actually granted (space-joined string like Claude's). |
| `client_registration` | Public `client_id` metadata only in the first slice; confidential-client secrets/DCR results are deferred until their credential storage, ACP, redaction, and rotation owner is specified. |

(Refresh-rotation-safety note: new columns must be additive/optional so old rows still
decode — `OAuthCredentialRow` derives all-Option fields, and
`oauth_credential_from_value` must default them, matching the existing `unwrap_or(false)`
discipline.)

### 2.3 Refresh seam — dynamic issuer, same single-flight owner

Do **not** add `OAuthRefreshKind::Mcp` (a closed enum cannot hold per-service issuer
data). Instead extend the dispatch with one dynamically-parameterized variant path:

```rust
pub enum OAuthRefreshKind {
    ChatGpt, Claude, Xai,
    /// Discovered AS metadata; endpoint/config owner is the credential row itself.
    Mcp { token_endpoint: String, client_auth: McpClientAuth },
}
```

`DbCredentialBearer::refresh_tokens` routes `Mcp` through a new
`mcp_oauth_refresh::refresh_mcp_token` that (a) re-validates the token endpoint URL
against the persisted `issuer` (HTTPS, exact host — see §2.5), (b) posts the standard
refresh grant with the same canonical `resource`, (c) cryptographically verifies signed
JWT access-token `iss`/`aud` claims against the pinned issuer/JWKS, while treating opaque
tokens as opaque and relying on the resource-bound grant plus resource-server rejection, and
(d) fails classified (`OAuthAuthProblem::{Missing, WrongMode, Expired, NotEntitled,
Other}` extended with `Revoked` mapping `invalid_grant`).

`shared_bearer`/`DbCredentialBearer` remain the OAuth refresh owner, but their dynamic
refresh inputs need an explicit design review. Static Bearer credentials must not use a
fake no-op refresh variant: the current cache returns a far-future token without
re-reading the row, and `invalidate()` enters the refresh path rather than reloading a
replacement row. #1532 therefore needs a separate execution-time row-load/generation
contract (and pool eviction) if rotation is added later.

### 2.4 Challenge classification + pool integration (owner: mcp_pool)

Extend `streamable_http_transport_config` to take the resolved bearer
(`auth_header(config.auth_header)`) — i.e. the pool's connect path resolves a
`BearerSource` first (see §3 flow), then configures the transport.

Classify connect failures at the rmcp boundary: map
`StreamableHttpError::AuthRequired(e)` and `InsufficientScope(e)` before conversion to
`anyhow::Error` into a new typed connect outcome so
the pool does **not** park them as generic strikes forever (a parked consent-required
service would hide the operator's to-do):

```rust
pub enum McpConnectAuthOutcome {
    Authorized,
    ConsentRequired { www_authenticate: String, metadata_url_hint: Option<String> },
    ScopeInsufficient { required_scope: Option<String> },
    None,
}
```

Do not recover these fields by parsing `Display`: rmcp 1.3 renders only `Auth required`
or `Insufficient scope`, so a string classifier loses the header and scope and is not a
sound contract. `record_strike` gains an auth-cause branch: `ConsentRequired` parks with a short,
explicitly-labeled backoff (same mechanics, new `reason`) and the health probe records
the auth cause rather than a generic transport failure.

### 2.5 Secure metadata validation (owner: new `mcp_oauth` module, shared with §3)

Single owner for the RFC 9728 + MCP authorization dance, test-first per §4.3:

```rust
pub struct ProtectedResourceMetadata { authorization_server: Option<String>, /* … */ }

/// Fail-closed with an operator-facing diagnosis string.
pub fn validate_metadata_urls(issuer: &str, meta: &AuthorizationMetadata, resource_uri: &str)
    -> Result<(), MetadataDiagnosis>;
```

Non-negotiable checks (each maps to a named test in §4.3):
1. Service endpoint and all metadata URLs are HTTPS (loopback `http://127.0.0.1` or
   `http://[::1]` allowed only for explicitly-flagged dev fixtures — mirroring
   mcp.rs's current `http`-only register path, which must migrate to
   `https`-default-with-loopback-exception).
2. Metadata URLs resolve to the issuer origin: exact scheme+host match on
   authorization/token/registration/jwks endpoints; no IP-literal, non-default-port, or
   userinfo trickery (`Url::host_str()` comparisons, case-normalized).
3. Issuer from authorization-server metadata equals the selected authorization-server
   identifier with exact code-point equality, including any trailing slash, as required
   by RFC 8414. Do not normalize before comparison. Protected-resource metadata's
   `resource` must likewise equal the canonical selected MCP service URI exactly, and
   every authorization/token request carries that same RFC 8707 `resource` value.
4. No redirects followed for token/metadata requests: `reqwest::Client` with
   `redirect(Policy::none())` (this is the SSRF/secret-leak boundary; the login crates
   already build their HTTP clients explicitly, this one must be documented).
5. PKCE S256 mandatory; `state` 128-bit random; the canonical `resource` parameter is
   always present.
6. Discovery order is pinned to RFC 9728/RFC 8414 path-aware well-known derivation: use
   the challenged `resource_metadata` URL when supplied; otherwise derive the protected
   resource well-known URI from the full resource path, and derive authorization-server
   metadata from the full issuer path using the RFC-defined insertion rules. Never
   collapse either identifier to the origin root or guess endpoints by string joining.

The rmcp `auth` feature (`rmcp::transport::auth::{AuthorizationManager, OAuthState,
AuthorizationSession, CredentialStore, StateStore}`) already implements discovery,
DCR, PKCE, token exchange, and refresh against `oauth2`; however it is feature-gated
(`features = ["auth"]` adds `dep:oauth2`); its default stores are in memory, while its
`CredentialStore`/`StateStore` traits can be backed by Gents documents/adapters. Gents
cannot enable new optional deps casually. The
recommendation is: **evaluate adopting `rmcp/auth` with a Gents `CredentialStore` backed
by the OAuthCredential document before hand-rolling exchange code**; the design above
(needs: issuer/host validation, mandatory `resource`, SSH-forwarded loopback redirect) mostly fits
its `AuthorizationManager::discover_metadata` + `start_authorization` +
`AuthorizationSession::handle_callback` shape. The decision (adopt vs. reuse
`gents-chatgpt-login`-style minimal flow) belongs to the implementation phase with the
parent; the metadata-validation owner (§2.5) and consent orchestration (§3) are needed
either way.

### 2.6 Consent orchestration — operator-owned, agent-observed (owner: new module + CLI first)

```rust
pub struct McpConsentRequest {
    pub service_id: String,
    pub agent_did: String,
    pub scopes: Vec<String>,
    pub resource: String,
    /// Host where the callback server must run and be reachable from the browser.
    pub callback: McpCallbackTransport,
}

pub enum McpCallbackTransport {
    /// Runtime binds 127.0.0.1:<ephemeral>; operator forwards it with SSH when remote.
    LoopbackForward,
}
```

Desktop relay is explicitly deferred. Process-local pending authorization cannot be
confirmed or consumed by another host, and no authenticated relay/binding protocol
exists today. A future desktop flow must first specify runtime enrollment, authenticated
message binding, replay protection, and which host owns token exchange and persistence;
it must not directly write credentials while claiming to consume runtime-local state.

State machine (persisted — no new collection; extends existing observation surfaces):

```
NoConfig → Configured → ConsentRequired ──(consent granted)──→ CredentialReady → Connected → ToolsDiscovered → ToolUsable
                             │  ▲                                  │
                             └──┴── (reauth requested/revoked) ────┘   ← any terminal auth failure
```

- `ConsentRequired` derives from probe classification (§2.4) and lives on
  `ToolServiceHealthState.last_error_class` (new class `auth_consent_required`) — the
  existing observation owner, consumed by desktop + CLI unchanged.
- The credential row is created **only** after a successful exchange (never a
  placeholder with secrets).
- **Operator ownership rule:** only the operator CLI may *start* consent and *complete*
  it in the initial slice. A future desktop flow requires the authenticated relay
  design above. Agents may
  request setup via the existing self-config surface (writes the `ToolServiceAuth::OAuth`
  selection, which references — never contains — credentials) and may inspect redacted
  status; agents must not be able to mint a consent URL callback receiver, read token
  stores, or observe `state`/`code_verifier`. Concretely: the consent URL is built and
  held only inside operator-invoked processes; the runtime's probe path sees only the
  resulting credential document.

### 2.7 Headless callback design (the load-bearing piece)

**Problem:** the canonical loopback flow assumes browser and callback server share a
host. On a cloud runtime the browser is on the operator's laptop.

**Primary recommendation — documented SSH loopback forwarding (no new protocol):**

1. Operator runs `gents mcp oauth login --service web --agent-did <did>` **on the
   cloud host**. The command binds the ephemeral callback server on
   `127.0.0.1:<p>` and prints the authorize URL **and** the exact forward command:

   ```
   Forward the callback port, then open this URL:
     ssh -N -L <p>:127.0.0.1:<p> <cloud-host>
   https://as.example/authorize?...
   ```

2. The operator opens the URL locally; the AS redirects to
   `http://127.0.0.1:<p>/mcp/auth/callback`, which the SSH tunnel carries back to the
   runtime's loopback listener. The redirect URI registered with the AS must be
   `http://127.0.0.1:<p>/mcp/auth/callback`; use the loopback IP literal required by
   this contract rather than `localhost`. Existing login crates are prior art for the
   server lifecycle, not for this redirect identifier.
3. Binding stays loopback-only on both ends: reuse the callback `bind_server` discipline
   and require the operator's forward to bind local `127.0.0.1` (for example
   `ssh -N -L 127.0.0.1:<p>:127.0.0.1:<p> <cloud-host>`). Never bind `0.0.0.0`; the cloud
   API is never exposed.
4. Security properties: the tunnel is operator-authenticated SSH; the callback server
   validates `state` before touching the code (existing pattern); the page response is
   the existing CSP'd `gents-login-ui` render; the code is exchanged server-side by the
   runtime that owns the credential — the operator's browser never sees tokens.
5. The port must be printed so the forward command is copy-pasteable; the port choice
   and `/mcp/auth/callback` path become the documented, deterministic contract the
   runbook (§4.6) pins.

**Why not alternatives (for the record):** device-code is explicitly out of scope ("Do
not assume device-code support" — xAI's flow exists as precedent for providers that
have it, but MCP fixtures must not require it); a runtime-hosted public redirect page
exposes the cloud API and violates acceptance item 5; an enrolled-client desktop relay
is viable but couples consent to pairing state. It remains an open future protocol, not
part of this implementation proposal.

### 2.8 Pending-authorization binding (replay/CSRF/pinning)

Pending consent state must be **process-local memory only** (never persisted — no
secrets, and a restart should require a fresh consent, which is also the restart test):

```rust
pub struct PendingAuthorization {
    pub service_id: String,
    pub agent_did: String,          // exact principal binding
    pub state: String,              // CSRF token (compare before any exchange)
    pub pkce_verifier: String,      // secret; never serialized
    pub issuer: String,             // pinned AS
    pub redirect_uri: String,       // pinned callback
    pub resource: String,           // canonical selected service URI / RFC 8707 audience
    pub requested_scopes: Vec<String>,
    pub created_at: Instant,        // enforce ~10-min max lifetime
}
```

Validation at callback (all-or-nothing; any failure → redacted 4xx page, no exchange):
`state` exact-match → `issuer` still equals the re-discovered AS → `redirect_uri`/
`resource`/`agent_did`/`service_id` unchanged → exchange with PKCE verifier → for a JWT
access token, verify signature and `iss`/`aud` against pinned metadata/JWKS; for an
opaque token, do not parse claims and rely on the resource-bound grant plus MCP resource
server rejection → upsert credential row bound to
`(agent_did, "mcp:<service_id>")`.

### 2.9 Refresh concurrency (inherited, but must be tested)

`DbCredentialBearer` already single-flights refresh per `credential_id`, has the 60 s
failure cooldown, force-refresh on 401 (`invalidate`), and persists rotated tokens with
3× retry. MCP integration requirements:
- One `shared_bearer` per `(agent_did, "mcp:<service_id>")` — never per connection — so
  two concurrent tool calls refresh once.
- Pool eviction on auth rejection mirrors `endpoint_changed` (§2.4): `invalidate()` the
  bearer and evict the connection so the next call reconnects with the rotated token.
- Restart: pool empty, credential row persists; next `current_bearer` loads from DB and
  only refreshes if stale — restart must not re-consent.
- Concurrent-refresh across processes is out of scope (one active runtime per principal
  is the documented operating convention; AGENTS.md explicitly defers enforcement).

---

## 3. End-to-end flow (target shape)

**Local flow (operator on runtime host):**
1. `gents mcp register --endpoint https://svc.example/mcp` (or desktop save) writes the
   registry row; operator later sets auth via self-config/CLI (`auth.kind = oauth`). The
   canonical service URI becomes the required RFC 8707 `resource`; it is not authored
   independently.
2. Health checker probes → 401 with `WWW-Authenticate` → `ConsentRequired` observation
   recorded (classified, redacted).
3. Operator runs `gents mcp oauth login --service web`: metadata discovery (§2.5) →
   configured public client id (DCR remains deferred unless separately approved) → loopback
   callback server → browser opens authorize URL → callback validated (§2.8) → token
   exchange with `resource` + PKCE → credential row upserted
   `(agent_did, "mcp:web")` → `ConsentRequired` clears on next probe.
4. Pool connects with `auth_header` from `shared_bearer`; `initialize`/`tools/list`
   succeed → `Healthy`; tool calls proceed (proofs.ToolExecution unchanged).

**Headless flow:** identical, except the operator forwards the callback port over SSH
(§2.7) before opening the URL; everything else byte-identical, which is exactly what
makes it testable (§4.2).

**Unauthorized→authorized transition on 401 mid-session (OAuth only):** tool call gets auth error →
`bearer.invalidate()` → next `current_bearer` force-refreshes → if refresh fails with
`invalid_grant` → classified `Expired/Revoked` error → health probe marks
`auth_reauth_required` → desktop shows "reauthorize" affordance → operator re-runs login.

---

## 4. Test plan (maps 1:1 to acceptance items)

No production code was changed; the plan below is the implementation checklist. Every
test runs in this worktree with `cargo test -p gents` /
`cargo test -p gents-cli` / desktop suites as applicable.

### 4.1 Fixture work (acceptance 1)
- New `tests/support/mcp_oauth_fixture.rs`: axum server hosting (a) an MCP endpoint
  gated on a Bearer token issued by (b) a mock AS with `/authorize`,
  `/token`, `/.well-known/oauth-protected-resource`, and a simulated browser-consent
  step (a direct code-minting test endpoint standing in for the browser; no real
  browser in CI). Wire-format fixtures committed under `tests/fixtures/providers/`
  style redaction rules (extend `provider_fixture_redaction.rs` patterns with
  `mcp_oauth` tokens).
- Fixture-host route for the headless variant: same fixture, callback port reachable
  only through a forwarded listener (test binds an extra loopback listener and proxies —
  exercises the identical code path with a different redirect host).

### 4.2 Headless callback routing (acceptance 5)
- Unit: authorize-URL builder emits exact `redirect_uri=http://127.0.0.1:<p>/mcp/auth/callback`
  and printed SSH command matches the pinned port.
- Integration: forward loopback→callback server; complete flow; assert runtime obtained
  credential, cloud API not exposed (bind listener asserts `127.0.0.1` only).
- Runbook test: the documented command in the new docs (`contracts/`-adjacent or
  `docs/` per repo conventions) parses with the same port/path constants the code
  emits (string-equality pin).

### 4.3 Security matrix (acceptance 2) — each a named unit test against §2.5/§2.8
- wrong principal: pending consent recorded for `did:A`, callback completed against a
  request that was re-targeted to `did:B` → rejected pre-exchange.
- replayed `state`: same state reused → second callback rejected (single-use pending).
- replayed `code`: second exchange attempt → AS fixture returns
  `invalid_grant`; runtime classifies `Expired/Revoked`, no credential overwrite.
- invalid PKCE: fixture AS rejects verifier mismatch → classified failure, redacted page.
- issuer mismatch: AS metadata issuer ≠ `authorization_server` in PR metadata →
  fail-closed with operator diagnosis, **no token request** (assert no outbound token
  POST recorded).
- audience mismatch: a signed JWT token whose verified `aud` differs from the canonical
  service resource → reject before use. An opaque token is never decoded for claims;
  the fixture instead proves the exact `resource` went to authorization/token requests
  and the resource server rejects a token issued for another resource.
- insecure metadata: `http://` AS endpoints on non-loopback → fail-closed; also the
  issue's v0.17.0 real-world case: HTTPS service advertising HTTP issuer/authorization/
  token URLs → precise diagnosis, no downgrade, no secret-bearing request.
- metadata URL origin escape (SSRF): `authorization_endpoint` pointing at
  `https://evil.example` or an IP-literal/port-variant of the issuer → rejected.
- cross-service credential borrow: service A completes consent; pool for service B (same
  issuer!) with `credential_id` of A → `lookup` by provider key `mcp:B` misses →
  classified Missing (no credential sharing).

### 4.4 State/lifecycle matrix (acceptance 3)
- restart: pool cleared, credential row intact → next probe connects without consent.
- refresh concurrency: two concurrent `call_tool`s on one service → exactly one refresh
  (existing `DbCredentialBearer` test pattern extended through the MCP path).
- expiry: short-TTL token → refresh before use (REFRESH_SKEW).
- revocation: AS fixture invalidates refresh token → `invalid_grant` → classified
  `Revoked`, health shows reauth-required, secrets redacted in every surface.
- cancellation: login command cancelled / callback server dropped → pending state
  dropped, no credential row, no leaked verifier.
- config-present vs consent-required vs credential-ready vs connected vs
  tools-discovered vs tool-usable: six probes of one service through the matrix, each
  classified by `ToolServiceHealthState::project` + the new auth classes; desktop view
  receives them through the existing `MCPServiceHealthView` (additive optional field,
  contract MINOR bump per `contracts/README.md`).

### 4.5 Redaction fences (acceptance 4, extends existing fences)
- `provider_fixture_redaction.rs` extended with MCP-OAuth token patterns; committed
  fixtures must pass.
- Config export / desktop `MCPServiceHealthView` / agent meta-tool output: assert
  `"<redacted>"` for any token-shaped field; agent self-config write attempts setting
  `auth.kind = oauth` with an inline secret → rejected by the §2.1 validator (mirrors
  `SelfConfig/Auth.lean`'s raw-key block).
- Desktop scope isolation: consent for remote principal/service must not touch local
  credential rows — test asserts `operator_access(agent_did)` scoping keeps a second,
  unrelated agent's OAuthCredential unread (row-count + redacted list equality).

### 4.6 CLI documentation (acceptance 5)
- New `docs/` (or `contracts/`) runbook with the exact SSH/systemd sequence; fenced by
  `conformance/docs.rs::grounding_doc_paths_resolve` (path tokens must exist) and a new
  port/path constant-equality test (§4.2).

### 4.7 Regression gates (per AGENTS.md)
- `cargo test -p gents`, `cargo check --workspace --all-targets`, `make fmt-check`;
  desktop `cargo test -p gents-desktop-bridge` (contract fingerprint) when views change;
  `lake build` only if Lean `ConfigDocuments`/`MCPHealth` vocabulary changes (§2.1
  schema addition would touch `ConfigDocuments.lean` field lists → proof update first
  per Foundation order).

---

## 5. Ownership summary and #1532 coordination boundaries

| Concern | Canonical owner (extend) | #1532 overlap |
| --- | --- | --- |
| Service auth selection | `ToolServiceRegistry` + `installation_validation.rs` | **shared** — Bearer needs the same `auth` selection; #1533 adds the `OAuth` variant only |
| Credential doc + CRUD + redaction | `oauth_credential.rs` / `gents-protocol` row+schema | **shared** — #1532 provisions Bearer rows; both need the additive columns to be coordinated in one schema change |
| Secret resolution at execution | `DbCredentialBearer` + `BearerSource` for OAuth; credential CRUD/ConfigAccess for static rows | **shared owner, different cache semantics** — #1532 must load at dial and add an explicit generation/eviction contract before claiming rotation |
| Refresh dispatch | `OAuthRefreshKind` | #1532 does not use it; #1533 adds `Mcp { token_endpoint, client_auth }` after design approval |
| Transport header injection | `mcp_pool::streamable_http_transport_config` | **shared** — Bearer also needs `auth_header`; land the parameterized resolver once |
| 401/403 challenge classification | `mcp_pool` connect outcome + `health_checker` | **shared** — same classification enum serves Bearer-missing |
| Metadata validation / consent / pending state / headless callback | new `mcp_oauth` module + CLI/desktop surfaces | #1532-independent |
| Health/auth state vocabulary | `ToolServiceHealthState.last_error_class` + `ToolServiceHealthState::project` | **shared** — Bearer-missing needs the same class |

**Minimal coordination boundary for the parent to hand to #1532:**
1. The `ToolServiceAuth` selection shape — freeze `Bearer` as a reference-only marker
   whose credential id is derived from principal + service, not caller-supplied;
   #1533 later adds only `OAuth { provider, scopes, resource }`.
2. The `OAuthCredential` additive columns (`issuer`, `resource`, `granted_scopes`,
   `client_registration`) — one schema change covering both issues' needs, additive and
   optional, so #1532's rows remain decodable.
3. The `McpConnectAuthOutcome` + auth health classes — #1532 consumes for Bearer-missing.
4. The transport `auth_header` resolver seam in `mcp_pool` — parameterize once with a
   `BearerSource`; #1533 passes the MCP bearer, #1532 passes a static-credential bearer.

Anything beyond these four seams should not be built twice: consent orchestration,
pending-authorization state, metadata validation, and the headless callback are #1533
only; static-token provisioning CLI is #1532 only.

---

## 6. Direction required before implementation

The OAuth lane must not start until the operator selects these product/security
decisions. Recommended defaults are listed first:

1. **Protocol engine:** adapt rmcp's `auth` state machine behind Gents-owned
   `CredentialStore`/`StateStore` adapters, while keeping Gents validation stricter at
   the metadata and destination boundaries; alternatively implement a smaller Gents
   flow directly.
2. **Client registration:** support operator-configured public clients first and add
   dynamic client registration only for issuers whose metadata explicitly advertises
   it; alternatively require DCR in the first slice.
3. **Headless consent:** document SSH loopback forwarding as the first supported remote
   topology; defer a desktop relay/enrolled-client protocol.
4. **Audience policy:** derive the resource from the canonical selected MCP service URI,
   require protected-resource metadata to match it exactly, and require exact issuer
   matching. For JWT access tokens, validate signature/JWKS plus `iss`/`aud`; for opaque
   tokens, never infer claims—use the resource-bound grant and fail closed on resource
   server rejection.
5. **Delivery boundary:** land discovery/validation + pending consent first, then token
   exchange/persistence, then refresh/revocation. Static Bearer rotation/reconnect stays
   a separate #1532 follow-up and is not a prerequisite for starting OAuth design work.

## 7. Explicit non-goals (this phase)

- No production source modified and no OAuth implementation is claimed by this design PR.
- No alternate secret storage (no keyring/kv divergence — credentials stay in
  DefraDB `OAuthCredential` documents under ACP, per AGENTS.md).
- No browser automation, no authenticated production calls, no real secrets, no
  histories/home-file inspection, no runtime DB mutation, no migration/reset, no
  admission bypass, no main push/merge/delete.
- No downgrade of the observed upstream defect (HTTPS service advertising HTTP
  issuer/token URLs stays a fail-closed operator diagnosis, never auto-allowed).

---

## 8. Prior-art reuse audit (turn 2 — every listed surface read in full)

Scope of this pass: `oauth_credential.rs` (already audited turn 1), `oauth_http.rs`
(526 lines, full), `xai_oauth_login.rs` (304, full), `chatgpt_oauth_refresh.rs` (229,
full), `claude_oauth_refresh.rs` (243, full), `gents-chatgpt-login` (710, full),
`gents-claude-login` (653, full), `gents-login-ui` (96, full turn 1), CLI
`codex_login.rs`/`claude_login.rs`/`grok_login.rs` + `mcp.rs` register path, desktop
`inference_setup.rs` (`desktop_codex_login` L391+/`desktop_claude_login` L820/
`desktop_grok_login` L597–645), `config_client` write plumbing, DID identity,
ACP projection bindings, P2P/hostname filtering.

### 8.1 The shell vs. the mechanisms — what #1532 actually gets for free

Turn 1's "oauth_credential.rs owner-only credential/cache/refresh-lock shell" decomposes
into exactly four reusable mechanisms, all **already provider-parameterized**:

**M1 — Credential persistence owner** (`oauth_credential.rs:198-513`).
`oauth_credential_id(agent_did, provider) = "{provider}:{agent_did}"` (L198-200),
`lookup_oauth_credential` (L260+), `lookup_oauth_credential_by_id`, `list_oauth_credentials`,
`upsert_oauth_credential[_on]` (immutable `agent_did` on update, pinned by test
`upsert_update_block_omits_immutable_agent_did` L994-1014), decode-with-defaults
(`oauth_credential_from_value` L490-513; null→None / blank→None / missing→default
behavior pinned by `oauth_credential_from_value_applies_defaults_and_cleans_blanks`
L961-991). All writes flow `ConfigAccess::write_local`/`access.write` through
`config_client` (GraphQL txn begin/commit `config_client/graphql.rs`, committed-write
owner; CLI `config_writes` wraps it with local/remote routing, and
`graphql_diagnostic_hint` `config_client/mod.rs:218-232` is the operator-hint surface).
This is generalizable as-is — **#1532's Bearer credentials need zero changes here**;
they are just `OAuthCredential` rows with a provider key convention and `enabled`.
Additive columns (`issuer`, `resource`, `granted_scopes`, `client_registration` from
§2.2) must be `Option` with decode defaults to preserve the pinned decode behavior.

**M2 — Cache/refresh single-flight** (`DbCredentialBearer` L596-826).
This mechanism is reusable for #1533's refreshable OAuth tokens, but not as-is for
#1532's static Bearer rotation. In particular, a fresh far-future cache hit precedes the
DB reload, and `invalidate()` forces refresh rather than row replacement. Its actual
behavior is:
- freshness check with skew (`REFRESH_SKEW = 5 min`, L41; `token_is_fresh` L540-543,
  tested L904-918),
- cache → DB reload → newer-row adoption loop (`current_bearer` L741-821: cache hit →
  `load_credential` → prefer the row with the later `access_token_expires_at` —
  already cross-process-safe against a second writer on the same credential),
- single-flight refresh under `refresh_lock` which also owns the last-failure entry,
- 60 s failure cooldown (`REFRESH_FAILURE_COOLDOWN` L47), whose semantics are pinned by
  tests: cooldown-served failure (`failed_refresh_is_not_retried_during_the_cooldown`
  L1058-1090, using the `one_shot_token_server` trick), and cooldown never re-serves a
  rejected-but-unexpired token after `invalidate()`
  (`failed_refresh_keeps_the_bearer_forced_and_never_reserves_the_bad_token` L1096-1130),
- `invalidate()` = force_refresh flag → next `current_bearer` refreshes even if the
  cached token is unexpired (L823-825),
- `is_owner` gate (non-owner serves cache/DB without refreshing; L776-779),
- rotated-token persistence with 200/400/800 ms retry (`persist_with_retry` L695-711)
  and the memory-serve-on-failure path with the "must be re-persisted" warning (L809-819).

**M3 — Failure vocabulary** (`OAuthAuthProblem` L83-92: `Missing | WrongMode{found_mode}
| Expired | NotEntitled | Other(String)`; `classify_oauth_auth_error(product, agent_did,
provider, problem)` L94+ with per-product operator copy in `OAuthProduct` L51-72 —
`name`, `backend_label`, `login_command`, `not_entitled_guidance`; the "which copy"
discipline is pinned by tests L1016-1045). #1532 needs: `Missing` (no credential row
yet), `WrongMode` (disabled), `Expired` (invalid_grant on rotation) — already existing.
The `Error` text is **token-free by construction** (`refresh_error_text_never_carries_tokens`
`claude_oauth_refresh.rs:230-242`; `exchange_error_never_echoes_the_code`
`gents-claude-login:586-605`; `oauth_errors_do_not_echo_unstructured_response_bodies`
`gents-chatgpt-login:652-661`) — this discipline carries to MCP.

**M4 — Bearer injection wrapper** (`oauth_http.rs` full).
`BearerSource` trait (L588-594: `current_bearer` + default no-op `invalidate`),
`BearerAuthHttpClient<S, P, T>` (L117+): per-request fresh bearer
(`fresh_auth_header` L173-186), rejection-status invalidation
(`bearer_to_invalidate` L197-202 over `is_bearer_rejection` L99-101), SSE content-type
fixup (`ensure_event_stream_content_type` L105-112), all with provider policy
`OAuthHttpPolicy` (L41-86: `REJECTION_STATUSES`, `merge_identity_headers`,
`patch_request_body`, `send_via`, `patch_streaming`) — the dedup that already replaced
two ~150-line provider copies, with `prepare_for_test` (L204-210) and the full
`CountingBearer`/`StatusInjectingClient` test kit (L334-526) as the template.
Genericable consumers today: `ChatGptCodexPolicy`/`ChatGptCodexHttpClient`
(`chatgpt_codex.rs:126,136,191`), `XaiGrokOAuthPolicy`/`XaiGrokOAuthHttpClient`
(`xai_grok_oauth.rs:102,112,150`), plus the non-rig path
`claude_subscription.rs:38-59` (`ClaudeSubscriptionClient<S: BearerSource =
DbCredentialBearer>`) and `claude_messages.rs:612-675` (`stream_messages<S:
BearerSource>` injects `authorization` with `set_sensitive(true)` at L653-659 and
invalidates-once-on-transport-401 at L626).

M1–M4 are the prior art. #1532 should reuse M1 and the shared ownership/error
patterns, but must not force static credentials through M2's refresh semantics or M4's
provider-specific retry assumptions.

### 8.2 Generalizable state / PKCE / browser-callback / refresh mechanisms (reusable for MCP)

| Mechanism | Current owner | Evidence | Generalizable? |
| --- | --- | --- | --- |
| Loopback-only bind | `gents-chatgpt-login:bind_server` L270-284 (hard `127.0.0.1:{1455,1457}`, explicit AddrInUse error), `gents-claude-login:105` (`127.0.0.1:0` ephemeral) | bind never `0.0.0.0` | ✅ exact — MCP consent server binds `127.0.0.1:0` and reports the port (§2.7) |
| PKCE S256 + state | `generate_pkce`/`generate_state` (both crates; chatgpt L292-308, claude L313-329) | 64-byte verifier → B64url, challenge = SHA256(verifier), 32-byte state; entropy+encoding pinned (`pkce_values_have_the_required_entropy_and_encoding` L643-649, `pkce_challenge_is_s256_of_verifier` L501-507) | ✅ verbatim — zero deps beyond `rand`+`sha2`, both already in-tree |
| Callback state machine | `CallbackOutcome::{Continue,Complete}` + `handle_callback_request` (chatgpt L162-264, claude L203-292) | path check → `state` compare (warn+400, **keeps listening** on mismatch — pinned `callback_with_wrong_state_is_rejected_and_keeps_waiting` claude L534-548 / chatgpt L682-695) → `error` param → `code` presence → exchange → CSP'd page | ✅ — this IS the pending-authorization validation from §2.8, minus service/principal/issuer pinning which MCP must add |
| Cancel + timeout plumbing | `ShutdownHandle`(Notify) + `tokio::select!` loop + `unblock()` + `block_until_done` (both crates) | cancel-path test `callback_server_can_be_cancelled_without_leaking_tokens` L697-710; desktop reuses identical pattern with `AtomicBool` cancel (`inference_setup.rs:622,645` grok; codex L391+ L820) | ✅ |
| Loopback relay threading | tiny_http `Server::http` + `thread::spawn` recv loop → `mpsc(8)` → `blocking_send` (both crates L111-152) | deterministic | ✅ |
| Redacted CSP'd callback pages | `gents-login-ui::response` (`gents-login-ui/src/lib.rs:81-96`) | CSP `default-src 'none'`, no-store/no-referrer, HTML-escaped, tested | ✅ — MCP consent pages must reuse, not re-roll |
| Authz-URL builders | `build_authorize_url` (chatgpt L310-336, claude L331-349) | query_pairs_mut, fixed scopes, `force_state` test hook (`LoginOptions.force_state`) | ✅ shape — MCP parameterizes issuer/scopes/resource/client-id instead of hardcoding |
| Token-exchange error shaping | `redacted_transport_error` (claude L430-439, chatgpt L383-392 — kind-only: timed out / could not connect / failed), `oauth_error_message` (chatgpt L433-449, allowlist-based echo) | secrets never echoed; pinned by tests | ✅ verbatim |
| Refresh-token POSTs | `chatgpt_oauth_refresh.rs` (JSON, L37-98), `claude_oauth_refresh.rs` (JSON + scope re-send + rotation-optional, L34-113), `xai_oauth_refresh.rs` (form-encoded, per `XaiLoginTokens`/device flow) | status→problem mapping: 401→Expired; 400+`invalid_grant`→Expired (claude L76-80); 403→NotEntitled (claude L81-83) | ✅ mapping — MCP needs: `invalid_grant`→Expired/Revoked on **any** 4xx body, discovered endpoint, `resource` param; ~80 new lines in `mcp_oauth_refresh.rs`, not a fork of the pattern |
| JWT claim read | `jwt_payload`/`jwt_expiration`/`decode_id_token_claims` (`chatgpt_oauth_refresh.rs:107-154`) | unsigned-claims OK per OIDC §3.1.3.7 (comment L100-106) — **advisory only, never a security boundary**; MCP `aud`/`iss` verification needs **real** verification (fixture JWKS in tests), a different trust level | ⚠️ reuse parsing, not the trust posture |
| Credential construction from tokens | `credential_from_login_tokens` (`xai_oauth_login.rs:248-277`, uses `resolve_access_token_expiry` + `oauth_credential_id`) + redacted result JSON (`claude_login_result_json` L117-122, `codex_login_result_json` L119-121) | MCP's equivalent = same call with `provider = "mcp:<service_id>"` + the §2.2 additive columns | ✅ |
| Device-code state machine | `xai_oauth_login.rs:76-246` — `DeviceCodeChallenge{device_code,user_code,verification_uri,verification_uri_complete,expires_in,interval}`, poll loop with cancel + deadline (L126-137), `authorization_pending`/`slow_down`(+5s cap 30)/`access_denied`/`expired_token` classification (L183-203), URL-callback variant for UI (`run_device_code_login_with_url_callback` L231-246) | the SSH-safe precedent — explicitly out of scope for MCP per issue, but its **poll/cancel/url-callback** shape is the template if a fixture ever needs it | ✅ shape, ❌ not an MCP dependency |
| Test kit | `one_shot_token_server` (`oauth_credential.rs:1134+`, claude-login's twin L608-652), `seed_credential`/`test_node` (cooldown tests L1050+), `StatusInjectingClient`, `force_state` hook, header-capture MCP servers (`mcp_pool/tests.rs:432-520`) | the mock-AS fixture (§4.1) is one-shot-server + header-capture composed | ✅ |

### 8.3 Provider-specific assumptions that must NOT be generalized

1. **Fixed first-party issuer/client constants.** `CLIENT_ID`/`DEFAULT_ISSUER`/
   `token_endpoint(issuer)` in `gents-protocol/src/chatgpt_oauth.rs:8-28` (single owner
   #1339, trailing-slash normalization pinned by test L30-42); `claude-login` mirrors
   `gents::claude_oauth` constants with a sync-pinning test (`lib.rs:4-5`); xAI's
   `XAI_OAUTH_CLIENT_ID` + `DEVICE_CODE_URL`/`TOKEN_URL` + override envs
   (`xai_oauth_login.rs:18-25`). MCP: issuer is per-service **discovered** — no
   compile-time constant, no global override env per provider; the credential row owns
   the endpoint (§2.2) and §2.5 validates it.
2. **Provider-specific scopes.** Codex connector scopes (chatgpt L327), Claude's
   `user:mcp_servers ...` scope list (claude L25-26), xAI's Grok-CLI scope string
   (`xai_oauth_login.rs:22`). MCP: scopes are per-service config (§2.1 `OAuth.scopes`),
   granted-set recorded from the AS response.
3. **Provider-specific rejection-status sets.** `REJECTION_STATUSES` (401+403 Codex,
   401-only xAI with the rotation-burn rationale `oauth_http.rs:42-47`). MCP: 401 only
   (403 = scope gate → `InsufficientScope`, a different repair path per rmcp §1.2).
4. **Provider-specific body/response shaping.** `patch_request_body` (Codex instructions
   hoist, xAI store:false), `send_via` (SSE→buffered rewrite), `patch_streaming`
   (usage folding) — none apply to MCP; MCP passes the bearer into the transport config
   (§2.4), not a rig `HttpClientExt` wrapper.
5. **Loopback-implies-same-machine assumption.** Both login crates assume the browser
   can reach `localhost:<p>` on the runtime host — the exact assumption #1533 must break
   with SSH forwarding (§2.7). The mechanism (loopback bind) generalizes; the
   same-host reachability assumption does not.
6. **`resolve_mcp_url` plain-HTTP address selection** (`mcp_pool.rs:857-889` +
   `ip_in_cidr` L891+): hostname==local → `http://127.0.0.1`, LAN-CIDR match → LAN IP,
   else tailscale/LAN/hostname — and `mcp.rs:32-34` **rejects** non-http endpoints
   ("the MCP registry currently stores HTTP endpoints"). HTTPS services (the issue's
   v0.17.0 OAuth case) cannot even be registered today. §2.1/§2.5 must extend this:
   `https` allowed, scheme preserved through `resolve_mcp_url` (out of scope to redo
   the address selection, but the scheme assumption is a hard blocker for acceptance 1).
7. **`send_agent_did` / x-agent-did identity header** (`meta_tools/shared.rs:310-316`,
   `mcp_pool.rs:24`): a trust *signal* to the service, not authentication; under OAuth
   the bearer is the authentication and the DID header stays a hint. No conflict, but
   the two must not be conflated in the design (the DID header identifies; the bearer
   authenticates).

### 8.4 DID/ACP/P2P filtering audit (binding + admission surfaces)

- **DID identity**: `AgentIdentity` trait (`identity.rs:43-50`: `did()`, `sign`,
  `verify`, `service_account`), `KeyIdentity` (L128+, `did:key` parsing via
  `crypto::parse_did_key` L560+, tested L127-140), and the commit-signer conversion
  `commit_signer_identity_for_did` (L128-140) — the principal type every credential
  row and pool key already uses. Consent binding (§2.6/§2.8) pins `agent_did` as this
  exact string; no new principal type is introduced.
- **ACP**: `ProjectionAcpBinding::validate` (`projection_acp.rs:8-40`) validates
  policy references (non-empty `policy_id`, staged≠active) — the canonical "reference,
  don't inline" pattern §2.1's derived credential key mirrors. Secrets stay in DefraDB
  documents gated by ACP; no alternate storage (per AGENTS.md and operating limits).
- **P2P/host filtering**: `resolve_mcp_url` hostname==local→127.0.0.1, LAN-CIDR
  gating via `local_subnet_cidr` (`MetaToolContext.local_subnet`), tailscale fallback
  (§8.3 item 6); `mcp_service_allowed` (`meta_tools/shared.rs:97-101`) + `service_selection`
  (L31-44) enforce the configured allow-list per principal; `lookup_service`
  (L318-330) fail-closes on missing/disabled/duplicate services.
- **Config write routing**: `ConfigAccess::{Embedded,Graphql}` (`config_client/mod.rs:170-196`),
  GraphQL txn plumbing (`config_client/graphql.rs`, 30 s timeout, conflict-retry
  owned by DefraDB), CLI `config_writes` (local/remote), desktop
  `operator_access(agent_did)` → `save_tool_service_registry`
  (`client/core/writes.rs:1205+`, `mutations/manage/tools.rs:33-60`).
  MCP OAuth persists **only** through these owners — no parallel write path.

---

## 9. Exact shared-interface recommendation for #1532 (freeze candidate)

**Principle: #1532 and #1533 share credential, binding, transport, and redaction owners;
#1533 extends refresh dispatch and adds MCP-only orchestration. Static credentials do
not masquerade as refreshable OAuth, and neither lane creates a second secret store.**

### 9.1 #1532 consumes (reviewed recommendation)

Static Bearer credentials use the provider-key namespace
`mcp-bearer:<service_id>`, distinct from #1533's `mcp:<service_id>`. At each new dial,
#1532 derives that key from the current principal and validated service id, loads the row
through `ConfigAccess`, verifies its approved destination against the registry endpoint,
and only then supplies the token to the transport.

Do not add `OAuthRefreshKind::None`: with a far-future cached token,
`current_bearer()` returns before re-reading the row, while `invalidate()` forces the
refresh branch and a no-op refresh fails instead of loading the rotated row. The first
#1532 slice therefore defers rotation/reconnect. A later rotation slice must add a
credential generation (or updated-at fingerprint), evict pooled connections when it
changes, and prove that the next dial loads the replacement row. #1533 adds the dynamic
OAuth variant only after its representation is approved:

```rust
pub enum OAuthRefreshKind {
    ChatGpt, Claude, Xai,
    /// #1533: discovered AS; token endpoint + client auth ride on the credential row.
    Mcp { token_endpoint: String, client_auth: McpClientAuth },
}
```

### 9.2 Proposed seams (the four-line contract between the two issues)

1. **Provider-key namespaces** (this is the only truly shared convention):
   `"mcp-bearer:<service_id>"` = #1532; `"mcp:<service_id>"` = #1533. Both flow through
   the unchanged `oauth_credential_id(agent_did, provider)`.
2. **`OAuthCredential` additive columns** (`issuer`, `resource`, `granted_scopes`,
   `client_registration`) — one schema change, all `Option` with decode defaults
   (M1 decode discipline), landed by #1533 (it is the sole consumer) but **documented
   for #1532** so its rows (which leave them `None`) remain decodable across the
   upgrade.
3. **Typed `McpConnectAuthOutcome`** (§2.4), captured before `anyhow` conversion, plus auth health classes on
   `ToolServiceHealthState.last_error_class` (`auth_consent_required`,
   `auth_reauth_required`, `auth_scope_insufficient`) — owned by #1533; #1532 consumes
   the same classes for "bearer missing/disabled".
4. **Transport seam**: `streamable_http_transport_config(config, auth_header:
   Option<String>)` — parameterized once; #1532 passes the static bearer, #1533 passes
   the `DbCredentialBearer`-resolved one. Whoever lands first owns the signature.

### 9.3 What stays strictly #1533 (no overlap with #1532)

Metadata discovery/validation (§2.5), DCR, consent orchestration + pending
authorization state (§2.6/§2.8), SSH-forwarded callback (§2.7), `resource`/audience
binding, mock-AS fixture + SSH-forward tests (§4.1–4.3). #1532's static-token path
touches none of these.

### 9.4 Anti-duplication guarantees (how this stays one framework)

- Bearer resolution: **one credential/config owner**, with `DbCredentialBearer`
  retained for refreshable OAuth and execution-time row lookup for #1532 static tokens.
  Both enforce principal/service/destination binding without pretending static reload
  has OAuth refresh semantics.
- Rejection handling: **one** `is_bearer_rejection` (mcp_pool maps rmcp's typed 401/403
  errors into the same health classes; it does not re-detect statuses at the transport
  layer).
- Refresh: **one** dispatch (`OAuthRefreshKind`) — `Mcp` variant delegates to
  `mcp_oauth_refresh.rs`, which follows the three existing refresh modules' shape
  (typed problem mapping, token-free errors, rotation-optional) rather than copying
  any one of them.
- Login UX: **one** callback server shape (`LoginServer`/`ShutdownHandle`/
  `gents-login-ui::response`) — MCP's consent server is a parameterized extraction of
  the shared parts, with the MCP-specific pinning (§2.8) as the only delta. If the
  parent prefers minimal churn, MCP's consent server can live in a new
  `gents-mcp-consent` crate alongside the two existing login crates, reusing
  `gents-login-ui` verbatim.
- Redaction: every new surface reuses the existing `<redacted>` Debug/result-JSON
  discipline and the `provider_fixture_redaction.rs` fence; no new redaction code.
