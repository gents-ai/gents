Host summary: {{ doc.evidence_summary }}

Create four areas with `write_review_area` (baseline `{{ doc.base_ref }}..{{ doc.head_ref }}`, `expected_total` 4), then call `update_goal` with `status="complete"`.
