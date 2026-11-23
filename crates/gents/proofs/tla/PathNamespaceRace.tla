---- MODULE PathNamespaceRace ----
\* lean: PathNamespace.step
\* crates: gents-fs-runner, gents-tools, gents-pack
EXTENDS LeanDecision_PathNamespace

\* A native file tool reads `a/f` beneath its root while another process
\* that may change the namespace beneath the root (a command the same Agent
\* started) swaps the directory `a` for a link to a directory outside the
\* root and back, and swaps the file `a/f` for a link to the outside file and
\* back. Outside the root lies the file `S`; inside it, `F`.
\*
\* With ByHandle the tool walks as `gents_fs_runner::beneath` does: it looks
\* each name up in the directory handle the previous step returned without
\* following a link there, decides each step by `PathNamespace.step`, holds
\* the directory object it descended into, and opens the last name in that
\* held directory, again without following a link. A swap renames `a` aside,
\* so a held handle keeps naming the original directory.
\*
\* Safeguard, removed by one constant:
\*   ByHandle   FALSE checks the path by resolving it whole, following
\*              links, and then resolves it again by path to use it.

CONSTANTS ByHandle

Entries == {"A", "F", "S", "LinkDir", "LinkFile", "none"}
Inside == {"A", "F"}

VARIABLES rootA, aF, pc, held, target
vars == <<rootA, aF, pc, held, target>>

\* The object outside the root `a/f` names when every link is followed.
FollowPath ==
  IF rootA = "LinkDir" THEN "S"
  ELSE IF aF = "LinkFile" THEN "S"
  ELSE aF

Observe(entry) ==
  CASE entry = "A" -> "directory"
    [] entry \in {"F", "S"} -> "non_directory"
    [] entry \in {"LinkDir", "LinkFile"} -> "link_outside_root"
    [] OTHER -> "missing"

\* The entry `f` of the directory object a handle names: the original `a`
\* keeps its own entries wherever it was renamed to.
EntryOf(dir, name) == IF dir = "A" THEN aF ELSE "none"

Init ==
  /\ rootA = "A"
  /\ aF = "F"
  /\ pc = IF ByHandle THEN "lookup_a" ELSE "check"
  /\ held = "none"
  /\ target = "none"

SwapDir ==
  /\ rootA' = IF rootA = "A" THEN "LinkDir" ELSE "A"
  /\ UNCHANGED <<aF, pc, held, target>>

SwapFile ==
  /\ aF' = IF aF = "F" THEN "LinkFile" ELSE "F"
  /\ UNCHANGED <<rootA, pc, held, target>>

LookupA ==
  /\ pc = "lookup_a"
  /\ LET decided == step("name", TRUE, FALSE, FALSE, FALSE, TRUE, Observe(rootA))
     IN IF decided = "descend"
        THEN /\ held' = rootA
             /\ pc' = "lookup_f"
        ELSE /\ pc' = "refused"
             /\ UNCHANGED held
  /\ UNCHANGED <<rootA, aF, target>>

LookupF ==
  /\ pc = "lookup_f"
  /\ LET decided == step("name", FALSE, TRUE, FALSE, FALSE, TRUE, Observe(EntryOf(held, "f")))
     IN pc' = IF decided = "land" THEN "open" ELSE "refused"
  /\ UNCHANGED <<rootA, aF, held, target>>

\* The open does not follow a link at the name.
Open ==
  /\ pc = "open"
  /\ LET entry == EntryOf(held, "f")
     IN IF entry \in {"LinkDir", "LinkFile"}
        THEN /\ pc' = "refused"
             /\ UNCHANGED target
        ELSE /\ target' = entry
             /\ pc' = "done"
  /\ UNCHANGED <<rootA, aF, held>>

Check ==
  /\ pc = "check"
  /\ pc' = IF FollowPath \in Inside THEN "use" ELSE "refused"
  /\ UNCHANGED <<rootA, aF, held, target>>

Use ==
  /\ pc = "use"
  /\ target' = FollowPath
  /\ pc' = "done"
  /\ UNCHANGED <<rootA, aF, held>>

Next ==
  \/ SwapDir \/ SwapFile
  \/ LookupA \/ LookupF \/ Open
  \/ Check \/ Use

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ rootA \in {"A", "LinkDir"}
  /\ aF \in {"F", "LinkFile"}
  /\ held \in Entries
  /\ target \in Entries

\* The operation never acts on an object outside the root.
TargetBeneathRoot == target \in Inside \cup {"none"}

====
