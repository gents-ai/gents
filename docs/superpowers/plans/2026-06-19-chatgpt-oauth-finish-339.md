# ChatGPT-OAuth Finish (#339) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `ChatGptCodex` (ChatGPT-subscription-over-OAuth) backend production-usable: refresh the OAuth bearer per request so long sessions don't 401, turn auth failures into actionable CLI errors, and document the fleet/remote credential-home behavior.

**Architecture:** Today `chatgpt_codex::build_responses_client` reads `CodexAuth::get_token()` **once** and bakes a static bearer into the rig client (`context.rs:203`, `oneshot.rs:139`); a long multi-turn loop outlives the token and 401s. We introduce a `BearerSource` trait whose `Arc<AuthManager>` impl calls `auth().await` per request (which proactively refreshes near-expiry), hold it inside a generic `ChatGptCodexHttpClient<S>`, and overwrite the `Authorization` header on every outbound request — `send`, `send_streaming`, and `send_multipart`. Auth errors are classified into precise missing / wrong-mode / expired problems with setup guidance, surfaced by the existing `codex-auth-probe` and `diagnose` commands. No DefraDB/schema/Lean change — this is auth plumbing on an existing seam.

**Key constraint discovered in rig:** `rig::providers::openai::Client::builder().build()` requires `H: Default + HttpClientExt` (`rig-core/src/client/mod.rs:602`, with `:624` `http_client.unwrap_or_default()`) — even when `.http_client()` supplies an instance, the **type** must impl `Default`. So `ChatGptCodexHttpClient<S>` carries `bearer: Option<Arc<S>>` and a hand-written `Default` impl (no `S: Default` bound); the existing `build_openai_responses_client` helper is reused unchanged.

**Tech Stack:** Rust, `codex-login` (`AuthManager`, `CodexAuth`, `AuthCredentialsStoreMode`; `refresh_failure_for_auth` for expiry detection), rig-core `HttpClientExt`, anyhow, tokio. (No dependency on `codex-app-server-protocol` — `AuthMode` is never named; wrong-mode is stringified.)

## Global Constraints

- **`tracing`, never `println`** in runtime/library code (`crates/defra-agent`). CLI command *user-facing* stdout (`crates/defra-agent-cli/src/commands/**`) uses `println!` by existing convention — match the surrounding command.
- **Gate with the full package suite** (`cargo test -p defra-agent` and `cargo test -p defra-agent-cli`), never `--lib` — integration tests are separate compile units.
- **`graphql::escape_graphql_string()`** for anything interpolated into a GraphQL string. (No GraphQL is written in this plan, but honor it if a step adds any.)
- **Never emit `[]` in a DefraDB mutation** — emit `null`. (No mutations here.)
- **No Lean/spec change required.** This changes no legal transition, no invariant, and not what the model is fed — auth header material only. Do not add proof obligations.
- **Codex auth ownership.** Defra Agent does not **create, relocate, or clobber** Codex credentials — it never writes a new `auth.json` or moves the home. But `AuthManager::auth()` performs Codex's *normal* proactive token refresh, which **does persist the updated managed token** to the configured store (`manager.rs:1886` `refresh_and_persist_chatgpt_token` → `persist_tokens`), exactly as the Codex CLI would. Do not claim "read-only / never writes the store" anywhere — say "may update the managed token during refresh, like the Codex CLI."

---

## File Structure

