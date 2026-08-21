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

`verdict` remains exactly `confirmed` or `refuted`. `confirmed` is allowed only
when every exploitability gate in the system prompt is supported by concrete
source evidence. Populate `claim_kind`, normalized `root_cause_key`,
`security_boundary`, `attacker_control`, `default_reachable`,
`required_configuration`, `violated_invariant`, and `contract_surface` even
when refuting; this is the durable explanation for downstream clustering and
contract review. Copy the candidate's exact `source_revision`; do not replace it
with the repository's current HEAD, and copy its `source_tree_state`. If HEAD
or tree state changed after bootstrap, adjudicate against the recorded
provenance and call the mismatch out in verification. A duplicate is a refuted
verdict with `duplicate_of` set.

Write `verification` as a compact gate ledger: `attacker`, `control`,
`entry_to_sink`, `boundary`, `default_reachability`, `guards`, `impact`,
`invariant`, and `counterevidence`, each with concrete source references. Set
severity only after accounting for required access, configuration, and existing
guards. Duplicate only the same defective control; similar categories or
different sinks are not duplicates.
