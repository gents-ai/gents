# Eval definition and checks design (issue #1515, spec 3a, milestone M3)

Status: design, approved 2026-09-22 after five decisions. Umbrella:
`2026-09-21-eval-and-optimization-umbrella.md`. Contract: `2026-09-21-eval-core-contract-design.md`.
Runner: `2026-09-21-eval-runner-design.md`. Inputs: the M3 pre-work on `eval/30-monitor-prework`
(`packs/eval_monitor/`, its twenty `cases/*.json`, and `CHECKS.md`).

## Goal

Turn the golden monitor subject and its twenty cases into the first decision-grade definition the
runner can execute and score: a definition pack, a check registry, and one calibration run that
settles what static authoring cannot.

## Decisions

| Question | Decision | Why |
|---|---|---|
| Where the definition lives | A separate pack `eval_monitor_findings` whose root document is `eval_definitions/monitor-findings.json`; each case is a sidecar file `cases/<case_id>.json` inlined by the pack loader | The subject pack stays test-free, so its digest never moves with a case, and protected content never enters a trial through the subject |
| Case format | The contract's shape, authored directly: `checks: [{check, params, tier, weight}]`. A one-time script converts the twenty from the `expected` shorthand; the shorthand is retired | One vocabulary at runtime; a check's params are what its Rust reads; tier and weight are explicit |
| Capture | Per stage, `EvalStage.capture: [{kind: documents, name, collection, filter, fields} \| {kind: file, name, glob}]`, additive on M1's typed struct. `fields` names what checks may read; `$trial` is the one runtime variable. Request-level captures remain the fallback | The definition describes what it grades; two runs of one definition see the same surface |
| Check registry | The seven checks the cases use, plus `mailbox_open_row` (status-aware) and `finding_text_excludes` (a ban scoped to one correlation's findings). `mailbox_text_excludes` keeps its inherited three-field scope, documented. `no_findings_for_correlation` is not registered until a case needs it. Cases are not rewritten | Closes the negative cross-correlation gap for future cases without changing what the twenty assert |
| First live run | A calibration run inside M3 before `comparability_version: 1` is final: one cell, one trial per case, env-gated; one fix pass on the cases follows | Settles the zero-finding row behavior and keyword survival; keeps M5's variance measurement from being taken against cases that then change |

## 1. The definition pack

`packs/eval_monitor_findings/`: `manifest.json` (kind `documents`, every case file and the root
document declared as assets), `pack_config.json` with one `eval_definitions` entry:

```json
{
  "definition_id": "monitor-findings",
  "comparability_version": 1,
  "title": "Monitor findings",
  "subject": {"kind": "behavior", "inference_slots": ["monitor"]},
  "cases": ["./cases/single-disk-warning.json", "..."],
  "tags": ["monitor"]
}
```

The loader gains one rule in `decode_pack_config`: for `Collection::EvalDefinition`, a `cases` entry
that is a string is a sidecar path, hydrated through the existing `read_sidecar` boundary and parsed
as one `EvalCase`. Everything else about the pack is the existing format. The installed document is
one `EvalDefinition`; its `desired_state_document_digest` is the definition's integrity anchor. The
subject pack `eval_monitor` is not modified.

## 2. The case format

```json
{
  "case_id": "single-disk-warning",
  "split": "train",
  "reducer": "weighted_mean",
  "fixtures": {"files": [{"path": "inventory.json", "contents": "..."}]},
  "stages": [{
    "stage_id": "report",
    "prompt": "...",
    "deadline_secs": 600,
    "capture": [{"kind": "documents", "name": "items", "collection": "MailboxItem",
                 "filter": {"requester_did": "$trial"},
                 "fields": ["title", "summary", "payload", "status"]}],
    "checks": [
      {"check": "payload_well_formed", "params": {"version": 1}, "tier": "acceptance", "weight": 1},
      {"check": "finding_count", "params": {"count": 1}, "tier": "acceptance", "weight": 1},
      {"check": "findings_match", "params": {"matchers": [{"correlation": "c-01-a", "state": "open",
        "keywords_all": ["disk", "81"]}], "exact": true}, "tier": "acceptance", "weight": 1},
      {"check": "mailbox_text_excludes", "params": {"keywords": ["docker"]}, "tier": "acceptance", "weight": 1}
    ]
  }]
}
```

The conversion script `scripts/evals/convert-expected-to-checks.py` maps the retired `expected`
keys one to one (`item_count` → `mailbox_item_count`, `payload_well_formed`, `pairs_unique` →
`finding_pairs_unique`, `finding_count`, `open_count`/`resolved_count` → `finding_state_counts`,
`findings` + `findings_exact` → `findings_match`, `no_finding_containing` → `mailbox_text_excludes`),
all at `tier: acceptance`, `weight: 1`, and adds the capture `fields`. It is run once and kept.
`EvalDefinition::validate` rejects two refs to one check on a stage.

## 3. The check registry

All in `crates/gents/src/eval/checks/`, version `"1"`, pure over one `StageEvidence`, reading the
`items` capture. Malformed params yield `Grader` with `reason_code: bad_params`; a missing capture
yields `Grader` with `missing_capture`; a subject failure yields `ModelAcceptance` with `score_bp: 0`;
a pass yields `Passed` with `score_bp: 10000`. Every check's `raw` carries a closed `reason_code`.

| Check | Params | Passes when |
|---|---|---|
| `mailbox_item_count` | `{count}` | the capture has exactly `count` rows |
| `mailbox_open_row` | `{}` | exactly one row and its `status` is open |
| `payload_well_formed` | `{version}` | every row's `payload` parses as `{"version", "findings": [{correlation, condition, state, detail}]}` with the given version and `state ∈ {open, resolved}` |
| `finding_pairs_unique` | `{}` | no two findings share `(correlation, condition)` |
| `finding_count` | `{count}` | the total number of findings equals `count` |
| `finding_state_counts` | `{open, resolved}` | the counts by state equal the params |
| `findings_match` | `{matchers: [{correlation, state, keywords_all}], exact}` | every matcher is satisfied by some finding with that correlation and state whose `condition + " " + detail` contains every keyword (token or hyphen-stem match, case-insensitive); with `exact`, no unmatched finding remains |
| `mailbox_text_excludes` | `{keywords}` | no keyword appears in any row's `title`, `summary` or `payload` (the inherited three-field scope) |
| `finding_text_excludes` | `{correlation, keywords}` | no keyword appears in the `condition` or `detail` of any finding with that correlation |

`captured_rows_count` from M2 stays registered. `CHECK_REGISTRY_VERSION` becomes `"2"`.

## 4. The M1 amendment

One additive commit on `eval/05-contract`, made before M3, M4 and M6b start so they share one pin:
`EvalStage.capture` (typed, default empty), the duplicate-ref validation, `RunOrigin.breaker_threshold`
(default from the runner's constant) and `TrialCompletion.evidence_digest: Option<String>`. The
runner reads stage captures into `TrialSpec` and writes the two new fields; `run.json` and
`evidence.json` stay as they are for one release, then go.

## 5. The calibration run

The last task. The assembled definition, one cell (the golden monitor on the operator's live
inference profile), one trial per case, `trials_per_case: 1`, run through `eval::runner::run` on the
embedded executor, gated by the same environment variables as the runner's live smoke. Its output is
a report of matcher misses and the two facts the inventory could not settle: whether a no-condition
input yields an empty-findings row or no row (then `mailbox_item_count` is added to the two waiting
cases), and whether keywords survive into the subject's `condition` spellings. One fix pass on the
cases follows; then `comparability_version: 1` is final and the pack's digest is recorded in the
umbrella. This run is not M5: it measures nothing statistical.

## 6. Testing

- Unit tests per check with hand-built `StageEvidence`, reaching every reason code.
- A loader test: install `eval_monitor_findings` into an embedded home; the installed document has
  twenty cases, each passing `EvalDefinition::validate`; the sidecar rule refuses an undeclared path.
- A runner integration test: two of the twenty cases on `ScriptedExecutor` with scripted
  `MailboxItem` rows, asserting the exact verdict rows, so the registry, capture and grading chain is
  proven without a model.
- The calibration run, ignored by default.

## 7. Phasing

| PR | Branch | Content |
|---|---|---|
| 1 | `eval/40-capture` on `eval/05-contract` | the M1 amendment; the loader's case-sidecar rule; the runner reading stage captures and writing the two fields |
| 2 | `eval/41-checks` on `eval/40-capture` (over the M2 stack) | the nine checks and their unit tests |
| 3 | `eval/42-definition` on `eval/41-checks` | the definition pack, the conversion script, the converted cases, the loader test, the scripted integration test, the calibration run and its fix pass |

## Out of scope, with a home

`StageInput::Document` (trigger-driven input; blocked on the trigger engine's overlap behavior);
WASM graders and the 18 host cases (spec 3b); the `llm_judge` development-tier check (spec 6);
`no_findings_for_correlation` (registered when a case needs it).
