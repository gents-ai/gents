# Reusable desktop packages

*Design spec — 2026-07-27. Issue [#877](https://github.com/source-inc/gents/issues/877)
(`design` label: design-only spec PR). Status: proposed, awaiting maintainer review.
Base: the iPhone/bearer-pairing series (`agent/iphone-amy-bearer-pairing`, six commits
ending `e3d19f7a`), which this design treats as load-bearing evidence, not incidental
history.*

Gents Desktop already keeps its reusable runtime behavior in
`crates/gents-desktop-core`, but everything above that — the Tauri command bridge, the
typed TypeScript transport, the view models, and the chat/fleet/operations workflows —
lives inside `apps/gents-desktop`. A downstream Tauri/React distribution therefore has
to fork the app or copy private source. This document specifies a first-party package
boundary that lets a downstream app own its identity, shell, storage, and domain while
reusing Gents chat, fleet, and operator behavior through versioned dependencies.

The motivating downstream is **Amygdala**, a separately branded household-operations
app. Its kitchen-inventory domain stays out of Gents; the extension seam it needs lives
here. Gents Desktop itself becomes the first-party consumer of every seam this document
defines, so the public boundary is exercised on every CI run rather than trusted on
faith.

Related docs: [gents.md](gents.md) (platform architecture),
[operations.md](operations.md) (pairing and desktop operation),
[../apps/gents-desktop/README.md](../apps/gents-desktop/README.md) (desktop app),
[../apps/gents-desktop/tests/AGENT_BROWSER.md](../apps/gents-desktop/tests/AGENT_BROWSER.md)
(semantic browser harness), [../crates/gents/proofs/README.md](../crates/gents/proofs/README.md)
(proven core).

## Current state, as evidence

The boundary proposed below follows the seams the code already has. The load-bearing
facts:

**Rust.** `crates/gents-desktop-core` has no Tauri dependency; it exposes `client`
(`ClientCore`, store, queries, mutations, bearer pairing), `local_runtime`, and
`remote_admin`, and owns identity (`PrincipalIdentity::load_or_create`), storage
layout (`DesktopPaths`, keyed off `GENTS_DESKTOP_HOME`), schema registration
(`ensure_runtime_schemas` + `subscribe_all_collections` inside `ClientCore::start`),
and the embedded DefraDB node. All Tauri coupling lives in
`apps/gents-desktop/src-tauri/src/bridge/`:

- `mod.rs` builds the `tauri::Builder` and registers **55 commands** in one
  `generate_handler!` list; there is no `.setup()` hook (client start is the lazy
  `desktop_client_start` command), one managed state type
  (`DesktopAppState { bridge: Mutex<DesktopBridge> }`), one plugin
  (`tauri-plugin-opener`), and exactly **one event name**,
  `desktop://client-updated`, with payload `{ reason }` where reason ∈
  `{store, health, lifecycle, config}`.
- `tauri_commands/*` are thin `#[tauri::command]` wrappers; the logic beneath them —
  `commands/*`, `snapshot/*`, `types/*` (view models), `cascade.rs`,
  `cause_derivation.rs` — is already Tauri-agnostic.
- The bridge depends on `gents-desktop-core`, and directly on the `gents` runtime
  crate (backend registry, tool-surface explain, graphql helpers) and
  `gents-protocol` (bearer tokens).
- The debug-only native-E2E commands (`desktop_native_e2e_config`,
  `desktop_native_e2e_status`) are registered unconditionally but double-gated:
  `#[cfg(debug_assertions)]` bodies plus a `GENTS_NATIVE_E2E=1` runtime check;
  release builds compile them to inert stubs.

**Frontend.** One private npm package (`gents-desktop-tauri`, Vite 7 + React 19, no
router, no state library). The Tauri transport is hard-coded in exactly three modules:
`src/lib/desktop-api.ts` (a `DesktopApiAdapter` object of ~50 typed methods over
`invoke`, with a test-only override `setDesktopApiAdapterForTests`),
`src/lib/desktop-events.ts` (one `listen` wrapper, same override pattern), and
`src/lib/nativeSimulatorE2e.ts` (the in-app native-E2E driver). View-model types are
hand-written mirrors of the Rust `bridge/types/views/*` structs (comment-enforced,
no codegen, no drift gate) and are imported by 49 files through the `lib/types`
barrel. `useDesktopShell` is a single god-hook, but its action factories are already
partitioned by domain (`desktopShellChatActions`, `...PeerActions`,
`...ConfigActions`, `...TaskActions`, `desktopShellEffects`), and pure projection
logic (`chat-shell.ts`, `conversation-selection.ts`, `fleetMetrics.ts`,
`lineageModel.ts`) is separable. Only 11 components call `desktop-api` directly;
the rest are prop-driven presentation. Styling is global CSS under `@layer` with
semantic tokens in `styles/tokens.css`, `[data-theme]` switching, and one primary
breakpoint (`max-width: 760px`) repeated as a literal in ~10 files. Branding is not
centralized: name strings and logo live in `components/fleet/BrandLockup.tsx`, brand
colors in `tokens.css`.

**Tests.** Unit/component suites and the deterministic browser harness import app
internals through deliberate seams (`setDesktopApiAdapterForTests`,
`setDesktopClientUpdatedListenerFactoryForTests`,
`setDesktopShellTimingConfigForTests`); the external lanes — `tests/agent-browser.mjs`
(deterministic and live modes, `iphone` default viewport) and
`tests/run-ios-simulator-e2e.mjs` + `tests/ios/GentsUITests.swift` — drive only public
surfaces: the Vite-served harness, the `bridge_runner` binary's JSON-ready protocol,
`data-testid` selectors, the `com.source-inc.gents` bundle id, and the
`native-e2e-status.json` temp-file contract. The iPhone branch added mobile bearer
pairing, chat recovery/reconnect/interrupt routing, responsive layout, the agent
browser, and the native Simulator lane; all of these are contracts this design must
keep working, and most of them already point at the seams a package boundary needs.

**Workspace and releases.** Cargo versions are workspace-inherited (`0.8.0`), the npm
version is kept in lockstep manually, releases are git tags `vX.Y.Z` validated against
`workspace.package.version` by `release-macos.yml`. Nothing is published to crates.io
or npm today — and the DefraDB dependencies are git-pinned
(`ssh://…/sourcenetwork/defradb.rs.git`), which makes crates.io publication of any
crate in this dependency cone **impossible**, a hard constraint on the release design
(§ Compatibility and release contract).

## Package and dependency graph

### Rust

One new crate, extracted from `apps/gents-desktop/src-tauri/src/bridge/`:

```
gents (runtime)        gents-protocol
      ▲                     ▲
      │                     │
      └──── gents-desktop-core ◄──────────────┐
                    ▲                         │
                    │                         │
            gents-desktop-bridge  (new; depends on tauri, gents,
                    ▲              gents-desktop-core, gents-protocol)
        ┌───────────┴───────────┐
 gents-desktop-tauri      <downstream host crate, e.g. Amygdala>
 (app binary; owns          (owns its own Builder, identity,
  Builder + branding)        schemas, extra commands)
```

- **`crates/gents-desktop-bridge`** takes the entire `bridge/` tree: the
  Tauri-agnostic logic (`commands/*`, `snapshot/*`, `types/*`, `cascade.rs`,
  `cause_derivation.rs`, `logging.rs`), the `#[tauri::command]` wrappers, the managed
  state, and the update pump. It exposes a Tauri **plugin** (§ Native composition
  contract) rather than a builder. It also gains ownership of the `bridge_runner`
  test binary (behind a `test-harness` cargo feature) so the live and agent-browser
  lanes exercise the extracted crate, not the app.
- **`gents-desktop-core` is unchanged in role**: no Tauri, no view models. It gains
  additive host-policy options (§ Native composition contract): a `HomePolicy` in
  place of the implicit `GENTS_DESKTOP_HOME`/`~/.gents` defaults, and host schema
  registration hooks on `ClientCoreOptions`.
- **`gents-desktop-tauri` shrinks to an app shell**: `tauri::Builder`,
  `generate_context!` (bundle identity `com.source-inc.gents`, icons, window,
  capabilities), the plugin registration, and any Gents-app-specific commands. Its
  `bridge/` module is deleted.

Dependency direction rules, enforced structurally: `gents-desktop-bridge` must not
depend on `gents-desktop-tauri` (crate graph makes this a compile error once the
extraction lands); view models live only in the bridge crate; the app crate must not
re-declare or fork them. The bridge's direct dependency on the `gents` runtime crate
is accepted and explicit — the bridge is an operator surface over the runtime, and
hiding that behind `gents-desktop-core` re-exports would add indirection without
adding a boundary.

### Frontend

Four published packages plus the private app, managed as npm workspaces (net-new; the
repo currently has a single `package.json`):

```
@source-inc/gents-desktop-client      transport interface + default Tauri transport,
        ▲   ▲   ▲                     canonical view-model types, event subscription,
        │   │   │                     contract-version handshake, testing entry point
        │   │   └── @source-inc/gents-desktop-chat        headless chat state + components
        │   └────── @source-inc/gents-desktop-fleet       discovery/pairing/health/peers
        └────────── @source-inc/gents-desktop-operations  holds, traces, cancel, health panels
                          ▲
                          │ (all four consumed by)
              apps/gents-desktop  (private shell: App, Sidebar, navigation,
                                   branding, theme choice, config workspace)
```

- **`@source-inc/gents-desktop-client`** — the only package that knows a transport
  exists. It defines the `DesktopApiAdapter` interface (already present in
  `desktop-api.ts`), a `TauriTransport` default implementation, the
  `desktop://client-updated` subscription, the canonical TypeScript view-model and
  request types (moved out of `src/lib/types/`, henceforth generated — see drift
  gate), and a `/testing` subpath export carrying the deterministic in-memory adapter
  seam that `tests/ui-harness/desktopHarness.ts` implements today. The npm scope is
  `@source-inc` because GitHub Packages requires scope = org; the issue's
  `@gents/*` names were illustrative.
- **`@source-inc/gents-desktop-chat`** — headless first: `chat-shell.ts` projection
  (`projectChatShell`, `ChatWorkflowState`, turn/send state), conversation selection,
  chat/task action factories rehomed as hooks (`useChatWorkflow`), and the
  presentational components (`ChatComposer`, `ChatHeader`, `ChatTranscriptPanel`,
  `Transcript`, `cancelUx/*`, `slashSkills`). Streaming, retry, interrupt, reconnect,
  and recovery behavior comes with the projection + actions, not reimplemented.
- **`@source-inc/gents-desktop-fleet`** — `FleetDashboard`, `FleetRow`,
  `AddPeerForm`, `QrScannerDialog`, `peerConnectionImport`, `NetworkPanel`,
  `fleetMetrics`, peer action hooks, and `peerConnectionErrors` formatting.
  `BrandLockup` does **not** move — the dashboard takes a `brand` slot/prop.
- **`@source-inc/gents-desktop-operations`** — `OperationsRail` + its context/tab
  registry, `HoldsPanel`/`useToolCallHolds`, `BackgroundedToolsPanel`/
  `useOperationsSnapshot`, `RequestTracePanel`, `subagentLineage/*`,
  `backendHealth/*`, `mcpHealth/*`, `WorkspaceTreePanel`, interrupt-cascade dialog
  plumbing.
- **Stays app-private**: `App.tsx`, `Sidebar` and sidebar widgets, hand-rolled view
  switching and shortcuts, theme persistence choice, branding assets and strings, the
  config workspace (`ConfigWorkspace` and the `config/*` panels), and
  `useDesktopShell` itself — reduced to composition of the package hooks. The config
  authoring surface is deliberately not packaged in v1 (see Unresolved decisions).

Chat/fleet/operations packages depend only on `-client` and React; they never import
each other or the app. Enforcement is mechanical, not conventional: package `exports`
fields hide internals, and a dependency-lint rule (`eslint no-restricted-imports` or
`dependency-cruiser`, wired into `format:check`'s CI step) forbids
`apps/gents-desktop/src` imports from packages and deep imports of package internals
from the app. This is the fence that keeps private imports from silently returning.

Design tokens are a contract, not a package of components: `-client` (or a tiny
sibling `@source-inc/gents-desktop-tokens` if maintainers prefer — naming decision
below) ships `tokens.css` split into **semantic** custom properties
(`--color-bg/surface/text/accent`, spacing, radii, fonts) that packaged components
reference exclusively, and Gents **brand** values (`--source-green`, brand fonts,
logo) that stay in the app. The existing `design-system-conformance.test.ts` moves
alongside the tokens and becomes the gate that packaged CSS uses only semantic vars.

## Native composition contract

### A Tauri plugin, not a builder

`gents-desktop-bridge` exposes:

```rust
pub struct BridgeConfig {
    /// Where the client's storage home comes from.
    pub home: HomePolicy,          // Default (GENTS_DESKTOP_HOME / ~/.gents),
                                   // FixedRoot(PathBuf), or EnvVar(&'static str)
    /// Whether desktop_client_start may auto-init a standard local runtime,
    /// or the host is paired-remote-only.
    pub bootstrap: BootstrapPolicy,
    /// Host-owned DefraDB schemas (SDL) + collection subscriptions, applied
    /// after ensure_runtime_schemas. Gents runtime schemas are not optional.
    pub host_schemas: Vec<HostSchema>,
    /// Host identity metadata for logs/diagnostics (not payloads): app name, version.
    pub app_meta: AppMeta,
}

pub fn init<R: tauri::Runtime>(config: BridgeConfig) -> tauri::plugin::TauriPlugin<R>
```

A downstream host composes it the ordinary Tauri way:

```rust
tauri::Builder::default()
    .plugin(gents_desktop_bridge::init(BridgeConfig { .. }))
    .plugin(tauri_plugin_opener::init())
    .invoke_handler(tauri::generate_handler![amygdala_inventory_list, ..])
    .run(tauri::generate_context!())   // host's context: bundle id, icons, windows
```

Why a plugin instead of an exported handler list: Tauri 2's supported cross-crate
composition mechanism is the plugin API — it carries its own `generate_handler!`,
manages its own state, declares its own **permissions** (the host adds
`gents-desktop-bridge:default` to its capability file, mirroring today's
`core:default`/`opener:default`), and composes with any number of host plugins and
commands without touching the host's invoke handler. The cost is that invoke paths
change from `desktop_chat_send` to `plugin:gents-desktop-bridge|desktop_chat_send`.
That rename is contained entirely inside `@source-inc/gents-desktop-client` (the only
code that speaks command strings) and lands in the same PR that plugin-izes the
bridge, so no consumer outside the client package ever observes it.

### Ownership split

The **bridge owns**: `DesktopAppState` (the `ClientCore` lifecycle), the update pump
that turns `store_updates()`/`p2p_health_updates()` into `desktop://client-updated`
events, all 55 command implementations, view-model serialization, snapshot builders,
interrupt-cascade preview/execution, and the dedicated large-stack runtimes the
iPhone work introduced (the 32 MiB-stack Tokio runtime and the 16 MiB client-start
thread move with the plugin — they exist for iOS DefraDB replay and are part of the
reusable behavior, not the app).

The **host owns**: the `tauri::Builder` and `generate_context!` (bundle identifier,
product name, icons, plists, entitlements, windows, title-bar style, CSP), the
capability files, every non-Gents plugin, its own commands and managed state, its own
schemas and their ACP policies, and — through `BridgeConfig` — the storage home and
bootstrap policy. Identity is *derived from* the storage home:
`PrincipalIdentity::load_or_create` in the home the host chose means each host app
install mints its own principal DID. Amygdala installs are distinct principals from
Gents Desktop installs by construction; nothing in the extraction lets one app assume
another's identity.

### Stable contracts

The bridge's public contract, versioned as one unit (§ Compatibility):

- **Commands**: the 53 production commands grouped as lifecycle/runtime (7),
  fleet/pairing/peers (8), chat (7), config (17), tasks/schedules (5), operations —
  trace, health, holds, cancel (9), plus one new command,
  `desktop_bridge_contract`, returning `{ contract_version, package_version }` so the
  TS client can fail fast on a mismatched host instead of failing weirdly later.
- **Events**: `desktop://client-updated` with `reason ∈ {store, health, lifecycle,
  config}`. The coarse ping-then-refetch model is the contract; fine-grained
  streaming events are explicitly out of scope for v1.
- **Errors**: commands currently return stringly errors, and the frontend already
  string-matches them (`peerConnectionErrors.ts`). The plugin-ization PR — the one
  breaking change window — moves to a serialized
  `BridgeError { code, message, retryable }` with `code` as a closed enum, and the
  client package maps codes to presentation.
- **View models**: the serialized shapes in `bridge/types/views/*` and
  `bridge/types/requests/*`, with no Gents branding, shell, or navigation assumptions
  in any payload (true today; the contract makes it a rule and the fixture app makes
  it testable).
- **Debug-only E2E surface**: `desktop_native_e2e_config`/`_status` stay in the
  bridge crate (module `e2e`), keep the dual `#[cfg(debug_assertions)]` +
  `GENTS_NATIVE_E2E=1` gate, and additionally move behind a `native-e2e` cargo
  feature that is on for dev/test profiles and off for release packaging. They are
  documented as an unsupported test contract: present so any host can run the native
  E2E lane, never part of the production API, structurally unable to ship active in
  release builds.

Sharp edges propagate to downstream hosts: the bridge crate's docs must carry the
repo's DefraDB rules (`graphql::escape_graphql_string()` for every interpolation;
never emit `[]` in a mutation — emit `null`) because host schema/mutation code hits
the same embedded node.

## Frontend composition contract

### Injected transport

`@source-inc/gents-desktop-client` inverts today's hard-coding. The package exports:

```ts
interface DesktopTransport {
  invoke<T>(command: string, args?: unknown): Promise<T>;
  listenClientUpdated(handler: (e: ClientUpdateEvent) => void): Promise<Unlisten>;
}
createDesktopClient(transport?: DesktopTransport): DesktopClient  // DesktopApiAdapter, typed
tauriTransport(): DesktopTransport      // default; the only @tauri-apps/api import
```

The app and downstream hosts pass nothing and get the Tauri transport; tests and the
browser harnesses pass a fake. This replaces the module-global
`setDesktopApiAdapterForTests` / `setDesktopClientUpdatedListenerFactoryForTests`
seams with ordinary constructor injection through a supported public API — the same
capability the harness uses today, no longer a test-only backdoor. The
`/testing` subpath export ships the deterministic in-memory adapter contract so the
existing `desktopHarness.ts` scenarios become package-level fixtures any consumer can
run their UI against.

### Canonical types and the drift gate

Canonical types are **generated from Rust**. The bridge crate's view-model and
request structs derive TS bindings (`ts-rs` is the working candidate; `typeshare` is
the fallback — tool choice is a phase-1 spike, see Unresolved decisions), emitted
into `@source-inc/gents-desktop-client/src/generated/`. Two CI gates replace today's
"1:1 mirror — keep in sync" comments:

1. **Codegen freshness**: CI regenerates and fails on diff (generated output is
   committed, reviewable, and versioned with the package).
2. **Contract fingerprint**: a generated `contracts/desktop-bridge.json` — command
   names, event names, error codes, and type schemas — is snapshot-checked. Any diff
   requires a contract-version bump in the same PR plus a changelog entry. This is
   how breaking bridge/view-model changes are *identified* rather than noticed.

This deliberately does not touch the Lean JSON contract machinery
(`proofs/Proofs/Conformance/Contracts/Json/*`): that fences runtime semantics; this
fences serialization shape between the bridge and its TS consumers. They are
different layers with different authorities.

### Headless state vs presentation

Every domain package is layered: a headless core (hooks + pure projections, no CSS,
no JSX beyond providers) and a component layer on top. The composition contract for a
host shell:

- **State**: `useChatWorkflow`, `useFleet`, `useOperations` hooks take a
  `DesktopClient` (via a `DesktopClientProvider` context from `-client`) and expose
  the state + action surface that `useDesktopShell` exposes today, partitioned.
  `useDesktopShell` becomes the reference composition of these hooks and stays in the
  app.
- **Presentation**: components take data + callbacks; the 11 components that call
  `desktop-api` directly today switch to the injected client. Slots/props replace
  hard-coded chrome: `FleetDashboard` takes a `brand` slot; panels take
  label/asset overrides where Gents strings exist today.
- **Navigation**: packages never navigate. Hosts own routes/views (Gents Desktop's
  hand-rolled `workspaceView` state is one valid host; a router-based host is
  another) and mount package surfaces wherever they choose. The operations rail's
  tab registry (`operationsRailContext`) is the model for host-extensible panels: a
  host registers extra tabs/panels through the same context API the package's own
  panels use.
- **Responsive ownership**: packages own component-level responsiveness — each ships
  its own media queries at the documented narrow breakpoint (`760px`, published as an
  exported constant `NARROW_BREAKPOINT_PX` and used to end the magic-number drift;
  CSS custom properties can't parameterize `@media`, so the constant is the contract
  and the literal is generated/documented, not ad-hoc). Hosts own **shell-level**
  responsive behavior: the mobile master/detail pane switching currently in `App.tsx`
  moves into a headless `useMasterDetail` helper in `-chat` so hosts get the iPhone
  branch's behavior without adopting Gents' layout.
- **Accessibility**: the existing roles/labels/testids are promoted to contract:
  every interactive packaged component documents its `data-testid` and ARIA surface,
  and the agent-browser's semantic targeting (role/label strategies) doubles as the
  a11y smoke across packages. Testids (`composer-input`, `fleet-pair-*`,
  `assistant-message`, …) are stable API — the native E2E driver, Playwright,
  Bombadil, and XCUITest all depend on them.
- **CSS**: packages ship compiled ESM + `.d.ts` + plain CSS files (no CSS-in-JS, no
  shadow DOM), keeping today's `@layer`-ordered global-CSS model. During extraction,
  moved stylesheets adopt a `gents-` class prefix and are re-baselined against the
  visual suite; class names remain non-contractual (testids are the contract),
  semantic tokens are the theming API, and `[data-theme]` switching keeps working
  with host-supplied token values.

## Compatibility and release contract

**One version train, lockstep, exact pinning.** All desktop crates and npm packages
release together at `workspace.package.version`, tagged `vX.Y.Z` exactly as today
(`release-macos.yml` already validates tag = workspace version; the npm workspace
versions join that check). Lockstep is the honest choice for packages that share one
serialized contract and one repo: independent versioning would manufacture a
compatibility matrix with only one supported diagonal.

**Distribution:**

- **Rust: git-tag pinning, not crates.io.** The DefraDB git dependencies make
  registry publication impossible for this dependency cone, so the supported
  mechanism is `gents-desktop-bridge = { git = "ssh://git@github.com/source-inc/gents.git", tag = "vX.Y.Z" }`
  (downstream needs repo access — true today for any consumer of this private repo).
- **npm: GitHub Packages** under the `@source-inc` scope, published by the release
  workflow on tag. If maintainers prefer to avoid a registry entirely, the fallback
  is npm git dependencies with a `prepare` build, at the cost of slower installs and
  worse lockfile ergonomics — flagged as a decision, with GitHub Packages
  recommended.

**Compatibility matrix.** A table in this document (moving to `CHANGELOG.md` once it
exists) with one row per release: tag, bridge crate version, npm package versions,
**bridge contract version**, minimum `gents` runtime the bridge speaks to. The
contract version increments only on breaking contract-fingerprint changes; the
runtime handshake command (`desktop_bridge_contract`) plus the client's startup check
turn version skew into a clear error at boot.

**Changelog and downstream update workflow.** A root `CHANGELOG.md` (net-new — the
repo has none) with a "Bridge contract" section per release listing every
contract-fingerprint change. The supported downstream update is: bump the git tag and
the npm pins to the same `vX.Y.Z`, read the Bridge-contract section, run the
downstream's contract + e2e + visual gates (the fixture app below is the template for
those gates), merge. Renovate/Dependabot-style automation is possible against GitHub
Packages but out of scope here.

