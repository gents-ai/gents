# Browser-use evals

This directory ships with the browser-use plugin. `manifest.json` is a separately
installable native Gents documents pack containing `browser-use-core`.
`subject/` supplies the browser-only agent and the `browser` inference slot.

The definitions are authored and pack-validated. They have **not yet been run by
native `gents eval`**. Trial provisioning still needs to install the browser plugin,
start/reset the fixture server, supply a session name, and publish trusted
observations before stage capture. Gents now owns worker startup and supplies its
connection internally when the installed pack has its child-process grant.
The existing `../worker/tests/live_model.py` remains the working end-to-end runner.

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

1. Install `gents/web` in the fresh trial home with its authority grant and bind
   the web researcher slot. Grant the subject only its declared browser tool.
2. Use a fresh worker/browser state and fixture server for every trial. Start
   `../worker/tests/model_fixture.py --port 19472 --output <private-state-file>`.
   Keep the state file outside the subject workspace. Use concurrency 1 while the
   worker and fixture use fixed ports 18765 and 19472; isolate network namespaces
   or add configurable endpoints before parallel runs.
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
unknown stages; missing observations fail the native acceptance checks. The
current embedded runner does not yet provide this browser-specific provisioning
and observation bridge. Do not pre-seed passing rows or downgrade these checks to
make a run appear green. Browser execution has host Python authority, so these
fixtures test cooperative behavior, not adversarial isolation.

After that bridge exists, install this definition pack and use `gents eval run
browser-use-core --cell browser=./subject:browser --profile browser=<profile-id>
--trials 3 --concurrency 1`. The profile should point at the desired inference
server, such as `http://workstation-1:8000/v1`. This command is a target invocation,
not a currently complete setup recipe.

## Validation and extension

From the repository root:

```sh
/path/to/gents pack check packs/gents/web/evals \
  packs/gents/web/evals/subject packs/gents/web
python3 -m unittest discover -s packs/gents/web/evals -p 'test_*.py'
```

Bump `comparability_version` when changing prompts, checks, fixtures or grading
semantics. These cases are public validation tasks, not held-out benchmark data.
Keep new cases and their fixtures in this pack. Next candidates are virtualized
lists, iframe/shadow DOM controls, ambiguous submit timeouts, download/edit/upload,
expired login, crash recovery, and research with conflicting sources. Public-web
research currently lives in `live_model.py --research`; factual synthesis still
requires independent review and is not scored by this acceptance definition.

The packaged `live_model.py --managed --connected` runner additionally tests
discovery, existing authenticated tabs, live editing, disconnect and browser
preservation. `--managed --web-research` exercises the combined public search,
HTTP fetch and rendered reader. These integration modes are executable today;
they are not claims that the native Gents trial-provisioning bridge is complete.
