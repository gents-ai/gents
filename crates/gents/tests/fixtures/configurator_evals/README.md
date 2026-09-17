# Engineer evaluation suites

The baseline model is `GLM-5.3-Flash-NVFP4`, temperature 1, top-p 0.95, with
requested reasoning `high`. Provider enforcement of reasoning is not measured.

Current suites:

- `host-steward`: isolated Linux host; preview, configuration, healthy checks,
  faults, deduplication, recovery, restart and actual scheduled execution.
  See [host/README.md](host/README.md) for environment and live commands.
- `monitor-mailbox`: document-triggered monitoring, in-place editing, canonical
  mailbox output and deduplication. Run `make live-mailbox-eval`.
- `progressive-configurator`: supplemental onboarding, Builder readiness,
  skill import/use and document automation. Run `make live-configurator-eval`.

Approval-driven maintenance and isolated accepted/rejected improvement candidates
are unfinished. A passing host-steward cohort does not establish those guarantees.
Pagoda creation/review/improvement and their dedicated browser grader are retired.
[Historical cohort results](HISTORICAL_COHORTS.md) remain available, with original
private receipts unchanged; do not compare different case catalogs as one cohort.

## Shared runner contract

Each suite owns its case catalog, grader and fixture provenance. Register cases
through `stages::checked`, return a `TrialResult`, and submit it to `RunReport`.
Grade canonical runtime documents, source-linked requests, tool receipts and
independent effects—not assistant prose. Missing prerequisites remain skipped and
non-passing. Keep provider, tool, runtime, infrastructure, grader and model
acceptance failures distinct. Do not overwrite raw outcomes or case receipts.

The launcher prints a private evidence directory. Inspect it with
`node scripts/evals/watch.mjs DIRECTORY` or `node scripts/evals/report.mjs DIRECTORY`.
`report.json` is the mutable aggregate; raw trial evidence is immutable and may
contain identities, prompts or credentials. Do not publish it wholesale.

Run `make test-evals` for shared Rust/terminal contracts and `npm run test:evals`
for JavaScript tests. `make test-host-eval-environment` exercises real container
faults and archive retention without inference. Live host images are pinned once
per cohort and memory capacity is checked before trials start. Begin with one
live trial before explicitly choosing larger `GENTS_LIVE_CONFIG_RUNS` and
`GENTS_LIVE_CONFIG_CONCURRENCY` values.
