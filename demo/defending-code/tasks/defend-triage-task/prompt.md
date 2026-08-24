Defense run {{ group.correlation_value }} has {{ group.count }} durable
verification completions (complete={{ group.complete }}):

{{ group.docs }}

Use `read_defense_candidate`, `read_defense_verification_assignment`, and
`read_defense_verdict` to load the bounded candidate, assignment, and verdict
ledgers. A
`:no-candidates` completion has no corresponding candidate or verdict; it is
the empty-set sentinel. When every completion carries the same
`scan_ledger_status`, carry it forward subject to the independent checks below;
when statuses disagree, apply the final precedence below and name every value
in the mismatch detail.

For a non-empty candidate ledger, compare the candidate and verdict identity
sets by `finding_id`. Every non-sentinel completion must join exactly one
candidate and assignment. Require exact agreement on `finding_id`, `area_id`,
`source_revision`, and `source_tree_state` before joining a pair. A verdict is
promotable only when that provenance agrees, `verdict=confirmed`,
`adjudicated_claim_kind=vulnerability`, `severity=HIGH|MEDIUM|LOW`, and its
completion has `status=verified`.
Every verdict must form one closed tuple: `confirmed` + `vulnerability` +
`HIGH|MEDIUM|LOW`, or `refuted` + one of
`hardening|correctness|operational|specification|not_a_finding` + `NONE`. Any other tuple is
a classification mismatch and makes the ledger inconsistent.
Verifiers do not dedupe; root-cause clustering is the sole consequence-collapse stage.
For each promotable verdict call
`write_defending_finding` by joining the exact candidate and verdict with that
`finding_id`. Preserve the candidate's `root_cause_key`, `category`, path/line,
title/description/exploit scenario, recommendation, and `threat_ids`. Set final
`claim_kind` from the verdict's `adjudicated_claim_kind`, and use the verdict's
provenance, security boundary, exploitability gates, impact, contract surface, `severity`, `confidence`, `evidence`,
`verification`, `preconditions`, and `access_level`; never promote the
candidate's `claimed_severity`. Set `verdict=confirmed` and derive a concise
`owner_hint` from the affected component/path. Do not perform source
verification yourself or rewrite either stage's evidence.

Finally call `write_defense_triage_summary` exactly once as the last write.
Use these exact formulas over the durable candidate/verdict join:

- `candidate_count = count(candidate rows)`
- `confirmed_count = count(verdict == "confirmed")`
- `refuted_count = count(verdict == "refuted")`
- `duplicate_count = 0`; only the later root-cause stage collapses consequences
- `eligible_confirmed_count = count(confirmed vulnerability verdicts with
  HIGH|MEDIUM|LOW severity, an exact candidate/assignment identity, matching
  provenance, and `completion.status=verified`)
- `promoted_count = count(DefendingFinding writes)` and it must equal
  `eligible_confirmed_count`

`candidate_count` must equal `confirmed_count + refuted_count` for a consistent
ledger; preserve the real counts when it does not.

If the identity sets, provenance, verdict tuples, or counts disagree, still close the stage:
promote only otherwise-eligible confirmed verdicts satisfying every criterion
above and name every missing/extra id or provenance/classification disagreement
in `summary`. Preserve any incoming non-`consistent` `scan_ledger_status`;
derive the final status with this precedence: `blocked_provenance` when any
incoming status or completion status is `blocked_provenance`;
`classification_mismatch:` when any incoming
classification or verdict tuple is invalid; `count_mismatch:` when incoming
statuses disagree, any completion is `blocked_handoff`, or identity,
provenance, coverage, or counts disagree; and
the shared incoming status otherwise. Name every simultaneous defect in the
status detail and summary.
For each non-empty assignment, `completion.status=verified` requires exactly
one verdict; `blocked_handoff` or `blocked_provenance` requires no verdict; and
`skipped` is valid only for the empty-set sentinel with no verdict. Treat every
other completion/verdict combination as `count_mismatch:` and never promote a
verdict whose completion is not `verified`.
The completion sentinel is absent
from both candidate and verdict ledgers and every count. Do not subtract
duplicates from `refuted_count`. Do not supply runtime-filled `run_id` or
`repository_path`. Never retry successful writes or call subagent tools.
