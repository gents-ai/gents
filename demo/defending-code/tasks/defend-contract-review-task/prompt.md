Review cluster `{{ doc.cluster_id }}` in `{{ doc.repository_path }}`.
Status: {{ doc.status }}
Primary finding: {{ doc.primary_finding_id }}
Members: {{ doc.member_finding_ids }}
Root cause: {{ doc.canonical_root_cause }}
Boundary: {{ doc.security_boundary }}
Proposed scope: {{ doc.remediation_scope }}

If status is `skipped`, write one `DefenseContractReview` with
`review_id={{ doc.cluster_id }}:contract`, `status=skipped`,
`disposition=no_findings`, `none` for narrative fields, and stop.

Otherwise call `read_defending_finding` once for this cluster's exact
`primary_finding_id`. Inspect repository-level and nearest `AGENTS.md` or
equivalent instructions, public documentation, relevant tests, history, and
formal specifications. Determine whether the proposed remediation would break
intentional behavior and identify the smallest architecturally valid fix
boundary. Do not overturn the verifier's security verdict; record conflicting
contract evidence for the report and human reviewer.

Call `write_defense_contract_review` exactly once with
`review_id={{ doc.cluster_id }}:contract`, `status=complete`, disposition
exactly `actionable`, `contract_conflict`, or `not_actionable`; explicit
`behavior_intentional`, `spec_impact`, `required_foundation_flow`, proof files,
compatibility constraints, recommended fix boundary, and concrete evidence.
Do not supply runtime-filled run, cluster, repository, or expected-total fields.
