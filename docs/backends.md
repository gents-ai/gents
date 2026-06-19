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
  the managed token in the configured store - the same write the Codex CLI makes.
  Your login is never replaced or moved; only the refreshed token is persisted.

### Fleet / remote

- A remote/fleet node needs its **own** credential home that is **readable and
  writable by the runtime user** (token refresh persists the renewed token to the
  store, so a read-only home will eventually fail on expiry); it does not share
  the operator's laptop `~/.codex`. Set `DEFRA_CODEX_HOME` on the node to a home
  provisioned with ChatGPT OAuth credentials.
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
