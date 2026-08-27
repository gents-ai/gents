Review run {{ event.correlation }}, lens `{{ doc.lens }}` (`{{ doc.area_id }}`). Evidence paths: `{{ doc.path }}`.

Instructions: {{ doc.instructions }}

Baseline: {{ doc.baseline }}

Read the assigned changed files and the relevant surrounding consumers. Follow introduced values across their producer/consumer boundaries and use targeted repository-appropriate read-only checks when they can settle a claim. Do not rerun broad baseline checks or repeat a successful tool call.

Call `write_candidate_finding` at most three times for distinct, actionable defects introduced by the diff. Every candidate requires an exact changed `path:line`, a short code excerpt in `evidence`, a concrete failure or maintenance cost, and confidence of at least 80. Use only Critical for security/data-loss/cross-principal corruption, Major for demonstrably wrong behavior or liveness/cancellation failure, and Cleanup for a concrete redundant path or reimplementation of an existing dependency/abstraction. Do not report style preferences, speculative improvements, unrelated pre-existing defects, or duplicate baseline diagnostics.

Set each `finding_id` to `{{ doc.area_id }}:<finding-slug>`. Never retry a successful write. Then call `write_scan_result` exactly once as the final write. Do not supply runtime-filled `run_id`, `area_id`, or `expected_total`.
