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
