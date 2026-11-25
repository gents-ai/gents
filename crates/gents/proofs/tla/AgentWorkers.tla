---- MODULE AgentWorkers ----
\* lean: none
\* crates: gents, gents-automation, gents-inference, gents-pack, gents-toolcalls
\* models: CanonicalOutput.Execution.WorkerLevels.level, CanonicalOutput.Execution.WorkerLevels.admit, CanonicalOutput.Execution.WorkerLevels.lane, Triggers.Durable.observedClaimAllowed
\* Turns of one Agent that each start a delegate of the same Agent and wait
\* for its result, against the Agent's request workers and its backend's
\* admission pool (#4191).
\*
\* A request takes a worker when the runtime dequeues it from the Agent's
\* executor queue for its level (`AgentDispatcher`, `run_slot_workers`) and
\* keeps it until the request ends, its tool calls and waits included
\* (`AgentDaemon::run`). A request's level is its causal hop, capped at the
\* last level (Lean `WorkerLevels.level`); each level has `LevelWorkers`
\* workers and admits a request only while one is free (Lean
\* `WorkerLevels.admit`). Every model call also takes a permit from the
\* backend's admission pool (`BackendAdmissionController::acquire`), shared
\* by the Agent's calls and by plugin model calls (`BackendCallPools`); the
\* permit is released when the call's stream ends, before its tool calls run
\* (`hold_stream_guard_mapped`, `owned_loop_stream`).
\*
\* A root turn makes a model call whose tool call starts its delegate at hop
\* one (`agent_new`), then waits in its turn for the delegate's result: the
\* model keeps the turn open with tool calls that poll or sleep. The delegate
\* makes a model call and then, with `PluginCalls`, a plugin model call from
\* its tool calls. A wait that times out is not modelled: the turn waits
\* until its delegate ends, so a delegate that cannot start never ends it.
\*
\* A turn that starts writes its session's title audit, an auxiliary request:
\* the one request the session queue offers while the session has a request
\* running. A worker starts it as a task of its own and holds no worker for it
\* (`spawn_auxiliary_request`); its model call takes a backend permit. With
\* `AuxiliaryLane` it waits in a lane whose worker only starts auxiliary
\* requests (Lean `WorkerLevels.lane`); without it, it waits on the first
\* level for a worker no request holds. With `TurnsEnd` FALSE a turn may stay
\* open for as long as it likes (a long wait, a hung call): the title must
\* start all the same.
\*
\* A delegate that ends wakes its turn's session with its result (a request
\* at hop zero on that session). A worker may dequeue the wake while the turn
\* still runs, since workers no longer serialize one Agent's requests. The
\* claim decides whether it runs (`claim_queue_allows`, Lean
\* `Triggers.Durable.observedClaimAllowed`): a request runs its session's turn
\* only while no other request of the session is running or arrived before
\* it. Both claims write the session's document, so two concurrent claims of
\* one session commit one at a time and the claim is one step here. A refused
\* wake returns to pending, freeing its worker, and is offered again.
\* `SessionGate` FALSE claims without that decision.
\*
\* `Levels = 1` with `LevelWorkers = BackendLimit` is the rule before #4191:
\* the Agent's workers were its backend's `max_concurrent`, one level for every
\* hop. `PermitAcrossTools` keeps a turn's permit while its tool calls run.
\* `PluginOwnLimiter` gives plugin calls a limiter of their own sized like the
\* backend, as `ModelCallLimiters` did. `AuxiliaryLane` FALSE is the rule before
\* #4191 for auxiliary requests.
\*
\* Chains deeper than `Levels` share the last level, where a turn waiting on
\* its own delegate can again hold the worker the delegate needs; that bound is
\* not modelled here.
EXTENDS Naturals, FiniteSets

CONSTANTS
  Roots,            \* root turns, each starting one delegate
  Levels,           \* worker levels (`WorkerLevels.levels`)
  LevelWorkers,     \* workers per level (`WorkerLevels.levelWorkers`)
  BackendLimit,     \* the backend's `max_concurrent`
  PluginCalls,      \* each delegate makes a plugin model call from its tools
  PermitAcrossTools,
  PluginOwnLimiter,
  AuxiliaryLane,
  TurnsEnd,         \* a turn whose delegate ended ends
  SessionGate

ASSUME /\ Roots \in Nat \ {0}
       /\ Levels \in Nat \ {0}
       /\ LevelWorkers \in Nat \ {0}
       /\ BackendLimit \in Nat \ {0}
       /\ PluginCalls \in BOOLEAN
       /\ PermitAcrossTools \in BOOLEAN
       /\ PluginOwnLimiter \in BOOLEAN
       /\ AuxiliaryLane \in BOOLEAN
       /\ TurnsEnd \in BOOLEAN
       /\ SessionGate \in BOOLEAN

