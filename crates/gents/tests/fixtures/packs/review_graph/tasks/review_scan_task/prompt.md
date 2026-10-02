Review run {{ event.correlation }}, lens `{{ doc.lens }}`. Read every evidence page with `read_review_evidence_manifest`/`read_review_evidence_page`.

Persist at most three `write_candidate_finding` entries, then `write_scan_result` and `update_goal` with `status="complete"`.
