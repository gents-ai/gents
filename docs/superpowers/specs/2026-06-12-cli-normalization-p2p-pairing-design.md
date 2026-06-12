# CLI normalization + P2P pairing — design

Date: 2026-06-12
Branch: `cli-normalization`
Status: draft for review

## Goals

1. Normalize the CLI surface: one grammar, one output convention, read/write
   coverage for every collection an operator cares about.
2. Make P2P pairing between two nodes a first-class, document-driven flow
   instead of a manual multiaddr copy-paste run on both servers.

Decided up front (2026-06-12): uniform grammar with deprecation aliases (not
additive-only); pairing = server-side reconcile + invite-token exchange; full
stack (Lean → conformance → Rust) on this branch.

## Part A — audit findings

### Surface today

26 top-level commands, ~60 subcommands (`crates/defra-agent-cli/src/cli/args.rs:34`).
Groups: lifecycle (init/provision/reset/server), interaction (chat/codex/
request/response/session/subagent), introspection (status/show/diagnose/query/
background/fleet/tools), config (validate/diff/apply/backend/behavior/tools/
profile/task/skill/export/import), p2p, schema, trace, task, mcp.

`query` is the generic read escape hatch (structured read-only queries against
any collection). `config export` is the generic config read. Both stay.

### Coverage matrix (collection × CLI surface)

| Collection | list | show | set/create | rm | notes |
|---|---|---|---|---|---|
| Skill | ✅ | ✅ | ✅ add | ✅ | gold standard; + enable/disable/import/export |
| AgentRequest | — | ✅ | ✅ submit | n/a | + interrupt/resend/wait |
| AgentResponse | — | ✅ | n/a | n/a | + wait |
| AgentToolCall (background) | ✅ | — | n/a | n/a | |
| Subagent lineage | ✅ | — | n/a | ✅ cancel | |
| Task | ✅ | ✅ | manifest | — | duplicated: `task` ≡ `config task` (args.rs:1356-1377) |
| InferenceBackend | ❌ | ❌ | ✅ set | ❌ | + discover-models |
| AgentBehavior | ❌ | ❌ | ✅ set | ❌ | |
| ToolSelection | ❌ | ❌ | ✅ set | ❌ | resolved view via `tools explain` only |
| InferenceProfile | ❌ | ❌ | ✅ set | ❌ | |
| EventTrigger | ❌ | ❌ | manifest | ❌ | no dedicated commands at all |
| Schedule | ❌ | ❌ | manifest | ❌ | no dedicated commands at all |
| AgentSession | ❌ | ❌ | n/a | n/a | only `session fork` |
| McpService | ❌ | ❌ | manifest | ❌ | only `mcp probe` |
| PeerPairingDesired | ✅ | — | ✅ set | ✅ | `p2p pairings` |
| AgentRuntime | n/a | ✅ | n/a | n/a | `show runtime`, `status` |

Write-only config collections (backend/behavior/tools/profile) are the worst
gap: you can write documents you cannot read back except via raw `query` or
`config export`.

### Convention drift

- **Output flags:** per-command enums — `Text|Json`, `Table|Json`,
  `Tree|Table|Json` — all spelled `--output` but with different value sets and
  defaults; no shared enum.
- **ID args:** some commands accept both `--request-id`/`--task-id` and a
  positional (request show, response show, task run, subagent cancel); others
  are flag-only (skill group, pairings).
- **Verb synonyms:** `rm` vs `remove` vs `unpair` (alias of `pairings remove`
  AND a separate top-level `p2p unpair`); `add` (skill, p2p collections) vs
  `set` (backend/behavior/tools/profile/pairings).
- **Duplicate trees:** `task` ≡ `config task` (identical enums); `show
  request|response` ≡ `request show` / `response show`.

## Part B — normalization design

### Grammar rules

1. **Noun groups, standard verbs:** `list`, `show`, `set` (idempotent upsert),
   `rm`, plus `enable`/`disable`/`import`/`export` where they exist today.
   `add` and `remove` become hidden aliases of `set`/`rm`.
2. **One output enum:** a single shared `OutputFormat { Text, Table, Json, Tree }`
   ValueEnum; each command declares its default and its supported subset, but
   the flag is always `--output` and values mean the same thing everywhere.
3. **IDs positional everywhere**, with the existing `--*-id` flags kept as
   hidden aliases. Conflict semantics follow the existing
   `resolve_request_id` helper (`request_helpers.rs:497`): both provided and
   equal → fine; both provided and different → error; neither → error. That
   helper becomes the shared resolver for every dual-form ID.
4. **Common access args** (`--home`, `--graphql`) stay as the universal
   pattern (already consistent).
5. **Deprecations, not removals:** every renamed/merged command keeps its old
   spelling as a hidden alias for one release; deprecated spellings print a
   one-line stderr note. Mechanism: clap aliases alone cannot do this — the
   handler never learns which spelling was used (e.g. the
   `aliases = ["rm", "unpair"]` on `pairings remove`, args.rs:1794). A small
   argv pre-scan before clap parsing matches deprecated spellings against a
   static table and emits the warning; clap aliases are kept purely for
   routing. Removals land in the release after.