- `crates/defra-agent/src/chatgpt_codex.rs` — **modify.** Add `ChatGptAuthProblem` + `classify_chatgpt_auth_error`; add `BearerSource` trait + `AuthManagerBearer` impl; add `resolve_chatgpt_auth` (precise problem classification); rework `ChatGptCodexHttpClient<S>` (generic, `Default`-able, injects a fresh bearer on every send variant); make `build_responses_client` construct via `resolve_chatgpt_auth`.
- `crates/defra-agent/src/inference_http.rs` — **unchanged.** The existing `build_openai_responses_client` (`H: Default + HttpClientExt`) is reused; `ChatGptCodexHttpClient<S>` impls `Default`, so no new builder is needed.
- `crates/defra-agent/src/agent/runtime/context.rs:203` — **modify only if a type annotation names the old client type.** Call form (`build_responses_client(&endpoint).await`) is unchanged.
- `crates/defra-agent/src/oneshot.rs:139` — **modify only if a type annotation names the old client type.**
- `crates/defra-agent-cli/src/commands/codex_auth_probe.rs` — **modify.** Use `resolve_chatgpt_auth` + `classify_chatgpt_auth_error` for actionable messages; add a 401/403 → expired hint on the models probe.
- `crates/defra-agent-cli/src/commands/diagnose/mod.rs` — **modify.** Add a structured `checks.chatgpt_auth` object to the JSON output (diagnose is JSON-only — never `println!`).
- `docs/backends.md` — **create.** Document `ChatGptCodex` setup, `DEFRA_CODEX_HOME` vs `CODEX_HOME`, and fleet/remote credential-home behavior. (This is also the file the broader #509 matrix lands in later — start it here with the Codex section.)
- Tests live inline in `chatgpt_codex.rs` `#[cfg(test)]` (unit) and `crates/defra-agent-cli/tests/` if a command-level test fits.

---

## Task 1: Auth-error classifier with actionable messages

**Files:**
- Modify: `crates/defra-agent/src/chatgpt_codex.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Produces: `pub enum ChatGptAuthProblem { Missing, WrongMode { found_mode: String }, Expired, Other(String) }` and
  `pub fn classify_chatgpt_auth_error(codex_home: &std::path::Path, problem: &ChatGptAuthProblem) -> String`
  (returns a multi-line, user-facing, actionable message). Consumed by Task 3/4. **`WrongMode` stores a local `String`, not `codex_login`'s `AuthMode`** — that type lives in `codex-app-server-protocol`, which this workspace does not depend on directly; leaking it would force a new dependency. The call site stringifies via `format!("{:?}", auth.auth_mode())`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/defra-agent/src/chatgpt_codex.rs`:

```rust
#[test]
fn classifies_missing_auth_with_login_guidance() {
    let home = std::path::Path::new("/tmp/codex-home");
    let msg = classify_chatgpt_auth_error(home, &ChatGptAuthProblem::Missing);
    assert!(msg.contains("/tmp/codex-home"), "names the home: {msg}");
    assert!(msg.contains("codex login"), "tells the user how to fix it: {msg}");
}

#[test]
fn classifies_wrong_mode_naming_found_mode() {
    let home = std::path::Path::new("/tmp/codex-home");
    let msg = classify_chatgpt_auth_error(
        home,
        &ChatGptAuthProblem::WrongMode { found_mode: "ApiKey".to_string() },
    );
    assert!(msg.contains("ChatGPT"), "asks for ChatGPT OAuth: {msg}");
    assert!(msg.contains("ApiKey"), "names what was found: {msg}");
}

#[test]
fn classifies_expired_with_reauth_guidance() {
    let home = std::path::Path::new("/tmp/codex-home");
    let msg = classify_chatgpt_auth_error(home, &ChatGptAuthProblem::Expired);
    assert!(msg.to_lowercase().contains("expired"), "{msg}");
    assert!(msg.contains("codex login"), "{msg}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p defra-agent classifies_ -- --nocapture`
Expected: FAIL — `cannot find type ChatGptAuthProblem` / `function classify_chatgpt_auth_error not found`.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `crates/defra-agent/src/chatgpt_codex.rs` (after the imports — no new crate import needed):

```rust
/// A user-actionable classification of why ChatGPT OAuth could not be used.
#[derive(Debug, Clone)]
pub enum ChatGptAuthProblem {
    /// No Codex credentials found in the resolved home.
    Missing,
    /// Credentials exist but are not ChatGPT OAuth (e.g. an API key).
    /// `found_mode` is the stringified `AuthMode` (kept local to avoid a
    /// `codex-app-server-protocol` dependency).
    WrongMode { found_mode: String },
    /// Credentials are ChatGPT OAuth but the token is expired/revoked.
    Expired,
    /// Anything else, with the underlying message.
    Other(String),
}

/// Render an actionable, multi-line message for a ChatGPT auth failure.
pub fn classify_chatgpt_auth_error(
    codex_home: &std::path::Path,
    problem: &ChatGptAuthProblem,
) -> String {
    let home = codex_home.display();
    match problem {
        ChatGptAuthProblem::Missing => format!(
            "No Codex credentials found in {home}.\n\
             To use the ChatGPT subscription backend, sign in with the Codex CLI \
             (`codex login`), or point DEFRA_CODEX_HOME at a home that already has \
             ChatGPT OAuth credentials."
        ),
        ChatGptAuthProblem::WrongMode { found_mode } => format!(
            "Credentials in {home} are {found_mode}, but the ChatGPT subscription \
             backend needs ChatGPT OAuth.\n\
             Run `codex login` to establish a ChatGPT session, or select an \
             API-key backend instead."
        ),
        ChatGptAuthProblem::Expired => format!(
            "ChatGPT OAuth credentials in {home} are expired or revoked.\n\
             Re-authenticate with `codex login` to refresh the session."
        ),
        ChatGptAuthProblem::Other(detail) => format!(
            "ChatGPT auth in {home} could not be used: {detail}"
        ),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p defra-agent classifies_ -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/chatgpt_codex.rs
git commit -m "feat(codex): actionable ChatGPT auth error classifier (#339)"
```

---

## Task 2: Per-request bearer refresh via a `BearerSource`

**Files:**
- Modify: `crates/defra-agent/src/chatgpt_codex.rs`
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from prior tasks.
- Produces:
  - `pub trait BearerSource: Send + Sync` with
    `fn current_bearer(&self) -> impl std::future::Future<Output = Result<String>> + Send;`
  - `ChatGptCodexHttpClient<S: BearerSource>` holding `inner: ReqwestClient` and
    `bearer: Option<Arc<S>>`, with `pub fn new(bearer: Arc<S>) -> Self` and a **hand-written
    `Default` impl** (bearer `None`) so the type satisfies rig's `H: Default` build bound.
  - An `AuthManagerBearer(Arc<AuthManager>)` newtype implementing `BearerSource` (calls
    `auth().await` then `get_token()`).
  Consumed by Task 3.

**Why a trait:** `AuthManager` needs a live network/home to refresh, so it can't be exercised in a unit test. The trait lets us unit-test the per-request injection with a fake that returns a chosen token, while the production impl wraps `AuthManager`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests`:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingBearer {
    token: String,
    calls: AtomicUsize,
}
impl BearerSource for CountingBearer {
    async fn current_bearer(&self) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.token.clone())
    }
}

