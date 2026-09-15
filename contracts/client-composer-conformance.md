# Client composer conformance

This follow-up sits above onboarding stack PR #1491. It does not change graph
execution, DefraDB dependencies, request lifecycle ownership, or document ACP.

## Matching terminal observation

`ClientShell.matching_terminal_snapshot_allows_follow_up` (in the existing
global ClientShell model) states a conditional guarantee: a matching observed
terminal request retires the local awaiting latch and admits a nonempty
follow-up when ordinary client/selection/behavior premises hold. A terminal
observation for a different request cannot acknowledge this submission.

Eight generated cases cover completed, failed, superseded and interrupted
requests, each with a matching and an unrelated request ID. The existing
frontend shell conformance consumer and native desktop snapshot consumer run
the generated cases. These exercise the actual projections, not a second
hand-maintained expected-state table.

This does not prove notification delivery, scheduler fairness, or a wall-clock
recovery bound. Issue #1493 remains unresolved until its original delayed
composer state is captured and explained; passing these cases does not close it.
