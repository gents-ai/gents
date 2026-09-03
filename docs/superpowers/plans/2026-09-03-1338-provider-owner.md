# #1338 Provider Layer Single Owners Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Backend client construction, credential resolution, and the OAuth bearer HTTP wrapper each have one owner across the daemon, one-shot, and builder paths.

**Architecture:** (1) `llm::client::build_backend_client(kind, backend, credentials, timeout) -> Result<BackendClient>` owns the four-way `BackendProviderKind` match that `agent/runtime/context.rs:157-340` and `oneshot.rs:60-190` both contain today; both call it, and one-shot goes through the same admission wrapping as the daemon (or documents the exemption in one place). (2) `config::resolve_backend_api_key` is the one credential resolver; `backend_registry::resolved_api_key` (silent `None` on a missing env var) is deleted, and the prober uses the resolver. (3) A generic `oauth_http::BearerAuthHttpClient<S, H>` owns fresh-bearer injection, rejection detection, and invalidation; `chatgpt_codex.rs` and `xai_grok_oauth.rs` instantiate it with a rejection-status allowlist (`401|403` vs `401`), their identity-header injector, and their body patch; the copied `ensure_event_stream_content_type` and bootstrap sequences collapse to one each. (4) `oauth_credential::resolve_access_token_expiry` owns the `jwt_exp → expires_in → now+fallback` chain used by both xAI login and refresh.

**Tech Stack:** Rust.

**Spec:** GitHub issue #1338.

## Global Constraints

- Behavior preserved and pinned by tests: Codex invalidates its bearer on 401 and 403 (`chatgpt_codex.rs:823`), xAI only on 401 (`xai_grok_oauth.rs:622`); the difference becomes a parameter, and both existing tests keep passing against the shared wrapper.
- One decided semantics for a missing API-key env var: hard error with the backend and behavior named (today's `config.rs:270` behavior). Any caller that relied on silent `None` (the prober, the builder) now surfaces the error; say where in the report.
- No Lean change: `Proofs/BackendHealth` is untouched; this PR does not change what is available, only how clients and credentials are built.
- Net code deletion (about 150 duplicated lines per OAuth wrapper).

---

### Task 1: One client constructor for daemon and one-shot

**Files:**
- Create: `crates/gents/src/llm/backend_client.rs` (the match, with the OAuth-branch build timeout the daemon applies today)
- Modify: `crates/gents/src/agent/runtime/context.rs:157-340`, `crates/gents/src/oneshot.rs:60-190` (call it); `oneshot.rs` wraps the model in the `AdmissionRegistry` exactly as `context.rs` does, or a single `// exemption:` comment in the constructor explains why one-shot bypasses concurrency ceilings and a test pins that decision.
- Test: existing daemon/oneshot tests; add one table test that each `BackendProviderKind` yields the expected client type from the shared constructor.

- [ ] Implement, `cargo test -p gents --lib oneshot agent::runtime llm` green, commit — `runtime: one backend client constructor (#1338)`.

### Task 2: One credential resolver

**Files:**
- Modify: `crates/gents/src/backend_registry.rs:103-111` (delete `resolved_api_key`; callers use `AgentBehavior::resolve_backend_api_key` or a free `resolve_backend_api_key(backend: &InferenceBackend) -> Result<Option<String>>` in `config.rs` that the behavior method delegates to)
- Modify: `crates/gents/src/backend_health.rs` (prober), `crates/gents/src/agent/builder.rs`, and `agent.rs:323` / `builder.rs:498-586` (the two `InferenceBackend → AgentBehavior` field assemblers collapse onto one `backend_fields(&InferenceBackend)`)
- Test: unit tests for missing/empty env var → error naming the backend; existing assembler tests.

- [ ] Implement, green, commit — `runtime: one backend credential resolver and field assembler (#1338)`.

### Task 3: One OAuth bearer HTTP wrapper

**Files:**
- Create: `crates/gents/src/oauth_http.rs` (`BearerAuthHttpClient<S: BearerSource, H: IdentityHeaders>` with `send`, `send_multipart`, `send_streaming`; `rejection_statuses: &'static [u16]`; one `ensure_event_stream_content_type`; one `bootstrap_oauth_client(kind, credential_lookup, ...)`)
- Modify: `crates/gents/src/chatgpt_codex.rs:130-320, 553-604` and `crates/gents/src/xai_grok_oauth.rs:97-303, 463-538` (instantiate the shared wrapper; delete the copies; keep provider-specific body patches where they are)
- Modify: `crates/gents/src/rendered_request/transport.rs` doc comments that name the old type names
- Modify: `crates/gents/src/xai_oauth_refresh.rs:85-92`, `xai_oauth_login.rs:255-263` (call `oauth_credential::resolve_access_token_expiry`)
- Test: the two existing rejection tests (401/403 vs 401) pass unchanged; one wrapper unit test per exit (fresh bearer injected; rejection invalidates; non-rejection does not).

- [ ] Implement, `cargo test -p gents --lib chatgpt_codex xai_grok_oauth oauth_http oauth_credential xai_oauth` green, commit — `runtime: one bearer-auth HTTP wrapper for OAuth providers (#1338)`.

### Task 4: Gate
- [ ] `cargo test -p gents`, `cargo check --workspace --all-targets`, `cargo fmt --all --check`; grep: `grep -rn 'fn is_bearer_rejection\|fn ensure_event_stream_content_type' crates/gents/src` returns one definition each; net deletion check; CHANGELOG `### Fixed`: "A backend whose API-key environment variable is unset now fails loudly on every path (previously the builder and prober silently ran without a key)."
