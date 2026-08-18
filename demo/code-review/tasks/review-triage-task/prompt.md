Run {{ event.correlation }} has a closed verification ledger: {{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed, and {{ doc.refuted_count }} refuted.

Verifier summary: {{ doc.summary }}

The verifier already persisted every FindingVerdict and promoted every confirmed row to Finding. Do not query the datastore and do not write findings. Your only job is the operator-facing merge report.

Call `write_triage_report` exactly once. `confirmed_count` and `refuted_count` must match the ledger above. `high_priority_count` is the number of confirmed Critical/Major findings named in the verifier summary (zero if none). The summary should lead with the merge verdict and rank the confirmed defects by severity and practical impact. If there are no confirmed findings, say so and recommend merge unless the verifier summary names a blocking process failure. Do not supply `run_id`: the tool hides it and the runtime fills it from `{{ event.correlation }}`.
