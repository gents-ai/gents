# Browser-use evals

This directory ships with the browser-use plugin. `manifest.json` is a separately
installable native Gents documents pack containing `browser-use-core`.
`subject/` supplies the browser-only agent and the `browser` inference slot.

The definitions are pack-validated. The embedded `gents utils eval` runner has
no browser-specific trial provisioning or observation bridge, so
`../worker/tests/live_model.py` and `run_live.py` are the end-to-end runners.

| Case | Acceptance evidence |
| --- | --- |
| `native_help` | Actual help result explains a false wait result; no worker needed |
| `pagination` | Correct answer plus fixture records of all three page requests |
| `form_tabs` | Delayed form submitted exactly once with exact fields; browser output contains page 3 after tab return |
| `upload` | Exactly one import with exact CSV content and receipt |
| `timeout_recovery` | Ordered stages: existing page, actual timeout result, successful page inspection |
| `error_recovery` | Ordered stages: existing page, structured exception with uncertain execution state and recovery guidance, successful inspection |

Each case uses the `all` reducer. A full pass requires every acceptance check to
score 10000; a finished run or partial score is not a pass. Tool and output checks
are behavioral smoke checks: arbitrary Python can fabricate stdout, so stdout
alone is not an independent website-state oracle. The tab and recovery checks do
not independently prove browser identity or absence of reloads.

## Trial integration contract

A runner that provisions these trials does the following:

1. Install `gents/web` in the fresh trial home with its authority grant and bind
   the web researcher slot. Grant the subject only its declared browser tool.
2. Use a fresh worker/browser state and fixture server for every trial. Start
   `../worker/tests/model_fixture.py --port 19472 --output <private-state-file>`.
   Keep the state file outside the subject workspace. The worker and fixture use
   fixed ports 18765 and 19472, so trials run one at a time unless each has its
   own network namespace.
3. Supply a unique browser session name in the subject's runtime context.
   Omit connection to use the native managed worker. Reuse
   the browser session across stages within a case. Reset between trials.
4. After `pagination/paginate`, `form_tabs/submit`, or `upload/import`, stop further
   subject execution, snapshot fixture state, and call `oracle.py`. Persist the
   returned document to `BrowserEvalObservation` in that trial home **before**
   native stage capture. `--trial-id` is the trial home's DID (the value bound by
   `$trial`), not the run ID or trial directory name. Keep exactly one observation
   per trial/case/stage. The subject must never publish its own oracle row.
5. Retain the original snapshot, traces and observation alongside the trial.
   Close the browser and stop the fixture and worker on completion or cancellation.

Example oracle conversion (this prints JSON; it does not write to Gents):

```sh
python3 oracle.py --state /private/path/fixture-state.json \
  --trial-id did:key:TRIAL_HOME --case upload --stage import
```

The oracle reads server state, never model answers. It rejects missing state and
unknown stages; missing observations fail the native acceptance checks. Do not
pre-seed passing rows or downgrade these checks to make a run appear green.
Browser execution has host Python authority, so these fixtures test cooperative
behavior, not adversarial isolation.

## Validation and extension

From the pack directory (`core-packs/web`):

```sh
gents config pack check evals evals/subject .
python3 -m unittest discover -s evals -p 'test_*.py'
```

Bump `comparability_version` when changing prompts, checks, fixtures or grading
semantics. These cases are public validation tasks, not held-out benchmark data.
Keep new cases and their fixtures in this pack. Public-web research is exercised
by `live_model.py --research`; its factual synthesis needs independent review and
is not scored by this acceptance definition.

The packaged `live_model.py --managed --connected` runner also tests discovery,
existing authenticated tabs, live editing, disconnect and browser preservation.
`--managed --web-research` exercises the combined public search, HTTP fetch and
rendered reader.

## Concurrent real-model runs

`run_live.py` provisions fresh Gents homes, fixture servers, worker ports and
browsers. It builds the WASM artifacts once, then builds trial archives with exact
loopback grants matching each worker port. The shipped pack defaults to 18765. No
wildcard network grant or personal browser is used.

```sh
uv run --locked --project worker python evals/run_live.py \
  --gents /path/to/gents \
  --endpoint <openai-compatible-base-url> \
  --concurrency-per-server 32 --output <fresh-dir>
```

`--endpoint` may be repeated; each endpoint runs `--concurrency-per-server`
independent trials, alternating the full six-task suite and a two-task
help/post-submit recovery probe. The probe deliberately calls an undefined helper
after Import; passing requires the actual exception, a subsequent successful
receipt inspection (read or Python DOM output), and exactly one import recorded
independently by the fixture. The form/tab task uses an independent CDP
connection to compare the original tab ID, URL, page contents and navigation
timestamp before and after the task; it also requires a distinct request tab and
exactly one recorded submission. Finished suite sessions close before the next
independent task to limit local browser memory use. The request wait and provider
idle deadlines both use `--timeout-secs` (default 900).

Use a fresh output directory. `plan.json`, incremental `results.json`,
`summary.json` and each trial's traces/logs remain there, including failed runs.
The plan records the base archive hash and live-runner comparability version,
which is separate from the native definition version. Trials are not
automatically rerun; any runtime/provider retries remain in the logs. The
concurrency setting counts active trial processes; each trial issues sequential
model requests and browser calls, so inference requests in flight vary. The runner
submits through `gents utils debug request submit`.
