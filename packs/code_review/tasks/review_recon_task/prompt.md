Host summary:
{{ doc.evidence_summary }}

Create these four assignments with `write_review_area`. Set `expected_total` to `4` and `baseline` to `{{ doc.base_ref }}..{{ doc.head_ref }}`. Repository and evidence fields are runtime-filled. Operator focus: {{ doc.focus }}

| `area_id` | `lens` | `path` | `instructions` |
| --- | --- | --- | --- |
| `{{ event.correlation }}:correctness` | `correctness` | `all changed paths` | `Review functional behavior, state transitions, compatibility, and tests. Operator focus: {{ doc.focus }}` |
| `{{ event.correlation }}:architecture-reuse` | `architecture-reuse` | `all changed paths` | `Review duplication, ownership boundaries, and reuse of existing abstractions.` |
| `{{ event.correlation }}:security-concurrency` | `security-concurrency` | `all changed paths` | `Review authorization, identity, filesystem safety, concurrency, and recovery.` |
| `{{ event.correlation }}:workflow-invariants` | `workflow-invariants` | `all changed paths` | `Review repository-specific workflow, evidence, workspace, and integration invariants.` |

On resumption, use the existing Goal and history to finish missing assignments. Once all four are persisted, call `update_goal` with `status="complete"`.
