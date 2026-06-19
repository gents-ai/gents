# ChatGPT-OAuth Finish (#339) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `ChatGptCodex` (ChatGPT-subscription-over-OAuth) backend production-usable: refresh the OAuth bearer per request so long sessions don't 401, turn auth failures into actionable CLI errors, and document the fleet/remote credential-home behavior.

**Architecture:** Today `chatgpt_codex::build_responses_client` reads `CodexAuth::get_token()` **once** and bakes a static bearer into the rig client (`context.rs:203`, `oneshot.rs:139`); a long multi-turn loop outlives the token and 401s. We introduce a `BearerSource` trait whose `Arc<AuthManager>` impl calls `auth().await` per request (which proactively refreshes near-expiry), hold it inside `ChatGptCodexHttpClient`, and overwrite the `Authorization` header on every outbound request. Auth errors are classified into missing / wrong-mode / expired with setup guidance, surfaced by the existing `codex auth-probe` and `diagnose` commands. No DefraDB/schema/Lean change — this is auth plumbing on an existing seam.

**Tech Stack:** Rust, `codex-login` (`AuthManager`, `CodexAuth`, `AuthMode`, `RefreshTokenError`), rig-core `HttpClientExt`, anyhow, tokio.

## Global Constraints

- **`tracing`, never `println`** in runtime/library code (`crates/defra-agent`). CLI command *user-facing* stdout (`crates/defra-agent-cli/src/commands/**`) uses `println!` by existing convention — match the surrounding command.
- **Gate with the full package suite** (`cargo test -p defra-agent` and `cargo test -p defra-agent-cli`), never `--lib` — integration tests are separate compile units.
- **`graphql::escape_graphql_string()`** for anything interpolated into a GraphQL string. (No GraphQL is written in this plan, but honor it if a step adds any.)
- **Never emit `[]` in a DefraDB mutation** — emit `null`. (No mutations here.)
- **No Lean/spec change required.** This changes no legal transition, no invariant, and not what the model is fed — auth header material only. Do not add proof obligations.
- **Do not write into the user's Codex home.** All Codex auth access in this runtime is **read + in-memory refresh** via `AuthManager`; never create, relocate, or overwrite `auth.json` in `~/.codex` / `CODEX_HOME`.

---

## File Structure