#[tokio::test]
async fn injects_fresh_bearer_on_each_request() {
    let bearer = std::sync::Arc::new(CountingBearer {
        token: "tok-123".to_string(),
        calls: AtomicUsize::new(0),
    });
    let client = ChatGptCodexHttpClient::new(bearer.clone());

    // Build a minimal /responses request with a STALE Authorization header
    // that the client must overwrite.
    let req = rig::http_client::Request::builder()
        .method("POST")
        .uri("https://example.com/v1/responses")
        .header("authorization", "Bearer STALE")
        .body(bytes::Bytes::from_static(b"{}"))
        .unwrap();

    let prepared = client.prepare_for_test(req).await.unwrap();
    let auth = prepared
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(auth, "Bearer tok-123", "stale bearer was replaced");
    assert_eq!(bearer.calls.load(Ordering::SeqCst), 1, "refreshed once per request");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p defra-agent injects_fresh_bearer_on_each_request`
Expected: FAIL — `BearerSource` / `ChatGptCodexHttpClient::new` / `prepare_for_test` not found, or the existing unit-struct `ChatGptCodexHttpClient` is not generic.

- [ ] **Step 3: Write minimal implementation**

In `crates/defra-agent/src/chatgpt_codex.rs`, add the trait and rework the client. Add imports `use std::sync::Arc;` and `use codex_login::AuthManager;` if missing.

```rust
/// Supplies a current OAuth bearer, refreshing it as needed.
pub trait BearerSource: Send + Sync {
    fn current_bearer(&self) -> impl std::future::Future<Output = Result<String>> + Send;
}

/// Production [`BearerSource`] backed by Codex's [`AuthManager`], whose `auth()`
/// proactively refreshes a near-expiry managed token before returning it.
#[derive(Clone)]
pub struct AuthManagerBearer(pub Arc<AuthManager>);

impl BearerSource for AuthManagerBearer {
    async fn current_bearer(&self) -> Result<String> {
        let auth = self
            .0
            .auth()
            .await
            .context("no Codex ChatGPT auth available")?;
        auth.get_token()
            .context("ChatGPT auth did not expose a bearer token")
    }
}
```

Replace the existing `ChatGptCodexHttpClient` definition (the `#[derive(Clone, Debug, Default)] struct { inner: ReqwestClient }`) and its `inject_required_instructions` helper with a generic form. Note `bearer: Option<Arc<S>>` + the manual `Default` (deriving `Default` would wrongly require `S: Default`):

```rust
pub struct ChatGptCodexHttpClient<S: BearerSource> {
    inner: ReqwestClient,
    bearer: Option<Arc<S>>,
}

// Manual Clone: `#[derive(Clone)]` would wrongly require `S: Clone`. We only
// clone the `Arc`, so no bound on `S` is needed. (The HttpClientExt impl and
// rig both clone this client; the test fake `CountingBearer` is not Clone.)
impl<S: BearerSource> Clone for ChatGptCodexHttpClient<S> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), bearer: self.bearer.clone() }
    }
}

