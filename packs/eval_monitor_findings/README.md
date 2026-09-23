# Monitor findings: the first decision-grade eval definition

`eval_monitor_findings` carries one `EvalDefinition`, `monitor-findings`
(comparability version 1), over the golden monitor subject in
`packs/eval_monitor`. It holds cases and nothing else: no behavior, no tools,
no inference slot. The subject pack never changes when a case does, so its
digest moves only with the subject, and no protected content reaches a trial
through it.

## Layout

| Path | Role |
| --- | --- |
| `pack_config.json` | The definition: id, version, subject (`behavior`, slot `monitor`) and twenty case sidecar paths |
| `cases/*.json` | One `EvalCase` each, inlined by the pack loader at install; the installed definition carries them inline |

## Cases

8 train / 6 validation / 6 held_out. Why two validation cases moved to
held_out, and which, is recorded in `packs/eval_monitor/CHECKS.md`, "Splits
(M3)".

| Case | Axis | Split | Reducer | Stages |
| --- | --- | --- | --- | --- |
| `ambiguous-empty-message` | ambiguous-input | train | all | 1 |
| `ambiguous-number-no-condition` | ambiguous-input | validation | all | 1 |
| `ambiguous-omitted-condition` | ambiguous-input | train | weighted_mean | 1 |
| `concurrent-grouping-by-correlation` | concurrent-sources | validation | weighted_mean | 3 |
| `concurrent-no-cross-talk` | concurrent-sources | held_out | all | 2 |
| `concurrent-two-correlations` | concurrent-sources | train | weighted_mean | 2 |
| `dedup-new-correlation-same-content` | dedup | validation | all | 2 |
| `dedup-same-correlation-new-content` | dedup | held_out | all | 2 |
| `dedup-same-correlation-resubmitted` | dedup | train | last_stage | 2 |
| `file-findings-from-inventory` | workspace-file | train | weighted_mean | 1 |
| `file-with-contradicting-input` | workspace-file | validation | weighted_mean | 1 |
| `file-without-contradiction-multi` | workspace-file | held_out | all | 1 |
| `numeric-threshold-stated-normal` | finding-content | validation | all | 1 |
| `recovery-clearance-never-reported` | recovery | held_out | all | 2 |
| `recovery-condition-cleared` | recovery | train | all | 2 |
| `recovery-partial-clearance` | recovery | validation | all | 2 |
| `service-name-only` | finding-content | held_out | weighted_mean | 1 |
| `single-disk-warning` | finding-content | train | weighted_mean | 1 |
| `three-conditions-with-service` | finding-content | held_out | weighted_mean | 1 |
| `two-conditions-disk-docker` | finding-content | train | weighted_mean | 1 |

## What a stage captures and checks

Every stage reads one capture after it ends:

```json
{"kind": "documents", "name": "items", "collection": "MailboxItem",
 "filter": {"requester_did": {"_eq": "$trial"}},
 "fields": ["title", "summary", "payload", "status"]}
```

`$trial` is the trial's own DID, the one runtime variable. The checks are the
builtin registry's mailbox checks (`crates/gents/src/eval/checks/`, registry
version `"2"`), all at `tier: acceptance`, `weight: 1`. What each check reads
and why it exists is recorded in `packs/eval_monitor/CHECKS.md`; the payload
they parse is the subject's output contract in `packs/eval_monitor/README.md`.

## Workspace files

The three `file-*` cases carry `fixtures.files`. The eval runner writes them
into each trial's workspace and sets `GENTS_EVAL_WORKSPACE_ROOT` to that
workspace for the trial, and the subject's read-only file tools resolve their
root from it (`host.root` is `${GENTS_EVAL_WORKSPACE_ROOT:-.}` in
`packs/eval_monitor/pack_config.json`).

## Installation

The pack is a documents pack with no inference slot. It installs through the
shared pack loader, which inlines each `./cases/…` sidecar as one `EvalCase`;
`crates/gents/tests/eval_monitor_findings.rs` installs it into an embedded home
and is the reference.

When a run freezes its cells, the subject pack is loaded against the launching
process's environment, where `GENTS_EVAL_WORKSPACE_ROOT` is normally unset, so
that load sees the Tools `host.root` as `.`. Freeze uses the loaded config only
to validate it (for example, refusing unrestricted host bash) and does not
persist it; the frozen cell records the pack digest. Each trial's own install
resolves `GENTS_EVAL_WORKSPACE_ROOT` to that trial's workspace. The difference
is expected and is not drift.

## Provenance

The cases were converted once by `scripts/evals/convert-expected-to-checks.py`
from `packs/eval_monitor/cases/` at `eval/30-monitor-prework` `0425499b1`: each
`expected` key became one check, the `test` split became `held_out`, the
script's `SPLIT_OVERRIDES` table moved two validation cases to held_out, and
`axis` moved into the table above. Later edits are made to the case files
directly and recorded in `packs/eval_monitor/CHECKS.md`.

## Validation

```sh
node scripts/check_packs.mjs
cargo test -p gents --lib pack::tests
cargo test -p gents --test eval_monitor_findings
python3 scripts/evals/test_convert_expected_to_checks.py
```
