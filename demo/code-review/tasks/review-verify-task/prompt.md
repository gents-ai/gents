Review run {{ group.correlation_value }} has {{ group.count }} completed lens scans (complete={{ group.complete }}):

{{ group.docs }}

Call `defra_query` for `CandidateFinding` with `run_id == "{{ group.correlation_value }}"`. Apply these gates to every candidate:

1. Quote the exact `path:line` artifact after reading it in this request.
2. Read the complete enclosing function, impl, or module and relevant callers/usages.
3. Check traits, derives, cfg gates, error propagation, crate edition, and the actual dependency API where relevant.
4. Refute claims already caught by the deterministic baseline, contradicted by surrounding behavior, based only on style, or lacking a concrete failing execution path.
5. For retry/resource claims, read the documented recovery contract and nearby cadence/batch bounds. Retry of failed, unmarked work is not duplicate durable work unless a non-idempotent effect can demonstrably repeat before its marker.

This is a bounded verification pass, not open-ended repository exploration. After the
candidate query, use at most three repository inspection tool calls per candidate:
one read of the named enclosing artifact, one targeted usage search, and at most one
follow-up read. Then immediately persist that candidate's verdict before inspecting
the next candidate. Do not run broad repository searches, enumerate unrelated files,
or investigate claims that were not emitted as candidates.

Call `write_finding_verdict` exactly once per candidate. Preserve its identity and content fields; set `verdict` to exactly `confirmed` or `refuted`; replace `evidence` with fresh verification evidence; and explain the refute-or-promote reasoning in `verification`. Then call `write_verification_summary` exactly once with candidate, confirmed, and refuted counts that balance exactly. Do not supply `run_id`; both write tools runtime-fill it from the group correlation.
