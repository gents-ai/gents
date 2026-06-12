# Mac local-inference demo rework — design

Date: 2026-06-10
Branch: demo-mac-local-inference
Status: approved (Jack, 2026-06-10)

## Goal

One tight getting-started story: a fresh Mac, local inference, a native Defra
agent that can make changes on your computer, driven through the best terminal
experience we have — the Codex TUI — without the user installing or
configuring Codex at all.

## The story (first 15 minutes)

```bash
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf   # OpenAI-compat /v1 on :8080
git clone … && scripts/install-local.sh                 # installs defra-agent
defra-agent init                                        # read-only by default
defra-agent server                                      # codex shim on by default
defra-agent codex                                       # embedded Codex TUI, chatting
```

To let the agent change things: re-run `defra-agent init --write` (sandboxed
writes under the tool root) or `defra-agent init --yolo` (full host access,
loud warning). That is the entire surface a new user sees.

## Components

### 1. `defra-agent codex` — embedded Codex TUI

Codex CLI is Rust; its terminal UI is the `codex-tui` crate, entrypoint
`run_main(cli, arg0_paths, loader_overrides, Option<RemoteAppServerEndpoint>)`.
We already pin five `codex-*` crates from `openai/codex` at rev `c4e53d10…`;
add `codex-tui` (and whatever support crates `Arg0DispatchPaths` /
`LoaderOverrides` require) at the same rev.

The subcommand:

- builds a `codex_tui::Cli` programmatically with
  `dangerously_bypass_approvals_and_sandbox = true` — Defra owns sandboxing;
  the init-time preset is the real permission boundary;
- resolves the shim endpoint via the exported `resolve_remote_addr()`,
  defaulting to `ws://127.0.0.1:9292/`, overridable with `--remote <addr>`;
- probes the shim socket first and exits with a friendly
  "start `defra-agent server` first" if nothing is listening;
- calls `run_main(…, Some(endpoint))`.

The user never installs Codex; `~/.codex` is untouched beyond what the shim
already manages.

Trade-off: `codex-tui` is a heavy dependency (ratatui tree, longer CLI
builds). Rejected alternative: exec an externally installed `codex` binary —
adds an npm/brew step and puts `--dangerously-bypass-approvals-and-sandbox`
in the user's terminal history. Known cost: `run_main`'s signature is not a
stability promise; every future rev bump may need the call adjusted (a cost
we already pay for the protocol crates).

### 2. init presets — one flag to add write

- **default (no flag):** today's read-only package — read-only file tools,
  allowlisted read-only bash, meta tools.
- **`--write`:** the existing `--write-tools` path renamed — ReadWrite file
  tools + `workspace_write` seatbelt-sandboxed bash, both scoped to
  `--tool-root` (default cwd). `--write-tools` remains as a hidden alias.
- **`--yolo`:** ReadWrite file tools + `unrestricted` bash. Prints a clear
  warning at init. New name over the existing `unrestricted` command policy.

No interactive picker, no profile enum. Read-write system prompt for both
`--write` and `--yolo`. No Lean change: config plumbing, not a
transition-legality change; existing tool-selection conformance covers the
document shapes.

### 3. Server defaults

- Codex shim **on by default** in `defra-agent server`, loopback-bound; the
  refuse-to-bind-unspecified guard stays; `--no-codex-shim` disables.
  "Experimental" framing drops from docs.
- Default backend preset flips **ollama → llama-cpp**: endpoint
  `http://127.0.0.1:8080/v1`, default model the Gemma-4 12B QAT alias (init's
  existing `/models` discovery resolves the exact id llama-server reports).
  Ollama stays a one-flag alternative (`--backend-preset ollama`).

### 4. Docs

- **README "Get running"** = the six-command story, nothing else.
- **docs/demo.md** rewritten end-to-end around the single story:
  prerequisites, happy path, then short go-deeper sections (write modes and
  what the sandbox guarantees, other backends, `defra-agent chat` as the
  no-Codex fallback).
- Desktop app / fleet / P2P pairing content cut from demo.md; anything
  operationally load-bearing moves to `docs/operations.md`; the rest is
  deleted (git history keeps it).
- `scripts/install-codex.sh` folds away; its codex one-liner is obsolete.

## Testing

- Unit tests: init flag → tool-selection document shapes (`--write`,
  `--yolo`); `defra-agent codex` arg → `Cli` mapping and endpoint resolution
  (no TUI spawn in tests).
- Serve tests adjusted for shim-on-by-default.
- Gates: `cargo test -p defra-agent && cargo test -p defra-agent-cli`
  (full package suites, never `--lib`).
- Manual e2e before handoff: llama-server with the Gemma model, init, server,
  embedded codex TUI through a real turn including a tool call; confirm
  read-only default and `--write` behavior.

## Risks

- Gemma-4 12B QAT q4_0 quality and tool-calling reliability through the shim
  are unvalidated; e2e verification exercises tool calls specifically.
- `codex-tui` build-time cost on the CLI crate.
