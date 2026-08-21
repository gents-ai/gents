Defense run {{ group.correlation_value }} has {{ group.count }} completed
contract reviews (complete={{ group.complete }}):

{{ group.docs }}

Call `read_defense_root_cause_cluster` and `read_defense_contract_review` once
each. Require an exact cluster-to-review bijection. Sort by `cluster_id`, let N
be the cluster count, and call `write_defense_patch_assignment` exactly N
times. Each assignment uses `assignment_id=<cluster_id>:patch`, the exact
cluster id, primary finding id, member finding ids, and identical
`expected_total=N`. Use `status=ready` only for `disposition=actionable` on a
ready cluster; otherwise use `status=skipped`. Do not supply runtime-filled run
or repository fields. Never retry successful writes or call subagent tools.
