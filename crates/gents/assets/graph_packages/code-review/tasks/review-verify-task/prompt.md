Review run {{ group.correlation_value }} has {{ group.count }} completed lens scans (complete={{ group.complete }}):

{{ group.docs }}

Call `read_candidate_finding` once to load every CodeReviewCandidateFinding for this run. For each candidate, freshly read the exact artifact, its enclosing behavior, and relevant callers/usages. Try to refute it using the actual repository language, dependency APIs, error propagation, tests, and surrounding invariants. Keep the issue attributable to changed lines; reject style-only, speculative, baseline-duplicate, or unrelated pre-existing claims.

Immediately call `write_finding_verdict` exactly once per candidate with the preserved identity/content fields, fresh evidence, verification reasoning, confidence from 0 through 100, and verdict exactly `confirmed` or `refuted`. Confidence below 80 must be refuted. After a confirmed verdict, call `write_finding` with the same fields and verdict `confirmed`; never write a finding for a refuted candidate. Do not repeat successful reads or writes.

Finally call `write_verification_summary` exactly once with candidate, confirmed, and refuted counts that balance exactly, including the zero-candidate case. Do not supply runtime-filled `run_id`.