// Manual Default: needed so the type satisfies rig's `H: Default` build bound,
// WITHOUT requiring `S: Default`. The default (bearer = None) is never the value
// rig actually uses — we always pass a `new(..)` instance via `.http_client(..)`.
impl<S: BearerSource> Default for ChatGptCodexHttpClient<S> {
    fn default() -> Self {
        Self { inner: ReqwestClient::default(), bearer: None }
    }
}

impl<S: BearerSource> ChatGptCodexHttpClient<S> {
    pub fn new(bearer: Arc<S>) -> Self {
        Self { inner: ReqwestClient::default(), bearer: Some(bearer) }
    }

    /// Resolve a fresh `Authorization: Bearer ...` header value.
    async fn fresh_auth_header(&self) -> http_client::Result<HeaderValue> {
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            http_client::Error::Instance(
                anyhow::anyhow!("ChatGptCodexHttpClient used without a configured BearerSource")
                    .into(),
            )
        })?;
        let token = bearer
            .current_bearer()
            .await
            .map_err(|e| http_client::Error::Instance(e.into()))?;
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| http_client::Error::Instance(anyhow::Error::from(e).into()))
    }

    /// Patch body (existing behavior) then overwrite Authorization with a fresh bearer.
    async fn prepare(&self, req: Request<Bytes>) -> http_client::Result<Request<Bytes>> {
        let req = Self::inject_required_instructions(req);
        let value = self.fresh_auth_header().await?;
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("authorization", value);
        Ok(Request::from_parts(parts, body))
    }

    fn inject_required_instructions(req: Request<Bytes>) -> Request<Bytes> {
        let (parts, body) = req.into_parts();
        let mut body = body;
        if parts.uri.path().ends_with("/responses") {
            if let Some(patched) = patch_instructions_body(&body) {
                body = patched;
            }
        }
        Request::from_parts(parts, body)
    }

    #[cfg(test)]
    pub async fn prepare_for_test(
        &self,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        self.prepare(req).await
    }
}
```

Update the `impl HttpClientExt for ChatGptCodexHttpClient` block to be generic and to inject a fresh bearer in **all three** send methods (the multipart path must not fall through with the placeholder api-key). The bodies become:

```rust
impl<S: BearerSource + 'static> HttpClientExt for ChatGptCodexHttpClient<S> {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            send_reqwest(inner, req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        async move {
            // Inject a fresh bearer (header-only; multipart body is not patched).
            let value = this.fresh_auth_header().await?;
            let (mut parts, body) = req.into_parts();
            parts.headers.insert("authorization", value);
            let req = Request::from_parts(parts, body);
            HttpClientExt::send_multipart(&inner, req).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            HttpClientExt::send_streaming(&inner, req).await
        }
    }
}
```

> Note: `send_streaming`'s return future is not `'static` in the trait; capture `this`/`inner` by move as shown. If the borrow checker rejects the non-`'static` future capturing `self`, clone `this` and `inner` before the `async move` (already done) — they own their data.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p defra-agent injects_fresh_bearer_on_each_request`
Expected: PASS.

- [ ] **Step 5: Verify the crate still builds (the generic ripples to call sites)**

