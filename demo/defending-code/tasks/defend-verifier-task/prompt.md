Verify assignment `{{ doc.assignment_id }}` for finding
`{{ doc.finding_id }}`. Status: `{{ doc.status }}`.

Call `read_defense_verification_assignment` once. If status is `skipped`, do
not read source or a candidate and do not write a verdict. Call
`write_defense_verification_completion` exactly once with `status=skipped`.
Do not supply runtime-filled ids, repository path, or `expected_total`.

Otherwise call `read_defense_threat_model` once and
`read_defense_candidate` once, then call `read_defense_candidate_ledger` once
for bounded duplicate comparison. The assigned-candidate read is restricted
to this run and exact finding; the ledger contains candidate facts but no
sibling verifier reasoning. Adjudicate only
`{{ doc.finding_id }}`, then call `write_defense_verdict` exactly once. Never
write a verdict for a different finding id. Do not supply runtime-filled
`run_id`, `repository_path`, or `expected_total`. After the verdict write
succeeds, call `write_defense_verification_completion` exactly once as the
last write with `status=verified`. Never write completion before the verdict.
