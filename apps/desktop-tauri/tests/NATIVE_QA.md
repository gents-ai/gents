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
   npm --prefix apps/desktop-tauri run tauri -- dev
   ```

3. Verify the native app window:
   - The app opens as `defra-agent desktop`.
   - The window is not blank.
   - There is no Vite/React framework error overlay.
   - The first visible shell state is either a usable fleet/chat surface or a
     handled runtime/bridge error.
   - The title bar, resize behavior, and app menu feel normal on macOS.

4. Verify the main entry points:
   - Fleet dashboard opens.
   - Chat composer is reachable.
   - Configure opens the config workspace.
   - Operations drawer opens and tabs switch.
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
