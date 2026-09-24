# Native macOS/Tauri QA

The desktop app has two UI layers:

- Browser QA exercises the React/HTML/CSS shell in Chromium. It is the fast
  inner loop for layout, accessibility, console errors, adapter behavior, visual
  baselines, and most interaction flows.
- Native macOS/Tauri QA exercises the real `.app` shell, system WebView,
  windowing, menus, process lifecycle, and runtime handoff. Keep this lane small
  and focused on things browser tests cannot prove.

## When To Use Each Layer

Use browser QA for:

- component/model correctness
- viewport and layout regressions
- transcript/config/operations workflows
- visual baselines
- Bombadil exploration
- browser-to-runtime smoke through `bridge_runner`

Use native QA for:

- app launch and foreground window behavior
- blank WebView or framework overlay failures in the real Tauri shell
- macOS menu/window chrome and close/quit behavior
- local runtime discovery and bootstrap handoff
- packaged/dev app process cleanup
- signing, entitlements, and bundle-specific regressions

## Manual Native Smoke

Run from the repo root.

1. Run the non-GUI preflight:

   ```bash
   make desktop-native-preflight
   ```

   This builds the frontend, builds the Tauri Rust shell, and verifies the local
   Tauri CLI is available.

2. Launch the Tauri dev app:

   ```bash
   make desktop-native-dev
   ```

   The direct npm form is:

   ```bash
   npm --prefix apps/gents-desktop run tauri -- dev
   ```

3. Verify the native app window:
   - The app opens as `Gents desktop`.
   - The window is not blank.
   - There is no Vite/React framework error overlay.
   - The first visible shell state is either a usable fleet/chat surface or a
     handled runtime/bridge error.
   - The title bar, resize behavior, and app menu feel normal on macOS.

4. Verify the main entry points:
   - Fleet dashboard opens.
   - Chat composer is reachable.
   - Configure opens the config workspace.
   - Background process lifecycle is visible inline in the chat transcript.
     Operations snapshots and subagent lineage are verified through the live
     bridge suites, not a conversation drawer.
   - If a local runtime is configured, send one short message and confirm the UI
     reaches a terminal state.

5. Quit cleanly:
   - Quit from the macOS app menu or `Cmd+Q`.
   - Confirm the app process exits.
   - Confirm no unexpected Vite/Tauri helper process remains from the dev run.

## Packaged Acceptance: Canonical Transcript Refactor

Browser and bridge passes do not replace this packaged-app check. Record the
tested commit, DMG path and checksum, macOS version, and architecture. A local
unsigned build is not evidence of release signing or notarization.

1. Build with `make desktop-native-build`, then use the DMG path printed by
   Tauri. Install and open the bundled app, not the development server.
2. Use a dedicated test profile; do not wipe or overwrite an existing user's
   database to make this check pass. Start the local backend through the app.
   For an isolated launch, set both `GENTS_DESKTOP_HOME` (desktop identity and
   database) and `GENTS_HOME` (managed backend data) to separate directories
   under a fresh `mktemp -d` directory, and launch the installed bundle's
   executable from that shell. Setting only one leaves the other on its normal
   user profile. Reuse those same paths for the relaunch check; a Finder launch
   does not inherit these shell overrides.
3. Configure OpenAI-compatible inference at `http://workstation-1:8000/v1`
   (or workstation-2), model `GLM-5.3-Flash-NVFP4`, API key `local-glm`.
   This requires access to those hosts. Do not silently fall back to a paid
   provider when they are unavailable.
4. Start a session with The Engineer. Send a small filesystem task in a test
   workspace; verify streaming text, tool activity, final output, and readable
   failures. Send another message while work is running: it should appear as
   queued input and become canonical transcript content when execution starts.
5. Exercise a subagent task and a background command. Verify their completion
   notifications and parent linkage, then cancel a running task and check that
   the UI reports cancellation without leaving owned work running.
6. Reopen the session, then quit and relaunch the installed app. Confirm the
   transcript reconstructs, configuration persists, and the backend can start
   again without duplicate ownership or orphaned processes.

Record each result separately. Installation, native lifecycle, and this manual
Engineer workflow remain unverified until performed on the packaged artifact.

## Live Fixture E2E Acceptance

`tests/tauri-driver.live.e2e-acceptance.test.tsx` renders the real app against
the live `bridge_runner` fixture and a configured inference endpoint. It uses
isolated temporary homes, forces a bash file write plus `read_file` read-back,
requires a nonterminal canonical `liveAssistant` observation, checks the final
assistant/tool timeline, and reloads the session to verify durability.

