# True Subagent Enablement — Design

**Issue:** #377 · **Unblocks:** #378 (workflow orchestration) · **Date:** 2026-06-03

## Summary

The subagent execution path is fully built and proven in Lean (`Background.lean`), with
the runtime plumbing already wired and conformance-tested. It has never been enabled by
default, never made natively uniform across the network, and never exercised on a real
fleet — because until now there were no deployed agents to run complex, cross-deployment
scenarios against. This spec takes that verified base **over the line to testing**: turn
subagents on per behavior, converge the spawn path so a spawn is genuinely *one document
write* regardless of locality, keep authorization as an explicit operator-controlled
static allowlist, and validate up a ladder — local → simulated fleet → real fleet. It
adds **no new execution semantics and no new Lean modules**.

## Context — current state (grounded)

- **The server is the agent; everything else is a view.** Tool calls execute server-side
  via the persistence hook; the Codex shim, desktop, and CLI are read-only projections
  over the same session documents. They never participate in tool routing.
- **Why now.** The gating blocker was the absence of deployed agents to exercise complex
  and cross-deployment scenarios. A 14-node fleet now exists, so we can finally drive this
  to real testing — but only as the *last* tier, after local and simulated-fleet testing,
  since real-fleet runs require deploying binaries to every node.
- **Tools exist, gated off.** `spawn_subagent` / `wait_subagent` / `cancel_subagent`
  (R4), `list_subagents` / `read_subagent_transcript` / `steer_subagent` (R4c), and
  `background_tool` / `wait_tool` / `cancel_tool` (R6) are all built. They are offered
  only when `subagent_spawn_enabled == true` **and** `subagent_targets` is non-empty
  (`tool_surface/selection.rs:21-23`); `subagent_spawn_enabled` defaults to `false`.
- **Direct execution is stubbed by design.** `SpawnSubagentTool::call()` returns
  `not_yet_executable_error` (`toolset/subagent.rs:193-196`); the hook intercepts every
  spawn at `on_tool_call` and returns `Skip` so `Tool::call()` never runs
  (`hook/persistence/message_spawn.rs:217-543`). This is server-side and correct.
- **Plumbing is live.** `SubagentSource` is a registered `TriggerSource` and the
  background-completion observer is spawned (`agent/runtime/startup.rs:204-263`).
- **Dual local/remote spawn path.** Same-deployment spawns create the child
  `AgentRequest` synchronously and locally with the parent's DID
  (`message_spawn.rs:445-512`); cross-deployment spawns return a background receipt and
  wait for the remote `SubagentSource` to materialize the child
  (`message_spawn.rs:434-443`). Both produce the same documents/lineage, but the code is
  two paths and locality is not transparent.
- **Trust + replication intent already modeled.** `PeerPairingDesired`
  (`schemas/agent/peer_pairing_desired.graphql`) records trusted `agent_did`s,
  collections, and `replicator_addresses`; `load_startup_paired_peer_dids`
  (`startup.rs:592-626`) loads DIDs into `paired_peer_dids`; the cross-deployment check
  is `snapshot.paired_peer_dids.contains(&parent_authoring_did)`
  (`subagent_source.rs:319`). Actual P2P transport is defradb's job.
- **Authorization today is a static allowlist + app-level filtering.** `subagent_targets`
  on `ToolSelection` is a static list of `behavior_id`s the orchestrator may spawn; the
  watcher filters `AgentRequest` by `agent_did` (`watcher/query.rs:66-98`). DefraDB ACP is
  not yet wired (no policy files, no `@policy`, identity never signs mutations).
- **`AgentBehavior` is not self-describing.** It has no `description`/`summary` field, so
  an orchestrator's allowlisted targets carry no human/LLM-facing "what this does."

## Goals

1. Make subagents enable-able per behavior through the apply path, ergonomically and with
   validation.
2. Ensure the server maintains, per session and per agent, queryable state of which
   subagents are running — consumed by the agent's own tools and projected uniformly to
   all read-only views (Codex shim, desktop, CLI).
3. Converge the spawn path so a spawn is genuinely **one uniform document write** that
   works the same whether the target is local or remote.
4. Make an orchestrator's **statically-allowed** targets self-describing (what each does),
   without any dynamic, fleet-wide discovery.
5. Validate up a ladder: local → simulated fleet (in-process multi-node + replication) →
   real fleet (deploy binaries; last).

## Non-goals

- New execution semantics, lifecycle states, or transitions (already in `Background.lean`).
- **Dynamic / fleet-wide agent discovery.** Authorization is an explicit static allowlist;
  we deliberately do not build a directory the agent queries to find arbitrary peers.
- Owning P2P replication transport setup (prerequisite; see C6).
- Wiring DefraDB ACP enforcement (separate dependency; this spec is honest about the
  interim and keeps the static allowlist as the operator-facing control).