- `crates/defra-agent/src/chatgpt_codex.rs` — **modify.** Add `BearerSource` trait + `AuthManager` impl; add `classify_chatgpt_auth_error`; rework `ChatGptCodexHttpClient` to hold a `BearerSource` and inject a fresh bearer per request; make `build_responses_client` construct an `Arc<AuthManager>`.
- `crates/defra-agent/src/inference_http.rs` — **modify.** Add `build_openai_responses_client_with` that drops the `H: Default` bound (the refreshing client can't be `Default`); keep the existing `Default`-bound helper for `SessionTaggingHttpClient`.
- `crates/defra-agent/src/agent/runtime/context.rs:203` — **modify.** Call site is already `async` and uses `build_responses_client`; no signature change, just confirm it compiles against the new internals.
- `crates/defra-agent/src/oneshot.rs:139` — **modify.** Same: confirm against new internals.
- `crates/defra-agent-cli/src/commands/codex_auth_probe.rs` — **modify.** Route load failures through `classify_chatgpt_auth_error` for actionable messages.
- `crates/defra-agent-cli/src/commands/diagnose.rs` — **modify.** Add a ChatGPT-auth check line using the same classifier.
- `docs/backends.md` — **create.** Document `ChatGptCodex` setup, `DEFRA_CODEX_HOME` vs `CODEX_HOME`, and fleet/remote credential-home behavior. (This is also the file the broader #509 matrix lands in later — start it here with the Codex section.)
- Tests live inline in `chatgpt_codex.rs` `#[cfg(test)]` (unit) and `crates/defra-agent-cli/tests/` if a command-level test fits.

---

## Task 1: Auth-error classifier with actionable messages

**Files:**
- Modify: `crates/defra-agent/src/chatgpt_codex.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Produces: `pub enum ChatGptAuthProblem { Missing, WrongMode(AuthMode), Expired, Other(String) }` and
  `pub fn classify_chatgpt_auth_error(codex_home: &std::path::Path, problem: &ChatGptAuthProblem) -> String`
  (returns a multi-line, user-facing, actionable message). Consumed by Task 4 (CLI) and Task 2 (load path).

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
        &ChatGptAuthProblem::WrongMode(AuthMode::ApiKey),
    );
    assert!(msg.contains("ChatGPT"), "asks for ChatGPT OAuth: {msg}");
    assert!(msg.to_lowercase().contains("apikey") || msg.contains("ApiKey"),
        "names what was found: {msg}");
}

#[test]
fn classifies_expired_with_reauth_guidance() {
    let home = std::path::Path::new("/tmp/codex-home");
    let msg = classify_chatgpt_auth_error(home, &ChatGptAuthProblem::Expired);
    assert!(msg.to_lowercase().contains("expired"), "{msg}");
    assert!(msg.contains("codex login"), "{msg}");
}
```

Add the import at the top of the test module if not present: `use codex_login::AuthMode;` (re-exported — verify with `cargo doc`; if not re-exported from `codex_login`, import via the path the crate exposes, e.g. `codex_app_server_protocol::AuthMode`, matching the existing `auth_mode()` return type).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p defra-agent classifies_ -- --nocapture`
Expected: FAIL — `cannot find type ChatGptAuthProblem` / `function classify_chatgpt_auth_error not found`.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `crates/defra-agent/src/chatgpt_codex.rs` (after the imports; add `use codex_login::AuthMode;` to the crate imports — confirm the exact path against `auth.auth_mode()`'s return type):

```rust
/// A user-actionable classification of why ChatGPT OAuth could not be used.
#[derive(Debug, Clone)]
pub enum ChatGptAuthProblem {
    /// No Codex credentials found in the resolved home.
    Missing,
    /// Credentials exist but are not ChatGPT OAuth (e.g. an API key).
    WrongMode(AuthMode),
    /// Credentials are ChatGPT OAuth but the refresh token is expired/revoked.
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
        ChatGptAuthProblem::WrongMode(mode) => format!(
            "Credentials in {home} are {mode:?}, but the ChatGPT subscription backend \
             needs ChatGPT OAuth.\n\
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
  - `ChatGptCodexHttpClient<S: BearerSource>` holding `inner: ReqwestClient` and `bearer: Arc<S>`,
    with `pub fn new(bearer: Arc<S>) -> Self`.
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

Replace the existing `ChatGptCodexHttpClient` definition (the `#[derive(Default)] struct { inner: ReqwestClient }`) and its `inject_required_instructions` helper with a generic form:

```rust
#[derive(Clone)]
pub struct ChatGptCodexHttpClient<S: BearerSource> {
    inner: ReqwestClient,
    bearer: Arc<S>,
}

impl<S: BearerSource> ChatGptCodexHttpClient<S> {
    pub fn new(bearer: Arc<S>) -> Self {
        Self { inner: ReqwestClient::default(), bearer }
    }

    /// Patch body (existing behavior) then overwrite Authorization with a fresh bearer.
    async fn prepare(&self, req: Request<Bytes>) -> http_client::Result<Request<Bytes>> {
        let req = Self::inject_required_instructions(req);
        let token = self
            .bearer
            .current_bearer()
            .await
            .map_err(|e| http_client::Error::Instance(e.into()))?;
        let (mut parts, body) = req.into_parts();
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| http_client::Error::Instance(anyhow::Error::from(e).into()))?;
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

Update the `impl HttpClientExt for ChatGptCodexHttpClient` block to be generic and to call `prepare` (async) inside the returned futures. The `send`/`send_streaming` bodies become:

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
        async move { HttpClientExt::send_multipart(&inner, req).await }
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

## Task 3: Wire the refreshing client through construction and call sites

**Files:**
- Modify: `crates/defra-agent/src/chatgpt_codex.rs` (`build_responses_client`)
- Modify: `crates/defra-agent/src/inference_http.rs` (add non-`Default` builder)
- Modify: `crates/defra-agent/src/agent/runtime/context.rs:203`, `crates/defra-agent/src/oneshot.rs:139` (only if the return type name changed)

**Interfaces:**
- Consumes: `AuthManagerBearer`, `ChatGptCodexHttpClient::new` (Task 2).
- Produces: `build_responses_client(endpoint: &str) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient<AuthManagerBearer>>>` (async, same name/arg as today).

- [ ] **Step 1: Add a non-`Default` responses-client builder**

The existing `build_openai_responses_client<H: Default + HttpClientExt>` requires `H: Default`; the refreshing client holds an `Arc<AuthManager>` and cannot be `Default`. Add a sibling in `crates/defra-agent/src/inference_http.rs` without that bound:

```rust
pub(crate) fn build_openai_responses_client_with<H>(
    api_key: &str,
    base_url: &str,
    http_client: H,
    http_headers: HeaderMap,
) -> Result<rig::providers::openai::Client<H>>
where
    H: HttpClientExt,
{
    rig::providers::openai::Client::builder()
        .api_key(api_key)
        .base_url(base_url)
        .http_headers(http_headers)
        .http_client(http_client)
        .build()
        .context("building OpenAI Responses client")
}
```

> If `cargo build` shows the rig builder itself requires `H: Default`, instead keep one helper and add `impl<S: BearerSource + Default> Default for ChatGptCodexHttpClient<S>` is *not* viable (Arc<AuthManager> has no Default). In that case, construct the rig client inline in `build_responses_client` exactly as `build_openai_responses_client_with` does, bypassing the shared helper. Pick whichever compiles; both produce the same client.

- [ ] **Step 2: Rebuild `build_responses_client` to construct the AuthManager**

Replace the body of `build_responses_client` in `chatgpt_codex.rs`:

```rust
pub async fn build_responses_client(
    endpoint: &str,
) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient<AuthManagerBearer>>> {
    let codex_home = resolve_codex_home(None)?;
    let auth_manager = Arc::new(
        AuthManager::new(
            codex_home.clone(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::Auto,
            /*chatgpt_base_url*/ None,
        )
        .await,
    );

    // Resolve once up front for headers + a fast, actionable failure if auth is unusable.
    let auth = auth_manager
        .auth()
        .await
        .ok_or_else(|| anyhow::anyhow!(
            classify_chatgpt_auth_error(&codex_home, &ChatGptAuthProblem::Missing)
        ))?;
    if !auth.is_chatgpt_auth() {
        bail!(classify_chatgpt_auth_error(
            &codex_home,
            &ChatGptAuthProblem::WrongMode(auth.auth_mode())
        ));
    }

    let headers = build_chatgpt_codex_headers(&auth)?;
    let endpoint = normalize_endpoint(endpoint);
    let http = ChatGptCodexHttpClient::new(Arc::new(AuthManagerBearer(auth_manager)));
    // api_key is a placeholder: the http client overwrites Authorization per request.
    crate::inference_http::build_openai_responses_client_with(
        "chatgpt-oauth-managed",
        &endpoint,
        http,
        headers,
    )
    .context("building ChatGPT Codex Responses client")
}
```

Keep `load_default_chatgpt_auth` / `load_chatgpt_auth` (the probe still uses them).

- [ ] **Step 3: Build and verify call sites compile unchanged**

Run: `cargo build -p defra-agent`
Expected: PASS. `context.rs:203` and `oneshot.rs:139` call `build_responses_client(&behavior.backend_endpoint).await` and store the result behind the generic `run_behavior_with_client` / one-shot path; the concrete `H` type changed but the call form did not. If a turbofish or explicit type annotation at either site names the old client type, update it to `ChatGptCodexHttpClient<AuthManagerBearer>`.

- [ ] **Step 4: Run the package suite**

Run: `cargo test -p defra-agent`
Expected: PASS (existing `patches_rig_responses_body_for_chatgpt_codex` and Task 1/2 tests included).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/src/chatgpt_codex.rs crates/defra-agent/src/inference_http.rs crates/defra-agent/src/agent/runtime/context.rs crates/defra-agent/src/oneshot.rs
git commit -m "feat(codex): construct refreshing Responses client end-to-end (#339)"
```

---

## Task 4: Actionable diagnostics in `codex auth-probe`

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_auth_probe.rs`

**Interfaces:**
- Consumes: `defra_agent::chatgpt_codex::{classify_chatgpt_auth_error, ChatGptAuthProblem, resolve_codex_home, load_chatgpt_auth}` (Task 1).

- [ ] **Step 1: Map the load failure to an actionable message**

In `codex_auth_probe`, replace the bare `?` on `load_chatgpt_auth` so missing/wrong-mode/expired become actionable. The current line:

```rust
let auth = defra_agent::chatgpt_codex::load_chatgpt_auth(codex_home.clone()).await?;
```

becomes:

```rust
let auth = match defra_agent::chatgpt_codex::load_chatgpt_auth(codex_home.clone()).await {
    Ok(auth) => auth,
    Err(err) => {
        // load_chatgpt_auth already distinguishes missing vs wrong-mode in its
        // context; surface its message, then append setup guidance.
        let guidance = defra_agent::chatgpt_codex::classify_chatgpt_auth_error(
            &codex_home,
            &defra_agent::chatgpt_codex::ChatGptAuthProblem::Other(err.to_string()),
        );
        anyhow::bail!("{guidance}");
    }
};
```

> If `load_chatgpt_auth` is refactored to return a `ChatGptAuthProblem` directly, prefer that and pass the precise variant. For this task the `Other` wrapper is sufficient because `load_chatgpt_auth`'s own `context` strings already name the home and the wrong-mode case.

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

- [ ] **Step 4: Test the message wiring with a missing home**

Add `crates/defra-agent-cli/tests/codex_auth_probe_messages.rs`:

```rust
// Runs the probe against a guaranteed-empty home and asserts the actionable
// guidance is present in the error.
#[tokio::test]
async fn probe_missing_home_is_actionable() {
    let tmp = std::env::temp_dir().join("defra-codex-probe-missing-xyz");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let args = defra_agent_cli::cli::args::CodexAuthProbeArgs {
        codex_home: Some(tmp.clone()),
        max_models: 5,
    };
    let err = defra_agent_cli::commands::codex_auth_probe::codex_auth_probe(args)
        .await
        .expect_err("no auth in empty home");
    let msg = format!("{err:#}");
    assert!(msg.contains("codex login"), "actionable: {msg}");
}
```

> If `CodexAuthProbeArgs` / `codex_auth_probe` are not `pub` to the test crate, either mark them `pub(crate)` + add an integration shim, or convert this to a `#[cfg(test)]` unit test inside `codex_auth_probe.rs`. Confirm `max_models`'s exact field name/type in `args.rs` (it is referenced at `codex_auth_probe.rs:76`).

- [ ] **Step 5: Run**

Run: `cargo test -p defra-agent-cli probe_missing_home_is_actionable`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_auth_probe.rs crates/defra-agent-cli/tests/codex_auth_probe_messages.rs
git commit -m "feat(codex): actionable auth-probe diagnostics for missing/expired (#339)"
```

---

## Task 5: Credential-home docs for fleet/remote

**Files:**
- Create: `docs/backends.md`
- Modify: `crates/defra-agent-cli/src/commands/diagnose.rs` (one ChatGPT-auth status line)

**Interfaces:**
- Consumes: `defra_agent::chatgpt_codex::{resolve_codex_home, load_chatgpt_auth, classify_chatgpt_auth_error}`.

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
3. Verify: `defra-agent codex auth-probe` (prints account, plan, and reachable models).

### Credential home: `CODEX_HOME` vs `DEFRA_CODEX_HOME`
- The Codex CLI reads/writes `CODEX_HOME` (default `~/.codex`).
- Defra Agent reads `DEFRA_CODEX_HOME` first, then falls back to `~/.codex`.
- **Defra Agent never writes to the Codex home** — it reads credentials and
  refreshes managed tokens in memory only. Your Codex install is never clobbered.

### Fleet / remote
- A remote/fleet node needs its **own** readable credential home; it does not
  share the operator's laptop `~/.codex`. Set `DEFRA_CODEX_HOME` on the node to a
  home provisioned with ChatGPT OAuth credentials.
- The Codex *frontend* (the `defra-agent codex` TUI) and the *server* credential
  home are independent: a remote frontend connecting to a node does not require
  the node to share the frontend's `CODEX_HOME`.

### Token refresh
- The OAuth bearer is refreshed **per request** from the credential home's managed
  token, so long-running sessions do not fail on token expiry.

### Diagnostics
- Missing, wrong-mode (API-key), or expired credentials produce actionable errors
  from `codex auth-probe` and `diagnose`, naming the home and the `codex login` fix.
```

- [ ] **Step 2: Add a ChatGPT-auth line to `diagnose`**

In `crates/defra-agent-cli/src/commands/diagnose.rs`, add a check that prints the resolved Codex home and whether ChatGPT auth is usable (read-only; non-fatal). Locate where `diagnose` prints its sections and add:

```rust
{
    let codex_home = defra_agent::chatgpt_codex::resolve_codex_home(None);
    match codex_home {
        Ok(home) => match defra_agent::chatgpt_codex::load_chatgpt_auth(home.clone()).await {
            Ok(_) => println!("ChatGPT auth: OK ({})", home.display()),
            Err(err) => {
                let guidance = defra_agent::chatgpt_codex::classify_chatgpt_auth_error(
                    &home,
                    &defra_agent::chatgpt_codex::ChatGptAuthProblem::Other(err.to_string()),
                );
                println!("ChatGPT auth: not configured\n{guidance}");
            }
        },
        Err(err) => println!("ChatGPT auth: home unresolved: {err}"),
    }
}
```

> Match the surrounding `diagnose` output style (it may use a section helper rather than bare `println!`); follow the file's existing convention. Keep it non-fatal — `diagnose` must still exit 0 when Codex auth is simply absent.

- [ ] **Step 3: Build**

Run: `cargo build -p defra-agent-cli`
Expected: PASS.

- [ ] **Step 4: Smoke-run diagnose**

Run: `cargo run -p defra-agent-cli -- diagnose`
Expected: output includes a `ChatGPT auth:` line; exit code 0 regardless of whether auth is present.

- [ ] **Step 5: Commit**

```bash
git add docs/backends.md crates/defra-agent-cli/src/commands/diagnose.rs
git commit -m "docs(codex): credential-home + fleet/remote guidance; diagnose auth line (#339)"
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
- A user with a Codex subscription can select `ChatGptCodex` and it works predictably → `codex auth-probe` succeeds and a turn completes without a mid-session 401 (per-request refresh, Task 2/3).
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
- **External-API confirmations the executor must make at compile time** (each flagged inline): the exact import path of `AuthMode` (re-export vs `codex_app_server_protocol`); whether rig's client builder hard-requires `H: Default` (Task 3 Step 1 fallback); the `CodexAuthProbeArgs` field visibility for the Task 4 test. These are dependency-boundary lookups, not design gaps.
- **No DefraDB/Lean/schema changes** — auth plumbing only.
