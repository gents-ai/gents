Defense run {{ group.correlation_value }} has {{ group.count }} durable
verification completions (complete={{ group.complete }}):

{{ group.docs }}

Call `read_defense_triage_summary` first and stop without writes if it already
exists. Otherwise call `read_defense_candidate` and `read_defense_verdict`
once each. A `:no-candidates` completion has no corresponding candidate or
verdict; it is the empty-set sentinel.

For a non-empty candidate ledger, require an exact candidate-to-verdict
bijection by `finding_id`. A verdict is promotable only when
`verdict=confirmed` and `duplicate_of` is empty. For each promotable verdict
call `write_defending_finding` with its adjudicated fields and
`verdict=confirmed`. Do not perform source verification yourself.

After every candidate has one verdict, create the closed patch-work set:

- If C promotable findings remain, call `write_defense_patch_assignment` C
  times, one per finding, with `assignment_id=<finding_id>:patch`, the exact
  `finding_id`, `status=ready`, and `expected_total=C` on every write.
- If C is zero (including an empty candidate ledger), call it once
  with `assignment_id={{ group.correlation_value }}:no-findings`,
  `finding_id=none`, `status=skipped`, and `expected_total=1`.

Finally call `write_defense_triage_summary` exactly once as the last write.
Its candidate/confirmed/refuted counts must balance the real candidate ledger;
the completion sentinel is already absent from both ledgers and every count.
Also record duplicate count and patch assignment count. Do not supply
runtime-filled `run_id` or
`repository_path`. Never retry successful writes or call subagent tools.
