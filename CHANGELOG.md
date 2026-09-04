# Changelog

All desktop crates and npm packages release together at `workspace.package.version`
(lockstep train). Bridge **contract** version (`MAJOR.MINOR`) moves independently
and is what compatibility decisions key on — see `contracts/desktop-bridge.json`.

## Unreleased

### Added

- Desktop bridge 6.2: `MCPServiceHealthView.displayState` (`healthy | stale |
  unreachable`) is the only MCP health classification; the desktop's
  synthetic `stuck` state is removed.

### Breaking changes

- `AgentRequest.status` is removed; `lifecycle_state` is the only request
  state column and `gents_protocol::request_lifecycle::RequestLifecycleState`
  is its only owner (#1330). The pre-claim `workspace_binding_pending` status
  is now the lifecycle state `workspaceBindingPending`. `AgentRequest` is a
  client-authored collection that evolves only by baseline re-pin, so
  existing stores fail `ensure_migrations` with `UnknownLineage` for
  `AgentRequest` after upgrading and must be reset or export/imported; there
  is deliberately no migration step.
- Desktop bridge contract 5.2 -> 6.0: `SubagentNodeView`, `TaskRunResult`,
  and `TaskRunSummaryView` lose their request `status` field. Clients on an
  older contract are rejected.
- CLI JSON output drops the duplicate request `status` fields
  (`SubagentTreeNode.status`, `SessionHistoryRow.latest_request_status`,
  `RequestShowHeader.status`, `ChildRequestView.status`,
  `GraphRunRequestView.status`); read `lifecycle_state` instead.

### Fixed

- Config documents are validated by one owner regardless of write path;
  `gents config agent-behavior set` now rejects unknown backends, models,
  tool selections and profiles.
- `/healthz` and `gents fleet-slots` now report backends the local prober has
  vetoed as degraded/not accepting, matching admission.
- `Goal.tokens_used` now reports the charged total (input incl. cached +
  output), matching the request ledger; `/self` utilization is measured
  against the effective input budget.

## 0.15.0 - 2026-09-01

### Breaking changes

- Advance the Rust and npm desktop package train together to 0.15.0 and the
  desktop bridge contract from 1.5 to 4.0.
- Replace the retired Lark/RocksDB storage backends with Regolith through the
  DefraDB v0.19.0 cutover. Existing legacy data directories are rejected at
  startup; reset runtime state, or use Gents v0.14.0 to export data first.
- Remove legacy bearer pairing, local mobile-runtime request creation, and
  unsigned authority paths. Enrollment, routing, readiness, and mobile
  requests now require authenticated runtime-owned state (#1310-#1313).

### Mobile authority and hydration

- Bind enrollment signatures, leases, revocation, replay protection, and
  generation changes to the final authority boundary, and prevent offline or
  accumulated authorizations from starving an active enrollment (#1312).
- Make the runtime the sole readiness authority and require requester-bound,
  terminally complete two-node hydration with explicit ordering and counts
  before the desktop projects success (#1310, #1311).
- Tag and authenticate every request source, remove the mobile local-runtime
  path, and replace compatibility pairing UI with status-based enrollment
  controls and actionable hold diagnostics (#1313).

### Runtime, research, and reliability

- Add the web deep-research graph pack, external dependency projection, live
  qualification harness, and the open-source research gateway integration
  (#1277, #1294, #1296).
- Harden Ethereum wallet submission and mobile first-run, idle hydration, and
  viewport ownership behavior (#1282, #1283, #1297, #1309).
- Evaluate readiness freshness against an injected observation clock so stale
  state still fails closed without making deterministic CLI tests wall-clock
  dependent (#1317).

### Dependencies

- Upgrade every DefraDB crate to the v0.19.0 tag (`03e1035f`), adopt Regolith
  as the sole durable backend, require transport-routable gossip origins, and
  enable verified post-merge rebroadcast for Gents' multi-hop deployments.

## 0.14.0 - 2026-08-28

### Bridge contract

- Advance the Rust and npm desktop package train together to 0.14.0 and the
  additive desktop bridge contract from 1.3 to 1.5.
- Add owner-scoped mailbox commands plus truthful session hydration and
  global sync-health projections, including explicit progress, stalled, and
  schema-skew evidence for mobile clients (#1205, #1248-#1252).

### Mobile and desktop

- Move transcript projection into bounded database queries and preserve
  request-owned session hydration across reconnects and app backgrounding
  (#1212, #1248-#1252).
- Bind iOS bearer readiness and issuer records to stable endpoint identity,
  and harden the native readiness acceptance harness (#1235-#1237).
- Publish a development-signed arm64 iOS IPA for registered-device testing
  alongside the CLI and desktop package artifacts.

### Runtime, graphs, and CLI

- Add the Lean-fenced graph execution contract, immutable graph runs, bundled
  code-review package, and graph quickstart (#1225-#1230).
- Share desired-state application through the runtime, lengthen long-running
  agent deadlines, and reject liveness windows that cannot expire before the
  request deadline (#1219, #1240, #1264).
- Hydrate materialized `response show` output consistently with wait/chat and
  scope Codex shim skill toggles to the bound agent (#1242, #1260).

### Reliability and maintainability

- Fail closed on occupied or ambiguous server ports, synchronize runtime and
  bridge readiness on durable events, and route flaky fixture writes through
  bounded transaction-conflict retry (#1243, #1253, #1254, #1257-#1259).
- Split the Codex shim, owned completion loop, desired-state validator, bearer
  pairing, and P2P reconciliation into domain-focused production and test
  modules while preserving their public and observability contracts.
- Retire stale Operations-drawer journeys and replace blind background-tool
  polling with event-backed terminal-state diagnostics (#1241, #1268).

### Dependencies

- Advance DefraDB to `81ff3cee`, including upstream schema relation/default,
  query ordering/join, P2P replay-marker, filtered truncate, and embedded HTTP
  schema-operation fixes.

## 0.13.0 - 2026-08-25

### Bridge contract

- Advance the Rust and npm desktop package train together to 0.13.0 and the
  desktop bridge contract from 0.9 to 1.3.
- Make client-authored requests the sole session creation path, removing the
  `desktop_session_fork` projection, and add revisioned live-session deltas,
  bounded transcript-page evidence, exact-total markers, and observer merge
  counters (#1160, #1154, #1203).

### Mobile and desktop

- Add request-owned remote session hydration with explicit pending, stalled,
  schema-skew, and terminal states; keep recovery truthful across mobile
  backgrounding and reconnects (#1154).
- Bound long-session work at the database query, bridge payload, and React
  rendering seams: query transcript pages at the tip and backward cursor,
  preserve tool-call boundaries, coalesce live response updates, and avoid
  remounting previously rendered rows (#1184, #1185, #1203).
- Centralize reliable multi-server pairing and replace repeated full-store
  replication scans with explicit document-set replay (#1186).

### Runtime and formal foundation

- Add Lean-fenced isolated workspaces, callback planning and execution,
  request-scoped tool roots, frozen instruction provenance, and explicit
  cleanup receipts for agent-owned workspace lifecycles (#1164-#1174).
- Publish the graph pipeline foundation, pure intent compiler, task-backed
  graph routes, and bounded graph tool surface (#1190-#1193).
- Discover live `AGENTS.md` instructions for unbound requests and tighten the
  graph-native defending-code review pack (#1173).

### CLI, testing, and reliability

- Keep explicit transport log filters intact and expand live pairing,
  hydration, pagination, and inference acceptance coverage (#1188, #1154,
  #1203).
- Add repeatable mobile interaction artifacts and structural budgets while
  keeping noisy wall-clock measurements report-only (#1203).

### Dependencies

- Advance DefraDB to `54b629b1`, including explicit document-set P2P replay;
  this revision is the direct child of the current DefraDB `main` head.

## 0.12.0 - 2026-08-20

### Bridge contract

- Keep the additive desktop bridge contract at 0.9 and advance the Rust and
  npm desktop package train together to 0.12.0.

### P2P and mobile

- Make mobile pairing converge across reconnects, layered filters, reverse
  pairings, and repeated reconcile passes; close fleet convergence
  amplification and pin DefraDB's merged replication-ownership fix (#1145,
  #1156, #1157).
- Eagerly replicate the requester-scoped session index to paired mobile peers
  and retry index synchronization from the P2P supervisor (#1141, #1148).
- Clarify mobile configuration back-navigation and skip disabled P2P metrics
  polling (#1146, #1130).

### Runtime and formal foundation

- Add correlated event-trigger fan-in, reliable background subagent
  continuations, and an optional native LSP tool for coding behaviors (#1113,
  #1117, #1115).
- Unify durable descendant authorization and projections, bound terminal
  GraphQL persistence retries, and centralize GraphQL validation and run
  provenance (#1124, #1132, #1136).
- Add bounded datastore query surfaces and keep client-authored conversation
  collections compatible with fresh stores (#1125, #1151, #1155).

### Agents, demos, and tooling

- Add the repository review harness plus executable maintenance, security-scan,
  and live code-review demo surfaces (#1121, #1135, #1151, #1155).
- Replace cluster triage labels with program milestones and enforce roadmap
  horizons through the issue-hygiene automation (#1107).

### Build and reliability

- Replace RocksDB with Lark, narrow DefraDB feature consumption, and improve
  release build attribution and artifact measurement (#1101, #1109, #1119).
- Consolidate integration-test binaries, make live gates explicit, and repair
  the post-merge conformance and migration fences (#1140, #1149, #1150).

### Dependencies

- Advance DefraDB to the ownership-corrected `f928b300` revision.

## 0.11.0 - 2026-08-11

### Bridge contract

- Advance the additive desktop bridge contract from 0.5 to 0.9: Grok OAuth
  login, managed local-server lifecycle and tray events, and provider-account
  inventory/disconnect controls (#973, #1013, #1089).
- Advance the Rust and npm desktop package train together to 0.11.0.

### Runtime and formal foundation

- Make schema provenance and signing DefraDB-native, and persist the exact
  rendered provider request before send (#1087, #1059).
- Add deterministic inference seeds and request-wide token budgets (#1062).
- Extend the Lean-fenced runtime foundation across prompt assembly, compaction,
  admission slot accounting, tool-call CAS transitions, recovery, and
  background/subagent lifecycle convergence (#999, #998, #1007, #1006).
- Align durable tool outcomes across timeline projections and add
  Harbor-compatible ATIF trace export (#1095, #1098, #988).

### Agents, configuration, and automation

- Add persona request flows, reusable directory persona and inference-profile
  catalogs, and safer self-configuration ergonomics (#1028, #1014, #1050,
  #1052, #1056, #1057).
- Add document-driven EventTrigger graph experiments and capture consumers
  for persisted runtime facts (#1081, #1080).

### Providers, CLI, and desktop

- Add Grok/xAI subscription OAuth across the runtime, CLI, and desktop (#973,
  #974), plus provider-account settings and disconnect controls (#1089).
- Make local desktop agent onboarding optional and add managed-server controls
  (#1013).
- Align CLI initialization and lineage behavior with signed provenance, while
  retaining compatibility with older provenance JSON (#1094, #1092).

### Build and reliability

- Split and cache Rust CI workloads, slim the CLI dependency graph, and add
  release dependency/binary metrics (#1011, #1024, #1097).
- Favor faster local and release builds with non-LTO profiles and parallel code
  generation; harden runtime cancellation, bridge reconciliation, and
  resource-contended conformance startup (#1097).

### Dependencies

- Advance DefraDB to v0.18.0 (`61e429fc`).

## 0.10.1 - 2026-07-30

### Dependencies

- Advance DefraDB to v0.17.4 (`f9e21c68`), following the v0.17.3 pin from #972.

### Runtime and reliability

- Stabilize mobile chat and stop synthetic agent turns (#930).
- Complete native backgrounding for subagents and tools with real GLM E2E (#937, #945).
- Fix machine pairing replicator install: `source_did` must be `@immutable` (#939).
- Hard-fail materialize + restore post-migration CI gates; complete and pin the
  runtime migration baseline catalog (#947, #949).

### Cleanup

- Integration cleanup pass for runtime, desktop, CLI, and tests (#970, #971).
- Clippy and harness hygiene for trigger tests (#961).

## 0.10.0 - 2026-07-29

### Bridge contract

- 0.5 (additive): merged #871 inference onboarding —
  `desktop_probe_inference_endpoint`, `desktop_codex_login`, and
  `desktop_codex_login_cancel` under `config-write`; new one-shot
  `desktop://codex-login-url` event.
- Inference onboarding request, response, and login-URL event payloads now come
  from generated Rust bindings instead of handwritten TypeScript mirrors.
- Local runtime initialization/reset is serialized with client start/shutdown
  and rejects storage mutation while a client is live.

### Runtime reliability

- Request interrupt latches use the standard bounded DefraDB
  transaction-conflict retry, eliminating an observed cascade-conformance flake.

### Bridge contract 0.4

- Additive: `Pairing` error code; fingerprint permission inventory aligned with
  grantable `[[set]]` entries (`core`, `client-lifecycle`, bundles).
- `BridgeConfig::default().snapshot_grants` is **fail-closed** `core_only()`.
- Documented v1 process-wide snapshot grant model (not per-caller ACL).

### Bridge contract 0.3

- Additive: structured `BridgeError` on command error paths; `SnapshotGrants`
  projection at the snapshot builder seam; `native-e2e` cargo feature.
- Command failures now serialize as `{ code, message, retryable }`; the client
  accepts both structured errors and legacy bare strings during migration.
- `RenderedTimelineItem` variant fields now serialize in camelCase, repairing
  the latent Rust/frontend mismatch (`itemKey`, not `item_key`).
- Breaking for pre-package bridge consumers: `desktop_peer_status_fetch`
  accepts a saved `peerId`, not an arbitrary `serverAddress`; arbitrary-address
  probing is restricted to the fleet-admin command.
- See prior entries for 0.2 (`desktop_bridge_contract`, address probe, and the
  saved-peer status lookup).

### Packages

- New npm workspace packages, distributed as GitHub Release tarballs:
  - `@source-inc/gents-desktop-client` — typed transport, shared store, errors, testing
  - `@source-inc/gents-desktop-ui` — accessible shared primitives
  - `@source-inc/gents-desktop-chat` — chat projection, components, and styles
  - `@source-inc/gents-desktop-fleet` — discovery, pairing, health, and peer UI
  - `@source-inc/gents-desktop-operations` — rail, holds, health, lineage, traces
  - `@source-inc/gents-desktop-tokens` — semantic CSS tokens
- Fixture host `apps/fixture-host` consumes all packages, renders bridge session
  snapshots, and registers a file-backed domain operations tab without
  `runtime-admin`. It proves package/plugin composition; the automated two-node
  Amygdala journey remains downstream evidence.
- Tag releases attach clean-install-verified npm tarballs to the GitHub Release;
  downstreams pin those assets exactly.

### Downstream update workflow

1. Bump the git tag pin for `gents-desktop-bridge` and npm pins to the same `vX.Y.Z`.
2. Read the **Bridge contract** section for additive vs breaking diffs.
3. Run contract + e2e + visual gates (fixture host is the template).
4. Merge.

## Compatibility matrix

| Tag        | Bridge crate | npm packages | contract_version | Notes                                         |
| ---------- | ------------ | ------------ | ---------------- | --------------------------------------------- |
| v0.15.0    | 0.15.0       | 0.15.0       | 4.0              | Authenticated mobile authority; Regolith; DefraDB v0.19.0 |
| v0.14.0    | 0.14.0       | 0.14.0       | 1.5              | Mobile sync health, graph review, clarity refactors; DefraDB `81ff3cee` |
| v0.13.0    | 0.13.0       | 0.13.0       | 1.3              | Hydration, bounded mobile transcripts, graph pipeline; DefraDB `54b629b1` |
| v0.12.0    | 0.12.0       | 0.12.0       | 0.9              | Mobile pairing convergence; eager session index; DefraDB `f928b300` |
| v0.11.0    | 0.11.0       | 0.11.0       | 0.9              | DefraDB v0.18.0; signed provenance and build metrics |
| v0.10.1    | 0.10.1       | 0.10.1       | 0.5              | DefraDB v0.17.4; mobile/subagent/migration fixes |
| v0.10.0    | 0.10.0       | 0.10.0       | 0.5              | Reusable desktop packages implemented in #878 |

## 0.8.0

Baseline before reusable package extraction (pre-#877 implementation).
