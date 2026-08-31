Run {{ event.correlation }} accepted sealed workspace
`{{ doc.workspace_id }}` for work unit `{{ doc.work_unit_id }}`
(implementation `{{ doc.implementation_id }}`).

This request is Integrate-bound. Do not git commit, git add, or mutate trunk.
Inspect the sealed tree if needed, then finish with a short textual
acknowledgement. Do not write an integration result: the host applies the
sealed diff only after this request succeeds, and a separate receipt-triggered
stage records `applied` after that host action has durably succeeded.
