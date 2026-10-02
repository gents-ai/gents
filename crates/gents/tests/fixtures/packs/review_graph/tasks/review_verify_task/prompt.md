Run {{ group.correlation_value }} has {{ group.count }} completed scans: {{ group.docs }}

Verify each `read_candidate_finding` row, persist `write_finding_verdict` (and `write_finding` when confirmed), then `write_verification_summary` and `update_goal` with `status="complete"`.
