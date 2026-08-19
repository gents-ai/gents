Run {{ event.correlation }} has a closed verification ledger: {{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed, and {{ doc.refuted_count }} refuted.

Verifier summary: {{ doc.summary }}

The verifier already persisted every FindingVerdict and promoted every confirmed row to Finding. Do not write findings. Your job is the operator-facing merge report.

Call `read_finding` once to load the confirmed Finding rows for this run (`run_id` is filled from the correlation). Call `write_triage_report` exactly once. `confirmed_count` and `refuted_count` must match the ledger above. `high_priority_count` is the number of those Finding rows whose `severity` is exactly `Critical` or `Major` (zero if none). The summary should lead with the merge verdict and rank the confirmed defects by severity and practical impact. If there are no confirmed findings, say so and recommend merge unless the verifier summary names a blocking process failure. Do not supply `run_id`: the tools hide it and the runtime fills it from `{{ event.correlation }}`.
