# Getting started

A short walkthrough for a first-time user: install, connect a model, have a
first chat, and give the agent a first background job. See the
[glossary](glossary.md) for what the terms below mean.

## What Gents adds over a plain AI chat assistant

A plain assistant forgets everything when the window closes and can only do
one thing while you watch. Gents adds:

- **Durable goals** — give the agent an objective once; it keeps working
  toward it across multiple turns instead of forgetting it after one reply.
- **Background work** — start a multi-step job (a *graph*) and check on it
  later instead of watching it run.
- **Subagents** — a job can dispatch specialized helper agents and assemble
  their results, instead of one model doing everything serially.
- **Resumable sessions** — conversations are saved documents. Close your
  terminal, come back tomorrow, and continue the same session.
- **Desktop and CLI on the same agent** — the desktop app is an observer/
  operator UI for the same runtime and documents the CLI uses; you can drive
  the agent from either, or both at once.

## Install

On an Apple Silicon Mac, download `gents-desktop_*_aarch64.dmg` from the
[latest release](https://github.com/gents-ai/gents/releases/latest), drag
**Gents** into **Applications**, and launch it. Linux uses the `.deb` or
AppImage from the same release. See
[`docs/macos-desktop-install.md`](macos-desktop-install.md) and
[`docs/linux-desktop-install.md`](linux-desktop-install.md) for details.

To use the CLI instead (or alongside the desktop app), build it from this
repo:

```bash
cargo build -p gents-cli --release
```

## Initialize with any provider

`gents init` sets up a local agent home at `~/.gents`: an identity, one
default behavior, and a safe read-only tool set. With no flags it expects a
local model server; pass a preset for a hosted or subscription provider
instead. Pick one:

```bash
# Local model — no account needed
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf   # serves on :8080
gents init

# OpenAI API key
gents init --backend-preset openai --model-name gpt-4.1

# OpenRouter API key
gents init --backend-preset openrouter --model-name MODEL

# ChatGPT / Codex subscription (OAuth)
gents init --backend-preset chatgpt-codex
gents codex-login

# Grok / xAI subscription (OAuth)
gents init --backend-preset xai-oauth
gents grok-login

# Claude subscription (OAuth)
gents init --backend-preset claude-cli-subscription
gents claude-login
```

If you want the agent to be able to edit files instead of only reading them,
add `--write` to `gents init` (sandboxed to the tool root); `--yolo` instead
removes the sandbox entirely and should only be used when you trust the
model with full access as your own user.

## Start the runtime

```bash
gents server
```

Leave this running in its own terminal (or let the desktop app manage it).
Everything else below talks to it.

## Your first chat

In another terminal:

```bash
gents chat "what can you see in this directory?"
```

This starts a new session, sends one message, and prints the reply. To keep
talking in the same session:

```bash
gents chat --session-id my-first-session "and what does the README say?"
```

Reuse `--session-id my-first-session` any time — today, or after restarting
`gents server` tomorrow — to continue exactly where you left off.

To give that session a durable goal instead of steering it turn by turn:

```bash
gents chat --session-id my-first-goal \
  --goal-objective "Read through this repo and summarize what it does" \
  "get started"
```

Check on it later with `gents goal show --session my-first-goal`.

## Your first background job

Chat runs one turn while you wait. A *graph* is a packaged, multi-step job
that runs in the background and reports back when it's done — the "background
work" from the intro above. Gents ships one you can try on any local Git
repository, built from a `code_review` pack:

```bash
gents pack install code_review

cd /path/to/a/git/repo
gents graph run code_review
```

This prints a run id. Watch it live, or walk away and check back:

```bash
gents graph watch <run-id>
gents graph result <run-id>
```

`gents graph run` defaults to the current directory, `origin/main`, and
`HEAD`; pass `--repo`, `--base`, or `--head` to point it elsewhere. Add
`--output json` to any of `install`/`run`/`watch`/`result` for
machine-readable output, and use `gents graph cancel <run-id>` to stop one.

Under the hood, a graph is built from named `Task` documents. List the ones
the pack installed, or run a single one by hand:

```bash
gents task list
gents task show <task-id>
gents task run <task-id>
```

## Where to go next

- `gents status` — is the runtime up, and what's it connected to.
- `gents mailbox list` — anything the agent is waiting on you for.
- `gents pack list` and [`packs/README.md`](../packs/README.md) — other
  bundled packs, and how to build your own.
- [`docs/glossary.md`](glossary.md) — what behaviors, tasks, triggers,
  schedules, subagents, and the rest actually mean.
- The main [README](../README.md) — architecture and how the pieces fit
  together.
