# Client composer conformance

This contract does not change graph execution, DefraDB dependencies, request
lifecycle ownership, or document ACP.

## Matching terminal observation

`matching_terminal_snapshot_allows_follow_up` (in the existing ClientShell
model) states a conditional guarantee: a matching observed
terminal request retires the local awaiting latch and admits a nonempty
follow-up when ordinary client/selection/behavior premises hold. A terminal
observation for a different request cannot acknowledge this submission.

Generated cases cover completed, failed, superseded and interrupted requests,
each with a matching and an unrelated request ID. The existing
frontend shell conformance consumer and native desktop snapshot consumer run
the generated cases. These exercise the actual projections, not a second
hand-maintained expected-state table.

This does not prove notification delivery, scheduler fairness, or a wall-clock
recovery bound.

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
identity, sync-health or lifecycle owner. The generated guard matrix and
deferred-callback tests bind these laws to actual callback admission.

Proofs do not establish wall-clock delivery, UI frame scheduling, or ordering
across independent native process lifetimes. Store/reconcile versions are not
globally ordered identities and must not be compared across restarts as if they
were. Full-session-snapshot versus live-delta ordering remains a separate audit
obligation, not a guarantee established by these laws.

## Frontend ownership

The frontend applies these contracts at every composer boundary:

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
- One lifecycle-owned publication generation orders snapshot reads. Startup,
  restart, config saves, and peer mutations request a fresh read after success;
  their returned payloads do not compete with reads issued during the mutation.
  Failed mutations leave pending reads eligible to publish.
- Task/schedule panels call the shell actions without selecting the resulting
  session. Accepted mutations remain accepted if observation fails. Overlapping
  task/schedule operations share an activity count.

`ClientSnapshotObservation` extends the existing observation laws to successful
mutation refreshes and failed writes. `ClientDraftOwnership` specifies origin-scoped
cleanup without erasing edits or other drafts. Deferred real-hook/action tests
exercise crossed completions; rendered composer tests exercise draft and selection
consumption. Neither extension adds a durable runtime lifecycle.

A later running-client observation recovers a transient client startup error;
it cannot clear a managed-server authority error. The startup projection is
checked against generated cases from the same observation model.

## CI handoff

Lean-backed generated consumers run in the proof job, where Lake and the proof
cache are available. Browser-independent widget tests run in the desktop UI job
without requiring Lean. Passing either layer does not establish wall-clock
delivery or process-lifetime ordering beyond the invariants above.
