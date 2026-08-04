# Bounded compaction summaries implementation plan

**Issue:** #1017

## Scope

Fix the Terminal-Bench compaction amplification at its source without adding
new persisted behavior configuration:

1. Give the internal summary completion an independent 4,096-token default
   and enforce an immutable 32,768-token ceiling in `DefraCompactor`.
2. Remove model-generated file arrays from the prompt and response type.
   Preserve compatibility by ignoring unknown fields in old-shape replies.
3. Keep structural file activity as the sole source of paths. Render at most
   100 entries per list by default, with an immutable 1,000-entry ceiling and
   an omission marker.
4. Render narrative, decisions, and pending work before file lists. Sanitize
   and byte-bound every rendered item, then apply `bounded_summary` before the
   result reaches persistence or provider reinjection.
5. Bound JSON and provider-error previews at the source so response documents,
   logs, Harbor errors, and ATIF projections cannot reproduce the multi-MiB
   incident.

## Regression coverage

- Capture the internal completion request and prove its output budget is
  independent of the parent turn.
- Prove direct `CompactionOptions` values cannot exceed either safety ceiling.
- Feed 15,000 structurally extracted paths through compaction and prove the
  rendered summary remains bounded while the durable structural list remains
  complete.
- Feed multi-MiB invalid model output through the parser and prove the emitted
  diagnostic is bounded and carries a truncation marker.
- Preserve the existing post-compaction provider budget guard tests.

## Gates

Run `cargo test -p gents` and `cargo check --workspace --all-targets` before
publishing the PR into `agent/add-atif-trace-projection` (#988).
