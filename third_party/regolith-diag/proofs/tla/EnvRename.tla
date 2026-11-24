---- MODULE EnvRename ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* MemEnv keeps its files in a map from a name to a file
\* (src/env/mem_env.rs, MemFs::files). Any thread may rename or remove a
\* name at any time, and nothing locks the map: each change is one atomic map
\* write. A rename is more than one write, so other calls can land in the
\* middle of it. Whatever lands there, the names must end up as some order of
\* the calls, run one at a time, would leave them, and each call must answer
\* what it would answer in that order. That is the promise checked here
\* (Linearizable).
\*
\* Tiny example, the one a reviewer found. "new" holds file N, "old" holds
\* file O, "third" holds nothing. Thread A renames "new" to "old". Thread B
\* renames "old" to "third". Thread C removes "new".
\*
\* The rename before the fix put N at "old" first (so an open that tries the
\* old name and then the new one always finds the file), then took "new"
\* away. If the take failed, because someone took "new" first, it put O back
\* at "old" and failed. Now let B run after A's put: B moves N from "old" to
\* "third". Let C remove "new". A's take fails; A wants to put O back at
\* "old", but "old" is empty now (B moved N away), and the put-back only
\* replaces N, so it does nothing; A answers "not found". The end: N at
\* "third", O nowhere, A failed, B and C succeeded. In every order of the
\* three calls where A fails, B moves O, not N, so O survives at "third".
\* No order gives this end: O was destroyed by a rename that failed.
\*
\* The fix. A first MARKS "new" as leaving, in one compare-and-swap that only
\* works while "new" still holds N and nobody else marked it. From that write
\* on the rename has happened as far as any other change can tell: a removal
\* of a marked name, or a rename of it, answers "not found". So nothing can
\* take N from "new" before A does, and A never has to undo anything. Then A
\* puts N at "old" and takes it from "new", as before. An open still finds N
\* at "new" while it is marked, so OpenFindsFile in EnvFiles.tla is untouched.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   held      which file each name holds, or "none" (MemFs::files, the
\*             `file` of each Named)
\*   leaving   whether each name is marked by a rename moving its file
\*             away (Named::leaving)
\*   pc        how far each call has got: the step it takes next
\*   moving    the file a rename looked up at its source (the `file` of
\*             MemEnv::rename)
\*   replaced  what the old rename's put displaced at the destination,
\*             which it put back when it failed (Red_Rollback only)
\*   answer    what each call answered: "ok", "notfound", or "none" while
\*             it still runs
\*
\* The three calls are the reviewer's: A = rename("new", "old"),
\* B = rename("old", "third"), C = remove("new"). A larger run adds
\* D = rename("new", "third"), a second rename racing A for the same source.
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Opens. A mark never takes a file away from a name, so it changes
\*     nothing an open sees; EnvFiles.tla (OpenFindsFile) checks opens
\*     racing the put and the take.
\*   - The bytes in a file. A file is only a name here (N or O): a rename
\*     moves a file, it never copies bytes.
\*   - Directories, and rename of a name onto itself (it only looks).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_EnvRename_Green                  mark, put, take; a removal and a
\*                                       rename skip a marked name.
\*   MC_EnvRename_Green_FourCalls        the same, with D added: two renames
\*                                       race for one source.
\*   MC_EnvRename_Green_Moved            the witness: the GREEN run with the
\*                                       claim "no call moves a rename's file
\*                                       on from its new name while that
\*                                       rename is still running" added. It
\*                                       must break: the race happens.
\*   MC_EnvRename_Red_Rollback           the rename before the fix: put,
\*                                       take, and put back on a failed take.
\*                                       Linearizable breaks.
\*   MC_EnvRename_Red_RemoveIgnoresMark  the fix, but a removal takes a
\*                                       marked name too: Linearizable breaks.

\* Functions, sequences, numbers and the size of a set.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* "Green" for the real code, or the name of one planted bug.
  Mode,
  \* TRUE to add the fourth call, D.
  WithD

\* The bug names this model knows.
ASSUME Mode \in {"Green", "Rollback", "RemoveIgnoresMark"}
\* The fourth call is in or out.
ASSUME WithD \in BOOLEAN

\* The names a file can have.
Names == {"new", "old", "third"}
\* The files: N starts at "new", O at "old".
Files == {"N", "O"}
\* The calls, each on its own thread.
Calls == IF WithD THEN {"A", "B", "C", "D"} ELSE {"A", "B", "C"}

