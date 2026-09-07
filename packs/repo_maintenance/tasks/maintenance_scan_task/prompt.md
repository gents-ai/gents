Maintenance run {{ event.correlation }}, category `{{ doc.category }}` (`{{ doc.area_id }}`). Routing paths: `{{ doc.path }}`.

Instructions: {{ doc.instructions }}

Deterministic baseline: {{ doc.baseline }}

Audit the whole relevant ownership surface, not merely the routed files. Read-only Git and GitHub history, Cargo metadata, targeted tests, and LSP navigation are allowed. Do not edit files or rerun the full workspace baseline.

Apply the category gate:

- `dead-surface`: prove absence of live callers and data-driven reachability across source, tests, examples, features, build scripts, schemas, generated code, public APIs, serialization, GraphQL, FFI, reflection, and compatibility contracts.
- `duplicate-ownership`: identify both implementations, name the canonical owner, compare semantics and coverage, and specify the exact deletion or reuse seam.
- `test-value`: show the false-green, tautology, obsolete contract, identical duplicate coverage, or unreliable synchronization. Preserve intentionally redundant boundary/conformance tests.
- `module-boundaries`: line count is only a signal. Name cohesive units and an extraction that preserves public paths, visibility, test names, behavior, and formal/conformance ownership.
- `comment-contract-drift`: quote exact stale or narrative text and its canonical replacement or deletion. Preserve rationale, invariants, safety arguments, operator guidance, formal design, and non-obvious constraints.
- any added category: follow its supplied proof and exclusion rules.

Check open issues and recent merged work before emitting a candidate. Link matching work in `existing_issue`; do not invent duplicate work.

Emit at most three candidates with `write_maintenance_candidate`. Each must include:

- globally unique `finding_id` as `{{ doc.area_id }}:<finding-slug>`
- exact `category`; `priority` as `high`, `medium`, or `low`; confidence from `80` to `100`
- exact `path`, optional `line`, title, detail, and quoted evidence
- `preservation`: behavior, interface, proof, and operator obligations that cannot change
- `validation`: exact proportional commands or inspections
- `estimated_scope`: `small` (1-3 files), `medium` (4-8 files), or `large` (9+ files)
- `existing_issue`: issue/PR reference or an empty string

The runtime fills `repository_path`, `worktree_parent`, `worktree_path`, `suggested_branch`, `base_ref`, and `pr_base` from the area trigger for both candidate and scan-result writes. Do not supply or reinterpret those authority-bearing fields.

Reject subjective style, naming taste, line-count-only claims, broad redesigns, and any cleanup whose semantic safety cannot be stated. Finally call `write_maintenance_scan_result` exactly once, even with zero candidates. Do not supply runtime-filled `run_id` or `expected_total`.
