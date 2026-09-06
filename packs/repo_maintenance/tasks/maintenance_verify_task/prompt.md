Maintenance run {{ group.correlation_value }} has {{ group.count }} completed area scans (complete={{ group.complete }}):

{{ group.docs }}

Call `defra_query` for `MaintenanceCandidate` in this run. For each candidate, freshly read the exact artifact, its canonical owner, usages, history, tests, cfg/features, and open issue context. Try to refute it before promoting it.

Confirmation requires all of the following:

1. The maintenance cost is concrete and current, not a count or style heuristic.
2. Reachability and ownership evidence covers generated, feature-gated, public, serialization, GraphQL, FFI, reflection, compatibility, proof, example, and build surfaces as relevant.
3. The proposed action preserves behavior, interfaces, formal/conformance contracts, observability, and operator expectations.
4. The validation plan would detect the plausible regression and includes `cargo test -p gents` or `cargo check --workspace --all-targets` when their construction boundaries are touched.
5. The work is cleanup rather than a semantic feature or architecture redesign.
6. The scope estimate and existing issue linkage are honest.

Call `write_maintenance_verdict` exactly once per candidate, preserving the candidate's non-authority content fields, setting verdict to exactly `confirmed` or `refuted`, reassessing confidence, replacing evidence with fresh verification evidence, and explaining the decision in `verification`. The runtime fills `repository_path`, `worktree_parent`, `worktree_path`, `suggested_branch`, `base_ref`, and `pr_base` from the completed scan group for every verdict and the summary; do not supply or reinterpret them. Confidence below 80 is refuted. Never silently drop a candidate.

Finally call `write_maintenance_verification_summary` exactly once with balanced candidate, confirmed, and refuted counts, including the zero-candidate case. Do not supply runtime-filled `run_id`.
