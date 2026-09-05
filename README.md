# Gents

**An agent runtime where the database is the control plane.**

Gents runs LLM agents on top of [DefraDB](https://github.com/sourcenetwork/defradb): every piece of state — configuration, requests, responses, sessions, tool calls, schedules — is a replicated, access-controlled document. Agents get verifiable DID-based identity, document-level permissions, and P2P event propagation for free, because the database provides them.

## Get running

Everything local, on a Mac, in a few minutes:

```bash
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf   # local inference on :8080

# Install the Codex CLI separately and make sure `codex` is on PATH.
# `gents chat` remains the dependency-free fallback UI.

gh release download --repo gents-ai/gents -p 'gents-aarch64-apple-darwin.tar.gz'
tar -xzf gents-aarch64-apple-darwin.tar.gz
sudo install gents-aarch64-apple-darwin/gents /usr/local/bin/gents

gents init      # provision a safe read-only agent under ~/.gents
gents server    # start the runtime (embedded DefraDB + GraphQL + P2P)
gents codex     # launch Codex against the Gents app-server shim
```

**[The getting-started guide](docs/demo.md)** walks every step and the paths
off it: letting the agent change things (`init --write` / `--yolo`, and what
each preset guarantees), pointing `init` at other OpenAI-compatible backends,
verifying the signed binary, building from source, and the fallback `chat`
REPL. Desktop app, fleet bring-up, and authenticated enrollment:
[docs/operations.md](docs/operations.md).
Operators performing the breaking product cutover should use the single
[Gents cutover runbook](docs/gents-cutover.md).
The plan for packaging the desktop chat, fleet, and Tauri bridge surfaces for
downstream apps (#877) is specified in
[docs/reusable-desktop-packages.md](docs/reusable-desktop-packages.md).

For the interactive fleet demo, run `gents demo` — it ships in the binary,
no checkout, `make`, or mock required. It boots a single curated agent (read-only
tools + demo skills) on a backend you pick on first run, then drops into a
`demo>` shell: `chat` with the agent, `pair` a 2nd node (a **Worker**),
`delegate` a cross-node subagent that runs on the worker over P2P (the result
replicates back), `desktop` to open the same fleet through the native Fleet
Dashboard UI, and `reconfigure` to switch backends.
New chat turns use the configured model backend, reachable on both nodes. Keep
the local `llama-server` above running, or launch with a hosted preset and
model, e.g. `OPENAI_API_KEY=... gents demo --backend-preset openai --model
gpt-5.4-mini`. Add `--desktop` to launch the native app as soon as the runtime
is ready.

The binary also carries an immutable catalog of useful graphs. Cataloging is
read-only. Interactive init can configure OpenAI API access, ChatGPT/Codex
OAuth, Grok OAuth, a local model, or a custom endpoint. A bundled graph inherits
that default backend when it is installed:

```bash
gents init                 # choose ChatGPT / Codex and complete OAuth
gents server               # keep this running in another terminal

gents pack show code_review
gents pack install code_review

cd /path/to/repo
gents graph run code_review
gents graph watch <run-id>
gents graph result <run-id>
```

The run command defaults to the current directory, `origin/main`, and `HEAD`;
use `--repo`, `--base`, or `--head` to override them. Add `--output json` to
install, run, watch, or result for machine-readable output. Result prints the
durable review report and confirmed findings as well as retaining their exact
document/commit references. Use `gents graph cancel <run-id>` to request cancellation. The
code-review graph accepts any local Git work tree and uses the existing
principal, deployment, tool-surface, workspace, request, trigger, and graph-run
machinery; bundling it grants no tools or execution authority.

The catalog also includes a web deep-research graph. First stand up the public
search and extraction stack; its single entrypoint waits for real SearXNG and
Firecrawl smoke checks before returning:

```bash
git clone https://github.com/source-inc/web-research-mcp.git
cd web-research-mcp
./scripts/stack install-mcp
```

`install-mcp` starts the released real stack, waits for both backend smoke
checks, registers `http://127.0.0.1:9213/mcp` against the running local Gents
node, and probes readiness. The graph package installs Gents documents and
declares this external dependency; it deliberately does not silently allocate
the roughly 12 GB Docker stack during `pack install`.

Then install the graph and run it with live fan-out progress:

```bash
gents pack install web_deep_research
gents graph run web_deep_research \
  --question "What changed in the MCP security guidance, and what should operators do?" \
  --investigator-count 4 \
  --watch
gents graph result <run-id> | tee web-research-report.md
```

The graph plans a closed assignment set, fans out investigators, waits for the
complete evidence barrier, adjudicates claims, and writes a cited report from
the verdict ledger. Each investigator submits one idempotent bounded evidence
bundle: several planned searches become a capped, deduplicated set of fetched
sources with stable IDs, hashes, and gateway-verified excerpts. Plan,
adjudication, and report stages have no web authority; only investigators can
reach the named research service. The paid
acceptance gate is `scripts/web-research-live-e2e.sh`: it starts real SearXNG
and Firecrawl infrastructure and runs the complete graph against a real model.
It contains no mock backend path.

## Why this exists

Agent frameworks bolt persistence, identity, and coordination onto a loop. Gents inverts that: the loop is thin and formally specified, and the hard properties come from the substrate.

- **The data store is the control plane.** Configure an agent by writing documents; trigger work by writing documents; debug by reading them. The runtime watches request documents and writes responses back. Multi-agent coordination is document replication, not RPC.
- **Identity is cryptographic and layered.** A *principal* (DID) is the permission and audit boundary. *Behaviors* — prompt, tools, model — are reusable interfaces on a principal. *Deployments* place principals on hosts. Least privilege falls out of the model.
- **The core is proven.** The request, process, persistence, tool-call, and subagent lifecycles — and what the runtime feeds the model — are specified in Lean 4 with zero `sorry`s, fenced by conformance tests, and only then implemented. See [the proofs](crates/gents/proofs/README.md).

## Architecture

```text
            documents in ────────────► documents out
                 │                            ▲
   ┌─────────────▼────────────────────────────┴──────────────┐
   │  Gents runtime (the core)                              │
   │  watcher → request lifecycle → owned completion loop    │
   │  → tool surface (files/bash/MCP/subagents/skills)       │
   │  → persistence hooks → live response streaming          │
   ├─────────────────────────────────────────────────────────┤
   │  embedded DefraDB: identity (DID) · ACL · P2P           │
   └─────────────────────────────────────────────────────────┘
        ▲                ▲                  ▲
     CLI (operate)   desktop (observe)   other peers (replicate)
```

- **Runtime** (`crates/gents`) — the agent loop, lifecycles, tool execution, triggers/schedules, compaction, recovery. The core; everything else supports it.
- **Protocol** (`crates/gents-protocol`) — schemas, the persisted message vocabulary, and the turn-observation protocol shared by every peer.
- **CLI** (`crates/gents-cli`) — init/serve/chat, plus declarative config apply/diff: agent manifests in, documents out.
- **Desktop** (`apps/gents-desktop`, `crates/gents-desktop*`) — an observer UI enrolled with runtimes and synchronized over P2P.
- **Proofs** (`crates/gents/proofs`) — the Lean models the runtime conforms to.

Subagents are requests: a parent's tool call spawns a child request — possibly on another deployment — and the child's terminal state projects back onto the parent's transcript. Automation is the same shape: Tasks, Schedules, and EventTriggers materialize requests with lineage stamped on every one.

## Development

Building from source needs a few system dependencies (Rust, a C/C++ toolchain,
`protoc`, `libclang`, and OpenSSL headers). DefraDB dependencies are public and
use pinned HTTPS revisions.
Build, test, and toolchain setup live in **[DEVELOPMENT.md](DEVELOPMENT.md)**.

The development flow is foundation-first: Lean model → conformance tests → implementation. `CLAUDE.md` is the working brief; the [proofs README](crates/gents/proofs/README.md) maps the formal coverage.

## Status

Pre-1.0 and under active development. Extracted from a larger system
(Amygdala) and intentionally narrow: the runtime and its formal specification.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
