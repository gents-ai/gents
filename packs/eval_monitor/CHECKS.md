# Check inventory for `packs/eval_monitor`

## Status (M3)

This inventory was spec 3a's input and stays as the record of why each check
exists. As of M3:

- The checks exist, one file each under `crates/gents/src/eval/checks/`
  (registry version `"2"`): every check below except
  `no_findings_for_correlation`, plus `mailbox_open_row` (status-aware) and
  `finding_text_excludes` (a ban scoped to one correlation's findings).
- The twenty cases moved to `packs/eval_monitor_findings/cases/`, converted to
  the contract's `checks` format by `scripts/evals/convert-expected-to-checks.py`.
  The `expected` shorthand below is retired and `cases/` no longer exists here.
- Keyword matching is **token-sequence** matching, superseding "substring"
  below: text and keyword are split into lowercase runs of letters and digits,
  and a keyword matches when its runs occur consecutively. `api` matches
  `api-gateway` and `api_gateway`; `disk` does not match `disks`.
- The capture filter is `{"requester_did": {"_eq": "$trial"}}` with `fields`
  `["title", "summary", "payload", "status"]`, which settles the field-union
  open item below.
- A row missing a field a check reads is a grader outcome (`missing_field`), so
  a capture that omits `title` can no longer make `mailbox_text_excludes` pass.
- The finding payload shape the checks parse is the subject's output contract
  in `README.md`, "Inputs and outputs".
- The Tools `host.root` is `${GENTS_EVAL_WORKSPACE_ROOT:-.}`, superseding
  `GENTS_EVAL_MONITOR_ROOT` below; the eval runner sets it per trial to the
  trial's workspace, where it writes `fixtures.files`.

## Splits (M3)

The pre-work split the cases 8 train / 8 validation / 4 test. The converted set
is **8 train / 6 validation / 6 held_out**: `test` became `held_out`, and two
validation cases moved to held_out, because held-out confirmation at alpha 0.05
over three rounds needs at least six held-out cases.

Which two moved: each must leave its axis still represented on validation, and
its difficulty must be at least the validation median. Only workspace-file and
finding-content had two validation cases, so one case moves from each. The
pre-work records no difficulty, so the ruling uses the case's findings-matcher
count as the proxy, with ties broken by total check refs. The validation median
is 2.5 matchers.

| Case | Axis | Matchers | Split | Why |
| --- | --- | --- | --- | --- |
| `file-without-contradiction-multi` | workspace-file | 3 | validation → held_out | It has more matchers than the other case in its axis pair (3 vs 2); workspace-file stays on validation through `file-with-contradicting-input` |
| `three-conditions-with-service` | finding-content | 3 | validation → held_out | It has more matchers than the other case in its axis pair (3 vs 1); finding-content stays on validation through `numeric-threshold-stated-normal` |

The moves live in the conversion script's `SPLIT_OVERRIDES` table, so the case
files remain reproducible from the pre-work.

This file is spec 3a's input. It names every check the twenty cases under `cases/` need, so the
check registry can be written against real demand rather than against a guess.

- Every check is a **pure function over the captured `items` rows** — the `MailboxItem` rows captured
  at the end of a stage. No check reads a file, a document, the transcript, or the model's reply.
  No case captures files: all thirty stages carry exactly one capture, `{"name": "items",
  "collection": "MailboxItem", "filter": {"requester_did": "$trial"}}`. Three cases install
  `fixtures.files`, but the files are inputs to the subject, never graded artifacts.
- **Every check listed is acceptance-tier eligible.** The contract's rule is "the acceptance tier is
  deterministic checks only in v1"; every expectation in the twenty cases is a deterministic
  predicate over the captured rows, so none needs the development tier and none is an `llm_judge`.
- The `expected` block in each case is the **provisional container** defined by the case-authoring
  standard. Spec 3a maps it onto `checks: [{check, params, tier, weight}]`; the mapping is one
  `expected` key to one check row below, with the two deliberate merges noted under the table.
- The subject names conditions in snake_case (`disk_usage_high`), so keyword matching is
  **case-insensitive substring** over `condition + " " + detail`, never equality on `condition`.

## The checks

| Proposed name | Parameters | `expected` key graded | #1512 grader ported | Acceptance tier | Capture fields read | Used by (cases / stages) |
|---|---|---|---|---|---|---|
| `mailbox_item_count` | `{"count": 1}` | `item_count` | new — #1512 never asserted a row count; its graders accepted any grouping of findings across rows (`host_scenarios.rs:665-666`, "accepts grouping") | yes | `_docID` only | 18 / 28 |
| `payload_well_formed` | `{"version": 1}` | `payload_well_formed` | ports the parse-and-shape step of `crates/gents/tests/configurator_evals/host_scenarios.rs:602-663` (payload read and `serde_json::from_str` at lines 640-644, array-shape `context` at lines 645-647, non-empty `ensure!` at line 648) | yes | `payload` | 20 / 30 |
| `finding_pairs_unique` | `{}` | `pairs_unique` | new — #1512 deduplicated into a `BTreeSet` (`host_scenarios.rs:609`, `:638`, inserts at `:650-655`) and so could not observe a duplicate | yes | `payload` | 20 / 30 |
| `finding_count` | `{"count": <int>}` | `finding_count` | new — same reason: set semantics hid cardinality | yes | `payload` | 20 / 30 |
| `finding_state_counts` | `{"open": <int>, "resolved": <int>}` | `open_count`, `resolved_count` | new — #1512 had no finding state vocabulary | yes | `payload` | 6 / 12 |
| `findings_match` | `{"matchers": [{"correlation": "c-01-a", "state": "open", "keywords_all": ["disk", "88"]}], "exact": true}` | `findings`, `findings_exact` | ports the keyword-containment idea of `crates/gents/tests/configurator_evals/onboarding_scenarios.rs:315-333`, narrowed from "somewhere in the joined row text" to per-finding scope; the `exact` flag ports the set-equality of `host_scenarios.rs:658-661` | yes | `payload` | 20 / 30 |
| `mailbox_text_excludes` | `{"keywords": ["postgres"]}` | `no_finding_containing` | inverse of `onboarding_scenarios.rs:315-333`: same `["title", "summary", "payload"]` field triple (payload only through parsed findings, per Shared predicates, not as raw row text), same `.to_lowercase()`, `ensure!(!contains)` instead of `ensure!(contains)` | yes | `title`, `summary`, `payload` | 19 / 28 |
| `no_findings_for_correlation` | `{"correlations": ["c-13-b"]}` (illustrative — no case uses this key) | `no_finding_for_correlation` | new — no #1512 analogue; #1512 had no correlation dimension | yes | `payload` | **0 / 0** (vocabulary key, unused) |

**Capture fields required (union):** `_docID`, `title`, `summary`, `payload`. No check reads
`status`, but spec 3a should consider adding it: the subject's "exactly one *open* record" invariant
is otherwise unobservable, and `mailbox_item_count` cannot distinguish one open row from one closed
row.

Two merge decisions, stated:

- **`findings` + `findings_exact` are one check (`findings_match`), not two.** The matcher list is the
  parameter of both, the one-to-one assignment and the set-equality are the same computation over
  that assignment, and both keys appear together in all 30 stages. Splitting them would copy the
  matcher list into two `params` blocks that can drift when a case is edited. Spec 3a may still split
  them if it wants exactness weighted separately from coverage; the cost is that duplication.
- **`open_count` + `resolved_count` are one check (`finding_state_counts`).** They always co-occur
  (6 cases, 12 stages, never one without the other) and both are projections of the same state tally.
- `no_findings_for_correlation` is in the standard's vocabulary but **no case uses it**. Register it
  or defer it; do not assume demand.

## Shared predicates

Defined once; the table rows above reference them.

- **Payload parse.** A captured row's `payload` is a JSON-encoded *string*. Parse it; require an
  object with `version == 1` and a `findings` array whose every entry is an object with string
  `correlation`, `condition`, `state` and `detail`, and `state` in the closed vocabulary
  `{"open", "resolved"}`. A row whose `payload` is absent, not a string, not JSON, the wrong version,
  or carries an out-of-vocabulary `state` fails `payload_well_formed`; every other check that reads
  findings treats the same condition as a failure of its own claim, not as an error.
- **Finding text match.** The matchable text of a finding is `condition + " " + detail`, lowercased.
  A keyword matches when it is a **substring** of that text, case-insensitively. Keywords are message
  tokens or the stem of a hyphenated token (`api` for `api-gateway`), never synonyms and never
  spelled-out numbers, because the subject chooses the `condition` spelling itself and names
  conditions in snake_case. A matcher is satisfied by a finding when `correlation` is equal, `state`
  is equal, and every keyword in `keywords_all` matches. Each matcher must be satisfied by exactly
  one finding, and no finding may satisfy two matchers (a one-to-one assignment).
- **Row text ban.** `mailbox_text_excludes` is *not* scoped to findings despite the `expected` key's
  name. It bans each keyword, case-insensitively, from every finding's matchable text **and** from
  each captured row's `title` and `summary`. This is inherited from the #1512 grader's field triple
  and is the set's main false-negative path (see Gaps).

## Runner requirements these cases assume

- **Fresh home per case-trial**, so the `MailboxItem` record is empty before stage 1. Every case's
  stage-1 expectation is written as if nothing preceded it; a reused home breaks `finding_count`,
  `mailbox_item_count` and `findings_exact` in every one of the twenty cases.
- **Stages are submitted sequentially**: stage N+1 only after stage N has reached a terminal
  `lifecycle_state`. Ordering is the guarantee that the dedup, recovery and concurrent-sources axes
  rest on; there is no other sequencing mechanism in the case format.
- **The `items` capture is `MailboxItem` filtered by the trial's requester DID (`$trial`), taken after
  each stage.** Every stage grades the rows captured at the end of *that* stage, so the capture must
  happen per stage and not once at the end of the trial.
- **Open item for spec 3a: the capture's field selection.** The case format's capture is
  `{name, collection, filter}` and names no fields; per the orchestrator the runner's document capture
  selects `_docID` plus an explicit `fields` list. If that is the shape spec 3a fixes, the list must
  include `title`, `summary` and `payload` (see the union above) or `mailbox_text_excludes` silently
  passes. Recorded as an open item, not as a property of the cases as written.
- **`fixtures.files` are written under the workspace root the pack's Tools `host.root` resolves to**
  (`${GENTS_EVAL_MONITOR_ROOT:-.}` in `pack_config.json`), **before the trial boots**, at the relative
  paths the case gives. The host file tools are `ReadOnly`, so the runner is the only writer.
- **No document fixtures in this set.** The standard permits `fixtures.documents` as static state
  and forbids only a `MonitorInput` document or any collection an EventSource watches, because the
  runner installs fixtures before the runtime boots and such a document would fire the pack's trigger
  and race the stage submission. No case here needs one, so the key is omitted throughout.
- **The pack's trigger never fires during evaluation.** Each stage's `prompt` is submitted directly as
  an `AgentRequest` in the trial's single session; the prompt text is what the Task template would
  have rendered.
- **`deadline_secs` is 600 on every stage.** A stage that does not reach a terminal state inside it
  is a `deadline` outcome: Fail with `score_bp` 0, per the core contract's outcome table
  (`2026-09-21-eval-core-contract-design.md:185`) and its rule that anything the subject could cause
  counts against it (`:174-175`). NotEvidence is reserved for provider-unavailable and infrastructure
  failures (`:190-191`), not for a slow stage.

## Spec 3a candidates

- **`StageInput::Document`** — writing a `MonitorInput` document and awaiting the trigger-created
  request — as a stage input kind, **not in spec 2a**. It is a candidate rather than a requirement
  because the pack's trigger is `"concurrency": "serial"`
  (`packs/eval_monitor/pack_config.json`), and under serial an input that arrives while a request is
  in flight is **skipped, never queued or retried**: `crates/gents/src/trigger_engine/mod.rs`
  lines 509-528, the `(false, ConcurrencyMode::Serial)` arm, where
  `has_active_runtime_request_for_trigger(...)` returning `Ok(true)` at line 520 builds
  `FireResult::Skipped { reason: "serial: prior fire still in-flight" }` (lines 521-523) and returns
  it at line 527. The input is not re-delivered, because
  `crates/gents/src/trigger_engine/event_source.rs` commits seen-state *before* the fire result is
  known: `commit_delivery_seen_state` (lines 752-763, `mark_seen` at line 761) is called at line 1738
  — immediately after `build_intents_for_all_matching` at lines 1730-1737 and *before*
  `take_first_and_queue_rest` at line 1739 — and again at line 1899 on the other delivery path;
  `has_seen` at line 1727 then skips the document forever. Switching the trigger to `parallel` does
  not fix it: two overlapping read-then-replace writes to the single mailbox row lose findings, since
  the subject's contract is "read the open record, then restate the complete record".
- **A `status`-aware one-open-row check.** The subject promises exactly one *open* row; the capture
  currently cannot see `status`. Cheap to add now, impossible to retrofit into recorded captures.
- **Whether to register `no_findings_for_correlation` at all.** Zero demand across twenty cases.
- **A finding-scoped ban**, distinct from `mailbox_text_excludes`, if spec 3a wants to fix the
  false-negative path below without changing the twenty cases' semantics.

## Gaps

- **Negative cross-correlation claims cannot be expressed.** "This correlation's detail must not
  contain that other condition's words" needs a ban scoped to one finding; `no_finding_containing`
  bans the words everywhere, including on the finding that legitimately carries them. The drafter hit
  this in the concurrent-sources axis and worked around it by choosing ban words that appear on no
  finding at all.
- **The ban also binds `title` and `summary`, which is the set's main false-negative path.** A monitor
  that writes an accurate payload but a chatty summary ("postgres is fine") fails a ban that was only
  ever meant to be about findings. This is inherited behaviour from the #1512 grader, kept
  deliberately, but it will produce failures that are not findings failures.
- **Most bans are inert, and inert bans are not coverage.** Of 29 `no_finding_containing` words
  across the set, 18 appear in no message of their case: they can fail only on invention, which is
  house style, not evidence. Two of those 18 do occur in a fixture file (`nginx` in
  `file-findings-from-inventory`, `ntp` in `file-without-contradiction-multi`), so for those two the
  ban can still fail on over-reporting from the file the monitor read. The remaining 11 name a word
  the case's own message mentions as healthy, cleared or merely referenced — and only 6 of the 11 are
  live at the stage that asserts them (5 name a word that first appears in a *later* stage of the
  same case, so they are inert where they stand). Count 6, not 29, as real "must not report this"
  coverage.
- **Zero-finding row behaviour is unspecified, so two cases cannot assert it.**
  `ambiguous-empty-message` and `ambiguous-number-no-condition` omit `item_count` (hence 18/28, not
  20/30) because it is unknown whether the subject writes an empty-`findings` row or writes nothing at
  all when an input describes no condition. The system prompt says "write the mailbox item once for
  each input", which implies one row; it has not been observed. A trial should settle this and the two
  cases should then gain `item_count`.

## Validation status

**No trial has run.** Every check and every count in this file is static: derived from the case files,
the subject's system prompt and the #1512 graders, never from an observed model response. The
assertions still untested against a real model are: that the chosen keywords survive into
`condition + " " + detail` at all (the subject picks the `condition` spelling, and a keyword could
land only in a `summary` the matcher does not read); the one-row-versus-no-row behaviour on
zero-finding inputs (above); whether the monitor actually reads the pointed-at workspace file rather
than answering from the message (the three `file-*` cases assume it does, and their expectations
depend on numbers that exist only in the file); carry-forward of untouched findings across stages
under the `all` and `weighted_mean` reducers (of the 9 multi-stage cases, 7 assert a later-stage
record that repeats a stage-1 matcher — same correlation and same `keywords_all`); that a cleared
condition flips to `resolved` rather than being dropped or re-worded past its keywords (the recovery
axis); and that the subject keeps a stable `condition` name for the same condition across inputs,
which `finding_pairs_unique` and the dedup axis both depend on. Treat every weight and tier
assignment as provisional until a first trial.

Two case-specific assumptions deserve naming, because a compliant monitor might defensibly break
them:

- `recovery-clearance-never-reported` expects **no** `resolved` finding for the clearance of a
  condition that was never reported. The system prompt's rule 4 covers flipping an *existing*
  finding, and rule 2 forbids inventing a condition the message does not describe — but rule 1 says
  to report every condition the message describes, and a clearance is arguably such a condition. A
  monitor that files it already-resolved would fail `finding_count` and `findings_exact` here without
  obviously violating its instructions. If a trial shows that reading, the prompt needs the rule, not
  the case.
- `two-conditions-disk-docker` expects the keywords `["srv", "76"]`, and its message
  (`/srv crossed 76% overnight ...`) no longer contains the word "disk". The monitor is free to name
  the condition `disk_usage_high`; if it then writes a terse `detail` that omits the mount point, the
  `srv` keyword has nowhere to match and the matcher fails on a correct finding. The keyword is
  legal under the standard (it is a token of the message), but it depends on the detail echoing the
  path.
