# Desktop Dashboard Design

Tracks GitHub issue #19. Aesthetic and layout source of truth: `./2026-04-13-desktop-dashboard-mockup.html`.

## Overview

Build a native desktop dashboard for observing and operating `defra-agent` deployments. The app is a pure-Rust egui binary that runs as a **DefraDB peer** — embedding its own `defra-node`, peering with one or more `defra-agent` runtimes via the IROH transport that `defra-node` uses internally, and reading and writing replicated documents locally.

The dashboard never runs the agent loop. It is a client-only peer participating in the same control plane that the runtime already uses. Target users are operators running one or more agent deployments; the register is field-serviceable power-user tool, not consumer chat app.

## Decisions at a glance

| Decision | Choice | Rationale |
|---|---|---|
| UI stack | egui via eframe | Pure Rust; immediate-mode fits live dashboards; workspace-native |
| Transport (day-one) | Embedded `defra-node` + IROH P2P (via `defra-node`'s public API) | Matches the CRDT-aware Lean client spec; push via `events::Subscription`; no polling; offline-capable; multi-deployment switching is free |
| Crate split | New `defra-agent-protocol` (pure types) + new `defra-agent-desktop` (binary) | Clean boundary between portable protocol types and platform-coupled application code |
| Identity | One principal per install, auto-generated on first launch | Multi-profile / key rotation / keychain storage are all P1+ |
| TUI | Deleted when desktop reaches parity | No active users; no deprecation ceremony needed |
| Observation model | Materialized `ClientStore` + `tokio::sync::watch` change signal | egui reads a snapshot each frame; background task updates store on `EventName::Update`, ticks the watch; UI requests repaint |

## Architecture

### Runtime topology

```
+------------------------------+        IROH gossip        +------------------------------+
|  defra-agent-desktop         |   CRDT replication of:    |  defra-agent server          |
|  (egui + eframe binary)      |                           |  (local or remote)           |
|                              |   AgentRequest, Response, |                              |
|  +------------------------+  |   Message, ToolCall,      |  +------------------------+  |
|  | egui UI layer          |  |   ToolResult, Conv.,      |  | runtime + embedded     |  |
|  +------+-----------------+  |   Runtime, Behavior,      |  | defra-node             |  |
|         |                    |   Principal, Scheduled-   |  +------------------------+  |
|  +------v-----------------+  |   Task, ToolSelection,    |                              |
|  | desktop::state         |  |   InferenceProfile,       |                              |
|  +------+-----------------+  |   InferenceBackend        |                              |
|         |                    |                           |                              |
|  +------v-----------------+  |                           |                              |
|  | embedded defra-node    |<-+---------------------------+>                             |
|  | + ClientStore          |  |                           |                              |
|  | + PeerDirectory        |  |                           |                              |
|  | + PrincipalIdentity    |  |                           |                              |
|  +------------------------+  |                           |                              |
|                              |                           |                              |
|  local replica (on disk)     |                           |                              |
+------------------------------+                           +------------------------------+
```

Consequences:

- Desktop writes hit the **local** node, signed by its principal DID. Replication is `defra-node`'s concern — the desktop does not bind IROH types directly.
- Reads and writes go through `EmbeddedNode::execute`, which is identity-aware — the desktop's principal scopes every call. No lower-level `query` crate dependency is needed.
- Observation is push-based via `events::Subscription(EventName::Update)` — the same mechanism `DefraWatcher` uses in the runtime (`crates/defra-agent/src/watcher.rs`).
- Peers are added by pasting an IROH node address / ticket string (whatever `defra-node`'s `p2p_handle` accepts); discovery mechanisms beyond that are post-MVP.
- Multi-deployment switching is effectively free: peer data already lives in the local store; the UI just re-filters by `agent_did`.
- Offline works: local reads always succeed; writes queue and gossip when a peer is reachable.

### Crate layout

```
crates/
  defra-agent/            runtime (depends on defra-agent-protocol)
  defra-agent-cli/        scripting CLI; keeps its HTTP GraphQL path
  defra-agent-protocol/   NEW -- schemas + client_protocol + row mirrors
  defra-agent-desktop/    NEW -- eframe binary
```

**`defra-agent-protocol`** (tiny, pure-types, dependency-light)
- `schemas/*.graphql` files plus `pub const SCHEMAS: &[&str]` — single source of truth for collection registration
- `client_protocol` module — `ClientTurnState`, `RequestLifecycleState`, `derive_attempt`, `derive_turn` (moved intact from `defra-agent`; Lean conformance tests port with it)
- Serde row mirrors for every replicated collection
- Dependencies: `serde`. No `defra-node`, no `tokio`, no `egui`

**`defra-agent-desktop`** (fat, self-contained binary)
- `main.rs` + `app.rs` — eframe entry, terminal-forward theme, window/layout
- `client/` — `AgentClient`, `ClientStore`, `PeerDirectory`, `PrincipalIdentity`, observation loop, submission API
- `views/` — `chat`, `operator`, `peers`, `logs` submodules plus shared sidebar components
- `state/` — view/selection/composer state (egui-shaped)
- Dependencies: `defra-agent-protocol`, `defra-node`, `events`, `identity`, `crypto`, `eframe`, `egui`, `egui_dock`, `egui_commonmark`, `syntect`, `tokio`. No direct `iroh` dependency — peer-address parsing and connection management go through `defra-node`'s public API.

**Existing `defra-agent` runtime**
- Adds `defra-agent-protocol` as a dependency, re-exports `client_protocol` for internal callers
- Schema registration switches to `defra_agent_protocol::SCHEMAS`
- Otherwise unchanged

### Observation pipeline

```
user composes  -->  AgentClient::submit_request(spec)
                     --> local store write, signed by principal DID
                     --> IROH gossip to peer (via defra-node)
                     |
peer runtime claims + processes,
writes AgentResponse / Message / ToolCall / ToolResult
                     |
              IROH gossip back to desktop
                     |
              events::Subscription fires EventName::Update
                     |
              background task re-queries changed collection,
              updates ClientStore, ticks watch::Sender
                     |
              egui view reads store snapshot,
              redraws via ctx.request_repaint
```

Turn derivation inside views calls `store.derive_turn(session_id)`, which wraps the Lean-proven `client_protocol::derive_turn`. The desktop is a new **consumer** of the already-proven projection; no new turn semantics are introduced.

## Aesthetic direction

Retro-future alien / machine / oxidized field-tool. Reads as a panel on equipment that has been running a long time. Reference register: Nostromo instrumentation, Dune fieldwork, burnished metal with patina.

**Palette.** Variables in the mockup are the source of truth. Summary:
- Base: cast-iron `#14110D`; elevated `#1C1812` → `#251F17`
- Strokes: warm browns `#3C342A` → `#554736`
- Text: bone warm `#E8DCC7` → dust `#B3A085` → washed ochre `#7D6C55`
- Accent (single): tarnished copper `#D17A3A` — only color used for active / affirmative state
- Warning brass `#E8A85A`; Danger oxidized red `#B85540`; Info dusty teal `#5E8282`

**Typography.**
- UI body: **Chakra Petch** — angular, chamfered, not overused
- Technical (IDs, timestamps, tool args, paths, DIDs): **Space Mono** — has character, reads as console output
- Stenciled labels (section headers, buttons, tab labels): **Big Shoulders Stencil Display**
- All three bundled as static TTFs via `include_bytes!` and registered in `egui::FontDefinitions`

**Shape language.** 1px borders over shadows. Cut/chamfered corners via polygon clip on major action buttons and framed chips. Flat fills. No gradients. Subtle radial background tints evoke two light sources (machine bay lighting).

**Motion.** Mechanical-instrument register: linear easing only (no ease-in-out), transitions capped at 200ms, stepped toggles (snap on/off rather than fade). Slow 3.6s "throb" on status dots (warning-lamp register, not chat pulse). Decorative scanline and CRT overlays in the mockup are stage dressing; skip them in the egui port. Active-turn indicators animate via `ctx.request_repaint_after` + sine-wave alpha.

**Decorative chrome.** L-shaped copper corner ticks on framed panel headers; copper seam gradient on top edge of the status bar; frame-counter readout in the status bar ("this is a live system").

The mockup at `./2026-04-13-desktop-dashboard-mockup.html` is the canonical reference for layout and aesthetic. The egui port should match sizes, spacings, and colors unless a platform constraint forces divergence.

## UI structure

**Four activities, selected via a 52px activity bar on the far left.** Each activity has its own contextual sidebar + main pane (+ optional right rail on non-Chat views). A shared identity chip at the bottom of the activity bar shows the principal DID abbreviation with a throbbing copper dot.

### Chat activity

Sidebar:
- `Deployments` section: each deployment row (one peer = one IROH node in the local address book) shows connection health + "N agents" metadata. Agents are indented beneath their deployment, connected by a 1px tree line. Tree grouping is display-only — it records which peer we most recently observed replicating an agent's state; it is **not** a primary key. `AgentConversation` and `AgentRequest` carry `agent_did` and no peer provenance (see `crates/defra-agent-protocol/schemas/agent/agent_conversation.graphql`), so selection routes by `agent_did` alone. Clicking an agent selects it as the active principal.
- `Conversations` section: filtered by `agent_did` only. Grouped Today / Yesterday / Earlier. Each row shows title, session meta, relative timestamp.

Main pane:
- Transcript with role-labeled messages, inline collapsible tool cards (status-differentiated borders), inline collapsible reasoning disclosures on assistant messages.
- Pulsing turn-state chip above the composer, driven by `store.derive_turn(session_id)`.
- Composer: text input, behavior picker chip, tool-selection chip, `Cmd+Enter` hint, chamfered Send button (disabled while a non-terminal turn is active).
- Header: breadcrumb `deployment / agent / conversation` plus turn status badge and Retry/Export buttons.

No right rail on Chat. Runtime state surfaces in the status bar; tool calls and reasoning are already inline in the transcript; duplicating them in a side panel adds noise without adding information.

### Operator activity

Sidebar:
- Same deployment → agents tree at the top.
- `Manage` category list: Runtime, Behaviors, Backends, Tool selections, Inference profiles, Scheduled tasks.
- `Inspect` category list: Request timeline, Recent failures.

Main: filterable entity list. Each row shows name + short id + key scalar fields (model, backend, default tag).

Right rail: in-place editor for the selected entity. All fields on `AgentBehavior`, `InferenceBackend`, `ToolSelection`, `InferenceProfile`, `ScheduledTask` map 1:1 to editor fields. `Apply` / `Discard` footer.

### Peers activity

Sidebar:
- `My identity` card at top: DID + Copy + Show QR.
- `Peered deployments` tree (same pattern as elsewhere).
- `Pending access` section with count, warning-colored.

Main: current principal's grants on the selected peer as a table (collection / action / granted_at). Below that, pending incoming access requests with inline Grant / Deny.

Right rail: peer detail (node_id, IROH address / ticket, bytes ↑↓, schema rev match) and per-collection replication status.

### Logs activity

Full-width log stream with filter chips (Replication, Peering, Turns, Writes, Warnings). Right rail shows summary diagnostics (local store size, replication lag p50/p99, peer counts, events/sec). Live only; no persisted log history in MVP.

### Status bar (always visible)

One monospaced line: `peered N/M · <agent> runtime: <state> · gossip lag Xms · replication: converged · errors N · frm:xxxx · did:defra:… · build`. Reads like a systems-tool readout.

Scalar metrics (gossip lag, replication lag, events/sec, error count) render a 40×14 single-polyline sparkline behind their label — 30-second rolling window, copper line, amber when a configured threshold is breached. Instant vibe-check telemetry without adding rows.

### First-launch experience

1. Generate principal on first start via `defra-node`'s identity layer. Storage location follows `defra-node`'s inherited identity persistence convention — the desktop does not pick a bespoke file path. Surface the DID with a Copy affordance so the operator can paste into an ACP grant on a peer.
2. No peers → sidebar replaced by an "Add deployment" empty-state card. Dialog takes label + IROH node address (or ticket) + agent_did, hands the address to `defra-node` to dial, verifies schema compatibility, registers replication.
3. No conversations for the selected agent → empty-state nudge in the center pane that creates a new conversation on click.

## MVP scope (15 tickets)

Each ticket represents a meaningful slice of work; detailed implementation plans come later from `writing-plans`.

| # | Ticket | Can start after |
|---|---|---|
| T1 | Extract `defra-agent-protocol` crate (schemas + `client_protocol` + row mirrors + port conformance tests) | — |
| T2 | Scaffold `defra-agent-desktop` + bundle fonts + apply theme to `egui::Visuals` + ship empty activity/status bars | T1 |
| T3 | Client core: boot embedded node, register schemas, generate/persist principal, load peer directory, dial peers | T1 |
| T4 | Client observation: subscribe to `EventName::Update`, materialize `ClientStore`, expose `watch::Receiver` change signal; add `ClientStore::focused_request_id: Option<String>` slot for cross-view selection (visible wiring in P1) | T3 |
| T5 | Client submission API: `submit_request`, `create_conversation`, `add_peer`, `remove_peer` | T4 |
| T6 | Chat activity MVP: deployment→agents tree + conversations + transcript with inline tool cards + composer + turn-state chip | T5, T2 |
| T7 | Chat rendering: `egui_commonmark` for markdown, `syntect` for code blocks, inline collapsible reasoning disclosures | T6 |
| T8 | Operator: Behaviors + Backends + Tool selections + Inference profiles (list + editor pattern for each) | T5 |
| T9 | Operator: Scheduled tasks (list + editor + run-now + enabled toggle) | T5 |
| T10 | Operator: Runtime inspector + Request timeline + Recent failures (read-only dashboards) | T5 |
| T11 | Peers activity: identity card + grants table + pending access Grant/Deny flow | T5 (also DB ACP surface) |
| T12 | Logs activity: tracing-layer subscription + IROH peer events (via defra-node) + filter chips + diagnostics rail | T3 |
| T13 | First-launch onboarding: identity generation screen + Add-deployment dialog + empty states | T6 |
| T14 | Command palette (Cmd+K) with verb-prefix actions spanning all activities (`> switch deployment`, `> new behavior`, `> run scheduled task`, `> pin request`, `> search logs`, etc.) rendered in Space Mono; supporting keyboard shortcuts; native menu bar; persisted window/activity layout | T6 |
| T15 | Delete `defra-agent-cli tui` and its ratatui / crossterm workspace deps | T6, T7, T10 |

### Dependency graph

```
T1 --> T2
T1 --> T3 --> T4 --> T5 --> {T6, T8, T9, T10, T11}
T6 --> {T7, T13}
T3 --> T12
T6 --> T14
{T6, T7, T10} --> T15
```

## Post-MVP phases

- **P1 · Multi-principal + operator polish.** Multiple identities in one install; per-principal UX state; identity switcher on the activity bar. Hover-reveal detail HUDs for abbreviated technical identifiers (DIDs, CIDs, IROH node addresses, node_ids) — small corner-pinned panel, not floating tooltip. Cross-view highlighting wired to `ClientStore::focused_request_id`: clicking a request_id in Logs pins it, highlighting it in Chat transcript and Operator timeline.
- **P2 · Multi-agent side-by-side + network map.** Docked split chat pane; "send to N" composer mode; correlated turn state across panes. Tactical P2P network map in the Peers view once peer count warrants (~5+): star or circular topology visualization with line thickness / color indicating replication lag and bandwidth.
- **P3 · OS integration.** Keychain for principal keys; OS notifications for turn completion / ACP denials / incoming access requests; menu-bar mode.
- **P4 · Mobile-reusable extract.** Revisit only if iOS needs a Rust client layer. Probable extraction target: `ClientStore` + observation loop into a `defra-agent-client` crate, wrapped via uniffi. Not done speculatively.
- **P5 · Distribution.** Notarized `.dmg`, signed `.msi`, `.AppImage`; auto-update channel.

## Deferred / caveats

- **ACP authoring.** MVP assumes the agent peer has already granted the desktop's DID. First-launch surfaces the DID for manual grant. First-class ACP authoring on the server side is out of scope here.
- **Schema-version skew.** If a peer runs a divergent schema revision, MVP blocks replication with a clear error instead of attempting to run on a skewed store. Resolution UX (migrate / pin / reject) is follow-on.
- **Key storage.** MVP inherits `defra-node`'s identity persistence mechanism; we do not invent our own on-disk format. OS keychain integration (if `defra-node` does not already provide it) is P3.
- **Peer discovery.** MVP requires manual paste of an IROH node address / ticket. mDNS / rendezvous / DHT-style discovery is post-MVP.
- **InputRequired lifecycle.** Per `Proofs/Conformance/Deviations.lean`, clients currently treat `inputRequired` as `waitingForClaim`. Desktop follows suit; a genuine input-required UX is a runtime/protocol task, not a desktop task.
- **Mobile portability.** The `defra-agent-protocol` crate is shaped to be the portable substrate if iOS ever needs it. The desktop's `ClientStore` + `PeerDirectory` are kept inside `defra-agent-desktop` — they are platform-coupled and not worth extracting until a real second consumer appears.

## References

- GitHub issue #19 — "Build a desktop dashboard app (egui) for defra-agent"
- `docs/protocols/client-state-machine.md` — client turn observation protocol
- `crates/defra-agent/proofs/Proofs/Client.lean` — Lean reference for T1–T5 properties
- `crates/defra-agent/src/client_protocol.rs` — current Rust implementation (moves to `defra-agent-protocol` in T1)
- `crates/defra-agent/src/watcher.rs` — reference implementation of `events::Subscription(EventName::Update)` usage
- `docs/superpowers/specs/2026-04-13-desktop-dashboard-mockup.html` — aesthetic and layout source of truth
