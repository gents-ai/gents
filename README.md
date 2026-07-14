# defra-agent

**An agent runtime where the database is the control plane.**

defra-agent runs LLM agents on top of [DefraDB](https://github.com/sourcenetwork/defradb): every piece of state — configuration, requests, responses, sessions, tool calls, schedules — is a replicated, access-controlled document. Agents get verifiable DID-based identity, document-level permissions, and P2P event propagation for free, because the database provides them.

## Get running

Everything local, on a Mac, in a few minutes:

```bash
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf   # local inference on :8080

gh release download v0.4.0 --repo sourcenetwork/defra-agent -p 'defra-agent-aarch64-apple-darwin.tar.gz'
tar -xzf defra-agent-aarch64-apple-darwin.tar.gz
sudo install defra-agent-aarch64-apple-darwin/defra-agent /usr/local/bin/defra-agent

defra-agent init      # provision a safe read-only agent under ~/.defra-agent
defra-agent server    # start the runtime (embedded DefraDB + GraphQL + P2P)
defra-agent codex     # chat in the Codex terminal UI (no Codex install needed)
```

**[The getting-started guide](docs/demo.md)** walks every step and the paths
off it: letting the agent change things (`init --write` / `--yolo`, and what
each preset guarantees), pointing `init` at other OpenAI-compatible backends,
verifying the signed binary, building from source, and the fallback `chat`
REPL. Desktop app, fleet bring-up, and P2P pairing:
[docs/operations.md](docs/operations.md).

For the interactive fleet demo, run `defra-agent demo` — it ships in the binary,
no checkout, `make`, or mock required. It boots a single curated agent (read-only
tools + demo skills) on a backend you pick on first run, then drops into a
`demo>` shell: `chat` with the agent, `pair` a 2nd node (a **Worker**),
`delegate` a cross-node subagent that runs on the worker over P2P (the result
replicates back), `desktop` to open the same fleet through the native Fleet
Dashboard UI, and `reconfigure` to switch backends.
New chat turns use the configured model backend, reachable on both nodes. Keep
the local `llama-server` above running, or launch with a hosted preset and
model, e.g. `OPENAI_API_KEY=... defra-agent demo --backend-preset openai --model
gpt-4.1-mini`.

## Why this exists

Agent frameworks bolt persistence, identity, and coordination onto a loop. defra-agent inverts that: the loop is thin and formally specified, and the hard properties come from the substrate.

- **The data store is the control plane.** Configure an agent by writing documents; trigger work by writing documents; debug by reading them. The runtime watches request documents and writes responses back. Multi-agent coordination is document replication, not RPC.
- **Identity is cryptographic and layered.** A *principal* (DID) is the permission and audit boundary. *Behaviors* — prompt, tools, model — are reusable interfaces on a principal. *Deployments* place principals on hosts. Least privilege falls out of the model.
- **The core is proven.** The request, process, persistence, tool-call, and subagent lifecycles — and what the runtime feeds the model — are specified in Lean 4 with zero `sorry`s, fenced by conformance tests, and only then implemented. See [the proofs](crates/defra-agent/proofs/README.md).

## Architecture

``` md
            documents in ────────────► documents out
                 │                            ▲
   ┌─────────────▼────────────────────────────┴──────────────┐
   │  defra-agent runtime (the core)                         │
   │  watcher → request lifecycle → owned completion loop    │
   │  → tool surface (files/bash/MCP/subagents/skills)       │
   │  → persistence hooks → live response streaming          │
   ├─────────────────────────────────────────────────────────┤
   │  embedded DefraDB: identity (DID) · ACL · P2P           │
   └─────────────────────────────────────────────────────────┘
        ▲                ▲                  ▲
     CLI (operate)   desktop (observe)   other peers (replicate)
```

- **Runtime** (`crates/defra-agent`) — the agent loop, lifecycles, tool execution, triggers/schedules, compaction, recovery. The core; everything else supports it.
- **Protocol** (`crates/defra-agent-protocol`) — schemas, the persisted message vocabulary, and the turn-observation protocol shared by every peer.
- **CLI** (`crates/defra-agent-cli`) — init/serve/chat, plus declarative config apply/diff: agent manifests in, documents out.
- **Desktop** (`apps/desktop-tauri`, `crates/defra-agent-desktop*`) — an observer UI over the same documents, paired via P2P.
- **Proofs** (`crates/defra-agent/proofs`) — the Lean models the runtime conforms to.

Subagents are requests: a parent's tool call spawns a child request — possibly on another deployment — and the child's terminal state projects back onto the parent's transcript. Automation is the same shape: Tasks, Schedules, and EventTriggers materialize requests with lineage stamped on every one.

## Development

Building from source needs a few system dependencies (Rust, a C/C++ toolchain,
`protoc`, `libclang`, OpenSSL headers, SSH access to the private DefraDB repos).
Build, test, and toolchain setup live in **[DEVELOPMENT.md](DEVELOPMENT.md)**.

The development flow is foundation-first: Lean model → conformance tests → implementation. `CLAUDE.md` is the working brief; the [proofs README](crates/defra-agent/proofs/README.md) maps the formal coverage.

## Status

Closed source during incubation. Extracted from a larger system (Amygdala) and intentionally narrow: the runtime and its formal specification.