Run: `cargo build -p defra-agent`
Expected: FAILs only at `build_responses_client` (it constructs `ChatGptCodexHttpClient::default()` and names the old non-generic type). That is Task 3. Do **not** fix here. Note: `backend_provider.rs` uses `load_default_chatgpt_auth` + `build_chatgpt_codex_headers` with its own reqwest `/models` probe and does **not** construct `ChatGptCodexHttpClient`, so it must not appear in the errors; if it does, you changed more than intended.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/chatgpt_codex.rs
git commit -m "feat(codex): refresh OAuth bearer per request via BearerSource (#339)"
```

---

## Task 3: Precise auth resolution + wire the refreshing client through construction

**Files:**
- Modify: `crates/defra-agent/src/chatgpt_codex.rs` (`resolve_chatgpt_auth`, `build_responses_client`)
- Modify: `crates/defra-agent/src/agent/runtime/context.rs:203`, `crates/defra-agent/src/oneshot.rs:139` (only if a type annotation names the old client type)

**Interfaces:**
- Consumes: `AuthManagerBearer`, `ChatGptCodexHttpClient::new` (Task 2); `ChatGptAuthProblem`, `classify_chatgpt_auth_error` (Task 1).
- Produces:
  - `pub async fn resolve_chatgpt_auth(codex_home: &std::path::Path) -> std::result::Result<(std::sync::Arc<AuthManager>, CodexAuth), ChatGptAuthProblem>` — builds the `AuthManager`, returns precise `Missing` / `WrongMode` problems. Consumed by Task 4.
  - `build_responses_client(endpoint: &str) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient<AuthManagerBearer>>>` (async, same name/arg as today).

- [ ] **Step 1: Add `resolve_chatgpt_auth` with precise classification**

In `chatgpt_codex.rs` add (imports `use std::sync::Arc;`, `use codex_login::{AuthManager, AuthCredentialsStoreMode, CodexAuth};` — `CodexAuth`/`AuthManager`/`AuthCredentialsStoreMode` are already imported at the top; reuse them):

```rust
/// Build an AuthManager for `codex_home` and resolve usable ChatGPT OAuth,
/// classifying the failure precisely so callers can give actionable guidance.
/// Note: `AuthManager::auth()` may proactively refresh and persist the managed
/// token (Codex's normal behavior), so this is not a read-only operation.
pub async fn resolve_chatgpt_auth(
    codex_home: &std::path::Path,
) -> std::result::Result<(Arc<AuthManager>, CodexAuth), ChatGptAuthProblem> {
    let manager = Arc::new(
        AuthManager::new(
            codex_home.to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::Auto,
            /*chatgpt_base_url*/ None,
        )
        .await,
    );
    // `auth()` attempts a proactive refresh; on failure it LOGS and returns the
    // STALE auth rather than erroring. So we must separately ask the manager
    // whether the last refresh for this auth failed permanently (expired/revoked).
    let auth = manager.auth().await.ok_or(ChatGptAuthProblem::Missing)?;
    if !auth.is_chatgpt_auth() {
        return Err(ChatGptAuthProblem::WrongMode {
            found_mode: format!("{:?}", auth.auth_mode()),
        });
    }
    if manager.refresh_failure_for_auth(&auth).is_some() {
        return Err(ChatGptAuthProblem::Expired);
    }
    Ok((manager, auth))
}
```

> `refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError>` is a real `AuthManager` method (`codex-rs/login/src/auth/manager.rs:1413`). We only test `.is_some()`, so no new type import is needed.

- [ ] **Step 2: Rebuild `build_responses_client` on top of it**

Replace the body of `build_responses_client`:

```rust
pub async fn build_responses_client(
    endpoint: &str,
) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient<AuthManagerBearer>>> {
    let codex_home = resolve_codex_home(None)?;
    let (manager, auth) = resolve_chatgpt_auth(&codex_home)
        .await
        .map_err(|problem| anyhow::anyhow!(classify_chatgpt_auth_error(&codex_home, &problem)))?;

    let headers = build_chatgpt_codex_headers(&auth)?;
    let endpoint = normalize_endpoint(endpoint);
    let http = ChatGptCodexHttpClient::new(Arc::new(AuthManagerBearer(manager)));
    // The api_key here is a placeholder — the http client overwrites Authorization
    // with a freshly-refreshed bearer on every request (Task 2). The existing
    // Default-bounded helper works because ChatGptCodexHttpClient<S>: Default.
    crate::inference_http::build_openai_responses_client(
        "chatgpt-oauth-managed",
        &endpoint,
        http,
        headers,
    )
    .context("building ChatGPT Codex Responses client")
}
```

Keep `load_default_chatgpt_auth` / `load_chatgpt_auth` (the probe still reads account email/plan via them, or migrate it in Task 4).

- [ ] **Step 3: Build and verify call sites compile**

Run: `cargo build -p defra-agent`
Expected: PASS. `context.rs:203` and `oneshot.rs:139` call `build_responses_client(&behavior.backend_endpoint).await` behind the generic `run_behavior_with_client` / one-shot path; the call form is unchanged. Only if an explicit type annotation names the old client type, update it to `ChatGptCodexHttpClient<AuthManagerBearer>`.

- [ ] **Step 4: Run the package suite**

Run: `cargo test -p defra-agent`
Expected: PASS (existing `patches_rig_responses_body_for_chatgpt_codex` + Task 1/2 tests included).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/chatgpt_codex.rs crates/defra-agent/src/agent/runtime/context.rs crates/defra-agent/src/oneshot.rs
git commit -m "feat(codex): precise auth resolution + refreshing Responses client (#339)"
```