## Migration and validation

### Extraction sequence

Each phase is one reviewable PR (or a small stack), lands green on the full existing
gate set, and keeps `apps/gents-desktop` behavior-identical unless stated. Entry
criterion for every phase: previous phase merged. Standing exit criteria for every
phase: `cargo check --workspace --all-targets`, `cargo test -p gents`, affected
desktop Rust suites, `npm run test:ui` (format, build, unit, Playwright e2e, short
fuzz), and `test:ui:agent --backend deterministic --viewport iphone`; phases that
touch the live bridge or native surface add the live/iOS lanes named below.

1. **Crate move, no behavior change.** Create `crates/gents-desktop-bridge`
   containing the Tauri-agnostic bridge logic (`commands/`, `snapshot/`, `types/`,
   `cascade.rs`, `cause_derivation.rs`, `logging.rs`); the app's `tauri_commands/*`
   wrappers stay put and import it. Cross-crate `generate_handler!` gymnastics are
   deliberately avoided by leaving the `#[tauri::command]` layer in the app until
   phase 2. Exit: app compiles against the new crate; no `bridge::` module remains
   for moved code; live suites (`test:live:chat`, `test:live:fleet`,
   `test:live:cascade`) pass unchanged.
2. **Plugin-ization — the one breaking window.** Move `tauri_commands/*`, `state.rs`,
   and the runtimes into the bridge crate behind
   `gents_desktop_bridge::init(BridgeConfig)`; declare plugin permissions; introduce
   `BridgeError { code, message, retryable }` and `desktop_bridge_contract`; move
   `bridge_runner` into the bridge crate behind `test-harness`; gate `e2e` module
   behind `native-e2e` feature. Update the app: builder shrinks to plugin + context.
   Update `desktop-api.ts` command strings (plugin-namespaced) in the same PR. Exit:
   `test:ui:agent --backend live --viewport iphone` and `test:ui:live:e2e` green
   against the relocated `bridge_runner`; `test:ui:ios:e2e` green (native surface
   changed); release build verified to exclude `native-e2e`.
