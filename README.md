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

For the local two-node P2P demo from source, run `make demo-p2p-two-node`.
It starts Amy + Coding, enrolls Coding through a signed `network-control`
invite, adds the conversation data plane, and proves request plus conversation
replication.
To see the same two-node substrate through the native fleet UI, run
`make demo-desktop-two-node`; it launches the Tauri shell with two runtimes —
**Orchestrator** and **Worker** — in the Fleet Dashboard, each with a tightened
tool surface (no `defra_query`). Ask the Orchestrator to use its worker subagent
and it delegates a child request that runs on the **Worker node** over P2P, with
the result replicating back.
New chat turns use the configured model backend. For working chat with no
external model, add `DEFRA_AGENT_DESKTOP_DEMO_MOCK_BACKEND=1` to start a bundled
OpenAI-compatible mock (canned replies; it even fires the subagent call).
Otherwise keep the local `llama-server` above running or launch with a hosted
preset and model, e.g.
`DEFRA_AGENT_DEMO_BACKEND_PRESET=openai DEFRA_AGENT_DEMO_MODEL=gpt-4.1-mini`.

## Why this exists

Agent frameworks bolt persistence, identity, and coordination onto a loop. defra-agent inverts that: the loop is thin and formally specified, and the hard properties come from the substrate.

- **The data store is the control plane.** Configure an agent by writing documents; trigger work by writing documents; debug by reading them. The runtime watches request documents and writes responses back. Multi-agent coordination is document replication, not RPC.
- **Identity is cryptographic and layered.** A *principal* (DID) is the permission and audit boundary. *Behaviors* — prompt, tools, model — are reusable interfaces on a principal. *Deployments* place principals on hosts. Least privilege falls out of the model.
- **The core is proven.** The request, process, persistence, tool-call, and subagent lifecycles — and what the runtime feeds the model — are specified in Lean 4 with zero `sorry`s, fenced by conformance tests, and only then implemented. See [the proofs](crates/defra-agent/proofs/README.md).

## Architecture

```
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

```bash
make help                                    # curated build/test targets
cargo test -p defra-agent                    # runtime suite (lib + integration)
cargo test --workspace                       # everything
cargo build -p defra-agent-cli --no-default-features  # CLI without embedded Codex TUI
cd crates/defra-agent/proofs && lake build   # the Lean proofs
```

The development flow is foundation-first: Lean model → conformance tests → implementation. `CLAUDE.md` is the working brief; the [proofs README](crates/defra-agent/proofs/README.md) maps the formal coverage.

## Status

Closed source during incubation. Extracted from a larger system (Amygdala) and intentionally narrow: the runtime and its formal specification.
