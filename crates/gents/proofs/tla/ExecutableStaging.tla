---- MODULE ExecutableStaging ----
\* lean: none
\* crates: gents-service
(***************************************************************************)
(* The service updater stages an executable (`copy_executable` in          *)
(* `gents-service/src/service_update/files.rs`) and the staged file is     *)
(* exec'd afterwards, while other threads of the same process spawn        *)
(* children.                                                                *)
(*                                                                          *)
(* Callers: `stage.rs` copies the candidate, the helper and the prior      *)
(* executable into a temporary directory and at once runs the staged       *)
(* helper and candidate (`ProcessCommandRunner`), in-process. The desktop  *)
(* repair path (`service_update.rs`, `stage_repair_executable`) copies into *)
(* a `NamedTempFile`, which holds a read-write descriptor of the file until *)
(* `persist`; the service manager execs the persisted file later.           *)
(* `repair_backups.rs` saves a prior program the same way.                  *)
(*                                                                          *)
(* `Copier` is who opens the staged file for writing: "updater" is          *)
(* `fs::copy` in the updater process (O_CLOEXEC, as std always opens);      *)
(* "child" is a `/bin/cp` child the updater reaps before going on.          *)
(* `TempHandle` is the `NamedTempFile` descriptor the updater holds from    *)
(* creating the staged file until `persist`.                                *)
(*                                                                          *)
(* Every sibling spawn forks the updater process and its child copies the  *)
(* updater's descriptors until it execs (O_CLOEXEC closes them only at      *)
(* exec). Linux execve refuses a file any process holds open for writing   *)
(* (ETXTBSY). `Exec` is whoever runs the staged file once staging is done: *)
(* the in-process probe at once or the service manager later. Nothing      *)
(* orders a later exec after the siblings' own execs, so "later" closes no *)
(* window: the model leaves the exec's timing free.                         *)
(*                                                                          *)
(* `NoTextBusy`: the exec of the staged executable succeeds. GREEN: a      *)
(* reaped child copies into a file the updater never opens for writing.    *)
(* RED: `InProcessCopy` (today's `fs::copy`), `TempHandle` (a child copy   *)
(* into a `NamedTempFile` the updater holds open), `Unwaited` (the copy    *)
(* child not reaped).                                                       *)
(***************************************************************************)

CONSTANTS
  Siblings,      \* other threads of the updater process that spawn
  Copier,        \* "updater" | "child"
  TempHandle,    \* the updater holds a read-write descriptor until persist
  WaitForCopier  \* the updater reaps the copy child before going on

VARIABLES
  writers,  \* processes holding a descriptor that may write the staged file
  u,        \* updater: start | created | copying | spawned | copied | persisted | ran | busy
  c,        \* copy child: none | forked | writing | exited
  sib       \* sibling child: idle | forked | execed

vars == <<writers, u, c, sib>>

Updater == "updater"
CopyChild == "copier"

TypeOK ==
  /\ Copier \in {"updater", "child"}
  /\ writers \subseteq ({Updater, CopyChild} \cup Siblings)
  /\ u \in {"start", "created", "copying", "spawned", "copied", "persisted", "ran", "busy"}
  /\ c \in {"none", "forked", "writing", "exited"}
  /\ sib \in [Siblings -> {"idle", "forked", "execed"}]

Init ==
  /\ writers = {}
  /\ u = "start"
  /\ c = "none"
  /\ sib = [s \in Siblings |-> "idle"]

\* The staged file's name is chosen; a `NamedTempFile` also opens it.
Create ==
  /\ u = "start"
  /\ u' = "created"
  /\ writers' = IF TempHandle THEN writers \cup {Updater} ELSE writers
  /\ UNCHANGED <<c, sib>>

\* "updater": `fs::copy` opens the target for writing in this process.
CopyOpen ==
  /\ u = "created"
  /\ Copier = "updater"
  /\ u' = "copying"
  /\ writers' = writers \cup {Updater}
  /\ UNCHANGED <<c, sib>>

\* "updater": the copy finishes and closes its own descriptor (a temp
\* handle, if any, stays open).
CopyClose ==
  /\ u = "copying"
  /\ u' = "copied"
  /\ writers' = IF TempHandle THEN writers ELSE writers \ {Updater}
  /\ UNCHANGED <<c, sib>>

\* "child": the updater spawns `/bin/cp`. Its fork copies the updater's
\* descriptors, including a temp handle.
SpawnCopier ==
  /\ u = "created"
  /\ Copier = "child"
  /\ u' = "spawned"
  /\ c' = "forked"
  /\ writers' = IF Updater \in writers THEN writers \cup {CopyChild} ELSE writers
  /\ UNCHANGED sib

\* `cp` has exec'd (its inherited copies closed) and opens the target.
CopierOpen ==
  /\ c = "forked"
  /\ c' = "writing"
  /\ writers' = writers \cup {CopyChild}
  /\ UNCHANGED <<u, sib>>

CopierExit ==
  /\ c = "writing"
  /\ c' = "exited"
  /\ writers' = writers \ {CopyChild}
  /\ UNCHANGED <<u, sib>>

CopierDone ==
  /\ u = "spawned"
  /\ WaitForCopier => c = "exited"
  /\ u' = "copied"
  /\ UNCHANGED <<writers, c, sib>>

\* `persist` (or the end of staging) closes a temp handle.
Persist ==
  /\ u = "copied"
  /\ u' = "persisted"
  /\ writers' = writers \ {Updater}
  /\ UNCHANGED <<c, sib>>

\* The probe or the service manager execs the staged file, at any time
\* after staging.
Exec ==
  /\ u = "persisted"
  /\ u' = IF writers # {} THEN "busy" ELSE "ran"
  /\ UNCHANGED <<writers, c, sib>>

Fork(s) ==
  /\ sib[s] = "idle"
  /\ sib' = [sib EXCEPT ![s] = "forked"]
  /\ writers' = IF Updater \in writers THEN writers \cup {s} ELSE writers
  /\ UNCHANGED <<u, c>>

ChildExec(s) ==
  /\ sib[s] = "forked"
  /\ sib' = [sib EXCEPT ![s] = "execed"]
  /\ writers' = writers \ {s}
  /\ UNCHANGED <<u, c>>

Next ==
  \/ Create \/ CopyOpen \/ CopyClose
  \/ SpawnCopier \/ CopierOpen \/ CopierExit \/ CopierDone
  \/ Persist \/ Exec
  \/ \E s \in Siblings : Fork(s) \/ ChildExec(s)

Spec == Init /\ [][Next]_vars

NoTextBusy == u # "busy"

====