### Specific moves

- `task` (top-level) absorbs `config task`; `config task` becomes a hidden
  alias group. (Task triggering is an operator action, not configuration.)
- `show request|response|runtime` become hidden aliases of `request show`,
  `response show`, and a new `status --runtime`-equivalent (`show runtime`
  kept as alias; no new noun needed).
- `p2p unpair` (top-level dup) folds into `p2p pairings rm`.

### Gap fills (new commands)

| Group | New verbs | Notes |
|---|---|---|
| `config backend` | list, show, rm | |
| `config behavior` | list, show, rm | |
| `config tools` | list, show, rm | |
| `config profile` | list, show, rm | |
| `config trigger` (new) | list, show | EventTrigger; writes stay manifest-first (`config apply`), rm via `--prune` (#387) |
| `config schedule` (new) | list, show | same stance as trigger |
| `config mcp` (new) | list, show | registry rows; `mcp probe` unchanged |
| `session` | list, show | + existing fork |

Reads go through the same client/query layer the existing list/show commands
use; no new write paths besides what `set` already does. The rm
implementations reuse the proven delete path from the apply/prune model
(ApplyReconcile is already Lean-fenced for these collections); any collection
whose delete the Lean model does not cover gets list/show only until it does.

## Part C — P2P pairing

### Ground truth (researched 2026-06-12, defradb.rs v0.14.2 @ b3df179)

- **Transport is iroh.** Peers dial `EndpointAddr`s; every place defra-agent
  accepts a peer address already parses iroh `EndpointTicket` strings —
  a single-string encoding of peer id + relay + direct addresses
  (`defradb.rs crates/p2p/src/iroh/addr.rs:16,38`).
- **No P2P admin channel** (defradb.rs#1012): a node cannot ask a remote peer
  to subscribe a collection or install a replicator over the P2P wire. Any
  "configure both sides from one terminal" requires the remote's HTTP ops API
  (auth story: defra-agent#180).
- **Replicators and subscriptions persist inside DefraDB** across restart;
  what does NOT persist is live connectivity — the headless server does no
  peer re-dial at startup today.
- **No transport-level auth/allowlist**: unknown peers can subscribe to
  collection gossip; ACP gates document reads at merge time. A pairing is
  therefore a *trust declaration* (trusted-fleet stance, same as the
  cross-deployment subagent decision on #377).
- **Events exist for a reconciler**: PeerConnected/PeerDisconnected,
  replicator_completed, MergeComplete.
- **Pairing state today**: `PeerPairingDesired { peer_id (unique), agent_did,
  collections, replicator_addresses, timestamps }`
  (`defra-agent-schemas/schemas/agent/peer_pairing_desired.graphql`).
  Reconciled ONLY by the desktop supervisor behind
  `DEFRA_AGENT_PAIRING_RECONCILE=1`; headless servers never converge desired
  pairings (`commands/p2p/pairings.rs:15`). `p2p pair` exists but is
  imperative, one-directional, and must be run on both servers with manually
  exchanged addresses (`docs/operations.md:57-82`).

### Design: pairing is a document; the runtime converges it

**1. Server-side pairing reconcile (Lean first — by extension).**
A PairingReconcile model already exists
(`proofs/Proofs/PairingReconcile/{State,Transition,Convergence,Executable}.lean`)
and is already narrower than the Rust it fences: the Lean `DiffOp` covers
only installCollection/teardownCollection (State.lean:36) while the Rust diff
also emits replicator ops (`desktop-core/src/remote_admin/diff.rs:23`). This
work **extends the existing model** — no parallel spec — with explicit new
obligations:

- **Replicator dimension** (close the existing model↔code gap first).
- **Connection/dial dimension** (peer connected/disconnected; event nudges —
  PeerDisconnected → redial; periodic sweep rides the #477 registry).
- **Ownership-safe removal.** Today `compute_pairing_diff` tears down ANY
  actual collection/replicator not in desired state, and
  `load_desired_for_peer` returns empty desired state when the row is
  missing — so deleting (or failing to read) a pairing row diffs into
  teardown of everything live, including wiring installed manually via the
  low-level `p2p collections/replicators` commands. The model gains a
  managed set: each pairing row's reconciler tracks what it introduced
  (persisted alongside the desired row as applied-state), and teardown ops
  are restricted to managed objects. Unmanaged wiring is never touched.
- **Read-failure is not desire.** `load_desired_for_peer` also returns empty
  desired state on a GraphQL *error* (supervisor.rs:426 region) — a
  transient query failure is currently indistinguishable from "tear it all
  down". The model distinguishes `desired = ∅` (positive read of absence)
  from `desired = unknown` (read failed → tick is a no-op).
- Existing properties retained and re-proved over the larger state:
  convergence, idempotence, no-flap.

Conformance tests mirror the extended model under `tests/conformance/` per
the structure fence.

**Placement.** The production reconcile engine lives in
`defra-agent-desktop-core` (`remote_admin/diff.rs`,
`client/core/supervisor.rs`), which the runtime must not depend on — but the
dependency already points the other way (`desktop-core/Cargo.toml:14` depends
on `defra-agent`). So: the diff + reconcile engine moves into the
`defra-agent` runtime crate (e.g. `agent/p2p_reconcile/`), started/stopped by
`run_agent` alongside the other daemons; the desktop supervisor calls the
moved engine and `remote_admin/diff.rs` is deleted. Headless startup today
only ensures `PeerPairingDesired` migrations and loads paired peer DIDs into
the snapshot (`runtime/startup.rs:56,603`) — it gains the reconciler
unconditionally (the `DEFRA_AGENT_PAIRING_RECONCILE` flag dies), fixing the
no-redial-after-restart gap.

**2. Invite/join handshake (CLI ergonomics over documents).**
Because there is no admin channel, bidirectional pairing is a two-terminal
token exchange — but each step is one paste instead of today's
read-runtime.json/copy/flag-assembly:

- `defra-agent p2p invite [--profile <p>...]` (node A): prints one token —
  versioned, base58/CBOR — embedding A's iroh EndpointTicket, peer id, agent
  DID, and offered collection profiles.
- `defra-agent p2p join <token> [--profile <p>...]` (node B): validates,
  writes B's `PeerPairingDesired` row for A (reconciler does the live
  wiring), then prints B's reciprocal token with instructions.
- `defra-agent p2p join <reciprocal-token>` (node A): same command,
  symmetric — writes A's row for B. Pairing converges on both sides.
- `--wait` on join blocks until the local reconciler reports the pairing
  live (peer connected + subscriptions + replicator installed).

Optional one-shot: `p2p join --remote-graphql <url>` writes the remote row
directly when the operator can reach the peer's ops API. Off by default;
carries the #180 auth caveat in help text.

**3. Existing commands realign.**
- `p2p pair` becomes sugar for "write/refresh the desired row + `--wait`"
  (document-driven instead of imperative triple-call); same flags.
- `p2p pairings list` gains live-health columns (desired vs connected vs
  replicating) sourced from runtime status.
- `p2p status`/`peers` unchanged; `p2p connect`, `collections`, `replicators`,
  `documents` remain as the low-level imperative layer (useful for surgery
  and for non-paired topologies).

### Schema change

`PeerPairingDesired` gains `profiles: [String!]` (nillable — written as
`null` when empty per the empty-list sharp edge) so a pairing can track
profile intent, not just the resolved collection list at write time. Today
profile intent is lost immediately: `p2p pairings set --profile` flattens
profiles into `collections` at the CLI (`commands/p2p/pairings.rs:53`) and
the schema has no profiles field. The reconciler resolves profiles →
collections at reconcile time, picking up profile-definition changes.
`collections` remains for explicit pins.

This is one atomic slice — the pieces are coupled and ship together:
schema migration (via the existing `ensure_peer_pairing_desired_migrations`
vehicle, startup.rs:56), CLI write path (store profiles, keep writing the
flattened collections for back-compat), CLI read path, desktop pairing
bootstrap, and the reconciler's desired-state load.

## Testing

- Lean: PairingReconcile properties above, zero `sorry`s.
- Conformance: 1:1 mirror per the structure fence; reconcile-step table tests
  + liveness sweep tests (pattern from #473).
- Integration: two-node pairing test (invite → join → join → converged both
  ways), restart-reconverge test (kill one node, restart, pairing returns
  without operator action), removal test (rm row → wiring removed).
- CLI: snapshot tests for the normalized grammar; alias-compatibility tests
  asserting every deprecated spelling still parses and warns.
- Gate with `cargo test -p defra-agent` (full package) + CLI crate suite.

## Sequencing

1. Normalization mechanics (shared OutputFormat, positional IDs, aliases,
   task/config-task merge) — pure CLI, no spec impact.
2. Gap-fill read commands (list/show groups) — pure CLI.
3. Extend Lean PairingReconcile (replicator dimension first — closes the
   existing model↔code gap — then connections, managed-set removal,
   unknown-desired) + conformance mirror.
4. Move the reconcile engine from desktop-core into the runtime crate;
   headless reconciler in `run_agent` (+ desktop supervisor reuse, env flag
   removal, startup re-apply).
5. Profiles slice (schema migration + CLI + bootstrap + reconciler load,
   atomic per Part C).
6. Invite/join token flow + `p2p pair` rework + pairings health columns.
7. Gap-fill rm commands where ApplyReconcile already covers the delete.

## Open questions

- Token format: raw iroh EndpointTicket + sidecar fields, or fully custom
  CBOR envelope? (Decide at impl time; envelope recommended for versioning.)
- Whether `config trigger`/`config schedule` should get `set` now or stay
  manifest-only (manifest-only proposed; revisit after Skills slices land).
- `--remote-graphql` one-shot: ship now with caveats or hold for #180? (Hold
  proposed unless demo pressure says otherwise.)
