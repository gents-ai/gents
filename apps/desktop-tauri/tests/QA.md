# Desktop UI QA Checklist

Use this checklist for desktop UI PRs and for the broader desktop QA epic
tracked in sourcenetwork/defra-agent#531.

## Fast PR Gate

Run these before opening or updating a normal desktop UI PR:

```bash
npm --prefix apps/desktop-tauri run format:check
npm --prefix apps/desktop-tauri run build
npm --prefix apps/desktop-tauri run test:ui:unit
npm --prefix apps/desktop-tauri run test:ui:e2e
npm --prefix apps/desktop-tauri run test:ui:fuzz -- --time-limit 30s
```

Equivalent Makefile shortcuts:

```bash
make build-desktop-ui
make desktop-ui-unit
make desktop-ui-e2e
make desktop-ui-fuzz
```

PR descriptions should mention either the UI defects fixed or `No new desktop UI
defects found in the deterministic harness`.

## QA Sweep

Use the sweep when intentionally looking for defects:

```bash
npm --prefix apps/desktop-tauri run test:ui:qa-sweep
npm --prefix apps/desktop-tauri run test:ui:visual
npm --prefix apps/desktop-tauri run test:ui:fuzz:long
```

Equivalent Makefile shortcuts:

```bash
make desktop-ui-qa-sweep
make desktop-ui-visual
make desktop-ui-fuzz-long
```

Review:

- `apps/desktop-tauri/test-results`
- `apps/desktop-tauri/playwright-report`
- Bombadil output path printed by `tests/bombadil/run-bombadil.mjs`
- `README.md` inside each Bombadil output directory for inspect/reproduce commands
- browser console attachments on Playwright failure
- traces before screenshots when diagnosing interaction failures

Stable screenshots from `test:ui:screenshots` are diagnostic artifacts. Visual
baseline checks from `test:ui:visual` are the golden-snapshot layer.

## Bug Issue Format

Create one GitHub issue per confirmed reproducible defect with labels `bug` and
`ui`.

Template:

```md
## Summary

What the user sees.

## Expected

What should happen instead.

## Reproduction

- Command:
- Scenario / viewport:
- Artifact:
- Bombadil reproduce:

## Notes

Any trace, screenshot, console log, or suspected component.
```

If a finding is only a hunch, record it in `.agents/desktop-ui-qa-epic.md`
first. File a GitHub issue after it reproduces.

## Live Browser Smoke

The live browser path is intentionally outside the fast PR gate:

```bash
npm --prefix apps/desktop-tauri run test:ui:live:e2e
```

It starts the existing `bridge_runner`, serves the React shell in Chromium, and
uses the live bridge HTTP adapter. It should stay in manual or live-smoke
workflows until it has enough stability history.
