Scan run {{ group.correlation_value }} has {{ group.count }} completed
investigation batches (complete={{ group.complete }}):

{{ group.docs }}

Call `query_candidate_finding` once to load every candidate finding for
this run (the run filter is applied automatically). For each candidate,
in order:

1. Re-read the cited `path:line` and its enclosing function, impl, or
   module, plus relevant callers. Your file tools are already rooted at
   the scanned tree.
2. Check for mitigations the investigator may have missed: escaping,
   guards wrapping the handler directly, trusted-only data paths.
3. Consult git history — was this already fixed after the pre-scan?
4. Check for duplicates: two candidates describing one defect keep the
   strongest as primary; the other is refuted as a duplicate naming the
   primary `finding_id` in `verification`.
5. Immediately call `write_finding_verdict` for that candidate before
   inspecting the next: preserve `finding_id`, `batch_id`, `path`,
   `line`, `title`, `detail` verbatim; reassess `severity` and
   `confidence`; set `verdict` to exactly `confirmed` or `refuted`;
   replace `evidence` with what you verified; explain in `verification`
   starting with one of: true-positive, false-positive, fixed,
   uncertain, duplicate.

Use `lsp` and targeted read-only shell (`git log -p`, `git blame`,
`cargo test <specific test>`) as needed; no network, no modification.
Never repeat an identical tool call. Every candidate gets exactly one
verdict — no more, no fewer.

Finally call `write_revalidation_summary` exactly once, as your last
write: `candidate_count`, `confirmed_count`, `refuted_count` (the three
must balance exactly), and a short `summary`. Do not supply `run_id`;
it is runtime-filled.