VARIABLES
  phase,    \* per request
  permit,   \* per request: holds an agent model-call permit
  plugin,   \* per delegate: holds a plugin model-call permit
  title,    \* per root: its session's title audit
  wake      \* per root: the wake its delegate's result sends its session

vars == <<phase, permit, plugin, title, wake>>

RootIds == 1..Roots
ChildIds == (Roots + 1)..(2 * Roots)
Requests == RootIds \cup ChildIds
Child(r) == r + Roots
Parent(c) == c - Roots
Hop(q) == IF q \in RootIds THEN 0 ELSE 1
Level(q) == IF Hop(q) < Levels - 1 THEN Hop(q) ELSE Levels - 1

Phases == {"idle", "queued", "ready", "calling", "tools", "plugin", "waiting", "done"}
TitlePhases == {"idle", "queued", "started", "calling", "done"}
WakePhases == {"idle", "queued", "claiming", "running", "done"}

HoldsWorker(q) == phase[q] \in {"ready", "calling", "tools", "plugin", "waiting"}
WakeHoldsWorker(r) == wake[r] \in {"claiming", "running"}
Live(l) ==
  Cardinality({q \in Requests : HoldsWorker(q) /\ Level(q) = l})
    + (IF l = 0 THEN Cardinality({r \in RootIds : WakeHoldsWorker(r)}) ELSE 0)
TurnOpen(r) == phase[r] \in {"ready", "calling", "waiting"}

PluginHeld == Cardinality({c \in ChildIds : plugin[c]})
TitlesCalling == Cardinality({r \in RootIds : title[r] = "calling"})
PoolHeld ==
  Cardinality({q \in Requests : permit[q]})
    + TitlesCalling
    + (IF PluginOwnLimiter THEN 0 ELSE PluginHeld)
PoolFree == PoolHeld < BackendLimit
PluginFree == IF PluginOwnLimiter THEN PluginHeld < BackendLimit ELSE PoolFree

ServerInFlight ==
  Cardinality({q \in Requests : phase[q] = "calling"}) + PluginHeld + TitlesCalling

TypeOK ==
  /\ phase \in [Requests -> Phases]
  /\ permit \in [Requests -> BOOLEAN]
  /\ plugin \in [ChildIds -> BOOLEAN]
  /\ title \in [RootIds -> TitlePhases]
  /\ wake \in [RootIds -> WakePhases]

Init ==
  /\ phase = [q \in Requests |-> IF q \in RootIds THEN "queued" ELSE "idle"]
  /\ permit = [q \in Requests |-> FALSE]
  /\ plugin = [c \in ChildIds |-> FALSE]
  /\ title = [r \in RootIds |-> "idle"]
  /\ wake = [r \in RootIds |-> "idle"]

Admit(q) ==
  /\ phase[q] = "queued"
  /\ Live(Level(q)) < LevelWorkers
  /\ phase' = [phase EXCEPT ![q] = "ready"]
  /\ title' = IF q \in RootIds THEN [title EXCEPT ![q] = "queued"] ELSE title
  /\ UNCHANGED <<permit, plugin, wake>>

TitleStart(r) ==
  /\ title[r] = "queued"
  /\ AuxiliaryLane \/ Live(0) < LevelWorkers
  /\ title' = [title EXCEPT ![r] = "started"]
  /\ UNCHANGED <<phase, permit, plugin, wake>>

TitleCall(r) ==
  /\ title[r] = "started"
  /\ PoolFree
  /\ title' = [title EXCEPT ![r] = "calling"]
  /\ UNCHANGED <<phase, permit, plugin, wake>>

TitleEnds(r) ==
  /\ title[r] = "calling"
  /\ title' = [title EXCEPT ![r] = "done"]
  /\ UNCHANGED <<phase, permit, plugin, wake>>

Call(q) ==
  /\ phase[q] = "ready"
  /\ PoolFree
  /\ phase' = [phase EXCEPT ![q] = "calling"]
  /\ permit' = [permit EXCEPT ![q] = TRUE]
  /\ UNCHANGED <<plugin, title, wake>>

\* The stream ends; the root's tool call dispatches its delegate.
StreamEnds(q) ==
  /\ phase[q] = "calling"
  /\ permit' = [permit EXCEPT ![q] = PermitAcrossTools]
  /\ IF q \in RootIds
       THEN phase' = [phase EXCEPT ![q] = "waiting", ![Child(q)] = "queued"]
       ELSE phase' = [phase EXCEPT ![q] = "tools"]
  /\ UNCHANGED <<plugin, title, wake>>

