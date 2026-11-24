---- MODULE SubscriptionStreamEnd ----
\* lean: none
\* crates: gents-host, gents-store
\* models: SubscriptionStream.step, SubscriptionStream.renewalMargin
\* One subscriber's SSE subscription against a served Gents runtime, while
\* the node may revoke the subscriber's node read permission.
\*
\* Runtime (gents-host `shutdown_streams`): the pinned DefraDB checks node
\* read permission at the handshake only. The runtime ends the stream at its
\* bearer's expiry with `gents-end: expired`, and before delivering each event
\* it checks the subscriber still holds node read permission; at the first
\* that it does not, it delivers nothing and ends the stream with
\* `gents-end: revoked`.
\*
\* Subscriber (gents-store `subscribe`, the observer's renewal): each
\* handshake mints a fresh bearer (`Lifetime` ticks). The arm completes
\* within `Arm` ticks; `Half` ticks after it the subscriber renews with a new
\* handshake. An `expired` end is a renewal (Lean `an_expired_end_is_a_renewal`);
\* a `revoked` end, or a refused handshake, is a refusal, shown and not
\* renewed (Lean `a_revoked_end_is_a_refusal`).
\*
\* Time advances only when nothing is due, so a due renewal, cut or event
\* check happens before the clock passes it.
\* REDs: `Gate = FALSE` delivers events after revocation; `Cut = FALSE` with
\* `Renews = FALSE` (a caller that took the renewal cadence and kept the
\* stream) leaves a revoked subscriber's silent stream open past every bound.
\* With `Cut`, such a caller is still bounded by the bearer's expiry.
EXTENDS Naturals

CONSTANTS
  Lifetime,  \* bearer lifetime, in ticks
  Half,      \* renewal period after the arm (half the lifetime)
  Arm,       \* longest arm
  Horizon,   \* ticks checked
  Gate,      \* the runtime checks permission before each event
  Cut,       \* the runtime ends the stream at its bearer's expiry
  Renews     \* the subscriber renews on its own cadence

ASSUME /\ Lifetime \in Nat \ {0} /\ Half \in Nat \ {0} /\ Arm \in Nat
       /\ Horizon \in Nat /\ Gate \in BOOLEAN /\ Cut \in BOOLEAN /\ Renews \in BOOLEAN

VARIABLES
  now,       \* the clock
  member,    \* the subscriber holds node read permission
  revokedAt, \* when it lost it (Horizon + 1: not yet)
  phase,     \* "arming" | "open" | "refused"
  mintedAt,  \* when the current stream's bearer was minted
  armedAt,   \* when the current stream's arm completed
  leaked,    \* an event was delivered after revocation
  renewals   \* renewals so far (expired ends included)

vars == <<now, member, revokedAt, phase, mintedAt, armedAt, leaked, renewals>>

Init ==
  /\ now = 0 /\ member = TRUE /\ revokedAt = Horizon + 1
  /\ phase = "arming" /\ mintedAt = 0 /\ armedAt = 0
  /\ leaked = FALSE /\ renewals = 0

\* A handshake: refused without permission, else a stream whose bearer is
\* minted now and whose arm completes within `Arm`.
Handshake ==
  IF member
    THEN /\ phase' = "arming" /\ mintedAt' = now /\ UNCHANGED armedAt
    ELSE /\ phase' = "refused" /\ UNCHANGED <<mintedAt, armedAt>>

ArmDone ==
  /\ phase = "arming"
  /\ phase' = "open" /\ armedAt' = now
  /\ UNCHANGED <<now, member, revokedAt, mintedAt, leaked, renewals>>

Revoke ==
  /\ member /\ now <= Horizon
  /\ member' = FALSE /\ revokedAt' = now
  /\ UNCHANGED <<now, phase, mintedAt, armedAt, leaked, renewals>>

\* The node publishes an event the subscriber's query matches.
Event ==
  /\ phase = "open"
  /\ IF Gate /\ ~member
       THEN /\ phase' = "refused" /\ UNCHANGED leaked
       ELSE /\ leaked' = (leaked \/ ~member) /\ UNCHANGED phase
  /\ UNCHANGED <<now, member, revokedAt, mintedAt, armedAt, renewals>>

RenewDue == phase = "open" /\ Renews /\ now >= armedAt + Half
CutDue == phase \in {"arming", "open"} /\ Cut /\ now >= mintedAt + Lifetime

Renew ==
  /\ RenewDue
  /\ Handshake /\ renewals' = renewals + 1
  /\ UNCHANGED <<now, member, revokedAt, leaked>>

\* The runtime ends the stream at its bearer's expiry; the subscriber reads
\* it as a renewal and hands shakes again.
Expire ==
  /\ CutDue /\ ~RenewDue
  /\ Handshake /\ renewals' = renewals + 1
  /\ UNCHANGED <<now, member, revokedAt, leaked>>

ArmDue == phase = "arming" /\ now >= mintedAt + Arm

Tick ==
  /\ now < Horizon /\ ~RenewDue /\ ~CutDue /\ ~ArmDue
  /\ now' = now + 1
  /\ UNCHANGED <<member, revokedAt, phase, mintedAt, armedAt, leaked, renewals>>

Next ==
  \/ ArmDone
  \/ Revoke \/ Event \/ Renew \/ Expire \/ Tick

Spec == Init /\ [][Next]_vars

\* No event reaches a subscriber after it lost node read permission.
NoDeliveryAfterRevocation == ~leaked

\* A revoked subscriber is refused within one bearer lifetime plus an arm.
RefusedWithinALifetime ==
  now > revokedAt + Lifetime + Arm => phase = "refused"

\* A subscriber that keeps its permission is never refused.
MemberNeverRefused == member => phase # "refused"

====
