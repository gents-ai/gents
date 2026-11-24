---- MODULE EvalBackoffCancel ----
\* lean: none
\* crates: gents-eval
(***************************************************************************)
(* An eval run's backoff before it retries a not-evidence attempt, a      *)
(* cancel marker written into the run directory, and the runner test       *)
(* `a_marker_written_during_the_backoff_ends_the_wait` that observes them. *)
(*                                                                          *)
(* Actors and their atomic steps, as the code has them:                     *)
(*  - The run loop (`execute_frozen`). Its pass ends once the not-evidence  *)
(*    attempt is recorded; while the trial runs, each tick of its watch    *)
(*    reads the marker (`cancel_requested`). It then reads the marker once *)
(*    more, announces the backoff (`tracing::info!` with a `backoff`       *)
(*    field) and waits on a `select!` over the backoff sleep and a watch   *)
(*    interval whose every tick refreshes the holder (`heartbeat`) and,    *)
(*    with `TickChecksMarker`, reads the marker. When the sleep wins the   *)
(*    next pass reads the marker before it plans (`TopChecksMarker`) and   *)
(*    otherwise launches the retry. The marker is a file: every read sees  *)
(*    whether it exists, so no write can fall between a read and a wait.   *)
(*  - The marker writer. With `Writer = "any"` it writes at any point of   *)
(*    the run; with `Writer = "test"` it is the test, which writes once it *)
(*    has seen the backoff announced and the holder refreshed after it.    *)
(*  - tracing-core's callsite registry (0.1.36), shared by every test in   *)
(*    the binary. The announcement's callsite caches one interest for the *)
(*    process; a cached `never` drops the event for every subscriber. The  *)
(*    first thread to reach it pushes it onto the registry, picks a        *)
(*    rebuilder (`Dispatchers::rebuilder`), computes and stores. While at   *)
(*    most one dispatcher is registered the rebuilder is `JustOne`: the    *)
(*    computing thread's own default decides, unlocked. With two or more   *)
(*    it is `Read`: every live dispatcher decides, under a read lock that  *)
(*    registering a dispatcher (a write lock) waits for. Registering a     *)
(*    dispatcher rebuilds every pushed callsite from all live ones.        *)
(*  - Another test's thread, with no scoped subscriber, whose run reaches  *)
(*    the same callsite first (a retry test's 1 ms backoff).               *)
(*  - The test's subscriber, registered (`set_default`) before the run     *)
(*    starts. `Global` is the binary's global default: `"none"`, one that  *)
(*    enables no target (`"silent"`), or one that enables the runner's     *)
(*    target (`"target"`, `enable_scoped_event_capture`).                  *)
(*  - The test's observer. It awaits the announcement, then the holder's   *)
(*    refresh, writes the marker, and awaits the run's end. With           *)
(*    `Window = 0` it fails only when what it watches has not changed for  *)
(*    `Ceiling` ticks (`STALL_CEILING`): the run's events while it awaits  *)
(*    the announcement and the end, the progress file while it awaits the  *)
(*    refresh. A heartbeat is no event, so a wait that only refreshes its  *)
(*    hold still stalls. With `Window > 0` it fails when the announcement  *)
(*    has not arrived by tick `Window` (a timeout).                        *)
(*                                                                          *)
(* The marker is level state, so the product's question is only whether   *)
(* every path from its write to a launch reads it; the configuration      *)
(* `MCEvalBackoffCancelAnyWrite` writes it at every point of the run and  *)
(* loses none. The announcement is an edge, and                           *)
(* `MCEvalBackoffCancelThreadDefault` is its lost wake-up: in the test's  *)
(* observer, not the product.                                               *)
(*                                                                          *)
(* Time passes in ticks; the host runs each runnable step within           *)
(* `MaxDelay` ticks (WallClockBound's premise) and lets time pass freely   *)
(* while nothing is runnable. `BackoffUnreached` is a backoff beyond the   *)
(* hang ceiling (`UNREACHED_DEADLINE`); otherwise it lasts `Backoff`       *)
(* ticks.                                                                   *)
(*                                                                          *)
(* `NoRetryAfterMarker` and `MarkerStopsRun` are the product's promise: no *)
(* retry launches once the marker exists, and the run stops. `TestPasses`  *)
(* and `ObserverSettles` are the test's: it reaches a verdict, and it      *)
(* fails only a run that launched a retry or whose wait the marker did not *)
(* end.                                                                     *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Writer,            \* "any" | "test"
  TickChecksMarker,  \* each backoff tick reads the marker
  TopChecksMarker,   \* the pass after an elapsed backoff reads the marker first
  Global,            \* "none" | "silent" | "target"
  BackoffUnreached,  \* the backoff outlasts the hang ceiling
  Backoff,           \* ticks of a reachable backoff
  MaxDelay,          \* ticks a runnable step may wait to be scheduled
  Ceiling,           \* ticks without a watched change before the observer fails
  Window             \* 0: the observer waits on changes; n: it judges at tick n

ASSUME /\ Writer \in {"any", "test"}
       /\ TickChecksMarker \in BOOLEAN
       /\ TopChecksMarker \in BOOLEAN
       /\ Global \in {"none", "silent", "target"}
       /\ BackoffUnreached \in BOOLEAN
       /\ Backoff \in Nat \ {0}
       /\ MaxDelay \in Nat \ {0}
       /\ Ceiling \in Nat \ {0}
       /\ Window \in Nat

VARIABLES
  now,        \* ticks since the test started (saturating at Cap)
  stall,      \* ticks since the last step
  quiet,      \* ticks since the last change the observer watches
  loop,       \* the run loop's position
  waitStart,  \* `now` when the backoff began
  endedBy,    \* "none" | "marker" | "expiry": what ended the backoff wait
  marker,     \* the cancel marker exists
  refreshed,  \* the holder was refreshed after the observer first read it
  retriedWithMarker, \* the retry launched while the marker existed
  scoped,     \* the test's subscriber is registered
  reg,        \* the callsite's registration state
  cached,     \* the callsite's cached interest: "never" | "some"
  other,      \* the other thread's registration of the callsite
  otherValue, \* the interest the other thread computed
  otherLock,  \* the other thread holds the dispatchers' read lock
  announced,  \* the test's subscriber received the announcement
  obs,        \* the observer's phase
  verdict     \* "running" | "pass" | "fail"

vars == <<now, stall, quiet, loop, waitStart, endedBy, marker, refreshed,
          retriedWithMarker, scoped, reg, cached, other, otherValue, otherLock,
          announced, obs, verdict>>

Cap == Ceiling + Window + Backoff + MaxDelay + 2

Terminal == {"stopped", "retried"}

LoopPositions == {"setup", "trial", "precheck", "announce", "waiting", "replan"} \cup Terminal

TypeOK ==
  /\ now \in 0..Cap
  /\ stall \in 0..(Ceiling + MaxDelay)
  /\ quiet \in 0..(Ceiling + MaxDelay)
  /\ loop \in LoopPositions
  /\ waitStart \in 0..Cap
  /\ endedBy \in {"none", "marker", "expiry"}
  /\ marker \in BOOLEAN
  /\ refreshed \in BOOLEAN
  /\ retriedWithMarker \in BOOLEAN
  /\ scoped \in BOOLEAN
  /\ reg \in {"unregistered", "registering", "registered"}
  /\ cached \in {"never", "some"}
  /\ other \in {"idle", "pushed", "computed", "done"}
  /\ otherValue \in {"never", "some"}
  /\ otherLock \in BOOLEAN
  /\ announced \in BOOLEAN
  /\ obs \in {"announce", "refresh", "mark", "end", "done"}
  /\ verdict \in {"running", "pass", "fail"}

Running == verdict = "running"

Dispatchers == (IF Global = "none" THEN 0 ELSE 1) + (IF scoped THEN 1 ELSE 0)

(* The interest a thread without a scoped subscriber computes: its own     *)
(* default under `JustOne`, every live dispatcher under `Read`.             *)
OtherInterest ==
  IF Dispatchers <= 1
  THEN IF Global = "target" THEN "some" ELSE "never"
  ELSE IF scoped \/ Global = "target" THEN "some" ELSE "never"

LoopRunnable == ~(loop \in Terminal) /\ (loop # "setup" \/ scoped)

WriterRunnable ==
  /\ ~marker
  /\ IF Writer = "any"
     THEN loop \in {"trial", "precheck", "announce", "waiting", "replan"}
     ELSE obs = "mark"

ObserverRunnable ==
  \/ obs = "announce" /\ announced
  \/ obs = "refresh" /\ refreshed
  \/ obs = "end" /\ loop \in Terminal

Runnable ==
  \/ LoopRunnable
  \/ ~scoped
  \/ other \in {"idle", "pushed", "computed"}
  \/ WriterRunnable
  \/ ObserverRunnable

Init ==
  /\ now = 0
  /\ stall = 0
  /\ quiet = 0
  /\ loop = "setup"
  /\ waitStart = 0
  /\ endedBy = "none"
  /\ marker = FALSE
  /\ refreshed = FALSE
  /\ retriedWithMarker = FALSE
  /\ scoped = FALSE
  /\ reg = "unregistered"
  /\ cached = "never"
  /\ other = "idle"
  /\ otherValue = "never"
  /\ otherLock = FALSE
  /\ announced = FALSE
  /\ obs = "announce"
  /\ verdict = "running"

Stepped == stall' = 0

(* `set_default`: register under the write lock, then rebuild every pushed *)
(* callsite from all live dispatchers, the test's own among them.          *)
RegisterScoped ==
  /\ ~scoped /\ ~otherLock
  /\ scoped' = TRUE
  /\ cached' = IF other = "idle" /\ reg = "unregistered" THEN cached ELSE "some"
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, reg, other, otherValue, otherLock,
                 announced, obs, verdict>>

OtherPush ==
  /\ other = "idle" /\ reg = "unregistered"
  /\ other' = "pushed"
  /\ reg' = "registering"
  /\ Stepped
  /\ UNCHANGED <<now, quiet, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, cached, otherValue, otherLock,
                 announced, obs, verdict>>

OtherCompute ==
  /\ other = "pushed"
  /\ other' = "computed"
  /\ otherValue' = OtherInterest
  /\ otherLock' = (Dispatchers > 1)
  /\ Stepped
  /\ UNCHANGED <<now, quiet, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, announced, obs,
                 verdict>>

OtherStore ==
  /\ other = "computed"
  /\ other' = "done"
  /\ cached' = otherValue
  /\ reg' = "registered"
  /\ otherLock' = FALSE
  /\ Stepped
  /\ UNCHANGED <<now, quiet, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, otherValue, announced, obs,
                 verdict>>

Start ==
  /\ loop = "setup" /\ scoped
  /\ loop' = "trial"
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, obs, verdict>>

TrialTick ==
  /\ loop = "trial" /\ marker
  /\ loop' = "stopped"
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, obs, verdict>>

Recorded ==
  /\ loop = "trial"
  /\ loop' = "precheck"
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, obs, verdict>>

Precheck ==
  /\ loop = "precheck"
  /\ loop' = IF marker THEN "stopped" ELSE "announce"
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, obs, verdict>>

(* The announcement on the run loop's thread, whose default is the test's  *)
(* subscriber: it registers an unregistered callsite itself, a callsite   *)
(* another thread is registering answers `sometimes`, and a registered    *)
(* one answers its cached interest.                                        *)
Announce ==
  /\ loop = "announce"
  /\ loop' = "waiting"
  /\ waitStart' = now
  /\ reg' = IF reg = "unregistered" THEN "registered" ELSE reg
  /\ cached' = IF reg = "unregistered" THEN "some" ELSE cached
  /\ announced' = (reg # "registered" \/ cached = "some")
  /\ quiet' = IF announced' THEN 0 ELSE quiet
  /\ Stepped
  /\ UNCHANGED <<now, endedBy, marker, refreshed, retriedWithMarker,
                 scoped, other, otherValue, otherLock, obs, verdict>>

WaitTick ==
  /\ loop = "waiting"
  /\ refreshed' = TRUE
  /\ IF TickChecksMarker /\ marker
     THEN loop' = "stopped" /\ endedBy' = "marker"
     ELSE UNCHANGED <<loop, endedBy>>
  /\ quiet' = IF loop' = "stopped" \/ obs = "refresh" THEN 0 ELSE quiet
  /\ Stepped
  /\ UNCHANGED <<now, waitStart, marker, retriedWithMarker, scoped,
                 reg, cached, other, otherValue, otherLock, announced, obs,
                 verdict>>

Expire ==
  /\ loop = "waiting" /\ ~BackoffUnreached /\ now >= waitStart + Backoff
  /\ loop' = "replan"
  /\ endedBy' = "expiry"
  /\ Stepped
  /\ UNCHANGED <<now, quiet, waitStart, marker, refreshed, retriedWithMarker,
                 scoped, reg, cached, other, otherValue, otherLock, announced,
                 obs, verdict>>

Replan ==
  /\ loop = "replan"
  /\ IF TopChecksMarker /\ marker
     THEN loop' = "stopped" /\ UNCHANGED retriedWithMarker
     ELSE loop' = "retried" /\ retriedWithMarker' = marker
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, waitStart, endedBy, marker, refreshed, scoped, reg,
                 cached, other, otherValue, otherLock, announced, obs,
                 verdict>>

Write ==
  /\ WriterRunnable
  /\ marker' = TRUE
  /\ obs' = IF Writer = "test" THEN "end" ELSE obs
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, loop, waitStart, endedBy, refreshed, retriedWithMarker,
                 scoped, reg, cached, other, otherValue, otherLock, announced,
                 verdict>>

SeeAnnounced ==
  /\ Running /\ obs = "announce" /\ announced
  /\ obs' = "refresh"
  /\ refreshed' = FALSE
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, loop, waitStart, endedBy, marker,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, verdict>>

SeeRefreshed ==
  /\ Running /\ obs = "refresh" /\ refreshed
  /\ obs' = IF Writer = "test" THEN "mark" ELSE "end"
  /\ quiet' = 0
  /\ Stepped
  /\ UNCHANGED <<now, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, verdict>>

(* The run returned: the test asserts the marker ended the wait and no     *)
(* retry launched.                                                          *)
SeeEnd ==
  /\ Running /\ obs = "end" /\ loop \in Terminal
  /\ obs' = "done"
  /\ verdict' = IF loop = "stopped" /\ endedBy = "marker" THEN "pass" ELSE "fail"
  /\ Stepped
  /\ UNCHANGED <<now, quiet, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced>>

Stalled ==
  /\ Running /\ Window = 0 /\ quiet >= Ceiling
  /\ ~(obs = "announce" /\ announced)
  /\ ~(obs = "refresh" /\ refreshed)
  /\ ~(obs = "end" /\ loop \in Terminal)
  /\ obs # "mark"
  /\ verdict' = "fail"
  /\ UNCHANGED <<now, stall, quiet, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, obs>>

TimedOut ==
  /\ Running /\ Window > 0 /\ obs = "announce" /\ ~announced /\ now >= Window
  /\ verdict' = "fail"
  /\ UNCHANGED <<now, stall, quiet, loop, waitStart, endedBy, marker, refreshed,
                 retriedWithMarker, scoped, reg, cached, other, otherValue,
                 otherLock, announced, obs>>

Tick ==
  /\ IF Runnable THEN stall < MaxDelay ELSE stall < Ceiling
  /\ now' = IF now < Cap THEN now + 1 ELSE now
  /\ stall' = stall + 1
  /\ quiet' = IF quiet < Ceiling + MaxDelay THEN quiet + 1 ELSE quiet
  /\ UNCHANGED <<loop, waitStart, endedBy, marker, refreshed, retriedWithMarker,
                 scoped, reg, cached, other, otherValue, otherLock, announced,
                 obs, verdict>>

Next ==
  \/ RegisterScoped \/ OtherPush \/ OtherCompute \/ OtherStore
  \/ Start \/ TrialTick \/ Recorded \/ Precheck \/ Announce
  \/ WaitTick \/ Expire \/ Replan
  \/ Write \/ SeeAnnounced \/ SeeRefreshed \/ SeeEnd \/ Stalled \/ TimedOut
  \/ Tick

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(RegisterScoped) /\ WF_vars(OtherCompute) /\ WF_vars(OtherStore)
  /\ WF_vars(Start) /\ WF_vars(Recorded) /\ WF_vars(Precheck)
  /\ WF_vars(Announce) /\ WF_vars(WaitTick) /\ WF_vars(Expire) /\ WF_vars(Replan)
  /\ WF_vars(TrialTick)
  /\ WF_vars(Write) /\ WF_vars(SeeAnnounced) /\ WF_vars(SeeRefreshed)
  /\ WF_vars(SeeEnd) /\ WF_vars(Stalled) /\ WF_vars(TimedOut) /\ WF_vars(Tick)

NoRetryAfterMarker == ~retriedWithMarker

MarkerStopsRun == [](marker => <>(loop = "stopped"))

TestPasses == verdict # "fail"

ObserverSettles == <>(verdict # "running")

====
