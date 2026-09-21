# M3 pre-work brief: the golden monitor and the first twenty cases

Milestone M3, "first decision-grade definition", per
`docs/superpowers/specs/2026-09-21-eval-and-optimization-umbrella.md` section 6. Spec 3a is not
written yet; this brief covers the work that does not depend on it. The standing rules in
`2026-09-21-parallel-coordinators.md` bind every task here.

## Why this work can start before spec 3a

The umbrella fixes: one definition kind carried by a pack; Rust checks registered by name; the case
is the independent unit; the acceptance tier is deterministic checks only; the subject is a pack
snapshot digest plus a `behavior_id`. The contract spec (`2026-09-21-eval-core-contract-design.md`
section 1.1) fixes the `EvalDefinition` document and its embedded case shape:
`cases: [{case_id, split, reducer, stages: [{stage_id, prompt, deadline_secs, checks: [{check, params,
tier, weight}]}]}]`, with `fixtures` listing documents, files and schemas to install in a trial. The
runner spec (`2026-09-21-eval-runner-design.md`) adds a runner-side `capture: [{name, collection,
filter}]` per case and file captures `{name, kind: "file", glob}`.

Spec 3a will decide the on-disk pack layout for a definition, the exact check registry, and the
capture filter language. Case *content* survives those decisions: the input document, the fixtures,
the expected findings and the split. The container format is provisional and marked as such.

## The harness scout's findings that constrain this work

- The `monitor-mailbox` family in `crates/gents/tests/configurator_evals/onboarding_scenarios.rs` is
  document-driven: its prompt (`tests/fixtures/configurator_evals/onboarding/monitor_mailbox.md`)
  forbids real machine inspection. Findings arrive as a synthetic message in an input document (see
  `"Synthetic findings: disk=81%, docker=unavailable..."` around line 443); the monitor emits
  `MailboxItem` documents; graders check keyword containment (`assert_mailbox_findings`, lines
  315-333) and structured finding sets (`verify_finding_coverage_at` in `host_scenarios.rs` 598-663).
- `host-steward` and `host-maintenance` are host-only (real `df`, `sha256sum`, `stat`, `wget` in a
  container). They are out of scope here.
- No fixed monitor pack exists. Every #1512 suite has a configurator author the monitor in the trial.
- The embedded runner refuses a subject that grants `Unrestricted` bash. The golden monitor gets no
  bash at all.
- `packs/mailbox` is the existing mailbox datastore surface pack; read its `manifest.json` and
  `packs/README.md` for the pack format, and `crates/gents/src/pack.rs` for the loader.

## Deliverables

All three live in the M3 worktree (`gents-eval-definition-monitor`, branch `eval/30-monitor-prework`,
base `eval/03-definition` at `2629d4a11`). No cargo build is needed for deliverables 2 and 3. One
cargo invocation is allowed at the end for deliverable 1 (the pack loader test).

### 1. The golden monitor pack: `packs/eval_monitor/`

A fixed, document-driven monitor subject. It contains:

- `manifest.json` in the format `packs/mailbox` and `packs/README.md` describe.
- An `AgentBehavior` with `behavior_id` `eval-monitor`, and an `AgentContext` holding the system
  prompt. The prompt is the subject under evaluation and later under optimization, so it is written
  plainly: read the input, report each condition as one mailbox finding, do not invent conditions,
  do not duplicate a finding already open for the same correlation, mark a finding resolved when a
  later input says its condition cleared. It must not mention checks, expected values or case ids.
- A `Tools` document with **no bash**: the mailbox datastore surface (reuse `packs/mailbox`'s
  surface by reference if the pack format allows, otherwise copy it) and file tools in read-only
  mode under the workspace root.
- The input path: a schema for a `MonitorInput { correlation: String, message: String }` collection,
  an `EventSource` on document creation in it, a `Trigger`, and a `Task` whose template renders
  `message` into the request. Copy the shapes the #1512 fixtures make the model author (the
  `EvalMailboxInput` triple in `onboarding_scenarios.rs`), but fixed.
- A `README.md` stating what the pack is, that it is an eval subject, and that its prompt is the
  optimization target.

Acceptance: `cargo test -p gents --lib pack` (or the nearest existing pack loader test) loads it
without error, and `gents pack install --digest` preview on it (if a built CLI is available) shows
the expected documents. If neither can run without a long build, record that and stop at a
loader-level check.

### 2. Twenty case drafts: `packs/eval_monitor/cases/<case_id>.json`

Provisional container, one file per case, mirroring the contract's case shape plus a `fixtures`
block and an `expected` block that spec 3a will map onto checks:

```json
{
  "case_id": "single-disk-warning",
  "split": "train",
  "reducer": "weighted_mean",
  "fixtures": {
    "documents": [{"collection": "MonitorInput", "fields": {"correlation": "c-001", "message": "..."}}],
    "files": [{"path": "inventory.json", "contents": "..."}]
  },
  "stages": [
    {"stage_id": "report", "prompt": "...", "deadline_secs": 600,
     "capture": [{"name": "items", "collection": "MailboxItem", "filter": {"requester_did": "$trial"}}],
     "expected": {
       "findings": [{"keywords_all": ["disk", "81"]}],
       "finding_count": 1,
       "no_finding_containing": ["docker"]
     }}
  ]
}
```

Rules for every case:

- Independent: its own correlation ids, its own input, its own expected block. No case references
  another and no case assumes a prior case ran.
- Deterministic expectation: every `expected` entry must be checkable by a pure function over the
  captured `MailboxItem` rows and captured files. Keyword sets, counts, set equality of finding
  keys, presence and absence. No "looks reasonable".
- One or two stages. A second stage exists only for recovery and dedup cases, where the second
  input changes the world.
- Nothing protected inside a prompt or fixture: no expected values, no check names, no case ids.
- Split assignment: 8 `train`, 8 `validation`, 4 `test`. Validation and test cases must be at least
  as hard as train and must not be paraphrases of a train case.

Axes, from the scout, with a target count each:

| Axis | Cases | What varies |
|---|---|---|
| Finding content | 5 | one, two, three conditions; numeric thresholds; a service name |
| Dedup | 3 | same correlation resubmitted; new correlation, same content; new content |
| Recovery | 3 | a second input says a condition cleared; partial clearance; clearance of a condition never reported |
| Concurrent sources | 3 | two inputs in flight with distinct correlations; grouping; no cross-talk |
| Ambiguous input | 3 | a message that omits a condition; a message with a number but no condition; an empty message |
| Workspace file | 3 | findings derived from a fixture file read through the file tools, with and without a contradicting input document |

### 3. The check inventory: `packs/eval_monitor/CHECKS.md`

A table of every named check the twenty cases need, with: proposed name, parameters, the `expected`
key it grades, the #1512 grader it ports (file and line), and whether it is acceptance tier. No Rust
is written. This is spec 3a's input.

## Process

- Opus 5 writes the pack and the first three cases (one per reducer kind that appears) as
  exemplars. Grok 4.7 drafts the remaining seventeen from the exemplars and the axis table; the brief
  file plus the three exemplars are its whole interface.
- Opus 5 reviews every case against the rules above, individually, and rejects any that is not
  independent, not deterministically checkable, or leaks protected content.
- Commits: one for the pack, one per axis for cases, one for the check inventory.
- Report to the orchestrator at: pack loaded, all twenty cases reviewed, inventory written.