```bash
npm --prefix apps/gents-desktop run test:live:e2e-acceptance -- \
  --inference-url http://workstation-1:8000/v1 \
  --model-name GLM-5.3-Flash-NVFP4 --provider OpenAiCompatible
```

This is fixture-runtime coverage only. It does not exercise a managed backend,
DMG installation, first-run setup, signing/notarization, or relaunch of a
packaged app. The fixture provides the generic `Live Repo Audit Default`
behavior, not The Engineer, so the packaged Engineer workflow above remains a
separate manual acceptance requirement.

## Artifacts

Do not commit ad hoc native screenshots. Store manual screenshots/traces outside
the repo, then file confirmed defects as GitHub issues with:

- expected vs. actual
- exact launch command
- macOS version and architecture
- runtime/backend configuration
- screenshot or log path

Stable browser visual baselines remain in
`tests/playwright-visual/*-snapshots/`; native screenshots are diagnostic
evidence unless we later add a dedicated native automation workflow.

## Automation Decision

Native automation should stay thin. Before adding it to CI, prove that it can
reliably launch the real Tauri app on the macOS runner, observe a nonblank
WebView, and shut down without orphaning processes. Keep broad UI interaction
coverage in Playwright browser tests.

Useful commands:

```bash
make desktop-native-preflight
make desktop-native-dev
make desktop-native-build
```

## macOS native tabs

Each tab is a complete Gents view with its own navigation, composer draft and
scroll position. All views share the desktop client and managed agent. Moving a
tab does not reload its webview or move its durable session.

macOS uses a non-overlay (`Transparent`) native title bar. AppKit reserves the
title/tab chrome outside the webview, so the app's viewport and scroll areas use
only the content bounds. Do not restore overlay mode or fake a fixed tab-height
CSS inset: tab visibility, fullscreen and scaling change the native layout.
Check the header, rail, composer and scrolling after tab creation, detach/merge,
window resizing and fullscreen; no web content should be covered by the tab bar.

Onboarding stays in the original window. New Tab and New Window remain disabled
until the existing setup flow finishes. An already configured installation
unlocks them when startup reaches the ready state.

- File → New Tab (`Cmd+T`) and the native tab bar's `+` create another view.
- File → New Window (`Cmd+Shift+N`) creates a separate window, including when
  macOS is configured to always prefer tabs. `Cmd+N` still starts a conversation.
- Drag a tab out, or use Window → Move Tab to New Window; move it to another
  monitor, then drag it back or use Window → Merge All Windows.
- `Ctrl+Tab` / `Ctrl+Shift+Tab` select the next / previous tab.
- `Cmd+W` closes an additional view without cancelling its requests. The
  original view hides only while sibling views or the managed runtime need it
  as the automatic startup/recovery owner. Closing the last visible view with
  no managed runtime exits as before tabs; a hidden original view is released
  when the last sibling closes. While the runtime is active, reopening from
  the Dock or tray restores the original view. `Cmd+Q` quits the application.

For acceptance, use isolated `GENTS_HOME` and `GENTS_DESKTOP_HOME` directories
and a real configured provider. Keep different drafts in two views, send a
request, detach and merge while it completes, and verify both the response and
the other view's draft. Close the original tab and verify the remaining view
still works; close an additional tab during inference and reopen its session
from another view to verify terminal completion. Check the native `+` button,
menus, keyboard shortcuts, title-bar layout and Dock reopening separately from
browser tests. Retain screenshots and request IDs outside the repository.

Also verify that closing a lone window with no managed runtime exits (fresh
onboarding may already have started one), that a remote-only configured
installation exits after its last tab closes, and that opening a tab
does not auto-start a stopped/failed managed runtime. With two views open, the
tray's Stop Local Agent action must invoke exactly one stop operation. Run tasks
and schedules with matching IDs on different agents from their respective views;
retry and interrupt actions must carry the intended agent context independently
of the shared observation filter. Linux/Windows keep their prior close policy;
this feature does not add portable tab or window creation there.

## Local macOS signing

`desktop-native-build` applies the local Tauri signing overlay at
`src-tauri/tauri.local-sign.conf.json`. On macOS this gives the complete app
bundle an ad-hoc signature before Tauri packages the DMG, including a sealed
`Info.plist` and resources. Verify a local artifact with:

```bash
codesign --verify --deep --strict --verbose=2 \
  target/release/bundle/macos/Gents.app
hdiutil verify target/release/bundle/dmg/Gents_0.18.5_aarch64.dmg
```

An ad-hoc signature proves only that the local bundle is structurally sealed.
It has no Apple signing authority and is not Developer ID signed or notarized.
Release builds must continue to use their certificate and notarization path;
do not present a local `make desktop-native-build` artifact as release-signing
evidence.