PluginStart(c) ==
  /\ phase[c] = "tools"
  /\ PluginCalls
  /\ PluginFree
  /\ phase' = [phase EXCEPT ![c] = "plugin"]
  /\ plugin' = [plugin EXCEPT ![c] = TRUE]
  /\ UNCHANGED <<permit, title, wake>>

PluginEnds(c) ==
  /\ phase[c] = "plugin"
  /\ phase' = [phase EXCEPT ![c] = "done"]
  /\ plugin' = [plugin EXCEPT ![c] = FALSE]
  /\ permit' = [permit EXCEPT ![c] = FALSE]
  /\ wake' = [wake EXCEPT ![Parent(c)] = "queued"]
  /\ UNCHANGED title

ChildEnds(c) ==
  /\ phase[c] = "tools"
  /\ ~PluginCalls
  /\ phase' = [phase EXCEPT ![c] = "done"]
  /\ permit' = [permit EXCEPT ![c] = FALSE]
  /\ wake' = [wake EXCEPT ![Parent(c)] = "queued"]
  /\ UNCHANGED <<plugin, title>>

RootEnds(r) ==
  /\ phase[r] = "waiting"
  /\ phase[Child(r)] = "done"
  /\ phase' = [phase EXCEPT ![r] = "done"]
  /\ permit' = [permit EXCEPT ![r] = FALSE]
  /\ UNCHANGED <<plugin, title, wake>>

WakeDequeue(r) ==
  /\ wake[r] = "queued"
  /\ Live(0) < LevelWorkers
  /\ wake' = [wake EXCEPT ![r] = "claiming"]
  /\ UNCHANGED <<phase, permit, plugin, title>>

WakeClaim(r) ==
  /\ wake[r] = "claiming"
  /\ ~SessionGate \/ ~TurnOpen(r)
  /\ wake' = [wake EXCEPT ![r] = "running"]
  /\ UNCHANGED <<phase, permit, plugin, title>>

WakeRefused(r) ==
  /\ wake[r] = "claiming"
  /\ SessionGate /\ TurnOpen(r)
  /\ wake' = [wake EXCEPT ![r] = "queued"]
  /\ UNCHANGED <<phase, permit, plugin, title>>

WakeEnds(r) ==
  /\ wake[r] = "running"
  /\ wake' = [wake EXCEPT ![r] = "done"]
  /\ UNCHANGED <<phase, permit, plugin, title>>

Next ==
  \/ \E q \in Requests : Admit(q) \/ Call(q) \/ StreamEnds(q)
  \/ \E c \in ChildIds : PluginStart(c) \/ PluginEnds(c) \/ ChildEnds(c)
  \/ \E r \in RootIds : RootEnds(r) \/ TitleStart(r) \/ TitleCall(r) \/ TitleEnds(r)
  \/ \E r \in RootIds : WakeDequeue(r) \/ WakeClaim(r) \/ WakeRefused(r) \/ WakeEnds(r)

\* Admission and both pools serve their waiters in order; strong fairness is
\* that order's abstraction.
Fairness ==
  /\ \A q \in Requests : SF_vars(Admit(q)) /\ SF_vars(Call(q)) /\ WF_vars(StreamEnds(q))
  /\ \A c \in ChildIds :
       SF_vars(PluginStart(c)) /\ WF_vars(PluginEnds(c)) /\ WF_vars(ChildEnds(c))
  /\ \A r \in RootIds :
       /\ TurnsEnd => WF_vars(RootEnds(r))
       /\ SF_vars(TitleStart(r)) /\ SF_vars(TitleCall(r)) /\ WF_vars(TitleEnds(r))
       /\ SF_vars(WakeDequeue(r)) /\ SF_vars(WakeClaim(r)) /\ WF_vars(WakeRefused(r))
       /\ WF_vars(WakeEnds(r))

Spec == Init /\ [][Next]_vars /\ Fairness

\* The server never runs more of the backend's calls than it admits.
ServerWithinLimit == ServerInFlight <= BackendLimit

WorkersWithinLevels == \A l \in 0..(Levels - 1) : Live(l) <= LevelWorkers

\* Every turn gets its delegate's result: no turn waits forever on a delegate
\* that cannot start.
AllDone ==
  <>(/\ \A q \in Requests : phase[q] = "done"
     /\ \A r \in RootIds : wake[r] = "done")

\* A session runs one turn at a time: its wake never runs beside its turn.
OneTurnPerSession == \A r \in RootIds : ~(TurnOpen(r) /\ wake[r] = "running")

\* A session's title audit starts however long its turn stays open.
TitlesStart == \A r \in RootIds : (title[r] = "queued") ~> (title[r] # "queued")
====
