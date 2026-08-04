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

Four defects, four fixes, enforced at the compactor boundary:

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

- `parse_summary_response` error context carries at most a 256-byte prefix of the
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

### 5. Immutable safety ceilings

The two compaction options are internal policy, not persisted behavior
configuration. `CompactionOptions` supplies safe defaults and
`DefraCompactor::compact` clamps every caller-provided value before it reaches
the provider request or formatter. Two non-configurable ceiling constants
live beside the defaults in `config.rs`:

- `MAX_COMPACTION_SUMMARY_MAX_OUTPUT_TOKENS = 32_768`
- `MAX_COMPACTION_SUMMARY_FILE_LIST_MAX = 1_000`

The clamp range is `[1, MAX_*]`. The whole-summary `bounded_summary` pass at
creation (§3) remains the byte-level backstop. If operators later need to tune
these values per behavior, that can be designed as separate schema work
without weakening this safety boundary.

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
- Ceiling clamp: direct `CompactionOptions` values at `usize::MAX` are clamped
  before both the provider request and rendered file lists.
- **15,000-path regression (unit):** structural extraction over 15k
  read/write tool calls produces a formatted summary bounded well under
  `bounded_summary`'s limits with both markers present; a multi-MiB
  mid-string-truncated JSON model reply produces a parse error message ≤ a
  few KiB carrying `[truncated, {n} bytes total]`.
- **Downstream surfaces:** diagnostics are bounded before becoming an `Error`,
  and summaries are bounded before becoming a `CompactionResult`; response
  persistence, tracing, Harbor, and ATIF receive those same strings rather
  than the raw provider output. The 15,000-path and multi-MiB parser tests
  fence those two source boundaries, while existing projection and provider
  budget-guard tests fence their consumers.

**Gates:** `cargo test -p gents` (full package — integration tests are
separate compile units) and `cargo check --workspace --all-targets`.

## Decisions log

- **Model file lists: removed entirely** (not capped) — structural extraction
  is the sole source; old-shape replies parse but are ignored.
- **Internal policy, not behavior schema** — the incident fix does not require
  a DefraDB migration, CLI/desktop surface, or self-config field. Immutable
  ceilings (32,768 tokens / 1,000 entries) are clamped at the compactor
  boundary, so no direct caller can weaponize them.
- **Defaults:** 4096 summary output tokens; 100 rendered paths per list;
  256-byte error preview and 512-byte per-item render bound (constants, not
  configurable).
- **Byte bounds over item counts:** per-item sanitize+truncate plus a
  whole-summary `bounded_summary` pass at creation, because item-count caps
  alone cannot bound bytes when paths come verbatim from tool arguments.
- **Neutral truncation marker** — per-turn compaction injects summaries
  without persisting an entry, so the marker must not promise one.
- **Cap replaces inherited `max_tokens`** rather than `min()`ing.
- **No `adapter_projection` changes** — bounded at source; projection-side
  defensive truncation deferred (adjacent to #717/#988).
- **Lean impact:** none. The compaction model
  (`Proofs/Compaction/Summarize.lean`) treats the summary as an opaque
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
