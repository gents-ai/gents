# Security-scan experiment pack

**Date:** 2026-08-17
**Status:** Approved design, pre-implementation
**Pack:** `demo/security-scan`

## Purpose

A fully gents-native whole-codebase security scan, modeled on
[vercel-labs/deepsec](https://github.com/vercel-labs/deepsec) (Apache-2.0;
prompts and matcher taxonomy adapted with attribution). It differs from the
existing packs in scope: `code-review` reviews a PR diff, `repo-maintenance`
does a cleanup round — this pack scans the **entire tree**, using deepsec's
economics: a free mechanical regex pre-scan decides *what* gets investigated,
and paid model stages decide *how deep*.

deepsec's pipeline (scan → process → revalidate → report) maps onto the
established trigger-edge graph shape from `code-review`: closed cardinality
stamped at fan-out, sentinel-gated barrier, write-last summary contract,
exact candidate→verdict bijection enforced by the runner.

## Design principle: self-sufficient carrier documents

All inter-stage coordination is documents; **no stage queries the database**.
There is no `defra_query` anywhere in this pack. Every trigger edge delivers
its payload through prompt template injection:

- `{{ doc.<field> }}` on per-document edges — the trigger source document
  carries everything the consumer needs.
- `{{ group.docs }}` on the barrier edge — the grouped sentinel documents
  embed the candidate findings.

Where the next stage needs data that also exists as typed rows, the carrier
document embeds a copy. Typed rows (`CandidateFinding`, `FindingVerdict`)
remain the audit and acceptance surface — the runner verifies against them
and exports them to `results.json` — while the embedded copies exist purely
so prompts are assembled by the trigger engine, not by model-driven reads.

Payload discipline: **complete inventory, truncated evidence.** Caps never
silently drop items — a path+slug inventory line is always complete; only
excerpts truncate, and every truncation is recorded as an overflow count in
the same document.

## Graph

```text
[runner kickoff: ported scan engine runs, output embedded in the single seed doc]

ScanJob ──▶ scan-plan (planner)
              candidates arrive via {{ doc.candidates }}
              → writes N × InvestigationBatch (each stamps expected_total = N)
InvestigationBatch ──▶ scan-investigate × N
              self-contained evidence packet via {{ doc.* }}
              → CandidateFinding* + exactly 1 InvestigationResult sentinel
                (sentinel embeds its findings as JSON)
[N sentinels, fire_mode per_group] ──▶ scan-revalidate (single barrier consumer)
              all findings arrive via {{ group.docs }}
              → 1 FindingVerdict per candidate + 1 RevalidationSummary
                (written last; embeds the verdict ledger)
RevalidationSummary ──▶ scan-report
              ledger arrives via {{ doc.verdict_ledger }}
              → confirmed Finding* + 1 ScanReport
```

Four triggers, all `event_kind: created`: `scan-plan` (on `ScanJob`),
`scan-investigate` (on `InvestigationBatch`, per document), `scan-revalidate`
(on `InvestigationResult`, `fire_mode: per_group`, `correlation_field:
run_id`, `expected_count_field: expected_total`), `scan-report` (on
`RevalidationSummary`).

## Schemas (pack-scoped)

| Collection | Written by | Purpose |
| --- | --- | --- |
| `ScanJob` | runner seed | run_id, prompt/focus, scan root, `candidates` payload, `candidate_total`, `slug_counts`, `overflow_count`, batch bounds |
| `InvestigationBatch` | planner | `batch_id`, `expected_total`, file list, per-file hits + slug notes, `instructions` |
| `CandidateFinding` | investigators | typed finding row: `finding_id`, `batch_id`, severity, path, line, title, detail, evidence, confidence |
| `InvestigationResult` | investigators | sentinel: `batch_id`, counts, `findings_json` (embedded copy) |
| `FindingVerdict` | revalidator | per-candidate verdict row: verdict, reasoning, adjusted severity, fresh evidence |
| `RevalidationSummary` | revalidator | counts (balance exactly) + `verdict_ledger` (embedded copy) |
| `Finding` | report | confirmed findings only |
| `ScanReport` | report | run summary: counts by severity/slug, notable findings, coverage notes |