---

## Task 4: Actionable diagnostics in `codex-auth-probe`

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_auth_probe.rs`

**Interfaces:**
- Consumes: `defra_agent::chatgpt_codex::{classify_chatgpt_auth_error, ChatGptAuthProblem, resolve_codex_home, resolve_chatgpt_auth}` (Tasks 1, 3).

- [ ] **Step 1: Resolve auth with precise classification**

In `codex_auth_probe`, replace the bare `?` on the auth load with `resolve_chatgpt_auth`, mapping the precise problem to actionable guidance. The current line:

```rust
let auth = defra_agent::chatgpt_codex::load_chatgpt_auth(codex_home.clone()).await?;
```

becomes:

```rust
let (_manager, auth) = match defra_agent::chatgpt_codex::resolve_chatgpt_auth(&codex_home).await {
    Ok(resolved) => resolved,
    Err(problem) => {
        let guidance =
            defra_agent::chatgpt_codex::classify_chatgpt_auth_error(&codex_home, &problem);
        anyhow::bail!("{guidance}");
    }
};
```

This yields precise `Missing` / `WrongMode` guidance (both contain `codex login`), not a generic `Other`. The rest of the probe (account email, plan, models request) is unchanged — `auth` is the same `CodexAuth` value it used before.

- [ ] **Step 2: Map a non-success models probe (expired/revoked at the API) to guidance**

The probe's `if !status.is_success()` branch currently bails with the raw body. When the status is 401/403, append re-auth guidance:

```rust
if !status.is_success() {
    let body = String::from_utf8_lossy(&body);
    if status.as_u16() == 401 || status.as_u16() == 403 {
        let guidance = defra_agent::chatgpt_codex::classify_chatgpt_auth_error(
            &codex_home,
            &defra_agent::chatgpt_codex::ChatGptAuthProblem::Expired,
        );
        bail!("models request failed with HTTP {status}: {body}\n{guidance}");
    }
    bail!("models request failed with HTTP {status}: {body}");
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p defra-agent-cli`
Expected: PASS.

- [ ] **Step 4: Test the message wiring with a missing home (in-file unit test)**

`defra-agent-cli` is **bin-only** (`Cargo.toml:7` `autobins = false`, a single `[[bin]]`), so a `tests/*.rs` integration test cannot `use defra_agent_cli::...` — there is no library target. Add the test **inside** `codex_auth_probe.rs` instead, where `codex_auth_probe` and `CodexAuthProbeArgs` are in scope without any visibility change:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::CodexAuthProbeArgs;

    // Resolving auth in a guaranteed-empty home yields Missing -> actionable guidance,
    // and returns BEFORE any network call.
    #[tokio::test]
    async fn probe_missing_home_is_actionable() {
        let tmp = std::env::temp_dir().join("defra-codex-probe-missing-xyz");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let args = CodexAuthProbeArgs {
            codex_home: Some(tmp.clone()),
            max_models: 5,
        };
        let err = codex_auth_probe(args)
            .await
            .expect_err("no auth in empty home");
        let msg = format!("{err:#}");
        assert!(msg.contains("codex login"), "actionable: {msg}");
    }
}
```

> Confirm `CodexAuthProbeArgs`'s exact fields in `args.rs:170` (`codex_home: Option<PathBuf>`, `max_models` — referenced at `codex_auth_probe.rs:76`). If `DEFRA_CODEX_HOME` is set in the dev environment it overrides the explicit arg only when the arg is `None`; here `codex_home: Some(tmp)` wins (`resolve_codex_home` returns the explicit path first), so the test is hermetic.

- [ ] **Step 5: Run**

Run: `cargo test -p defra-agent-cli probe_missing_home_is_actionable`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_auth_probe.rs
git commit -m "feat(codex): actionable auth-probe diagnostics for missing/expired (#339)"
```

---

## Task 5: Credential-home docs for fleet/remote

**Files:**
- Create: `docs/backends.md`
- Modify: `crates/defra-agent-cli/src/commands/diagnose/mod.rs` (add `checks.chatgpt_auth` to the JSON output)

**Interfaces:**
- Consumes: `defra_agent::chatgpt_codex::{resolve_codex_home, resolve_chatgpt_auth, classify_chatgpt_auth_error}`.

- [ ] **Step 1: Write the backends doc (Codex section)**

Create `docs/backends.md`:

```markdown
# Backends

> This page is the home for the committed backend support matrix (#509). It
> starts with the ChatGPT-subscription (OAuth) backend; provider rows are added
> as #509 lands each one.

## ChatGPT subscription (ChatGptCodex, OAuth)

Use your existing ChatGPT/Codex subscription instead of an API key.

### Setup
1. Sign in with the Codex CLI (`codex login`) so credentials exist in your Codex home.
2. Configure a backend with `provider_kind = ChatGptCodex`.
3. Verify: `defra-agent codex-auth-probe` (prints account, plan, and reachable models).

### Credential home: `CODEX_HOME` vs `DEFRA_CODEX_HOME`
- The Codex CLI reads/writes `CODEX_HOME` (default `~/.codex`).
- Defra Agent reads `DEFRA_CODEX_HOME` first, then falls back to `~/.codex`.
- **Defra Agent does not create, relocate, or clobber your Codex credentials.**
  It does, however, perform Codex's normal proactive token refresh, which updates
  the managed token in the configured store — the same write the Codex CLI makes.
  Your login is never replaced or moved; only the refreshed token is persisted.

### Fleet / remote
- A remote/fleet node needs its **own** credential home that is **readable and
  writable by the runtime user** (token refresh persists the renewed token to the
  store, so a read-only home will eventually fail on expiry); it does not
  share the operator's laptop `~/.codex`. Set `DEFRA_CODEX_HOME` on the node to a
  home provisioned with ChatGPT OAuth credentials.
- The Codex *frontend* (the `defra-agent codex` TUI) and the *server* credential
  home are independent: a remote frontend connecting to a node does not require
  the node to share the frontend's `CODEX_HOME`.

### Token refresh
- The OAuth bearer is refreshed **per request** via Codex's `AuthManager`, so
  long-running sessions do not fail on token expiry. Near-expiry tokens are
  proactively refreshed and persisted to the managed store (Codex's own behavior).

### Diagnostics
- Missing, wrong-mode (API-key), or expired credentials produce actionable errors
  from `codex-auth-probe` and `diagnose`, naming the home and the `codex login` fix.
```

- [ ] **Step 2: Add a structured `chatgpt_auth` check to `diagnose`**

`diagnose` is **JSON-only**: it builds one `serde_json::json!({...})` value with a `checks` object and ends with `print_json(&output)` (`diagnose/mod.rs:167-199`). A bare `println!` would corrupt the JSON. Instead, compute a `chatgpt_auth_check` Value *before* the `json!` block and add it under `checks`.

Just before the `let output = json!({...})` block (around `diagnose/mod.rs:160`), add:

```rust
let chatgpt_auth_check = match defra_agent::chatgpt_codex::resolve_codex_home(None) {
    Ok(home) => match defra_agent::chatgpt_codex::resolve_chatgpt_auth(&home).await {
        Ok(_) => json!({ "ok": true, "codex_home": home.display().to_string() }),
        Err(problem) => json!({
            "ok": false,
            "codex_home": home.display().to_string(),
            "guidance": defra_agent::chatgpt_codex::classify_chatgpt_auth_error(&home, &problem),
        }),
    },
    Err(err) => json!({ "ok": false, "error": err.to_string() }),
};
```

Then add one line inside the `"checks": { ... }` object (alongside `"backends": backend_reports,`):

```rust
            "chatgpt_auth": chatgpt_auth_check,
```

Keep it non-fatal — `diagnose` still returns `Ok(())` / exits 0 when Codex auth is simply absent (`ok: false` is reported, not bailed).

- [ ] **Step 3: Build**

Run: `cargo build -p defra-agent-cli`
Expected: PASS.

- [ ] **Step 4: Smoke-run diagnose and assert valid JSON with the new key**

Run: `cargo run -p defra-agent-cli -- diagnose 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'chatgpt_auth' in d['checks'], d['checks'].keys(); print('ok:', d['checks']['chatgpt_auth'])"`
Expected: prints `ok: {...}`; the command exits 0 and stdout is still valid JSON (no corruption). (If `diagnose` requires a configured home/graphql to run, pass the same flags the existing diagnose tests use; the assertion is that output remains parseable JSON containing `checks.chatgpt_auth`.)

- [ ] **Step 5: Commit**

```bash
git add docs/backends.md crates/defra-agent-cli/src/commands/diagnose/mod.rs
git commit -m "docs(codex): credential-home + fleet/remote guidance; diagnose chatgpt_auth check (#339)"
```

---

## Task 6: Full-suite gate and #339 acceptance check

**Files:** none (verification only)

- [ ] **Step 1: Gate both packages**

Run: `cargo test -p defra-agent && cargo test -p defra-agent-cli`
Expected: PASS, no ignored-by-accident regressions.

- [ ] **Step 2: Lints/format**

Run: `cargo fmt --all && cargo clippy -p defra-agent -p defra-agent-cli --all-targets`
Expected: clean (no new warnings from the changed files).

- [ ] **Step 3: Manual acceptance against #339 criteria**

Confirm each #339 acceptance criterion by inspection/run:
- A user with a Codex subscription can select `ChatGptCodex` and it works predictably → `codex-auth-probe` succeeds and a turn completes without a mid-session 401 (per-request refresh, Task 2/3).
- Remote/fleet works without sharing the frontend credential home → documented in `docs/backends.md` (Task 5); runtime reads `DEFRA_CODEX_HOME`.
- Missing/wrong-mode/expired auth produces actionable CLI errors → Task 4 + Task 1 classifier.

- [ ] **Step 4: Commit any fmt/clippy fixups**

```bash
git add -A
git commit -m "chore(codex): fmt + clippy for #339 finish"
```

---

## Self-Review notes (for the executor)

- **Spec coverage:** This plan implements slice 1 of the #509 spec (`docs/superpowers/specs/2026-06-19-responses-finalize-multiprovider-design.md`). Slices 2–5 (`openai_wire_api` field, composable recorder/harness, Anthropic+Gemini, matrix doc) are **separate plans**, written when slice 1 lands — Task 2's generic `ChatGptCodexHttpClient<S>` is the seam the slice-3 recorder composes under (`SessionTagging<ChatGptCodex<Recorder<Reqwest>>>`).
- **Resolved from review (verified against source):** rig's `Client::builder().build()` requires `H: Default` (`client/mod.rs:602`) → handled by `ChatGptCodexHttpClient<S>: Default` (Task 2), no new builder. `codex_login::AuthMode` is not re-exported and `codex-app-server-protocol` is not a direct dep → `WrongMode` stores a `String` (Task 1). `AuthManager::auth()` refresh **persists** the managed token (`manager.rs:1886`) → docs/constraints say "may update during refresh," not "read-only." All three send variants inject the fresh bearer (Task 2).
- **Remaining compile-time confirmation** (one, flagged inline): `CodexAuthProbeArgs` / `codex_auth_probe` visibility for the Task 4 integration test (fall back to an in-file `#[cfg(test)]` unit test if not `pub`). A boundary lookup, not a design gap.
- **No DefraDB/Lean/schema changes** — auth plumbing only.
