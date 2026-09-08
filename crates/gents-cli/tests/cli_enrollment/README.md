# App pairing acceptance contract

Run `CARGO_INCREMENTAL=0 cargo test -p gents-cli --test cli_enrollment -- --nocapture`.

The runtime starts through `gents init` and `gents server`. The app starts
through the same `ClientCore` used by desktop and iOS. Enrollment consumes the
runtime's `/status` offer, creates a pending request, and uses operator approval.
The test must not install replicators or pairing documents itself.

Success requires:

- Approved enrollment and replicated behavior/readiness within 30 seconds.
- A completed request, response, and assistant body present in the phone database
  within 20 seconds of submitting a turn, including two seconds of deterministic
  model latency. User text, tool arguments, and reasoning do not count as replies.
- The fake provider withholds the offline reply until the app has shut down.
  After the app closes with a request accepted by the runtime, reopening the
  same home recovers the completed reply within 20 seconds, including startup.
- A subsequent turn in the same session includes both distinct earlier replies
  in the provider transcript and completes visibly within the same turn budget.
- Enrollment identity survives reopening; `AgentPrincipal` stays on the runtime.

These are initial integration-test latency budgets. The independent live-model
test exercises inference without substituting a fake provider. The deterministic
test substitutes only inference; storage, signatures, approval, transport,
replication, and observation remain real.

The live-model test exercises the app's local refresh/display path as well.
The deterministic contract is the stricter data-plane gate: it waits on local
database rows without requesting recovery or refreshing the UI.

The device acceptance run must additionally exercise actual iOS foreground and
background transitions, existing production store history, and the rendered
conversation. A passing Rust client-core test does not prove that device run.
Capture submit, runtime completion, and client visibility times separately.

The aged fixture recreates 2,500 old heartbeat updates of both `snapshot_json`
and `updated_at`, including the unchanged snapshot field. This crosses the
first CAR response's block limit and exercises selective continuation. New
runtimes must not create this history: generated publisher tests advance hours
of idle time and require no additional readiness writes.
Enrollment must observe the final seeded readiness timestamp or a later runtime
update, together with a usable semantic snapshot; an arbitrary historical row
does not satisfy the aged gate. Fixture construction is outside the UX budget.

Reconnect recovery must work without app-issued collection-wide replay. The
database's durable sender markers and receiver DAG state own that recovery;
the app must not add a second recovery counter or infer transport liveness
from a readiness timestamp.

## Separate replication from display

Session selection is data-plane intent: sessions interacted with on a phone
should remain synchronized into that phone's database, including after a
reconnect. An untouched session should not cause eager transcript replication
merely because its lightweight conversation index is visible.

Scrolling is a local database operation: read bounded transcript pages from
the phone's existing replica. It must not create another pairing, reset the
session's replication progress, or request a full network replay. Data-plane
convergence and local page ordering/completeness need separate assertions.

The contract requests session selection alongside conversation delivery; the
hydration owner may defer admission until the active request finishes. Reply
delivery is observed through local queries, not conditioned on hydration being
its cause. It does not refresh the UI or request recovery while waiting for the
reply. After reconnecting and completing three turns, it waits for hydration
completion, reads one-message local pages, and checks ordering, completeness,
and unchanged pairing/hydration fields (including served progress, not just IDs).
Untouched-session exclusion still needs
its own multi-session scenario; these checks do not establish that policy.

Pairing retains its existing owners: enrollment controls runtime authorization
and its outbound route; the app route manager controls the app's outbound route;
the database owns transport and DAG recovery; the sync projection owns product
status. Row counts and transport connectivity alone do not establish success.