All seeded `ScanJob` fields are String-typed — `seed_mutation` emits every
field as an escaped GraphQL string.

Identity discipline: `finding_id` = `<run_id>:<batch_id>:<finding-slug>`,
globally unique, preserved verbatim through verdict and confirmed rows.

`run_id` is runtime-filled via `fill: correlation` on every write surface;
`expected_total` on `InvestigationResult` via `source_field` fill. Both are
hidden from model input, as in `code-review`.

## Scan engine port

New module `crates/gents-cli/src/commands/demo/secscan/`, a Rust port of
deepsec's scan stage (`packages/scanner`):

- **Matcher registry** with deepsec's structure: slug, description, noise
  tier (`precise`/`normal`/`noisy`), file patterns, optional tech gate,
  inline `examples[]`.
- **Curated matcher set**: the deepsec matchers that fire meaningfully on
  Rust/polyglot repos — secrets-exposure, secret-in-fallback, secret-in-log,
  rce/command-injection, path-traversal, insecure-crypto, missing-auth on
  HTTP surfaces, webhook-handler — plus **gents-native matchers**:
  - `graphql-injection` (precise): string interpolation into GraphQL
    without `escape_graphql_string` — this repository's documented sharp
    edge.
  - `defra-empty-array` (precise): `[]` literal in a DefraDB mutation
    (must be `null`).
- **Testing pattern ported too**: one discovery test iterates the registry
  and asserts every matcher's `examples[]` produce ≥1 candidate — adding a
  case is one line.
- Tech gating collapsed to what we need (Rust/TS detection by sentinel
  files); noise tiers drive the candidate sort order embedded in the seed
  payload (precise first).

### Runner extension

`experiment.json` gains an optional top-level `scan` section:

```json
"scan": {
  "root": "${GENTS_SCAN_ROOT:-.}",
  "max_payload_chars": "49152"
}
```

When present, `gents demo run` executes the scan engine over the resolved
root **before seeding** and merges computed fields into the seed mutation:
`candidates` (formatted payload), `candidate_total`, `slug_counts`,
`overflow_count`. The pack model's "kickoff = one GraphQL create of the
seed collection" is preserved — the scan adds fields to that one create,
not extra documents.

Payload format (model-facing, not JSON): a summary header (totals per slug
and tier), then per-file blocks sorted precise-tier-first:

```
crates/gents/src/foo.rs
  [precise] graphql-injection L214: format!("... {name} ...")
  [normal]  secret-in-log L88: tracing::info!(token = %token, ...)
```

Under `max_payload_chars`, excerpts truncate before inventory lines drop;
if inventory itself must drop, remaining files are listed path-only and
counted in `overflow_count`.

## Stages

### scan-plan (planner)

Receives the full candidate payload in its prompt. Batches candidate files
deepsec-style: ~5 files per batch, noise-tier-first ordering, related files
grouped (same slug family or module). Decides the complete batch list and
immutable `expected_total` before the first write; never changes cardinality
after. Bounds: `${GENTS_SCAN_MIN_BATCHES:-4}`–`${GENTS_SCAN_MAX_BATCHES:-24}`,
seeded as `ScanJob` fields. Each batch's `instructions` (≤8,000 chars) name
the files, the fired slugs with notes, and what to investigate; batches are
self-contained. If `overflow_count` > 0, the planner assigns the path-only
files to batches by directory affinity and says so in instructions.

Tools: `write_investigation_batch` + read-only file tools (to sanity-check
paths and sizes when grouping). No shell, no network.

### scan-investigate (× N)

Prompt adapted from deepsec's investigation template (severity taxonomy
CRITICAL/HIGH/MEDIUM/HIGH_BUG/BUG, slug table, false-positive guidance,
candidates-as-starting-points framing, open-ended review of each assigned
file). deepsec's "static analysis only" rule is relaxed one notch to match
this pack's tool surface: **targeted read-only inspection and targeted
tests are allowed; exploitation, network attacks, and mutation of the tree
are prohibited.**

Tools: `write_candidate_finding`, `write_investigation_result`, read-only
file tools, native `lsp`, bash rooted at the scan root (targeted `cargo`
tests, `git log`/`git blame`), background process tools for long commands.

