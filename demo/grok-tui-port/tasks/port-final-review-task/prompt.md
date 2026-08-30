Grok TUI port run {{ event.correlation }} closed its integrate ledger:

<untrusted_integrate>
{{ group.docs }}
</untrusted_integrate>

Call `read_grok_port_job`, `read_port_integrate_result`, and
`read_port_surface`. Count every executable surface whose verdict is
`implement` or `shaped-stub`; set the legacy field `implement_surface_count`
to that exact non-ignore count. For a green report set
`live_result_count` to that count, or `1` when the count is zero. For every
non-green report set `live_result_count=1` so the blocked sentinel path closes.

If no integrate row has `status=applied`, do not review or modify source. Write
one report with `status=skipped`, zero rounds/findings, current HEAD, and the
counts above.

Otherwise fail closed unless every non-sentinel integrate row is `applied`.
The bundled graph is already installed in the fresh orchestration home. For
each full review round:

1. Require a clean tracked worktree and resolve current HEAD to an exact SHA.
2. Start the review package embedded by this pack and capture its run ID:
   `gents graph run code-review --repo . --base <job.base_sha> --head <head-sha> --home <job.orchestrator_home> --graphql <job.orchestrator_graphql> --output json`.
3. Watch that exact run to terminal state, then call `gents graph result` for
   the same run/home/GraphQL endpoint. Inspect persisted findings and the
   `CodeReviewTriageReport`; process exit alone is not evidence.
4. If confirmed findings exist after round one, freshly inspect each finding,
   make focused fixes, run affected tests plus `cargo fmt --all --check`, stage
   only the explicit fix paths, and create focused review-fix commits. Then run
   one fresh full review against the new exact HEAD.

At most two full rounds are allowed. Run `cargo test -p gents` and
`cargo check --workspace --all-targets` before declaring green. Never push or
open/merge a PR here.

Call `write_port_final_review_report` exactly once. `status=green` requires
zero confirmed findings, a clean tracked worktree, and successful gates.
Otherwise use `status=blocked`. Record the final exact `head_sha`, round and
finding counts, surface counts, and concise evidence. Do not supply `run_id`.
