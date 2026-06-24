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

The GitHub `Desktop UI QA Sweep` workflow runs the same review lane on a
schedule and by manual dispatch. It runs unit tests and deterministic browser
journeys, captures screenshots, checks desktop/laptop/narrow visual baselines,
runs a longer Bombadil sweep, and uploads `test-results` plus Playwright reports
even when the job passes so artifacts can be reviewed.

Review:

- `apps/desktop-tauri/test-results`
- `apps/desktop-tauri/playwright-report`
- Bombadil output path printed by `tests/bombadil/run-bombadil.mjs`
- `README.md` inside each Bombadil output directory for inspect/reproduce commands
- browser console attachments on Playwright failure
- traces before screenshots when diagnosing interaction failures

Stable screenshots from `test:ui:screenshots` are diagnostic artifacts. Visual
baseline checks from `test:ui:visual` are the golden-snapshot layer. Both cover
the standard desktop, laptop, and narrow viewport set.
The stable screenshot suite also attaches `desktop-screenshot-review.md` for
each viewport, listing the state, scenario, attachment name, artifact path, and
bug-issue details to copy into GitHub.
The visual baseline suite attaches `desktop-visual-review.md` for each viewport
project, listing the asserted stable states and snapshot names.

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
npm --prefix apps/desktop-tauri run test:ui:live:e2e:real -- \
  --inference-url <url> \
  --model-name <model> \
  --api-key-env-var OPENAI_API_KEY
```

It starts the existing `bridge_runner`, serves the React shell in Chromium, and
uses the live bridge HTTP adapter. By default, the browser smoke uses a local
OpenAI-compatible mock inference endpoint so the runtime path is deterministic.
Use `test:ui:live:e2e:real` or pass `--require-real-inference` when the goal is
to validate a real provider and accidentally falling back to the mock would hide
the failure. The `Live Smoke` workflow exposes manual `inference_endpoint` and
`model_name` inputs for the same path. This should stay in manual or live-smoke
workflows until it has enough stability history. Successful live browser runs
attach `desktop-live-browser-smoke.md`,
`desktop-live-browser-diagnostics.json`, and `desktop-live-browser-final.png`
so reviewers can see which runtime request completed and inspect the projected
desktop/remote diagnostics without rerunning the smoke. The workflow uploads
those artifacts even on successful runs.