Contract: findings written first (each with exact `path:line` + verbatim
excerpt evidence), then exactly one `write_investigation_result` as the
final write, embedding the same findings as `findings_json` and preserving
`batch_id` exactly.

### scan-revalidate (barrier)

Fires once when all N sentinels exist. Prompt adapted from deepsec's
revalidation pass: for each candidate in `{{ group.docs }}`, re-read the
cited artifact and enclosing context in this request, consult git history
("was this fixed?"), and write exactly one `FindingVerdict` with verdict ∈
`true-positive | false-positive | fixed | uncertain | duplicate`
(deepsec's taxonomy; `duplicate` must reference the primary `finding_id`),
optional adjusted severity, and fresh evidence. Persist each verdict before
inspecting the next candidate. Final write: one `RevalidationSummary` whose
counts balance exactly and which embeds the complete `verdict_ledger`.

Tools: `write_finding_verdict`, `write_revalidation_summary`, read-only
file tools, `lsp`, bash rooted at the scan root (git history, targeted
tests). No network.

### scan-report

Consumes the ledger from `{{ doc.verdict_ledger }}`. Publishes one
`Finding` row per true-positive (carrying forward identity, final severity,
evidence, revalidation reasoning) and exactly one `ScanReport` (counts by
severity and slug, notable findings, coverage/overflow notes).

Tools: `write_finding`, `write_scan_report` only. No shell, no file tools,
no network.

## Model backend

One `inference-backend` document: `${GENTS_SCAN_MODEL:-GLM-5.2}` at
`${GENTS_SCAN_ENDPOINT:-http://100.87.27.25:8000/v1}`, vLLM preset,
chat-completions wire API, **`concurrency: 8`**. All four behaviors share
it through one inference profile — at most eight requests in flight
regardless of batch count. Retargetable via env without editing tracked
config, per the pack convention.

## Acceptance (`experiment.json` expect block)

Mirrors `code-review`'s contract vocabulary:

- `trigger_ids`: `scan-plan`, `scan-investigate`, `scan-revalidate`,
  `scan-report`.
- `trigger_request_count_sources`: `scan-investigate` counted from
  `InvestigationBatch` / `run_id` / `expected_total`.
- `prompt_tool_contracts` per task — **write tools only**; no
  `required_query_collections` anywhere.
- `fan_in`: member `InvestigationBatch`, result `InvestigationResult`,
  report `ScanReport`, expected-count bounds from the batch-bounds env
  vars, `verification` sub-block mapping candidate `CandidateFinding` →
  decision `FindingVerdict` → summary `RevalidationSummary` → confirmed
  `Finding` (bijection + summary balance enforced by the runner).
- `collection_counts`: `RevalidationSummary: 1`, `ScanReport: 1`.
- `result_documents`: `Finding` and `ScanReport` exported to
  `results.json`.
- `signed_provenance: true`; all four projections.

## Testing

- **Unit (secscan)**: matcher example discovery test; tech gating; noise
  sort; payload formatting incl. truncation/overflow accounting; seed-field
  merge.
- **Unit (runner)**: manifest parsing for the `scan` section; scan-before-
  seed ordering.
- **Live e2e**: an `#[ignore]`d entry mirroring `lsp_live.rs` that drives
  `gents demo run security-scan` end to end — the "kick it off from a test"
  entry point.
- Gate with `cargo test -p gents` (full package suite) and
  `cargo check --workspace --all-targets` before pushing, per CLAUDE.md.

## Attribution

Pack README carries a notice: investigation and revalidation prompts and
the matcher taxonomy are adapted from vercel-labs/deepsec (Apache-2.0,
© 2026 Vercel, Inc. and contributors), with a link to the upstream NOTICE.

## Out of scope (deliberately)

- deepsec's incremental/resumable FileRecord merge semantics — pack runs
  use a fresh home; a run is one-shot.
- Diff-scoped modes (`--diff-working`, `--diff origin/main`) — that niche
  is `code-review`'s.
- The enrich stage (git committer / ownership attribution), notifiers,
  and plugin architecture.
- Matcher generation by a setup agent (deepsec's coverage loop). The
  registry is hand-curated; generation can become a follow-up pack stage
  later.
