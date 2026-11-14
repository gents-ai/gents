---- MODULE NativeCompletionHandshake ----
(***************************************************************************)
(* The scheduling handshake a test driver runs against the embedded         *)
(* transaction owner's caller-timeout step (Lean `NativeCompletion`         *)
(* `callerTimedOut`). Lean fixes the sequential order of the events; it     *)
(* cannot say which party may act when, and that interleaving is what a     *)
(* driver gets wrong.                                                       *)
(*                                                                          *)
(* Four parties on one current-thread runtime. The caller                   *)
(* (`execute_owned_native`) takes a native permit, spawns the native task   *)
(* and arms its storage-step deadline in one uninterrupted step: there is   *)
(* no await between the spawn and `tokio::time::timeout`, so the deadline   *)
(* is computed from the clock value the caller sees when it is first        *)
(* polled. The native operation then reports that it entered and parks.     *)
(* The driver pauses the clock and advances it to expire that deadline. The *)
(* caller's expiry abandons the tracker (`poisoned`), and a telemetry       *)
(* record of that abandonment may follow. The driver waits for one of the   *)
(* two signals.                                                             *)
(*                                                                          *)
(* The model has no real-time action at all: every step is the runtime's    *)
(* own clock and scheduler. A behavior in which the driver never observes   *)
(* the timeout is a behavior in which the driver can only get there by      *)
(* waiting on wall-clock time, which is what makes a test load-sensitive.   *)
(*                                                                          *)
(* `RequireArmed = FALSE` is a driver that advances the clock before the    *)
(* caller has armed its deadline: the advance then lands under the          *)
(* deadline the caller later computes. `ObserveTelemetry = TRUE` is a       *)
(* driver that waits on the `pending_native` write-attempt event instead of *)
(* the tracker's own state; that event is a projection the owner may        *)
(* suppress once the attempt has recorded a terminal outcome.               *)
(* `PermitSemantics = FALSE` is a signal that only wakes a driver already   *)
(* waiting, the lost wakeup that the tracker's enable-then-check wait       *)
(* avoids.                                                                  *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Timeout,             \* storage-step timeout, in clock units
  MaxAdvance,          \* clock advances the driver's paused window may spend
  RequireArmed,        \* driver advances only after the native entry it waited for
  ObserveTelemetry,    \* RED: driver waits on the telemetry projection
  PermitSemantics,     \* signal stores a permit when no waiter is registered
  TelemetrySuppressible \* the attempt may already hold a terminal outcome

NoDeadline == (MaxAdvance + 2) * Timeout

VARIABLES
  clock,          \* the runtime's virtual clock
  paused,         \* driver holds the clock paused
  advances,       \* clock advances spent
  caller,         \* "idle" (spawned, never polled) | "running" | "timedOut"
  deadline,       \* clock value at which the caller's storage step expires
  nativeSpawned,
  nativeEntered,  \* native task reported entry and parked on its release
  sawEntered,     \* driver observed that entry
  poisoned,       \* tracker abandoned by the caller guard
  recorded,       \* telemetry emitted the pending-native outcome
  suppressed,     \* telemetry record skipped: terminal outcome already held
  driverWaiting,
  wokeDriver,     \* ghost: the awaited signal fired with a waiter registered
  observed,       \* driver completed its observation of the timeout
  advancedUnarmed \* ghost: the driver advanced before the caller armed

vars == <<clock, paused, advances, caller, deadline, nativeSpawned,
          nativeEntered, sawEntered, poisoned, recorded, suppressed,
          driverWaiting, wokeDriver, observed, advancedUnarmed>>

Armed == caller # "idle"

TypeOK ==
  /\ clock \in 0..(MaxAdvance * Timeout)
  /\ paused \in BOOLEAN
  /\ advances \in 0..MaxAdvance
  /\ caller \in {"idle", "running", "timedOut"}
  /\ deadline \in 0..NoDeadline
  /\ nativeSpawned \in BOOLEAN /\ nativeEntered \in BOOLEAN
  /\ sawEntered \in BOOLEAN /\ poisoned \in BOOLEAN
  /\ recorded \in BOOLEAN /\ suppressed \in BOOLEAN
  /\ driverWaiting \in BOOLEAN /\ wokeDriver \in BOOLEAN
  /\ observed \in BOOLEAN /\ advancedUnarmed \in BOOLEAN

Init ==
  /\ clock = 0 /\ paused = FALSE /\ advances = 0
  /\ caller = "idle" /\ deadline = NoDeadline
  /\ nativeSpawned = FALSE /\ nativeEntered = FALSE /\ sawEntered = FALSE
  /\ poisoned = FALSE /\ recorded = FALSE /\ suppressed = FALSE
  /\ driverWaiting = FALSE /\ wokeDriver = FALSE
  /\ observed = FALSE /\ advancedUnarmed = FALSE

(* The caller task. Its first poll takes the permit, spawns the native task
   and arms the deadline with no await in between. *)
CallerArm ==
  /\ caller = "idle"
  /\ caller' = "running"
  /\ deadline' = clock + Timeout
  /\ nativeSpawned' = TRUE
  /\ UNCHANGED <<clock, paused, advances, nativeEntered, sawEntered, poisoned,
                 recorded, suppressed, driverWaiting, wokeDriver, observed,
                 advancedUnarmed>>

(* The native operation reports entry and parks until released; the release
   never arrives within this step of the handshake. *)
NativeEnter ==
  /\ nativeSpawned /\ ~nativeEntered
  /\ nativeEntered' = TRUE
  /\ UNCHANGED <<clock, paused, advances, caller, deadline, nativeSpawned,
                 sawEntered, poisoned, recorded, suppressed, driverWaiting,
                 wokeDriver, observed, advancedUnarmed>>

(* `Notify::notify_one` stores a permit, so the driver observes an entry that
   fired before it waited. *)
DriverSeeEntered ==
  /\ nativeEntered /\ ~sawEntered
  /\ sawEntered' = TRUE
  /\ UNCHANGED <<clock, paused, advances, caller, deadline, nativeSpawned,
                 nativeEntered, poisoned, recorded, suppressed, driverWaiting,
                 wokeDriver, observed, advancedUnarmed>>

DriverPause ==
  /\ ~paused /\ paused' = TRUE
  /\ UNCHANGED <<clock, advances, caller, deadline, nativeSpawned, nativeEntered,
                 sawEntered, poisoned, recorded, suppressed, driverWaiting,
                 wokeDriver, observed, advancedUnarmed>>

DriverAdvance ==
  /\ paused /\ advances < MaxAdvance
  /\ RequireArmed => sawEntered
  /\ clock' = clock + Timeout
  /\ advances' = advances + 1
  /\ advancedUnarmed' = (advancedUnarmed \/ ~Armed)
  /\ UNCHANGED <<paused, caller, deadline, nativeSpawned, nativeEntered,
                 sawEntered, poisoned, recorded, suppressed, driverWaiting,
                 wokeDriver, observed>>

(* The expiry, the abandonment guard's drop and the poisoning are one
   straight-line continuation on the caller's own task. *)
CallerExpire ==
  /\ caller = "running" /\ clock >= deadline
  /\ caller' = "timedOut" /\ poisoned' = TRUE
  /\ wokeDriver' = (wokeDriver \/ (driverWaiting /\ ~ObserveTelemetry))
  /\ UNCHANGED <<clock, paused, advances, deadline, nativeSpawned, nativeEntered,
                 sawEntered, recorded, suppressed, driverWaiting, observed,
                 advancedUnarmed>>

Record ==
  /\ poisoned /\ ~recorded /\ ~suppressed
  /\ recorded' = TRUE
  /\ wokeDriver' = (wokeDriver \/ (driverWaiting /\ ObserveTelemetry))
  /\ UNCHANGED <<clock, paused, advances, caller, deadline, nativeSpawned,
                 nativeEntered, sawEntered, poisoned, suppressed, driverWaiting,
                 observed, advancedUnarmed>>

SuppressRecord ==
  /\ TelemetrySuppressible /\ ~recorded /\ ~suppressed
  /\ suppressed' = TRUE
  /\ UNCHANGED <<clock, paused, advances, caller, deadline, nativeSpawned,
                 nativeEntered, sawEntered, poisoned, recorded, driverWaiting,
                 wokeDriver, observed, advancedUnarmed>>

DriverWait ==
  /\ ~driverWaiting /\ ~observed
  /\ driverWaiting' = TRUE
  /\ UNCHANGED <<clock, paused, advances, caller, deadline, nativeSpawned,
                 nativeEntered, sawEntered, poisoned, recorded, suppressed,
                 wokeDriver, observed, advancedUnarmed>>

SignalFired == IF ObserveTelemetry THEN recorded ELSE poisoned

DriverObserve ==
  /\ driverWaiting /\ ~observed
  /\ IF PermitSemantics THEN SignalFired ELSE wokeDriver
  /\ observed' = TRUE
  /\ UNCHANGED <<clock, paused, advances, caller, deadline, nativeSpawned,
                 nativeEntered, sawEntered, poisoned, recorded, suppressed,
                 driverWaiting, wokeDriver, advancedUnarmed>>

Next ==
  \/ CallerArm \/ NativeEnter \/ CallerExpire \/ Record \/ SuppressRecord
  \/ DriverSeeEntered \/ DriverPause \/ DriverAdvance \/ DriverWait \/ DriverObserve

Fairness ==
  /\ WF_vars(CallerArm) /\ WF_vars(NativeEnter) /\ WF_vars(CallerExpire)
  /\ WF_vars(Record)
  /\ WF_vars(DriverSeeEntered) /\ WF_vars(DriverPause) /\ WF_vars(DriverAdvance)
  /\ WF_vars(DriverWait) /\ WF_vars(DriverObserve)

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* The driver advances the clock only once the caller has armed the deadline
\* that advance is meant to expire.
AdvanceFollowsArm == ~advancedUnarmed

\* The driver never reports the timeout before the owner has abandoned the
\* tracker.
ObservationImpliesAbandonment == observed => poisoned

\* The model contains no real-time step, so the driver's observation of the
\* caller timeout is reachable on the runtime's own clock and scheduler
\* alone. A violation is a driver that can only finish by waiting on wall
\* time.
ObservableWithoutRealTime == <>observed

====
