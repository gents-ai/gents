Review run {{ group.correlation_value }} has {{ group.count }} completed lens scans (complete={{ group.complete }}):

{{ group.docs }}

Load candidates with `read_candidate_finding`. Check each against the actual source and relevant behavior. Persist one `write_finding_verdict` per candidate, preserving its identity and content, with fresh evidence, verification reasoning, confidence from 0 through 100, and verdict `confirmed` or `refuted`. Confirmation requires confidence of at least 80. For each confirmed candidate, also persist `write_finding` with matching fields and verdict `confirmed`.

Finish with `write_verification_summary`; candidate, confirmed, and refuted counts must balance, including when there are no candidates. Run identity is runtime-filled. Resume missing work from the existing Goal and history. Once all verdicts, confirmed findings, and the summary are persisted, call `update_goal` with `status="complete"`.
