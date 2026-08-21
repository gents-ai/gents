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

Finally call `write_defense_triage_summary` exactly once as the last write.
Use these exact formulas over real verdict rows:

- `confirmed_count = count(verdict == "confirmed")`
- `refuted_count = count(verdict == "refuted")`, including duplicates
- `duplicate_count = count(verdict == "refuted" && duplicate_of != "")`
- `candidate_count = confirmed_count + refuted_count`
- `promoted_count = count(DefendingFinding writes)` and it must equal
  `confirmed_count`

The completion sentinel is absent from both candidate and verdict ledgers and
every count. Do not subtract duplicates from `refuted_count`. Do not supply
runtime-filled `run_id` or `repository_path`. Never retry successful writes or
call subagent tools.