- Workflow orchestration primitives (#378 — this unblocks them, doesn't build them).

## Design tenets

1. **A spawn is a document write.** Spawning = writing a bridge/request document to a
   collection. Replication carries it to the deployment that owns the target
   `(agent_did, behavior_id)`; that deployment's watcher claims and runs it; the terminal
   document replicates back. Locality is transparent — same-node is the degenerate case
   where replication is a no-op.
2. **One path, not two.** Collapse the synchronous-local vs async-remote fork into a
   single write-and-claim path so tenet 1 is real in the code, not just conceptual. The
   local case becomes the zero-replication-lag case of the remote case.
3. **Authorization is an explicit, operator-controlled static allowlist.**
   `subagent_targets` is the deliberate permission surface — explicit and auditable.
   DefraDB ACP at the document layer is the deeper enforcement target when it lands; the
   allowlist is not a throwaway interim.
4. **The server is the single execution and state authority; views are read-only
   projections.** Subagent state lives as session-scoped DefraDB documents; the Codex
   shim, desktop, and CLI all render the same documents and never route tool calls.
5. **Least privilege stays opt-in.** Subagent spawning is off by default per
   principal/behavior; enabling is a deliberate, audited config act.
6. **Use the verified base; don't rebuild it.** The Lean state machine and runtime
   plumbing exist. The work is enablement, one targeted convergence, and testing — not
   net-new design.

## Components

Each component is tagged **[exists → validate]** (already built; prove/operationalize it)
or **[new]** (genuinely new code).

### C1 — Enablement surface **[new, small]**

- Keep `subagent_spawn_enabled` (default `false`) and expose the **full R4c/R6 tool
  surface** when enabled.
- Ergonomic apply-path config on `ToolSelection`, with **apply-time validation**:
  `subagent_targets` entries resolve to a known `AgentBehavior` (local or replicated); the
  target principal is enabled; surface a clear error rather than silently offering inert
  tools.
- Document the stubbed `Tool::call()` as a permanent **hook-only invariant** (it must
  never execute directly); ensure `not_yet_executable_error` never reaches operators in
  normal use.

### C2 — Server-maintained subagent state **[exists → validate]**

For each session and agent the server already records running-subagent state as
documents: `AgentToolCall` bridge rows + child `AgentRequest` lineage, surfaced to the
agent via `list_subagents` / `read_subagent_transcript` and to every view by reading the
documents. The work is to **confirm this state is complete and queryable** end-to-end
(running, completed, failed, cancelled) — not to add view-layer plumbing.

### C3 — Converge the spawn path **[new]**

- Make the spawn write-path uniform: always persist the `AgentToolCall` bridge with a
  pre-allocated child `request_id`; let the owning deployment's `SubagentSource`
  materialize and claim the child `AgentRequest`. The current synchronous-local create
  becomes an internal fast-path of this single path, not a separate semantic.
- Completion always projects from the (possibly replicated-back) terminal child
  `AgentRequest` via the `BackgroundCompletionObserver`.
- Preserve all existing `Background.lean`-proven behavior; this is a code-path
  unification, not a semantic change. The 17 `subagent_source_conformance.rs` tests are
  the regression gate.

### C4 — Self-describing static targets **[new, small]**

- Add `description` / `summary` fields to `AgentBehavior` (what this agent does,
  LLM-facing). Adding fields to an existing collection touches **no `Collection` enum
  variant and no Lean parity** — it is desired-state config only.
- Surface the orchestrator's **statically-allowed** targets (its `subagent_targets`) with
  their descriptions to the agent via prompt-context injection — so it knows what it may
  spawn and what each does — **without** a directory collection or a dynamic discovery
  tool.

### C5 — Authorization (static allowlist; ACP later) **[exists → validate]**

- Primary surface: the static `subagent_targets` allowlist + the existing `agent_did`
  query filtering. Explicit, operator-controlled, auditable.
- Future enforcement: DefraDB ACP at the document layer (a caller may spawn iff it can
  write the spawn document into the target's scope). When ACP lands it *complements* the
  allowlist (defense in depth); it does not require removing it.

### C6 — Replication prerequisite **[external dependency]**

Cross-deployment requires these collections to replicate across trusted peers:
`AgentRequest`, `AgentToolCall`, `AgentBehavior`, `PeerPairingDesired`. This spec does not
own transport setup — that is the existing P2P work (#363, defradb.rs#1012/#1013).
Simulated-fleet testing (Tier 2) uses in-process multi-node replication; real-fleet
testing depends on transport being enabled across the 14 nodes. This spec contributes a
**replication health check** (are the required collections live and converging between an
orchestrator and its targets?).

## Data flow — uniform spawn (local or remote)

1. Orchestrator picks a target from its static `subagent_targets` (self-describing via the
   behavior's `description`).
2. Orchestrator calls `spawn_subagent(target)`. The hook intercepts (`on_tool_call` →
   `Skip`) and writes the `AgentToolCall` bridge with a pre-allocated `child_request_id`.
3. The bridge reaches the deployment owning the target (replication; a no-op when local).
   That deployment's `SubagentSource` materializes the child `AgentRequest` (DID per
   trusted-peer rules) and its watcher claims and runs it.
4. The child reaches a terminal state; the terminal `AgentRequest` reaches the orchestrator
   (replication; local when same-node). The `BackgroundCompletionObserver` projects it into
   the parent bridge and enqueues a wake-up so `wait_subagent` resolves.

## Schema changes

- `AgentBehavior`: add `description`, `summary` (fields on an existing collection — no
  `Collection` enum change, no apply-order change, no Lean parity delta).
- No new collections.

## Error handling

- **Unclaimed remote spawn:** existing `unclaimed_deadline_at` bounds the remote claim
  (`message_spawn.rs:428-430`); surface a tool failure with a clear cause.
- **Replication lag / partition:** the spawn document persists and converges when the
  partition heals; the health check (C6) and the unclaimed deadline bound the wait.
- **Invalid target at spawn:** rejected against the static allowlist with a clear cause;
  no partial state.
- **Denied write (future ACP):** spawn document write rejected → tool failure with a
  permission cause; no partial state.

## Formal-methods posture

**No Lean changes required.** `Background.lean` already proves parent/child spawn,
completion, cascade-cancel, and depth-bound properties; the path convergence (C3) is a
code-path unification that must continue to satisfy those proofs, enforced by the existing
conformance tests. No new collection means no `ApplyReconcile/Collections.lean` parity
delta. The only spec-adjacent Lean touch is optionally recording in
`Conformance/Boundaries.lean` the external assumptions this rests on: (a) replication
delivers the bridge/request/terminal documents, and (b) ACP — when wired — is the
document-layer authorization boundary.

## Testing & validation ladder

Real-fleet runs require deploying binaries to every node, so they are **last**. Cross-
deployment correctness is proven in simulation first.

- **Tier 1 — Local (single node):** enable subagents; orchestrator spawns a local child;
  foreground and background `wait`; result returned; per-session subagent state (running →
  terminal) queryable via `list_subagents` and visible as documents. No P2P.
- **Tier 2 — Simulated fleet (in-process multi-node + replication):** multiple
  `EmbeddedNode`s with replication between them exercise the *uniform* spawn path across
  "deployments": cross-deployment spawn / wait / completion, behavioral parity with the
  local case, lineage, terminal projection, and a small fan-out (one orchestrator → N
  targets). This is where C3 and cross-deployment correctness are proven without shipping
  binaries.
- **Tier 3 — Real fleet (deploy binaries; last):** with replication transport enabled
  across the 14 nodes, run the complex scenarios (e.g. `amy` → stewards, fan-out) on real
  hardware/network; verify lineage and completion end-to-end.
- The 17 `subagent_source_conformance.rs` tests and `r4_subagent_tools/` tests stay green;
  `cargo test`, clippy, fmt clean throughout.

## Implementation slices

1. **Enablement (C1):** config ergonomics + apply-time validation + document the
   hook-only-invariant for the stubbed `call`.
2. **State completeness (C2):** confirm/repair per-session subagent state; prove with the
   Tier-1 local E2E.
3. **Path convergence (C3):** unify the local/remote spawn into one write-and-claim path;
   regression-gated by conformance tests.
4. **Self-describing targets (C4):** `AgentBehavior.description`/`summary` + prompt-context
   injection of allowed targets. Small, independent.
5. **Simulated fleet (Tier 2):** in-process multi-node + replication harness; cross-
   deployment correctness, parity, fan-out; replication health check (C6).
6. **Real fleet (Tier 3, last):** deploy binaries to the 14 nodes; complex scenarios;
   depends on replication transport (#363).

## Out of scope

- Dynamic/fleet-wide agent discovery (static allowlist is the chosen model); ACP
  enforcement wiring; P2P transport setup; workflow primitives (#378);
  multi-tenant/untrusted-fleet spawning (gated on #180).

## Open questions

- Is the server's per-session subagent state already complete enough to answer "what do I
  have running," or are there gaps to close (resolved by the Tier-1 E2E)?
- Does an in-process multi-node + replication harness already exist for Tier-2, or do we
  need to build it? (Likely partially present in `subagent_source_conformance.rs`.)
- Surface allowed targets to the agent via prompt-context injection only, or is a
  read-only "list my allowed targets" tool worth it too? (Leaning: context injection only,
  to stay clear of anything resembling dynamic discovery.)

## Related

#377 (this) · #378 (workflow orchestration, unblocked) · #9 (principal/behavior/
deployment identity) · #213/#177/#216/#200 (subagent substrate) · #363,
defradb.rs#1012/#1013 (replication) · #8 (apply path) · #369 (shared schema) ·
`Background.lean`, `Triggers.lean`.