\* What kind of call each one is.
Kind(c) == IF c = "C" THEN "remove" ELSE "rename"
\* The name a call takes a file from (for a removal, the name it removes).
Src(c) == CASE c = "A" -> "new" [] c = "B" -> "old" [] c = "C" -> "new" [] c = "D" -> "new"
\* The name a rename puts the file at.
Dst(c) == CASE c = "A" -> "old" [] c = "B" -> "third" [] c = "C" -> "none" [] c = "D" -> "third"

\* Where the names start: N at "new", O at "old", nothing at "third".
Start == [n \in Names |-> CASE n = "new" -> "N" [] n = "old" -> "O" [] OTHER -> "none"]

\* The state that changes from step to step.
VARIABLES
  \* [Names -> Files \cup {"none"}]: the file each name holds.
  held,
  \* [Names -> BOOLEAN]: whether a rename marked the name.
  leaving,
  \* [Calls -> step]: the step each call takes next.
  pc,
  \* [Calls -> Files \cup {"none"}]: the file a rename is moving.
  moving,
  \* [Calls -> Files \cup {"none"}]: what the old rename's put displaced.
  replaced,
  \* [Calls -> {"ok", "notfound", "none"}]: what each call answered.
  answer

\* Every variable, for "nothing else changes".
vars == <<held, leaving, pc, moving, replaced, answer>>

\* The steps a call can be at.
Steps == {"look", "mark", "put", "take", "restore", "done"}

\* Every variable holds the kind of thing it should.
TypeOK ==
  \* Each name holds a file or nothing.
  /\ held \in [Names -> Files \cup {"none"}]
  \* Each name is marked or not.
  /\ leaving \in [Names -> BOOLEAN]
  \* Each call is at one of the steps.
  /\ pc \in [Calls -> Steps]
  \* Each rename moves a file, or none yet.
  /\ moving \in [Calls -> Files \cup {"none"}]
  \* Each old-style rename remembers what it displaced, or nothing.
  /\ replaced \in [Calls -> Files \cup {"none"}]
  \* Each call has answered, or not yet.
  /\ answer \in [Calls -> {"ok", "notfound", "none"}]

\* Nothing has happened yet.
Init ==
  \* The names as they start.
  /\ held = Start
  \* No name is marked.
  /\ leaving = [n \in Names |-> FALSE]
  \* Every call is about to look at its name.
  /\ pc = [c \in Calls |-> "look"]
  \* No rename has looked up a file.
  /\ moving = [c \in Calls |-> "none"]
  \* No put has displaced anything.
  /\ replaced = [c \in Calls |-> "none"]
  \* Nobody has answered.
  /\ answer = [c \in Calls |-> "none"]

-----------------------------------------------------------------------------
\* THE CALLS, one atomic map operation per step.

