Review run {{ event.correlation }}, lens `{{ doc.lens }}` (`{{ doc.area_id }}`). Evidence paths: `{{ doc.path }}`.

Instructions: {{ doc.instructions }}

Baseline: {{ doc.baseline }}

First call each of `read_review_evidence_0` through `read_review_evidence_17` exactly once, preferably together in one model turn. Each read returns sixteen fields. Concatenate all `evidence_chunk_0` through `evidence_chunk_287` values in numeric order; empty tail chunks are valid. Every field is deliberately smaller than the datastore's 2,000-byte per-string display ceiling, and every sixteen-field result is below the total tool-result ceiling. If any result reports truncation or any chunk contains `HOST EVIDENCE TRUNCATED`, stop and record that the scan is incomplete rather than reviewing partial evidence. Do not repeat a successful evidence read.

Then assess the assigned invariants without repository inspection calls; file, shell, and language-server tools are deliberately absent. Treat quoted patch text as candidate-generation evidence, not proof beyond what it contains. Finishing with zero candidates is correct.

Call `write_candidate_finding` at most three times for distinct, actionable defects introduced by the diff. Every candidate requires an exact changed `path:line`, a short code excerpt copied from the packet into `evidence`, a concrete failure or maintenance cost, and confidence of at least 80. Use only Critical for security/data-loss/cross-principal corruption, Major for demonstrably wrong behavior or liveness/cancellation failure, and Cleanup for a concrete redundant path or reimplementation of an existing dependency/abstraction. Do not report style preferences, speculative improvements, unrelated pre-existing defects, or duplicate baseline diagnostics.

Set each `finding_id` to `{{ doc.area_id }}:<finding-slug>`. Never retry a successful write. Then call `write_scan_result` exactly once as the final datastore write. Do not supply runtime-filled `run_id`, `area_id`, or `expected_total`. After that terminal result is durably written, call `update_goal` with `status="complete"`. Never complete the goal before the result exists.
