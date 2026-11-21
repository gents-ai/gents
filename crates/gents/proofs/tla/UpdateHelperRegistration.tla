---- MODULE UpdateHelperRegistration ----
\* lean: RuntimeReconcile.UpdateHandoff.helperRegistration, RuntimeReconcile.UpdateHandoff.statusTakesLock
\* crates: gents-service
\* models: RuntimeReconcile.UpdateHandoff.step
\* paths: .github/scripts/test-managed-self-update.py
EXTENDS LeanDecision_RuntimeReconcile_UpdateHandoff
(***************************************************************************)
(* Registering one managed update's helper job beside the helper's own     *)
(* retirement of it. A registrant is a submitter (a duplicate or resumed   *)
(* `service update`) or a status reader (`service update-status`): it      *)
(* reads the receipt, may take the operation's `helper.lock` and ask Lean  *)
(* `helperRegistration`, then registers by writing the job definition,     *)
(* reloading and starting it. A status reader takes the lock only for a    *)
(* phase Lean `statusTakesLock` admits, so before acknowledgement only the *)
(* submitter and the helper contend for it. The helper holds `helper.lock` *)
(* while it serves; once the operation is finished it removes its          *)
(* definition, reloads, queues its own stop and returns. It may crash at   *)
(* any step, and the kernel then drops its lock; systemd respawns a        *)
(* crashed helper (`Restart=on-failure`). `"accepted"` stands for Accepted *)
(* through Installed, which every registrant treats alike.                 *)
(*                                                                         *)
(* External premise (systemd v255): `start` answers "Unit not found"       *)
(* exactly when the definition was absent at the last reload and is still  *)
(* absent; a definition written since the last reload is loaded first. The *)
(* auto-restart path enqueues its start through the same transaction. The  *)
(* steps are systemd's only: launchd's (write the plist, `print`,          *)
(* `bootstrap` or `kickstart`; remove the plist, `bootout`) are not        *)
(* modelled, although the same job and lock run on macOS.                  *)
(*                                                                         *)
(* Left out, and why the green result covers them. There is no             *)
(* `"registered"`: it behaves as `"accepted"` does or with fewer steps     *)
(* (the submitter re-registers it under the lock, a status reader leaves   *)
(* it to the submitter, the helper can only fail it). None of the          *)
(* submitter's own transitions (Register, Acknowledge, its Fail after a    *)
(* failed registration) or `reject_pending`'s expiry is modelled, and a    *)
(* Staged submitter's two registrations (first registration, then          *)
(* re-registration) are one. Each of those transitions moves a live phase  *)
(* to a live one or fails it, and a finished phase has no successor (Lean  *)
(* `finished_phases_have_no_successor`); only the helper removes the       *)
(* definition, under the lock. So a registrant that decides under the lock *)
(* on the receipt read under it either reads a live phase and holds the    *)
(* lock through its start, or reads the finished phase that every          *)
(* retirement leaves, whoever finished it and however many registrations   *)
(* run. This argument is not model checked.                                *)
(***************************************************************************)

CONSTANTS
  Submitters,
  Readers,
  InitialPhases,
  LockedReregistration,    \* the submitter's re-registration takes the lock
  ReadUnderLock,           \* the decision reads the receipt after taking the lock
  LockedFirstRegistration, \* first registration of a Staged operation takes the lock
  StatusGate               \* a status reader locks only where `statusTakesLock` admits

Registrants == Submitters \cup Readers
Caller(r) == IF r \in Submitters THEN "submitter" ELSE "status"
Phases == {"staged", "accepted", "healthy", "failed"}
Finished == {p \in Phases : helperRegistration("submitter", TRUE, p) = "finished"}
PCs == {"read", "trylock", "decide", "write", "reload", "start", "unlock", "done"}
HelperStates == {"none", "launched", "working", "remove", "reload", "stop", "unlock", "exited"}

VARIABLES
  unitFile, \* the helper's job definition is on disk
  loaded,   \* the definition was on disk at the last reload
  phase,    \* the receipt
  lock,     \* helper.lock: "free", "helper" or the registrant holding it
  helper,   \* the helper process
  pc,
  seen,     \* the receipt phase each registrant read
  taken,    \* what each registrant's try_lock returned
  retired,  \* ghost: the helper removed the definition of a finished operation
  missed,   \* ghost: registrants whose start found no unit
  late,     \* ghost: registrants that wrote a definition after retirement
  starved   \* ghost: Staged submitters whose try_lock met a lock the helper did not hold

vars == <<unitFile, loaded, phase, lock, helper, pc, seen, taken, retired, missed, late, starved>>

Locked(r) ==
  \/ Caller(r) = "status"
  \/ IF phase = "staged" THEN LockedFirstRegistration ELSE LockedReregistration

TypeOK ==
  /\ unitFile \in BOOLEAN
  /\ loaded \in BOOLEAN
  /\ phase \in Phases
  /\ lock \in {"free", "helper"} \cup Registrants
  /\ helper \in HelperStates
  /\ pc \in [Registrants -> PCs]
  /\ seen \in [Registrants -> Phases \cup {"none"}]
  /\ taken \in [Registrants -> BOOLEAN]
  /\ retired \in BOOLEAN
  /\ missed \subseteq Registrants
  /\ late \subseteq Registrants
  /\ starved \subseteq Submitters

Init ==
  /\ phase \in InitialPhases
  /\ unitFile = TRUE
  /\ loaded = TRUE
  /\ lock = "helper"
  /\ helper = "working"
  /\ pc = [r \in Registrants |-> "read"]
  /\ seen = [r \in Registrants |-> "none"]
  /\ taken = [r \in Registrants |-> FALSE]
  /\ retired = FALSE
  /\ missed = {}
  /\ late = {}
  /\ starved = {}

Goto(r, to) == pc' = [pc EXCEPT ![r] = to]
Release(r) == IF taken[r] THEN "unlock" ELSE "done"

Read(r) ==
  /\ pc[r] = "read"
  /\ seen' = [seen EXCEPT ![r] = phase]
  /\ Goto(r, CASE Caller(r) = "submitter" /\ phase \in Finished -> "done"
               [] Caller(r) = "status" /\ StatusGate /\ ~statusTakesLock(phase) -> "done"
               [] Locked(r) -> "trylock"
               [] OTHER -> "write")
  /\ UNCHANGED <<unitFile, loaded, phase, lock, helper, taken, retired, missed, late, starved>>

TryLock(r) ==
  /\ pc[r] = "trylock"
  /\ taken' = [taken EXCEPT ![r] = (lock = "free")]
  /\ lock' = IF lock = "free" THEN r ELSE lock
  /\ starved' = IF Caller(r) = "submitter" /\ seen[r] = "staged" /\ lock \notin {"free", "helper"}
                  THEN starved \cup {r} ELSE starved
  /\ Goto(r, "decide")
  /\ UNCHANGED <<unitFile, loaded, phase, helper, seen, retired, missed, late>>

Decide(r) ==
  /\ pc[r] = "decide"
  /\ LET d == helperRegistration(Caller(r), taken[r],
                                 IF ReadUnderLock THEN phase ELSE seen[r])
     IN Goto(r, IF d = "register" THEN "write" ELSE Release(r))
  /\ UNCHANGED <<unitFile, loaded, phase, lock, helper, seen, taken, retired, missed, late, starved>>

Write(r) ==
  /\ pc[r] = "write"
  /\ unitFile' = TRUE
  /\ late' = IF retired THEN late \cup {r} ELSE late
  /\ Goto(r, "reload")
  /\ UNCHANGED <<loaded, phase, lock, helper, seen, taken, retired, missed, starved>>

Reload(r) ==
  /\ pc[r] = "reload"
  /\ loaded' = unitFile
  /\ Goto(r, "start")
  /\ UNCHANGED <<unitFile, phase, lock, helper, seen, taken, retired, missed, late, starved>>

Start(r) ==
  /\ pc[r] = "start"
  /\ IF ~loaded /\ ~unitFile
       THEN missed' = missed \cup {r} /\ UNCHANGED helper
       ELSE /\ helper' = IF helper \in {"none", "exited"} THEN "launched" ELSE helper
            /\ UNCHANGED missed
  /\ Goto(r, Release(r))
  /\ UNCHANGED <<unitFile, loaded, phase, lock, seen, taken, retired, late, starved>>

Unlock(r) ==
  /\ pc[r] = "unlock"
  /\ lock' = "free"
  /\ Goto(r, "done")
  /\ UNCHANGED <<unitFile, loaded, phase, helper, seen, taken, retired, missed, late, starved>>

HelperLock ==
  /\ helper = "launched"
  /\ IF lock = "free"
       THEN /\ lock' = "helper"
            /\ helper' = IF phase \in Finished THEN "remove" ELSE "working"
       ELSE helper' = "exited" /\ UNCHANGED lock
  /\ UNCHANGED <<unitFile, loaded, phase, pc, seen, taken, retired, missed, late, starved>>

HelperFinish ==
  /\ helper = "working"
  /\ phase' \in IF phase = "staged" THEN {"failed"} ELSE Finished
  /\ helper' = "remove"
  /\ UNCHANGED <<unitFile, loaded, lock, pc, seen, taken, retired, missed, late, starved>>

HelperRemove ==
  /\ helper = "remove"
  /\ unitFile' = FALSE
  /\ retired' = TRUE
  /\ helper' = "reload"
  /\ UNCHANGED <<loaded, phase, lock, pc, seen, taken, missed, late, starved>>

HelperReload ==
  /\ helper = "reload"
  /\ loaded' = unitFile
  /\ helper' = "stop"
  /\ UNCHANGED <<unitFile, phase, lock, pc, seen, taken, retired, missed, late, starved>>

HelperStop ==
  /\ helper = "stop"
  /\ helper' = "unlock"
  /\ UNCHANGED <<unitFile, loaded, phase, lock, pc, seen, taken, retired, missed, late, starved>>

HelperUnlock ==
  /\ helper = "unlock"
  /\ lock' = "free"
  /\ helper' = "none"
  /\ UNCHANGED <<unitFile, loaded, phase, pc, seen, taken, retired, missed, late, starved>>

HelperCrash ==
  /\ helper \in {"launched", "working", "remove", "reload", "stop"}
  /\ helper' = "exited"
  /\ lock' = IF lock = "helper" THEN "free" ELSE lock
  /\ UNCHANGED <<unitFile, loaded, phase, pc, seen, taken, retired, missed, late, starved>>

Respawn ==
  /\ helper = "exited"
  /\ loaded \/ unitFile
  /\ helper' = "launched"
  /\ UNCHANGED <<unitFile, loaded, phase, lock, pc, seen, taken, retired, missed, late, starved>>

Registrant(r) ==
  Read(r) \/ TryLock(r) \/ Decide(r) \/ Write(r) \/ Reload(r) \/ Start(r) \/ Unlock(r)

Helper ==
  HelperLock \/ HelperFinish \/ HelperRemove \/ HelperReload \/ HelperStop \/ HelperUnlock
  \/ HelperCrash \/ Respawn

Next == Helper \/ \E r \in Registrants : Registrant(r)

Spec == Init /\ [][Next]_vars

\* (a) No registrant's start names a unit whose definition was absent at the
\* last reload.
StartFindsUnit == missed = {}

\* (b) No registration begins after the helper retired a finished operation.
NoRegistrationAfterRetirement == late = {}

\* What `test-managed-self-update.py` asserts of the duplicate submission.
DuplicateReportsAccepted == \A s \in Submitters : pc[s] = "done" => seen[s] = "accepted"

\* A Staged submitter that cannot take the lock leaves registration to its
\* holder, which then must be the helper.
StagedSubmitterYieldsOnlyToHelper == starved = {}

====
