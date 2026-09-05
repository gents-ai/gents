# SPEC: Claude spike Phase 0 — Toolchain / isolation

**Historical (superseded 2026-09-04).** The proxy / process-seat design described here no longer exists; the shipped design is `docs/backends.md` § Claude subscription (agent-scoped `OAuthCredential`, Messages HTTP).

Parent: [`claude-subscription-spike.md`](./claude-subscription-spike.md)

## Goal

Make the experiment safe and reproducible **without** creating Claude usage.

## Non-goals

- Any `claude -p` / auth / network completion
- Billing claims
- gents provider code

## Constraints

- Obey the **Claude write gate** (parent doc). Phase 0 must not issue a write request unless the human explicitly asks for a non-local Claude command.
- Never write under production `~/.gents` or the user’s normal Claude config dir.
- Artifacts under `.scratch/claude-spike/` only (gitignored).

## Acceptance criteria

- [ ] `.scratch/claude-spike/{claude-config,workdir,logs,bin,proxy}` exists
- [ ] `.scratch/` is gitignored
- [ ] `claude` binary is on `PATH`; version recorded in `.scratch/claude-spike/logs/toolchain.txt`
- [ ] Local-only capability notes recorded: presence/absence of `--tools`, `--bare`, `--max-turns`, `--permission-mode`, `--no-session-persistence` (from `claude -p --help` / `claude --help` — **local help only**)
- [ ] `gents` / `gents-cli` runnable; GraphQL endpoint for spike work identified (or noted as Phase 3 concern)
- [ ] Env contract documented in stub scripts: `CLAUDE_CONFIG_DIR`, `GENTS_SPIKE_HOME`, strip list for `ANTHROPIC_*` and cloud-provider Anthropic vars
- [ ] No files created outside `.scratch/claude-spike/` and `docs/design-notes/`

## Exit

Phase 1 may begin when ACs above are checked. Still **no** Claude completion calls.

## Notes

Recording CLI help output is local and does not hit Anthropic. If a command would phone home, stop and open a Claude write request.
