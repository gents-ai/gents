Scan run {{ event.correlation }} revalidation is closed:
{{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed,
{{ doc.refuted_count }} refuted. Summary: {{ doc.summary }}

Call `query_finding_verdict` once to load every verdict for this run.
Then:

1. For each verdict with `verdict` equal to `confirmed`, call
   `write_finding` carrying every field forward verbatim (`finding_id`,
   `batch_id`, `severity`, `confidence`, `path`, `line`, `title`,
   `detail`, `verdict`, `evidence`, `verification`). Publish nothing for
   refuted verdicts.
2. Then call `write_scan_report` exactly once as your final write:
   - `candidate_total`: the number of verdicts you loaded
   - `batch_count`: the number of distinct `batch_id` values
   - `confirmed_count` / `refuted_count`: exact tallies of your loaded
     verdicts
   - `severity_counts`: like `CRITICAL=1 HIGH=2 MEDIUM=0 HIGH_BUG=1 BUG=0`
     over confirmed findings
   - `slug_counts`: confirmed findings tallied by the slug portion of
     `finding_id`, same `name=count` format
   - `summary`: at most ten sentences — lead with the most severe
     confirmed findings, then coverage notes.

Do not supply `run_id` on any write; it is runtime-filled. Never retry a
successful write.
