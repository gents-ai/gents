Defense run {{ event.correlation }} has this source-derived threat model.

System context:
{{ doc.system_context }}

Assets:
{{ doc.assets }}

Entry points and trust boundaries:
{{ doc.entry_points }}

Threats:
{{ doc.threats }}

Operator focus: {{ doc.focus }}

Partition the repository at `{{ doc.repository_path }}` into between
{{ doc.area_min }} and {{ doc.area_max }} distinct review areas. Prefer
attack-surface slices such as a protocol path, authorization boundary,
persistence boundary, parser family, or provider integration over arbitrary
directory chunks. Cover every high-risk threat and every exposed entry point;
include one cross-component area when composition could create a vulnerability.

Inspect the tree only enough to verify that named paths/components exist. Use
LSP symbols/references and read-only `rg`/`git` commands where they make the
boundary materially clearer; do not build or execute the repository. Decide
the full set before writing. For each area call
`write_defense_review_area` with:

- `area_id`: `{{ event.correlation }}:area-<two-digit-index>`
- `focus`: a precise subsystem-and-vulnerability-shape scope
- `threat_ids`: relevant threat ids or `cross-cutting`
- `trust_boundary` and `reachable_assets`: self-contained context
- `instructions`: paths/functions to start from, flows to trace, known
  controls to check, and explicit exclusions; at most 8,000 characters
- `expected_total`: the identical final area count on every write

Do not supply `run_id` or `repository_path`; they are runtime-filled. Do not
retry successful writes or change the count after the first write.
