Review run {{ event.correlation }}, lens `{{ doc.lens }}` (`{{ doc.area_id }}`). Paths: {{ doc.path }}. Baseline: {{ doc.baseline }}.

{{ doc.instructions }}

Load `read_review_evidence_manifest` with no arguments, then `read_review_evidence_page` for each `page_index` from `0` through `page_count - 1`. Evidence identity is runtime-bound to `{{ doc.evidence_id }}`. Each read must return one untruncated row with matching identity and manifest metadata (format version `1`). Read chunks `evidence_chunk_0` through `evidence_chunk_15` in page order, ignoring trailing empty padding. Review progressively; incomplete or inconsistent evidence is a blocker, not a completed scan.

Use `write_candidate_finding` for at most three distinct actionable defects. Include an exact changed `path:line`, a supporting excerpt, the concrete impact, and confidence of at least 80. Set `finding_id` to `{{ doc.area_id }}:<finding-slug>`. Severity: Critical for security/data loss, Major for incorrect behavior or liveness, Cleanup for concrete duplication or unnecessary complexity.

After reviewing all pages, persist `write_scan_result`, including when there are no candidates. Identity and expected-count fields are runtime-filled. Resume missing work from the existing Goal and history. Once the result is persisted, call `update_goal` with `status="complete"`.