\* A rename looks at its source (the map's `get`). A name that holds nothing,
\* or that another rename marked, is gone: the rename answers "not found".
\* Otherwise it remembers the file it found.
Look(c) ==
  \* This call is a rename about to look.
  /\ Kind(c) = "rename"
  \* It has not looked yet, or it lost a race and looks again.
  /\ pc[c] = "look"
  \* What it finds at its source.
  /\ IF held[Src(c)] = "none" \/ leaving[Src(c)]
        \* Nothing there, or a file another rename is taking: not found.
        THEN /\ pc' = [pc EXCEPT ![c] = "done"]
             \* It answers "not found".
             /\ answer' = [answer EXCEPT ![c] = "notfound"]
             \* It moves nothing.
             /\ UNCHANGED moving
        \* A file: remember it, and mark the name (or, before the fix, put it
        \* at the destination at once).
        ELSE /\ moving' = [moving EXCEPT ![c] = held[Src(c)]]
             \* The next step: the mark, or the old rename's put.
             /\ pc' = [pc EXCEPT ![c] = IF Mode = "Rollback" THEN "put" ELSE "mark"]
             \* It has not answered yet.
             /\ UNCHANGED answer
  \* The names and the marks are untouched by a look.
  /\ UNCHANGED <<held, leaving, replaced>>

\* The mark (the map's `replace_if` on the source): it works only while the
\* source still holds the file the rename found and nobody marked it. If it
\* works, the rename has happened as far as every other change can tell. If
\* not, someone changed the source first: look again.
Mark(c) ==
  \* This rename looked and found a file.
  /\ pc[c] = "mark"
  \* Whether the source is as it found it.
  /\ IF held[Src(c)] = moving[c] /\ ~leaving[Src(c)]
        \* Yes: mark it, and go on to the put.
        THEN /\ leaving' = [leaving EXCEPT ![Src(c)] = TRUE]
             \* The put comes next.
             /\ pc' = [pc EXCEPT ![c] = "put"]
        \* No: nothing changes, and the rename looks again.
        ELSE /\ UNCHANGED leaving
             \* Back to the look.
             /\ pc' = [pc EXCEPT ![c] = "look"]
  \* The files at each name are untouched by a mark.
  /\ UNCHANGED <<held, moving, replaced, answer>>

\* The put (the map's `insert` at the destination): the file now has both
\* names for a moment, as with a hard link. It replaces whatever the
\* destination held, mark and all.
Put(c) ==
  \* This rename marked its source (or, before the fix, only looked).
  /\ pc[c] = "put"
  \* The destination holds the file.
  /\ held' = [held EXCEPT ![Dst(c)] = moving[c]]
  \* A fresh entry: the destination is not marked.
  /\ leaving' = [leaving EXCEPT ![Dst(c)] = FALSE]
  \* The old rename remembers what it displaced, to put it back on failure.
  /\ replaced' = IF Mode = "Rollback"
                   \* The old rename: what the destination held.
                   THEN [replaced EXCEPT ![c] = held[Dst(c)]]
                   \* The fix keeps nothing to put back.
                   ELSE replaced
  \* The take comes next.
  /\ pc' = [pc EXCEPT ![c] = "take"]
  \* No answer yet, and the file moved is the same.
  /\ UNCHANGED <<moving, answer>>

\* The take (the map's `remove_if` on the source). The fix: only the mark
\* this rename set is taken; if a later rename put another file at the
\* source, that stays. Either way the rename succeeded. Before the fix: the
\* file is taken if the source still holds it, and if not the rename goes on
\* to put the destination back.
Take(c) ==
  \* This rename put its file at the destination.
  /\ pc[c] = "take"
  \* Which code runs.
  /\ IF Mode = "Rollback"
        \* Before the fix: does the source still hold the file?
        THEN IF held[Src(c)] = moving[c]
                \* Yes: take it away and succeed.
                THEN /\ held' = [held EXCEPT ![Src(c)] = "none"]
                     \* The rename is done.
                     /\ pc' = [pc EXCEPT ![c] = "done"]
                     \* It answers "ok".
                     /\ answer' = [answer EXCEPT ![c] = "ok"]
                \* No: someone took the source first; put the destination back.
                ELSE /\ UNCHANGED <<held, answer>>
                     \* The put-back comes next.
                     /\ pc' = [pc EXCEPT ![c] = "restore"]
        \* The fix: take the source only if it still holds this rename's mark.
        ELSE /\ held' = IF held[Src(c)] = moving[c] /\ leaving[Src(c)]
                          \* Its mark is there: the source holds nothing now.
                          THEN [held EXCEPT ![Src(c)] = "none"]
                          \* A later rename put another file there: it stays.
                          ELSE held
             \* The rename is done.
             /\ pc' = [pc EXCEPT ![c] = "done"]
             \* It answers "ok": it happened when it marked the source.
             /\ answer' = [answer EXCEPT ![c] = "ok"]
  \* A name the take empties is not marked any more; others keep theirs.
  /\ leaving' = IF held'[Src(c)] = "none"
                  \* An empty name carries no mark.
                  THEN [leaving EXCEPT ![Src(c)] = FALSE]
                  \* Otherwise the marks stay as they are.
                  ELSE leaving
  \* The file moved and what was displaced are unchanged.
  /\ UNCHANGED <<moving, replaced>>

\* The old rename's put-back (the map's `replace_if` or `remove_if` at the
\* destination): only while the destination still holds the file it put
\* there. Then it looks at the source again.
Restore(c) ==
  \* The old rename's take failed.
  /\ pc[c] = "restore"
  \* The destination gets back what it held, if it still holds this file.
  /\ held' = IF held[Dst(c)] = moving[c]
               \* It does: the destination holds what it held before the put.
               THEN [held EXCEPT ![Dst(c)] = replaced[c]]
               \* Someone moved or replaced it since: nothing is put back.
               ELSE held
  \* Back to the look.
  /\ pc' = [pc EXCEPT ![c] = "look"]
  \* Nothing else changes.
  /\ UNCHANGED <<leaving, moving, replaced, answer>>

\* A removal (the map's `remove_if`): it takes a name that holds a file and
\* is not marked. A marked name is gone already. The bug RemoveIgnoresMark
\* takes a marked name too.
Remove(c) ==
  \* This call is a removal that has not run.
  /\ Kind(c) = "remove"
  \* It runs in one step.
  /\ pc[c] = "look"
  \* Whether the name holds a file this removal may take.
  /\ IF held[Src(c)] # "none" /\ (~leaving[Src(c)] \/ Mode = "RemoveIgnoresMark")
        \* Yes: the name holds nothing now.
        THEN /\ held' = [held EXCEPT ![Src(c)] = "none"]
             \* And it is not marked.
             /\ leaving' = [leaving EXCEPT ![Src(c)] = FALSE]
             \* The removal answers "ok".
             /\ answer' = [answer EXCEPT ![c] = "ok"]
        \* No: nothing changes, and it answers "not found".
        ELSE /\ UNCHANGED <<held, leaving>>
             \* The answer.
             /\ answer' = [answer EXCEPT ![c] = "notfound"]
  \* The removal is done.
  /\ pc' = [pc EXCEPT ![c] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<moving, replaced>>

\* Some call takes its next step.
Next == \E c \in Calls : Look(c) \/ Mark(c) \/ Put(c) \/ Take(c) \/ Restore(c) \/ Remove(c)

\* Every run: the start, then steps.
Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* THE PROMISE

\* One call run alone on the names `m`: the names after it, and its answer.
OneAtATime(m, c) ==
  \* A removal.
  IF Kind(c) = "remove"
    \* It empties the name if it held a file.
    THEN IF m[Src(c)] = "none"
           \* Nothing to remove.
           THEN [names |-> m, said |-> "notfound"]
           \* The name holds nothing now.
           ELSE [names |-> [m EXCEPT ![Src(c)] = "none"], said |-> "ok"]
    \* A rename: the file moves from its source to its destination.
    ELSE IF m[Src(c)] = "none"
           \* Nothing to move.
           THEN [names |-> m, said |-> "notfound"]
           \* The destination holds the file, the source nothing.
           ELSE [names |-> [m EXCEPT ![Dst(c)] = m[Src(c)], ![Src(c)] = "none"],
                 said |-> "ok"]

\* Run the calls of `order`, from its i-th on, one at a time from the names
\* `m`, with the answers `said` so far.
RECURSIVE InOrder(_, _, _, _)
\* The names and answers when all of `order` from i on has run.
InOrder(m, order, i, said) ==
  \* All run: what they leave.
  IF i > Len(order)
    \* Nothing left to run.
    THEN [names |-> m, said |-> said]
    \* Run the i-th, then the rest.
    ELSE LET step == OneAtATime(m, order[i])
         \* The rest, from the names it leaves, with its answer added.
         IN InOrder(step.names, order, i + 1, [said EXCEPT ![order[i]] = step.said])

\* How many calls there are.
N == Cardinality(Calls)
\* Every order the calls could run in, one at a time.
Orders == {o \in [1..N -> Calls] : \A i, j \in 1..N : i # j => o[i] # o[j]}

\* Every call has answered.
AllDone == \A c \in Calls : pc[c] = "done"

\* Once every call has answered, some order of the calls, run one at a time,
\* gives the same answers and leaves the names as they are. Rules out the
\* story's end: O destroyed by a rename that answered "not found".
Linearizable ==
  \* Only an end is judged; in between, a rename may be half done.
  AllDone =>
    \* Some order of the calls.
    \E o \in Orders :
      \* Run one at a time from the start, they leave what the run did.
      LET seq == InOrder(Start, o, 1, [c \in Calls |-> "none"])
      \* Same names, same answers.
      IN seq.names = held /\ seq.said = answer

\* The witness, which must break in GREEN: "B never moves A's file on from
\* the name A put it at while A is still running". It breaks when B moves N
\* from "old" to "third" before A took "new" away: the race the fix is for.
NeverMovedOn == ~(pc["A"] = "take" /\ held["third"] = "N")

====
