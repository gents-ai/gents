# Client composer conformance

This follow-up sits above onboarding stack PR #1491. It does not change graph
execution, DefraDB dependencies, request lifecycle ownership, or document ACP.

## Matching terminal observation

`matching_terminal_snapshot_allows_follow_up` (in the existing ClientShell
model) states a conditional guarantee: a matching observed
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

## Presentation agreement

The composer may add its local empty-text blocker, but for nonempty text its
button, hint and submit action consume the canonical shell decision. It must
not reconstruct route, behavior or turn readiness from a second set of fields.
Stop remains a separate action: an active request may be interrupted even when
another message cannot be submitted. Behavior discovery does not grant authority.
An empty draft disables Send, not editing. The kit's `disabled` prop disables
the textarea too, so only the canonical nonempty-content blocker is passed to
that prop. Regression tests render the real kit composer rather than a mock
that silently ignores textarea disablement.

## Observation ordering

The ClientShell transition consumes an already-present matching observation
when a mutation acknowledgment arrives. Generated interrupted and streaming
cases fence both orderings: the terminal case allows a follow-up without an
extra notification; the streaming case continues to block another turn.

The async presentation model uses ephemeral intent generations. Navigation
increments the generation, including away-and-back navigation. A late result
may update presentation only while its captured generation is still current.
This never rolls back a successful request or introduces a durable session,
identity, sync-health or lifecycle owner. The generated 25-case guard matrix
and deferred-callback tests bind these laws to actual callback admission.

Proofs do not establish wall-clock delivery, UI frame scheduling, or ordering
across independent native process lifetimes. Store/reconcile versions are not
globally ordered identities and must not be compared across restarts as if they
were. Full-session-snapshot versus live-delta ordering remains a separate audit
obligation, not a guarantee established by these laws.

## Frontend ownership follow-up

The follow-up above #1495 applies the existing contracts at previously bypassed
frontend boundaries, with one commit per ownership area:

- Session selection changes through explicit navigation. Snapshot reconciliation
  cannot replace a fresh composer with the first existing session or clear a
  selected session whose row is temporarily missing (`snapshot_preserves_selection`).
- The behavior picker displays the same effective behavior selection as composer
  admission; it has no separate picked/default behavior state.
- The kit composer uses the shell's agent/session/behavior-keyed draft owner.
  Accepted sends clear only unchanged submitted text, not a subsequently edited draft.
- All send and retry entry points share synchronous submission admission, including
  before React renders busy state (`start_submit_gated`). The kit adapter has no
  separate submission latch or busy state.
- One lifecycle-owned publication generation orders configuration snapshots from
  refresh, startup, restart, config saves, and peer operations. Current publication
  resolves loading/startup presentation without waiting for obsolete fetches.
- Task/schedule panels call the shell actions. Late results cannot select a session
  after compose intent changes; accepted mutations remain accepted if observation
  fails. Overlapping task/schedule operations share an activity count.

The last two areas reuse `ClientObservationOrdering`'s current-generation acceptance
and stale-completion rejection laws. Deferred real-hook/action tests exercise crossed
completions, and rendered composer tests exercise canonical draft and selection
consumption. These are implementation conformance fixes, not new legal transitions;
no Lean or Rust semantics changed. They do not establish a cause or fix for #1493.

## Validation and limitations

- Full runtime package: 2,855 passed, five expected ignored.
- Workspace/all-target check passed; desktop-core 236 passed and bridge 157
  passed, two expected ignored.
- Generated projection consumers cover 26 frontend and 22 native snapshot
  cases; presentation and async-fence generators exercise their real adapters.
- Live paired-node GLM interruption/follow-up passed on 2026-09-15. Session
  `8f10bb89-d3fb-4da0-b031-53f0e26206f3` interrupted request
  `c81d306b-32e4-4453-a470-c6431e2a7e0c` with cause `interrupted`, then submitted
  follow-up `2ad0f0fd-8474-4a23-8eb6-857fe4577fa3` in that same session.
  The test uses the manual paired fixture and real GLM at workstation-1; it is
  not new native managed-pairing or account-login acceptance.
- The first live launch omitted its required provider argument (fixture refused
  startup). The next run exposed the empty textarea regression described above;
  it was corrected before publication and the real-component rerun passed.
- Graph execution was explicitly excluded. No graph issue is claimed fixed.
- No claim is made that one passing live run explains or eliminates #1493.
