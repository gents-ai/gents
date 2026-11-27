---- MODULE ClientIdentityMetadata ----
\* lean: none
\* crates: gents-desktop-core, gents-store
\* traces: violates
\* models: ClientIdentityMetadata.decide
(***************************************************************************)
(* First-run writers of one home's client identity metadata                *)
(* (`principal.json`, `PrincipalIdentity::load_or_create`): two `gents`    *)
(* invocations, or one beside the desktop app (#3659, the concurrent eval  *)
(* pilots). Each writer has already loaded or created the signing key,     *)
(* whose file is published without replacing an existing one               *)
(* (`publish_private_key_file`), so writers of one home describe the same  *)
(* key. Each then reads the metadata (`validate_or_persist_metadata`):     *)
(* stored metadata is compared with its own; absent metadata is staged in  *)
(* a file beside the destination, filled, and renamed over the destination *)
(* (`write_json_atomically`), after which the writer reads the destination *)
(* again. A rename replaces its destination atomically and fails when its  *)
(* source is gone. A writer may die at any step.                           *)
(*                                                                          *)
(* What a writer does with what it read is Lean                             *)
(* `ClientIdentityMetadata.decide`; this spec adds only the interleaving   *)
(* of the writers' file operations.                                        *)
(***************************************************************************)
\* trace actors: Read, Create, Fill, Clear, Rename, Decide, Crash => pc
EXTENDS Naturals

CONSTANTS
  Writers,
  UniqueStaging,       \* each writer stages under a name no other can pick
  RemoveBeforeRename,  \* the destination is removed before the rename
  Reread,              \* a writer reads the destination again after its rename
  SingleWinnerKey,     \* every writer of the home describes the same key
  CanCrash

VARIABLES
  dest,     \* absent | torn | the metadata installed
  stage,    \* staging file by name: absent | empty | metadata
  pc,       \* start | create | fill | clear | rename | decide | outcomes
  installed,\* ghost: some rename has installed the destination
  okSaw     \* ghost: the writer accepted while the destination held its metadata

vars == <<dest, stage, pc, installed, okSaw>>

Meta(w) == IF SingleWinnerKey THEN "key" ELSE w
Metas == {Meta(w) : w \in Writers}
Names == IF UniqueStaging THEN Writers ELSE {"principal.tmp"}
Name(w) == IF UniqueStaging THEN w ELSE "principal.tmp"

Outcomes == {"ok", "mismatch", "missingFile", "missingAfterInstall", "parseError",
             "crashed"}

TypeOK ==
  /\ dest \in {"absent", "torn"} \cup Metas
  /\ stage \in [Names -> {"absent", "empty"} \cup Metas]
  /\ pc \in [Writers -> {"start", "create", "fill", "clear", "rename", "decide"}
                          \cup Outcomes]
  /\ installed \in BOOLEAN
  /\ okSaw \in [Writers -> BOOLEAN]

Init ==
  /\ dest = "absent"
  /\ stage = [n \in Names |-> "absent"]
  /\ pc = [w \in Writers |-> "start"]
  /\ installed = FALSE
  /\ okSaw = [w \in Writers |-> FALSE]

\* Lean `ClientIdentityMetadata.decide` on what the writer read.
Decision(w, afterInstall) ==
  CASE dest = Meta(w) -> "ok"
    [] dest = "torn" -> "parseError"
    [] dest = "absent" -> IF afterInstall THEN "missingAfterInstall" ELSE "create"
    [] OTHER -> "mismatch"

Read(w) ==
  /\ pc[w] = "start"
  /\ pc' = [pc EXCEPT ![w] = Decision(w, FALSE)]
  /\ okSaw' = [okSaw EXCEPT ![w] = (dest = Meta(w))]
  /\ UNCHANGED <<dest, stage, installed>>

\* Opening the staging file creates or truncates it.
Create(w) ==
  /\ pc[w] = "create"
  /\ stage' = [stage EXCEPT ![Name(w)] = "empty"]
  /\ pc' = [pc EXCEPT ![w] = "fill"]
  /\ UNCHANGED <<dest, installed, okSaw>>

\* The write goes to the file the writer opened. If another writer renamed
\* that file away, it is the destination now.
Fill(w) ==
  /\ pc[w] = "fill"
  /\ IF stage[Name(w)] # "absent"
       THEN stage' = [stage EXCEPT ![Name(w)] = Meta(w)] /\ UNCHANGED dest
       ELSE dest' = (IF dest = "torn" THEN Meta(w) ELSE dest) /\ UNCHANGED stage
  /\ pc' = [pc EXCEPT ![w] = IF RemoveBeforeRename THEN "clear" ELSE "rename"]
  /\ UNCHANGED <<installed, okSaw>>

Clear(w) ==
  /\ pc[w] = "clear"
  /\ dest' = "absent"
  /\ pc' = [pc EXCEPT ![w] = "rename"]
  /\ UNCHANGED <<stage, installed, okSaw>>

Rename(w) ==
  /\ pc[w] = "rename"
  /\ IF stage[Name(w)] = "absent"
       THEN /\ pc' = [pc EXCEPT ![w] = "missingFile"]
            /\ UNCHANGED <<dest, stage, installed>>
       ELSE /\ dest' = (IF stage[Name(w)] = "empty" THEN "torn" ELSE stage[Name(w)])
            /\ stage' = [stage EXCEPT ![Name(w)] = "absent"]
            /\ installed' = TRUE
            /\ pc' = [pc EXCEPT ![w] = "decide"]
  /\ UNCHANGED okSaw

Decide(w) ==
  /\ pc[w] = "decide"
  /\ pc' = [pc EXCEPT ![w] = IF Reread THEN Decision(w, TRUE) ELSE "ok"]
  /\ okSaw' = [okSaw EXCEPT ![w] = (dest = Meta(w))]
  /\ UNCHANGED <<dest, stage, installed>>

Crash(w) ==
  /\ CanCrash /\ pc[w] \notin Outcomes
  /\ pc' = [pc EXCEPT ![w] = "crashed"]
  /\ UNCHANGED <<dest, stage, installed, okSaw>>

Step(w) == Read(w) \/ Create(w) \/ Fill(w) \/ Clear(w) \/ Rename(w) \/ Decide(w)

Next == \E w \in Writers : Step(w) \/ Crash(w)

Spec == Init /\ [][Next]_vars /\ \A w \in Writers : WF_vars(Step(w))

(* Properties. *)

\* No writer fails because a file it expected was missing: its staging file
\* at the rename, or the destination after its own rename.
NoMissingFileError ==
  \A w \in Writers : pc[w] \notin {"missingFile", "missingAfterInstall"}

\* The destination is never a partial file, and no writer reads one.
NoTornMetadata ==
  /\ dest # "torn"
  /\ \A w \in Writers : pc[w] # "parseError"

\* Once installed, the destination exists for every later reader.
NeverUninstalled == installed => dest # "absent"

\* A writer accepts only metadata it read as equal to its own.
OkSawOwnMetadata == \A w \in Writers : pc[w] = "ok" => okSaw[w]

\* A writer that accepted keeps describing the installed file.
AgreedInstall == \A w \in Writers : pc[w] = "ok" => dest = Meta(w)

\* Every writer ends accepted or with the hard mismatch, unless it died.
WriterSettles ==
  \A w \in Writers : <>(pc[w] \in {"ok", "mismatch", "crashed"})

\* Writers of one key all accept.
SameKeyAccepts ==
  SingleWinnerKey => \A w \in Writers : <>(pc[w] \in {"ok", "crashed"})

====
