# Glossary

Plain-language definitions of the terms Gents uses. Each one is a kind of
document stored in the embedded database (DefraDB); you rarely touch the
database directly — the CLI, desktop app, and packs create and read these
documents for you.

## Agent

The identity that everything else belongs to. An agent is a cryptographic
DID (a self-owned ID, not a username/password account) created by `gents
init`. Every document below — behaviors, sessions, tasks, tools — is scoped
to one agent's `agent_did`. One initialized home (`~/.gents`) holds one
agent.

## Behavior

The reusable configuration bundle a session runs with: which model/provider
to use, what instructions to give it, which tools and skills it can reach,
and how to compact a long conversation. You don't configure a chat directly;
you pick a behavior, and a session binds to it by `behavior_id`. `gents init`
creates one default behavior for you; `gents config behavior` lets you create
more (for example, a read-only one and a separate write-capable one).

## Inference backend

The connection to a model provider: an endpoint, credentials, and a wire
format. `gents init --backend-preset ...` writes one of these — for a local
`llama.cpp`/Ollama/vLLM server, an OpenAI-compatible API key, OpenRouter, or
subscription OAuth for ChatGPT/Codex, Grok, or Claude. A behavior points at a
backend indirectly (through an inference profile); switching providers means
writing a new backend, not rewriting your behaviors or tools.

## Session

One durable, resumable conversation (`AgentSession`). A session remembers
which behavior it uses, when it was created, and a compact history of its
turns. `gents chat --session-id <id>` continues an existing session instead
of starting a new one, and `gents session fork` branches a new session from
a point in an existing one. Sessions are what makes a conversation something
you can walk away from and pick back up later, from the CLI or the desktop
app.

## Request

One turn of work the runtime executes: a user or system message in, a model
response and any tool calls out (`AgentRequest`). Requests are the unit the
runtime tracks progress and state on — pending, running, done, failed,
cancelled. You don't normally create requests by hand; chatting, running a
task, or firing a trigger all create one under the hood. `gents request` and
`gents response` are low-level commands for inspecting them directly.

## Task

A reusable, named prompt template (`Task`), independent of any one
conversation. A task has a behavior to run under and a prompt template to
fill in; running it creates a request. Unlike a chat message, a task is
something you can name, list, and re-run: `gents task list`, `gents task
show <task-id>`, `gents task run <task-id>`. Tasks are also what triggers and
schedules fire — see below.

## Trigger

Desired configuration that says "when this happens, fire this task"
(`Trigger`). A trigger has a source — a schedule, or an event such as an
incoming webhook — and points at one `Task`. Firing a trigger doesn't create
a new kind of run; it renders the task into a normal request through the
same completion loop as running it manually.

## Schedule

The cadence a trigger runs on: either a fixed interval or a cron expression
(`Schedule`). A schedule is reusable — more than one trigger can point at the
same schedule — and only decides *when*; a trigger still decides *what runs*.

## Goal

A durable, session-scoped objective the agent works toward across multiple
requests without you re-prompting it each time (`gents goal set`, `gents
goal show`, `gents goal resume-request`). A goal is what lets an agent keep
making progress on something in the background and be resumed later, rather
than forgetting the objective the moment one request finishes.

## Graph

An installed, multi-step pipeline made of several tasks and behaviors working
together — for example, the bundled `code_review` graph that recons, scans,
verifies, and reports on a change set. You run one with `gents graph run
<name>`, watch it with `gents graph watch <run-id>`, and read its durable
output with `gents graph result <run-id>`. A graph is heavier than a single
task: it fans work out to subagents and assembles their results into one
report.

## Pack

A distributable bundle of the documents above — behaviors, tasks, tools,
schemas, prompts, and optionally a graph — plus any supporting assets. `gents
pack list` shows what's bundled in your binary; `gents pack install <name>`
writes its documents into your agent's home. Installing a pack grants no
extra tools or execution authority by itself; it just adds configuration you
can then run. See [`packs/README.md`](../packs/README.md) for how packs are
built.

## Tools and tool groups

The capabilities a behavior is allowed to use: reading/writing files,
running shell commands, calling MCP services, or dispatching subagents
(`Tools`). These capabilities are organized into named groups (host tools,
remote/MCP tools, subagent tools, built-in tools) nested inside one `Tools`
document, so a behavior's permission boundary is one thing you can inspect
with `gents tools explain`. `gents init` defaults to a safe, read-only tool
set; `--write` or `--yolo` opt into write access, with `--yolo` removing the
sandbox entirely.

## Skills

Named, optional instructions a behavior can turn on for a request — extra
guidance for a specific kind of task, without changing the tools available.
A skill declares which tools *it needs*; it never grants a tool the
behavior's `Tools` document doesn't already allow. Manage them with `gents
config skill`.

## Subagents

A request that another request spawned as a tool call, possibly under a
different behavior or on another machine (a dispatched child request). The
parent sees the child's final result appear in its own transcript once the
child finishes. This is how graphs fan work out to specialized helpers, and
how tasks, schedules, and event triggers can spawn work without you writing
new orchestration code. Inspect the tree with `gents subagent list --root
<request-id>`.

## Mailbox

A durable list of items that need a person's attention — an approval, a
question, a notification — surfaced by the agent instead of being buried in
a transcript (`MailboxItem`). `gents mailbox list` shows open items for you;
`gents mailbox reply` or `gents mailbox dismiss` resolves one.

## Workspace

The sandboxed root directory a behavior's file and shell tools are scoped
to — usually the repository or project directory you pointed `--tool-root`
at during `gents init`. For isolated or multi-agent work, a workspace can
also mean an isolated worktree an agent is given to make changes in, which
gets sealed and handed back for review rather than writing directly to your
checkout.
