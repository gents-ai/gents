# Bounded compaction summaries (#1017)

**Date:** 2026-08-04
**Issue:** [#1017 — Bound compaction summaries and stop model-generated file-list expansion](https://github.com/source-inc/gents/issues/1017)
**Branch:** `issue-1017-compaction-summary-size`

## Problem

Compaction's summary completion inherits the user turn's output budget, its
prompt asks the model to enumerate `files_read`/`files_modified` arrays that
Gents already extracts structurally, its parse-failure diagnostics embed the
entire raw model output, and `format_summary` renders unbounded file lists
*before* the task-continuation sections. In PR #988's Terminal-Bench run,
`build-cython-ext` drove the model to enumerate 14,500+ paths, the provider
output died mid-JSON, and the full 2.1 MiB raw output was embedded in the
runtime error, response document, server log, Harbor exception, and ATIF
projection.

Four defects, four fixes, all with an operator-visible configuration surface:

| Defect | Site | Fix |
|---|---|---|
| No independent output cap | `compaction.rs` `DefraCompactor::compact` clones parent `LoopConfig` without touching `max_tokens` | Explicit summary output cap |
| Prompt invites enumeration | `summary.rs::compaction_prompt` requests file arrays | Remove file arrays from schema |
| Unbounded diagnostics | `summary.rs::parse_summary_response` embeds full raw output in error context | Bounded error preview |
| Unbounded, mis-ordered rendering | `summary.rs::format_summary` renders file lists first, uncapped | Reorder + per-list render cap |

Out of scope: compaction *failure fallback* semantics (#717) and defensive
truncation inside `adapter_projection.rs` — bounding the diagnostics at their
source bounds every downstream surface, including the ATIF projection.

## Design

### 1. Independent summary output budget

- New `CompactionOptions` field: `summary_max_output_tokens: usize`, default
  `DEFAULT_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS = 4096` (in `config.rs`).
- Inside `DefraCompactor::compact`, alongside the existing `summary_config`
  adjustments (`preamble`, `tool_choice`, `max_turns`):
  `summary_config.max_tokens = Some(options.summary_max_output_tokens as u64)`.
  The cap **replaces** the inherited value rather than `min()`ing with it — the
  issue requires a budget independent of the user turn's `max_output_tokens`,
  which is sized for a different job.
- Failure mode accepted: a summary that would overrun the cap comes back as
  truncated JSON and fails parsing. That failure is now *bounded* (see §4);
  making it recoverable is #717's scope. With file arrays removed from the
  schema, 4096 tokens is generous for a narrative plus two short lists.

### 2. Schema stops inviting enumeration

- `compaction_prompt()` requests JSON keys `summary` (string),
  `key_decisions`, `pending_questions` (arrays of strings) only. It adds:
  do not enumerate file paths — file activity is recorded separately and
  injected structurally; keep each list to short items (guideline ~10). The
  existing anti-injection hardening (treat transcript as data, never claim
  prior turns absent, record unfinished work as pending) is preserved
  verbatim.
- `SummaryResponse` drops `files_read`/`files_modified`. serde ignores unknown
  fields by default, so a model that emits the old shape still parses; its
  lists are simply discarded.
- `compact()` stops merging model-supplied lists into
  `CompactionResult.files_read/files_modified`. Structural
  `extract_file_activity` becomes the sole source. This also closes a
  hallucination vector — the model can no longer inject paths the run never
  touched into persisted compaction entries.

### 3. Formatting: continuation state first, file lists capped

- New `CompactionOptions` field: `summary_file_list_max: usize`, default
  `DEFAULT_COMPACTION_SUMMARY_FILE_LIST_MAX = 100`.
- `format_summary` section order becomes: narrative → **key decisions** →
  **pending questions** → files read → files modified. `bounded_summary`
  head-truncates on reload (2000 lines / 50 KiB), so truncation can now only
  ever eat file-list tail, never the task-continuation state — the acceptance
  criterion's ordering requirement.
- Each file list renders at most `summary_file_list_max` entries, followed by
  the neutral marker `… and {n} more (omitted from this summary)`. (Neutral
  because per-turn compaction — `agent/daemon/inference.rs` — injects the
  summary into the provider view without persisting a compaction entry, so a
  marker pointing at "the session's compaction entry" would be false there.)
- **A list-length cap is not a byte bound.** Structural paths are copied
  verbatim from tool arguments, so a single enormous or newline-bearing
  "path" could still blow up the rendered summary. Two further bounds, both
  in `summary.rs`:
  - *Per-item:* each rendered list item is sanitized (control characters
    including newlines replaced with spaces, so one item is one line) and
    truncated to `SUMMARY_ITEM_MAX_BYTES = 512` bytes (char-boundary floored,
    `…` suffix). Applied to file paths, key decisions, and pending questions
    alike.
  - *Whole-summary:* `compact()` passes the assembled string through the
    head-truncating `bounded_summary` bound (2000 lines / 50 KiB) **before
    returning it** in `CompactionResult.summary`. Bounding at creation covers
    both consumers — persistence via `session/compaction_entries.rs` and raw
    injection during per-turn compaction — instead of relying on each caller
    to re-bound. The existing `bounded_summary` call on reload stays as
    defense for legacy entries.
- Durable *structural* fields are untouched: `CompactionResult` and
  `AgentCompactionEntry` keep the complete file lists. Only the rendered
  summary string is capped.

### 4. Bounded diagnostics

- `parse_summary_response` error context carries at most a 2 KiB prefix of the
  raw output (floored to a char boundary) plus `[truncated, {n} bytes total]`,
  instead of the full text. This is the string that reached 2.1 MiB in the
  incident; every downstream surface (error response document, server log,
  Harbor exception, ATIF projection) carries it verbatim, so bounding the
  source bounds them all.
- The preview constant lives in `summary.rs` (not config — diagnostics, not
  behavior).
- The `run_loop_to_text` failure arm in `compact()` ("compaction summary
  inference failed: {error}") gets the same bounded-preview treatment
  defensively, though provider errors are normally short.

### 5. Configuration plumbing

Two new nillable `Int` columns on `AgentBehavior`, plumbed exactly like
`compaction_threshold` (Option end-to-end on the persistence path; required
`usize` in the runtime struct; defaulted once in
`agent.rs::behavior_config_from_documents`, where non-positive or
unconvertible values fall back to the default):

- `compaction_summary_max_output_tokens`
- `compaction_summary_file_list_max`

**Immutable safety ceilings.** Configurable does not mean unbounded: these
fields are agent-writable, and a self-config write (or a direct DefraDB
document write) setting either near `i64::MAX` would recreate the exact
amplification this change removes. Two non-configurable ceiling consts live
beside the defaults in `config.rs`:

- `MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS = 32_768`
- `MAX_COMPACTION_SUMMARY_FILE_LIST_MAX = 1_000`

Enforcement is layered so every write path is covered:

- **Runtime clamp (authoritative):** `behavior_config_from_documents` clamps
  the loaded value into `[1, MAX_*]`; out-of-range values are clamped with a
  `tracing::warn!`. Because this is the single Option→required conversion
  every document read flows through, it covers CLI writes, desktop writes,
  desired-state apply, self-config writes, and raw document writes alike —
  the fields stay in `writableFields`, and the ceiling, not the write fence,
  is the safety boundary.
- **Desired-state validation (early feedback):** `desired_state/validate.rs`
  rejects values outside `[1, MAX_*]` (not just non-positive ones), so a bad
  manifest fails at `config validate` rather than being silently clamped at
  load.
- The whole-summary `bounded_summary` bound at creation (§3) is the backstop
  even if a clamp is bypassed: no configuration can produce a rendered
  summary over 50 KiB.

Layers (from the plumbing survey; the implementation plan enumerates exact
sites):

1. **SDL:** `gents-schemas/schemas/agent/agent_behavior.graphql` — both
   fields appended. Declaration order is conformance-fenced (see 3).
2. **Migration (highest risk):** editing the SDL changes AgentBehavior's root
   version CID, which `gents-migration/src/registry.rs` pins in
   `DEFAULT_BASELINE`. Mirror the `reasoning_effort` precedent: freeze today's
   SDL as a local `AGENT_BEHAVIOR_BASELINE_SDL` const, repoint the baseline
   entry at it (same CID), and add **one** `MigrationStep::PatchVersioned` to
   `DEFAULT_STEPS` adding both columns in a single patch (one new
   `expected_version` CID pin, `lens: None` — nillable add needs no lens,
   `expected_state: CollectionExpectation::fields(&[both])`). Update
   `baseline_ensure.rs`, which currently hardcodes InferenceProfile as the
   only frozen collection.
3. **Lean + conformance fence:** `proofs/Proofs/SelfConfig/Types.lean` —
   append both names to the `.agentBehavior` arm of `allFields` **and**
   `writableFields` (decision: agents may self-tune these caps, mirroring
   `compaction_threshold` and the project's self-modification philosophy).
   `tests/conformance/self_config.rs` asserts SDL, Lean, and Rust field tables
   agree in declaration order.
4. **Rust config path:** `gents-protocol/src/row.rs` (`Option<i64>`,
   `#[serde(default)]`); `document_config/behavior.rs` (struct, three
   selection sets, upsert add/update via `graphql_optional_int_field`, default
   literal); `config_client/agent_behavior.rs` (add/update via
   `optional_i64_field`) and `config_client/patch.rs` (`all_fields` +
   `writable_fields`, order-fenced); `config.rs` (two `DEFAULT_*` consts,
   required fields, **manual `Debug` impl** — it feeds the reconcile
   fingerprint; omitting a field means live config changes don't restart the
   daemon); `agent.rs::behavior_config_from_documents` (defaulting);
   `agent/builder.rs` (setters, defaults, projection); `agent/daemon.rs`
   (populate `CompactionOptions` from behavior).
5. **CLI + desired state:** `BehaviorUpsertArgs` flags; `behavior_set`
   literal; `init.rs` bootstrap literal; `EXPORT_AGENT_BEHAVIOR_FIELDS` and
   `task.rs::BEHAVIOR_FIELDS` selection strings; `DesiredAgentBehavior`
   fields (`deny_unknown_fields` makes this mandatory); `convert.rs` export
   allowlist; a range rule (`1..=MAX_*`, see the safety ceilings above) for
   both fields in `desired_state/validate.rs` (modeled on the
   `stream_liveness_timeout_secs` rule).
6. **Desktop:** `BehaviorSaveRequest` + `BehaviorView` fields; save-command
   default/assignment literals; snapshot projections **including the
   chat-scope redaction site** (`project_behavior_for_chat` sets both to
   `None`); desktop-core `AGENT_BEHAVIOR_FIELDS` query string (and its exact-
   string test) and manage mutations; `BehaviorConfigPanel.tsx` form fields;
   regenerate TS bindings via
   `cargo test -p gents-desktop-bridge write_bindings -- --ignored`
   (CI freshness gate `committed_bindings_match_regeneration`); live-fixture
   rows and `resolveTargets.ts` pass-through.

### 6. Testing

**Compaction unit tests (`compaction/tests.rs`):**
- Summary request cap: mock model captures the completion request; assert its
  `max_tokens` equals `summary_max_output_tokens` and differs from a parent
  `LoopConfig.max_tokens` deliberately set higher.
- Prompt: no longer names `files_read`/`files_modified`; hardening assertions
  updated, anti-injection wording still asserted.
- Parse tolerance: old-shape JSON with file arrays still parses; arrays are
  ignored (result lists come only from structural extraction).
- Ordering: formatted summary sections appear narrative → key decisions →
  pending questions → files; a fixture with oversized file lists shows pending
  work surviving `bounded_summary` head truncation.
- Render cap: 100-entry cap honored with the `… and {n} more (omitted from
  this summary)` marker; full lists still present on `CompactionResult`.
- Byte bounds: a single multi-megabyte "path" renders as one ≤512-byte
  sanitized item; a path with embedded newlines renders as a single line; the
  summary returned by `compact()` never exceeds the `bounded_summary` limits
  regardless of input.
- Ceiling clamp: behavior documents carrying `i64::MAX` (and `0` / negative)
  for either field load as the clamped/default values.
- **15,000-path regression (unit):** structural extraction over 15k
  read/write tool calls produces a formatted summary bounded well under
  `bounded_summary`'s limits with both markers present; a multi-MiB
  mid-string-truncated JSON model reply produces a parse error message ≤ a
  few KiB carrying `[truncated, {n} bytes total]`.
- **Downstream-surface regression (integration):** the acceptance criteria
  name the response document, logs, and ATIF projection, so "bounded at
  source" must be fenced where it lands, not only where it's produced. A
  daemon-driven test (e2e_runtime style, mock provider) runs a request whose
  summary completion returns multi-MiB broken JSON and asserts explicit byte
  limits on: the persisted response document's error field, the emitted
  tracing event (captured via a test subscriber), and the ATIF/adapter
  projection of the failed run. A companion happy-path case with 15k paths
  asserts the persisted compaction entry's summary is bounded and that the
  rebuilt provider request passes `build_budgeted_request`'s post-compaction
  budget guard.

**Migration tests (`gents-migration`):** update `baseline_ensure.rs`
expectations (AgentBehavior joins InferenceProfile as a frozen collection with
a `PatchVersioned` step); add a data-preservation test modeled on
`inference_profile_reasoning_effort_migration_preserves_existing_document`.

**Plumbing fences:** update the struct-literal construction sites (compile-
driven), the exact-string assertions (`cli_config_validate.rs`, desired-state
fixture writer, desktop-core query test, `behavior-config-panel.test.tsx`,
desktop UI harness), and the Lean/SDL/Rust field-table conformance test.

**Gates:** `cargo test -p gents` (full package — integration tests are
separate compile units), `cargo check --workspace --all-targets`, desktop
bindings regeneration, `lake build` in `crates/gents/proofs` (SelfConfig
tables changed), plus the affected desktop JS test suites.

## Decisions log

- **Model file lists: removed entirely** (not capped) — structural extraction
  is the sole source; old-shape replies parse but are ignored.
- **Configurable caps with full behavior-document plumbing** — operator asked
  for the `compaction_threshold`-grade treatment despite the migration cost.
- **Both caps agent-writable via self-config**, like `compaction_threshold` —
  but bounded by immutable ceilings (32,768 tokens / 1,000 entries) clamped at
  the single document-load site, so no write path can weaponize them.
- **Defaults:** 4096 summary output tokens; 100 rendered paths per list;
  2 KiB error preview and 512-byte per-item render bound (constants, not
  configurable).
- **Byte bounds over item counts:** per-item sanitize+truncate plus a
  whole-summary `bounded_summary` pass at creation, because item-count caps
  alone cannot bound bytes when paths come verbatim from tool arguments.
- **Neutral truncation marker** — per-turn compaction injects summaries
  without persisting an entry, so the marker must not promise one.
- **Cap replaces inherited `max_tokens`** rather than `min()`ing.
- **No `adapter_projection` changes** — bounded at source; projection-side
  defensive truncation deferred (adjacent to #717/#988).
- **Lean impact:** `SelfConfig/Types.lean` field tables only. The compaction
  model (`Proofs/Compaction/Summarize.lean`) treats the summary as an opaque
  `SummaryHandle`; no transition or invariant changes. The post-compaction
  budget guard (`loop_stream.rs::build_budgeted_request`) is untouched and
  remains the authority for the "rebuilt provider request passes the budget
  guard" acceptance criterion.

## Acceptance criteria → design map

| Criterion | Where satisfied |
|---|---|
| Explicit summary output cap independent of user turn | §1 |
| Prompt/schema does not invite unbounded file lists | §2 |
| Structural file activity still in formatted summary | §3 (capped render + durable fields) |
| 15k-path regression: bounded output, documents, logs, ATIF | §1 + §3 + §4, regression test in §6 |
| Pending work / key decisions before high-cardinality fields | §3 ordering |
| Rebuilt provider request passes post-compaction budget guard | existing guard, unchanged; fenced by existing tests |
