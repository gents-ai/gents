Review run {{ group.correlation_value }} has {{ group.count }} completed lens scans (complete={{ group.complete }}):

{{ group.docs }}

Call `defra_query` for `CandidateFinding` with `run_id == "{{ group.correlation_value }}"`. Apply these gates to every candidate:

1. Quote the exact `path:line` artifact after reading it in this request.
2. Read the complete changed file plus the enclosing function, impl, or module and relevant callers/usages.
3. Check traits, derives, cfg gates, error propagation, crate edition, and the actual dependency API where relevant.
4. Keep the defect attributable to changed lines in the pull-request diff. Surrounding code may prove or refute the path, but an unrelated pre-existing defect is out of scope.
5. For lifecycle, provider-input, scheduler, recovery, trigger, transcript, or permission claims, check the corresponding Lean model and conformance boundary when one exists. Absence of a proof change is only a defect when the PR changes modeled semantics.
6. For missing-database-feature claims, inspect workspace DefraDB feature declarations and available pinned API/source evidence. Refute recommendations that reinvent an existing DefraDB or Gents capability or create a second owner for an established flow.
7. For Cleanup claims, require an exact redundant path or exact comments/docs that can be removed or replaced by a link to the canonical source without losing rationale, invariant, safety, or operator-contract information.
8. Refute claims already caught by the deterministic baseline, contradicted by surrounding behavior, based only on style, or lacking a concrete failing execution path or maintenance cost.
9. For retry/resource claims, read the documented recovery contract and nearby cadence/batch bounds. Retry of failed, unmarked work is not duplicate durable work unless a non-idempotent effect can demonstrably repeat before its marker.

This is focused verification, not unrelated repository exploration. For each candidate,
read the named enclosing artifact, use targeted `lsp` references/definition or text usage
searches, and follow the relevant path as deeply as needed to establish or refute the
claim. Then immediately persist that candidate's verdict before inspecting the next
candidate. Do not enumerate unrelated files or investigate claims that were not emitted
as candidates.

Call `write_finding_verdict` exactly once per candidate. Preserve its identity and content fields; reassess `confidence` as an integer string from `0` through `100`; set `verdict` to exactly `confirmed` or `refuted`; replace `evidence` with fresh verification evidence; and explain the refute-or-promote reasoning in `verification`. A confidence below 80 must be refuted. Then call `write_verification_summary` exactly once with candidate, confirmed, and refuted counts that balance exactly. Do not supply `run_id`; both write tools runtime-fill it from the group correlation.
