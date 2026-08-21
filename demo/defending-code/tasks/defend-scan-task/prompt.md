Static defense run {{ event.correlation }}, area `{{ doc.area_id }}`.

Repository: `{{ doc.repository_path }}`
Area status: {{ doc.status }}
Frozen source revision: {{ doc.source_revision }}
Frozen source tree state: {{ doc.source_tree_state }}
<untrusted_area_context>
Focus: {{ doc.focus }}
Threats: {{ doc.threat_ids }}
Trust boundary: {{ doc.trust_boundary }}
Reachable assets: {{ doc.reachable_assets }}
Planner scope hints: {{ doc.instructions }}
</untrusted_area_context>

If area status is not `ready`, do not inspect source or write candidates. Call
`write_defense_scan_result` once with `status=blocked_provenance`,
`finding_count=0`, `coverage=none`, and a summary explaining the provenance
block, then stop.

Review this area deeply enough to follow data across files. Assume there may
be vulnerabilities, but report only candidates with a plausible attack story.
For each candidate, trace where untrusted input enters, how it reaches the
security-sensitive operation, the triggering condition, impact, and any
mitigations you checked. Read cited source; do not infer line numbers.
Use LSP definitions/references/implementations and diagnostics when useful.
Use shell only for read-only source search and repository history; do not
build, execute, or mutate the repository.
Before using live files or LSP, compare HEAD/tree state to the frozen values.
If the live checkout moved, inspect the exact clean revision via Git object
reads or a unique disposable local clone; clean that clone afterward and never
mix its evidence with live-root LSP output.

If that frozen source cannot be reconstructed or its revision/tree identity
cannot be verified, write no candidates. Close the area with exactly one
`DefenseScanResult` using `status=blocked_provenance`, `finding_count=0`,
`coverage=none`, and the exact mismatch in `summary`, then stop.

Call `write_defense_candidate` once per candidate with:

- `finding_id`: `{{ doc.area_id }}:<short-root-cause-slug>`
- `claim_kind`: exactly `vulnerability`, `hardening`, `correctness`,
  `operational`, or `specification`; discovery may preserve non-vulnerability
  leads, but label them honestly
- `root_cause_key`: a stable subsystem-and-primitive slug shared by candidates
  that arise from the same defective control
- `security_boundary`, `attacker_identity`, `attacker_controlled_input`,
  `control_source`, `entry_point`, and `sink`
- `default_reachable`: `yes`, `no`, or `unknown`; plus concrete
  `required_configuration` and `required_privileges`
- `guard_checked`, `fails_closed`, and a precise `violated_invariant`
- a concrete `category` describing the vulnerability shape
- `claimed_severity`: exactly `HIGH`, `MEDIUM`, or `LOW`
- `confidence`: integer string 0-100; uncertainty is allowed
- exact relative `path` and `line`
- concise `title`, root-cause `description`, concrete `exploit_scenario`,
  specific `recommendation`, source excerpt/call-chain `evidence`, and
  relevant `threat_ids`

Do not call operator-controlled environment variables, authenticated
administrator actions, documented advisory metadata, or intentionally public
interfaces vulnerabilities unless you demonstrate the additional trust
boundary and attacker control that makes them exploitable.

Zero candidates is valid. Finally call `write_defense_scan_result` exactly
once as your last write with `status=complete`, `finding_count`, a `coverage` inventory of the
files/functions and trust-boundary paths actually inspected, and a short
`summary`. Do not supply `run_id`, `area_id`, `repository_path`, or
`expected_total`, `source_revision`, or `source_tree_state`; they are
runtime-filled from the frozen threat-model provenance. Never retry a
successful write.
