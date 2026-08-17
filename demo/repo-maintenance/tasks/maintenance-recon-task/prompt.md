Maintenance job {{ event.correlation }} audits the repository at `{{ doc.repository_path }}` at `{{ doc.head_ref }}`.

All later worktrees use the operator-owned boundary `{{ doc.suggested_branch }}` at the absolute direct-child path `{{ doc.worktree_path }}` under `{{ doc.worktree_parent }}`.

Focus: {{ doc.focus }}

Create a closed set of parallel concern assignments. Do not investigate findings deeply and do not edit the repository.

1. Resolve `{{ doc.head_ref }}` to its exact commit SHA and record dirty-tree state, workspace shape, language/toolchain versions, and the largest source and test files. Treat size as routing evidence only.
2. Start `cargo check --workspace --all-targets` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` concurrently through background-capable Bash. Collect both results once; scanners must not report those diagnostics as discoveries.
3. Inspect up to {{ doc.history_depth }} first-parent commits plus available issue/PR metadata to identify prior cleanup waves, canonical owners, recent churn, and already-tracked maintenance. Read only enough source to route assignments.
4. Finalize the immutable area count, then call `write_maintenance_area` exactly once per area.

Area policy is `{{ doc.area_count }}`. If it is a positive integer, create exactly that many areas. If it is `auto`, choose the smallest adequate count from {{ doc.area_min }} through {{ doc.area_max }}. The first five areas are mandatory and must remain distinct:

- `dead-surface`: unreachable code, unused dependencies/assets/config, obsolete compatibility paths, and unwired scaffolding.
- `duplicate-ownership`: repeated helpers, pathways, tests, fixtures, generated artifacts, or capabilities with an existing canonical owner.
- `test-value`: hollow, tautological, false-green, stale, flaky, or exactly redundant tests and harness surface.
- `module-boundaries`: oversized or mixed-responsibility files whose cohesive units can be extracted without semantic change.
- `comment-contract-drift`: narration, stale implementation history, duplicated docs, or misleading comments, while preserving rationale and contracts.

Additional areas must be evidence-driven and non-overlapping, such as dependency/build hygiene, schema/config drift, or repository-specific generated surface. Partition by concern rather than directory.

Every area write must use:

- `area_id`: `{{ event.correlation }}:<category-slug>`
- `base_ref`: the exact resolved commit SHA for `{{ doc.head_ref }}`
- `category`: the concern slug
- `path`: a compact comma-separated routing list, or `repository-wide`
- `instructions`: category-specific proof gates, likely owners, history clues, and exclusions
- one shared `baseline` and `expected_total`

The runtime fills `repository_path`, `worktree_parent`, `worktree_path`, `suggested_branch`, and `pr_base` from the seed; do not supply them to `write_maintenance_area`.

Never retry a successful write or change cardinality after the first. Do not finish until the entire closed set is durable.