3. **Host policy.** `HomePolicy`/`BootstrapPolicy`/`host_schemas`/`app_meta` on
   `BridgeConfig`, with the additive `ClientCoreOptions` extensions in
   `gents-desktop-core`; Gents Desktop passes `HomePolicy::Default`. Exit: a Rust
   integration test boots the plugin with a fixed non-default home + one host schema
   and round-trips a host document; clean-install iOS lane re-run (storage home path
   is what it exercises).
4. **npm workspaces + `@source-inc/gents-desktop-client`.** Workspace bootstrap at
   repo root; extract transport interface, injected client, events, `/testing`
   adapter contract; codegen spike lands here (ts-rs vs typeshare decided by this
   PR's evidence); both drift gates wired into CI; dependency-lint fence on. The
   ui-harness switches from test-only setters to public injection. Exit: zero imports
   of `src/lib/desktop-api` outside the app-shell composition layer; drift gates red
   on synthetic contract change (test the fence itself).
5. **`@source-inc/gents-desktop-chat`.** Headless projection + actions + components;
   `useMasterDetail`; `gents-` class prefixing for moved CSS; visual baselines
   re-approved. Exit: chat unit suites run from the package; app consumes package;
   deterministic + live agent-browser chat journeys green on `iphone` viewport.
6. **`@source-inc/gents-desktop-fleet`.** Same shape; `BrandLockup` stays in-app via
   `brand` slot. Exit: pairing (QR import, bearer) journeys green in deterministic +
   live agent-browser and `test:live:fleet`.
7. **`@source-inc/gents-desktop-operations`.** Same shape, including the
   host-extensible rail-tab registry. Exit: `test:live:operations`,
   `test:live:interrupt`, `test:live:cascade` green from package surfaces.
8. **Tokens/theming split.** Semantic vs brand token separation;
   `design-system-conformance` moves to the packages and enforces semantic-only usage
   in packaged CSS. Exit: app renders identically (visual suite); a token-override
   smoke shows retheming without patching components.
9. **Downstream fixture host.** `apps/fixture-host` (name open): a minimal Tauri +
   React app with a different bundle id, product name, icon, storage home
   (`HomePolicy::FixedRoot`), one host schema, one extra native command, one extra
   route/panel, and non-Gents branding — consuming only published package surfaces.
   CI builds it and runs the agent-browser deterministic journeys against *its*
   shell; its iOS project runs the simulator lane with `GENTS_IOS_BUNDLE_ID` (new
   env; today's hard-coded `com.source-inc.gents` becomes the default) proving
   clean-install bearer pairing, replicated chat, recovery, interrupt, and
   unexpected-exit detection inside a host-owned iOS shell with retained evidence.
   Exit: fixture gates in CI; a checklist maps each host-ownership acceptance
   criterion to a fixture assertion.
10. **Release wiring.** GitHub Packages publish on tag; `CHANGELOG.md` + compat
    matrix; tag-validation extended to npm versions; documented downstream update
    workflow. Exit: a dry-run tag publishes all packages at one version and the
    fixture app consumes them by exact pin.

### How the existing lanes keep working

- **`test:ui:agent`** (deterministic/live, `iphone` default): the harness keeps
  driving the real app shell; its adapter injection goes through the public
  `-client` API after phase 4, and its live mode targets the relocated
  `bridge_runner` after phase 2. Because it never imported app internals, its JSONL
  protocol, semantic targeting, and viewport presets are unchanged — and the fixture
  app reuses it wholesale by pointing it at a different harness entry.
- **`test:ui:ios:e2e`**: the mint-invite → clean-install → pair → chat → stability
  flow is untouched; the bundle id and app-bundle path become parameters
  (defaulting to Gents values), the `native-e2e-status.json` contract and staged
  status reporting stay, and the debug-only command pair stays reachable in any
  host's debug build via the `native-e2e` feature. The XCUITest OCR lane needs no
  change beyond bundle-id parameterization.
- **Unit/component suites** move with their code into the packages they test;
  app-level suites keep covering composition. The `playwright-fixture-guard` pattern
  extends to the new packages (specs go through shared fixtures only).

### Traceability

| #877 acceptance criterion | Package / API | Phase | Verification gate |
|---|---|---|---|
| Minimal downstream app owns binary, identity, storage home, schema registration, extra commands | `gents-desktop-bridge::init(BridgeConfig)`: `HomePolicy`, `host_schemas`, host `Builder`/context | 2–3, proven in 9 | Fixture-host CI build + host-schema round-trip test + clean-install iOS lane under host bundle id |
| Working chat surface: streaming, retry, interrupt, reconnect, recovery — no copied source | `@source-inc/gents-desktop-chat` headless + components over `-client` | 5 | Agent-browser deterministic + live chat journeys (`iphone`), `test:live:chat`, fixture-host chat journey |
| Fleet pairing, health, peer management via package API | `@source-inc/gents-desktop-fleet` (+ bridge peer/pairing commands) | 6 | `test:live:fleet`, QR/bearer agent-browser journeys, fixture-host pairing, iOS clean-install pairing |
| Operator holds/traces/cancellation via package API | `@source-inc/gents-desktop-operations` | 7 | `test:live:operations`/`interrupt`/`cascade`, deterministic operations scenarios |
| Own branding, semantic theme, navigation, domain routes without patching components | Semantic tokens contract, `brand` slots, host-owned navigation, rail-tab registry | 8–9 | Token-override smoke, fixture-host distinct branding + extra route/panel, visual suite |
| Gents Desktop builds and passes its checks consuming the extracted packages | App consumes all four packages + plugin | every phase | Standing exit gates on each phase (app is the first consumer throughout) |
| Documented version-bump/update workflow | Lockstep train, exact pins, `CHANGELOG.md`, compat matrix, contract handshake | 10 | Dry-run tag publish + fixture pin-bump rehearsal |
| Non-goals: no plugin marketplace; no Amygdala domain code upstream; no weakened Gents semantics | Extension = slots/registry/config only; fixture host's domain stays in fixture; runtime authority unchanged (§ Security) | — | Review fence: dependency-lint + crate graph; no runtime-semantic diffs in extraction PRs |

## Security and runtime integrity

**The runtime stays the semantic authority.** Every bridge command already delegates
to `gents-desktop-core` and the `gents` runtime; extraction moves code across crate
boundaries without changing what transitions are legal, what invariants hold, or what
the provider is fed. Interrupt cascades, request lifecycles, tool-call holds, and
recovery all remain runtime-owned; the packages render and request, they never
decide. Downstream hosts get no API to override lifecycle behavior — that absence is
the design, not an oversight.

**Identity and ACP boundaries stay explicit.** Principals are minted per storage
home by `PrincipalIdentity`; bearer pairing keeps its full verification chain
(issuer signature, freshness, network-admin check, signed behavior binding, ticket
peer id) inside `gents-desktop-core`, untouched by packaging. Host schemas are
host-owned documents under the host's ACP policies in the host's home; nothing in
`BridgeConfig` grants a host app another principal's documents. Timeline/trace
projections keep their ACP-enforced redaction modes.

**Lean obligations.** This is a packaging design: it moves seams, adds additive
config, and renames invoke paths. It does not change legal runtime transitions,
invariants, or provider inputs, so it requires no speculative proof changes. Two
watchpoints where implementation could drift into Lean territory, called out so
follow-up PRs treat them correctly: (1) `BootstrapPolicy` must only *select among*
existing bootstrap paths (`init_standard_local_runtime` vs paired-only), never add a
new lifecycle; (2) any temptation to enrich the event contract beyond the coarse
`client-updated` ping into semantic lifecycle events would put event ordering into
the contract and must go through the Lean model → conformance test → Rust flow
before shipping.

## Rejected alternatives

- **Copying or git-subtree sharing app source.** Rejected: no versioned contract, no
  drift detection, and every upstream chat-stability fix becomes a manual merge — the
  exact failure mode #877 exists to end. Subtrees also copy private internals
  wholesale, erasing the public/private line this design draws.
- **One monolithic desktop UI package.** Rejected: it couples chat consumers to
  fleet/operations churn, makes semver signals meaningless (everything breaks
  everything), and forecloses hosts that want chat without the operator surface.
  The cost of four packages is low because they share one version train.
- **Bridge crate owning a complete `tauri::Builder`.** Rejected: the builder is where
  host identity lives (`generate_context!`, bundle id, icons, windows, capabilities,
  host plugins). A bridge-owned builder would either hardcode Gents identity or grow
  a config surface that re-implements Tauri. The plugin boundary composes instead.
- **Duplicated Rust and TS view models without a drift gate.** Rejected — this is the
  status quo, held together by "keep in sync" comments across 49 importing files. It
  already costs a hand-written normalizer (`normalizeInitSummary`) and will silently
  corrupt downstream apps the first time a field renames. Generated types + contract
  fingerprint are non-negotiable in this design.
- **Letting downstreams override Gents runtime semantics.** Rejected per #877
  non-goals and the repo's foundation: the proven lifecycle/invariant core is the
  product. Extension is additive (commands, schemas, panels, tokens); semantic
  override would turn every downstream bug into a Gents trust problem.
- **Keeping global (non-plugin) command registration to preserve invoke names.**
  Considered for phase 2: cross-crate `generate_handler!` re-export tricks avoid the
  `plugin:` rename but are unsupported, fragile across Tauri versions, and leave
  permissions/capabilities unmodeled. One contained rename inside the client package
  is cheaper than a permanently awkward composition seam.

## Unresolved decisions

Stated openly rather than buried as implementation detail:

1. **npm scope and final names** (`@source-inc/gents-desktop-*` proposed; separate
   `-tokens` package or tokens-in-client). Owner: maintainers, at design review.
   GitHub Packages forces the `@source-inc` scope if that registry is chosen.
2. **npm distribution: GitHub Packages vs git dependencies.** Recommended: GitHub
   Packages. Evidence needed: whether Amygdala's CI can authenticate to the org
   registry. Owner: maintainers + Amygdala.
3. **Type-generation tool: `ts-rs` vs `typeshare`.** Decided by the phase-4 spike on
   real view models (enum representations, `serde` attrs, chrono/uuid handling are
   the known differentiators). Owner: implementer of phase 4, with the spike diff as
   evidence.
4. **Config workspace packaging.** The agent/behavior/backend authoring surface
   stays app-private in v1. If Amygdala needs config authoring (not just consuming
   configured agents), a `-config` package is a follow-up with the same layering.
   Owner: Amygdala requirements; revisit after phase 7.
5. **Error-code taxonomy.** The `BridgeError.code` enum needs a pass over current
   string-matched failure modes (peer connection, pairing, save conflicts) before
   phase 2 freezes it. Owner: phase-2 implementer; evidence: existing
   `peerConnectionErrors.ts` cases and live sad-path suites.
6. **Fixture-host location and iOS project generation** (`apps/fixture-host` with
   committed generated Xcode project vs XcodeGen-on-demand like the main app).
   Owner: phase-9 implementer; constraint: the lane must stay runnable on the
   self-hosted macOS runner.

## References

Issue: [#877](https://github.com/source-inc/gents/issues/877). Base series:
`agent/iphone-amy-bearer-pairing` (`54edbe3e…e3d19f7a`) — mobile bearer pairing,
chat recovery/interrupt routing, responsive shell, agent-browser harness, native
Simulator E2E. Key code anchors: `apps/gents-desktop/src-tauri/src/bridge/mod.rs`
(builder + 55-command handler), `crates/gents-desktop-core/src/client/`
(`core/bearer_pairing.rs`, `paths.rs`, `principal_identity.rs`, `schema.rs`),
`apps/gents-desktop/src/lib/desktop-api.ts`, `src/lib/types/`,
`src/hooks/useDesktopShell.ts` + `desktopShell*`,
`apps/gents-desktop/tests/{agent-browser.mjs,run-ios-simulator-e2e.mjs,ios/GentsUITests.swift,ui-harness/}`.
