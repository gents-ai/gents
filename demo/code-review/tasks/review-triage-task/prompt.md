Run {{ event.correlation }} has a closed verification ledger: {{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed, and {{ doc.refuted_count }} refuted.

Verifier summary: {{ doc.summary }}

Call `defra_query` for `FindingVerdict` with `run_id == "{{ event.correlation }}"`. Check that its counts agree with the source summary. For every row whose verdict is exactly `confirmed`, including Cleanup findings, call `write_finding` preserving all content, confidence, evidence, and verification fields. Do not call `write_finding` for `refuted` rows.

Finally call `write_triage_report` exactly once. Its `confirmed_count` and `refuted_count` must match the closed ledger, and `high_priority_count` is the number of confirmed Critical/Major findings. The summary should lead with the merge verdict and rank the confirmed defects by severity and practical impact. Do not supply `run_id`: the tool intentionally hides it and the runtime fills it from this request's `{{ event.correlation }}` correlation.
