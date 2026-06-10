# defra-agent

**An agent runtime where the database is the control plane.**

defra-agent runs LLM agents on top of [DefraDB](https://github.com/sourcenetwork/defradb): every piece of state — configuration, requests, responses, sessions, tool calls, schedules — is a replicated, access-controlled document. Agents get verifiable DID-based identity, document-level permissions, and P2P event propagation for free, because the database provides them.

## Get running

Everything local, on a Mac, in a few minutes:

```bash
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf   # local inference on :8080

cargo install --profile dev-install --locked --path crates/defra-agent-cli

defra-agent init      # provision a safe read-only agent under ~/.defra-agent
defra-agent server    # start the runtime (embedded DefraDB + GraphQL + P2P)
defra-agent codex     # chat in the Codex terminal UI (no Codex install needed)
```

Want the agent to actually change things? Re-run `defra-agent init --write`
(writes sandboxed under the directory you ran `init` from) or `--yolo`
(unrestricted, full host access).

`init` defaults to llama.cpp's server (`http://127.0.0.1:8080/v1`). Point it
at anything OpenAI-compatible: `defra-agent init --inference-url
http://HOST:PORT/v1 --model-name MODEL`, or use a preset
(`--backend-preset ollama|openai|openrouter|chatgpt-codex|vllm`).

The full walkthrough, what each permission preset guarantees, and the fallback
`chat` REPL: **[docs/demo.md](docs/demo.md)**. Desktop app, fleet bring-up,
and P2P pairing: **[docs/operations.md](docs/operations.md)**.

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
cargo test -p defra-agent                    # runtime suite (lib + integration)
cargo test --workspace                       # everything
cd crates/defra-agent/proofs && lake build   # the Lean proofs
```

The development flow is foundation-first: Lean model → conformance tests → implementation. `CLAUDE.md` is the working brief; the [proofs README](crates/defra-agent/proofs/README.md) maps the formal coverage.

## Status

Closed source during incubation. Extracted from a larger system (Amygdala) and intentionally narrow: the runtime and its formal specification.
