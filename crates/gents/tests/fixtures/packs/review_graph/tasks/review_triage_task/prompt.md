Run {{ event.correlation }}: {{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed, {{ doc.refuted_count }} refuted.

Read confirmed findings with `read_finding`, persist one `write_triage_report`, then call `update_goal` with `status="complete"`.
