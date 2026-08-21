Defense run {{ event.correlation }} closed triage with
confirmed={{ doc.confirmed_count }}, refuted={{ doc.refuted_count }}, and
promoted={{ doc.promoted_count }}.

Call `read_defending_finding` once. Require its row count to equal
`doc.promoted_count`. Partition every row into exactly one root-cause cluster.
Sort clusters by their lexicographically smallest member finding id. For N
clusters, call `write_defense_root_cause_cluster` N times with:

- `cluster_id={{ event.correlation }}:cluster-<two-digit-index>`
- `base_revision`: the one exact `source_revision` shared by every member;
  never mix revisions or tree states in one cluster—split them if necessary
- `base_tree_state`: the one frozen source-tree state shared by every member
- `status=ready`, one `primary_finding_id`, all `member_finding_ids`, and any
  consequence-only ids in `consequence_finding_ids`; both lists are sorted,
  comma-delimited strings, or `none` when the consequence subset is empty
- a canonical title/root cause, claim kind, maximum supported severity,
  security boundary, affected paths, and precise remediation scope
- the identical `expected_total=N`

If there are no confirmed findings, write one sentinel with
`cluster_id={{ event.correlation }}:no-findings`, `status=skipped`, `none` for
`base_revision`, `base_tree_state`, and all finding and narrative fields, and
`expected_total=1`. Do not supply
runtime-filled run or repository fields. Never retry a successful write.
