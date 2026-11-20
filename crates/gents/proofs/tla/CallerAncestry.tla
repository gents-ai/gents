---- MODULE CallerAncestry ----
\* lean: ServiceLifecycle.CallerObservation.callerRunsOnTarget, ServiceLifecycle.admission
\* crates: gents-service
EXTENDS Naturals, FiniteSets, LeanDecision_ServiceLifecycle, LeanDecision_ServiceLifecycle_CallerObservation

\* A `gents runtime service stop` caller decides whether the runtime's job is
\* its ancestor (`lifecycle::ancestry::is_ancestor`) while the operating system
\* runs on: processes exit, their children are adopted, and freed process IDs
\* are reused. The caller asks the service manager for the job's pid, reads its
\* own parent, then reads one parent at a time; each read is atomic and nothing
\* holds the chain still between reads. Lean decides what the observation
\* admits. Pid 1 is the root (launchd or init) and `Caller` never exits.
\*
\* UnreadableIsUnobservable: an unreadable link (a sandbox denies it, or its
\*   process is gone) ends the walk as `unobservable`; FALSE ends it as if the
\*   root were reached.
\* Bounded / MaxDepth: the walk gives up as `unobservable` after MaxDepth reads.
\* JobIsSubreaper: the job adopts orphans of its own tree, which the Lean
\*   premise excludes.
CONSTANTS Pids, Caller, MaxDepth, Bounded, UnreadableIsUnobservable, JobIsSubreaper,
          DeniedChoices

VARIABLES parent, job, denied, pc, seen, cur, depth, result, quiet, outsider, selfStop
vars == <<parent, job, denied, pc, seen, cur, depth, result, quiet, outsider, selfStop>>

Root == 1
Alive(p) == p = Root \/ parent[p] # 0
N == Cardinality(Pids)

RECURSIVE Up(_, _)
Up(p, n) == IF n = 0 \/ p = Root \/ parent[p] = 0 THEN {}
            ELSE {parent[p]} \cup Up(parent[p], n - 1)

Descends(c, a) == a # 0 /\ a \in Up(c, N)

\* The agent's shell (Caller under the job) or a terminal (Caller beside it).
Init == /\ \E shellParent \in {2, Root} :
             parent = [p \in Pids |-> CASE p = Root -> 0
                                       [] p = 2 -> Root
                                       [] p = 3 -> shellParent
                                       [] p = 4 -> 3
                                       [] p = Caller -> 4
                                       [] OTHER -> 0]
        /\ job = 2
        /\ denied \in DeniedChoices
        /\ pc = "ask" /\ seen = 0 /\ cur = 0 /\ depth = 0
        /\ result = "none" /\ quiet = FALSE /\ outsider = FALSE /\ selfStop = FALSE

\* Linux adopts an orphan into its nearest living subreaper ancestor, else
\* the root; macOS always the root.
Reaper(p) == IF JobIsSubreaper /\ job # 0 /\ p # job /\ Descends(p, job) THEN job ELSE Root

Exit(p) == /\ p \notin {Root, Caller} /\ Alive(p)
           /\ parent' = [q \in Pids |-> IF q = p THEN 0
                                       ELSE IF parent[q] = p THEN Reaper(p)
                                       ELSE parent[q]]
           /\ job' = IF p = job THEN 0 ELSE job

Spawn(p, q) == /\ p \notin {Root, Caller} /\ ~Alive(p) /\ Alive(q)
               /\ parent' = [parent EXCEPT ![p] = q]
               /\ UNCHANGED job

StartJob(p) == /\ job = 0 /\ p \notin {Root, Caller} /\ ~Alive(p)
               /\ parent' = [parent EXCEPT ![p] = Root]
               /\ job' = p

System == /\ \/ \E p \in Pids : Exit(p)
             \/ \E p, q \in Pids : Spawn(p, q)
             \/ \E p \in Pids : StartJob(p)
          /\ quiet' = FALSE
          /\ UNCHANGED <<denied, pc, seen, cur, depth, result, outsider, selfStop>>

Ask == /\ pc = "ask"
       /\ seen' = job
       /\ outsider' = ~Descends(Caller, job)
       /\ quiet' = TRUE
       /\ IF job = 0 THEN result' = "no_runtime" /\ pc' = "decide"
                     ELSE UNCHANGED result /\ pc' = "start"
       /\ UNCHANGED <<parent, job, denied, cur, depth, selfStop>>

Start == /\ pc = "start"
         /\ cur' = parent[Caller] /\ depth' = 0 /\ pc' = "walk"
         /\ UNCHANGED <<parent, job, denied, seen, result, quiet, outsider, selfStop>>

Unreadable == IF UnreadableIsUnobservable THEN "unobservable" ELSE "not_ancestor"

Conclude(observation) == /\ result' = observation /\ pc' = "decide"
                         /\ UNCHANGED <<cur, depth>>

Walk == /\ pc = "walk"
        /\ IF Bounded /\ depth = MaxDepth THEN Conclude("unobservable")
           ELSE IF cur = seen THEN Conclude("ancestor")
           ELSE IF cur <= Root THEN Conclude("not_ancestor")
           ELSE IF ~Alive(cur) \/ cur \in denied THEN Conclude(Unreadable)
           ELSE /\ cur' = parent[cur]
                /\ depth' = IF Bounded THEN depth + 1 ELSE depth
                /\ UNCHANGED <<result, pc>>
        /\ UNCHANGED <<parent, job, denied, seen, quiet, outsider, selfStop>>

Decide == /\ pc = "decide"
          /\ pc' = IF admission("stop", callerRunsOnTarget(result), FALSE) = "admitted"
                   THEN "act" ELSE "done"
          /\ UNCHANGED <<parent, job, denied, seen, cur, depth, result, quiet, outsider, selfStop>>

Act == /\ pc = "act"
       /\ selfStop' = Descends(Caller, job)
       /\ pc' = "done"
       /\ UNCHANGED <<parent, job, denied, seen, cur, depth, result, quiet, outsider>>

Walker == Ask \/ Start \/ Walk \/ Decide \/ Act

Next == Walker \/ System
Spec == Init /\ [][Next]_vars /\ WF_vars(Walker)

\* Admission never ends the runtime the caller runs under.
NoSelfStop == ~selfStop

\* A caller outside the job whose chain holds still and is readable is admitted.
QuietOutsiderIsAdmitted ==
  (pc \in {"act", "done"} /\ quiet /\ outsider /\ Up(Caller, N) \cap denied = {})
    => result \in {"no_runtime", "not_ancestor"}

Terminates == <>(pc = "done")
====
