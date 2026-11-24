---- MODULE GoalBlockedPause ----
\* lean: none
\* crates: gents-automation
\* models: Goals.decide, Goals.resumeOnMessage
(***************************************************************************)
(* One blocked report pauses a Goal; the user's next message resumes it.  *)
(*                                                                          *)
(* Actors and their atomic steps:                                           *)
(*   Agent   its goal turn reports blocked: `update_goal` writes status     *)
(*           blocked in the turn's own transaction, before the turn ends    *)
(*           (operations::goal::apply_set_in_txn, CAS on status and         *)
(*           continuation sequence).                                        *)
(*   Source  the Goal source reads the Goal once the session is idle,       *)
(*           decides with Lean `Goals.decide` (continue only while active), *)
(*           and claims the continuation in a later transaction             *)
(*           (`claim_continuation_from_access`, CAS on status and           *)
(*           sequence: `ClaimFenced`).                                      *)
(*   User    sends a message; core claims it once the session is idle and, *)
(*           in the claim transaction, the Goals hook resumes a blocked     *)
(*           Goal (Lean `Goals.resumeOnMessage`) by a write fenced on the   *)
(*           revision it was shown (`ResumeFenced`). The user may also stop *)
(*           the Goal at any time; a stop is final.                         *)
(* The database validates reads by document id; both fences are reads of   *)
(* the Goal by id, so a write that commits between read and write aborts   *)
(* the later transaction.                                                   *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxTurns,       \* continuation turns the source may publish
  ClaimFenced,    \* the continuation claim is fenced on the Goal it decided from
  ResumeFenced    \* the message resume is fenced on the Goal it was shown

VARIABLES
  goal,       \* [status, rev]
  turn,       \* the running goal turn: "none" | "running"
  turns,      \* continuation turns published
  src,        \* the source's read: [pc, status, rev]
  msg,        \* the user's message: "none" | "queued" | "claimed" | "running" | "done"
  shown,      \* the Goal the message claim was shown: [status, rev]
  badTurn,    \* ghost: a continuation published while the Goal was not active
  stopped     \* ghost: the user stopped the Goal

vars == <<goal, turn, turns, src, msg, shown, badTurn, stopped>>

NoRead == [pc |-> "idle", status |-> "none", rev |-> 0]

Init ==
  /\ goal = [status |-> "active", rev |-> 0]
  /\ turn = "running"
  /\ turns = 1
  /\ src = NoRead
  /\ msg = "none"
  /\ shown = [status |-> "none", rev |-> 0]
  /\ badTurn = FALSE
  /\ stopped = FALSE

Idle == turn = "none" /\ msg \notin {"claimed", "running"}

\* Lean `Goals.decide` for a completed turn with activity on an idle
\* session: an active Goal continues; every paused or ended Goal does not.
Continues(status) == status = "active"

\* Lean `Goals.resumeOnMessage`: only a blocked Goal resumes.
ResumedBy(status) == IF status = "blocked" THEN "active" ELSE status

AgentBlock ==
  /\ turn = "running" /\ goal.status = "active"
  /\ goal' = [status |-> "blocked", rev |-> goal.rev + 1]
  /\ UNCHANGED <<turn, turns, src, msg, shown, badTurn, stopped>>

TurnEnds ==
  /\ turn = "running"
  /\ turn' = "none"
  /\ UNCHANGED <<goal, turns, src, msg, shown, badTurn, stopped>>

SourceRead ==
  /\ src.pc = "idle" /\ Idle
  /\ src' = [pc |-> "read", status |-> goal.status, rev |-> goal.rev]
  /\ UNCHANGED <<goal, turn, turns, msg, shown, badTurn, stopped>>

SourceClaim ==
  /\ src.pc = "read"
  /\ src' = NoRead
  /\ IF Continues(src.status) /\ turns < MaxTurns /\ Idle
        /\ (ClaimFenced => goal.rev = src.rev)
       THEN /\ turn' = "running"
            /\ turns' = turns + 1
            /\ badTurn' = (badTurn \/ goal.status # "active")
       ELSE UNCHANGED <<turn, turns, badTurn>>
  /\ UNCHANGED <<goal, msg, shown, stopped>>

UserSends ==
  /\ msg = "none"
  /\ msg' = "queued"
  /\ UNCHANGED <<goal, turn, turns, src, shown, badTurn, stopped>>

\* The claim transaction: fenced, the hook's read and write are one atomic
\* step; unfenced, the write applies what an earlier read was shown.
MessageClaim ==
  /\ msg = "queued" /\ Idle
  /\ IF ResumeFenced
       THEN /\ msg' = "running"
            /\ goal' = IF ResumedBy(goal.status) # goal.status
                         THEN [status |-> ResumedBy(goal.status), rev |-> goal.rev + 1]
                         ELSE goal
            /\ UNCHANGED shown
       ELSE /\ msg' = "claimed"
            /\ shown' = goal
            /\ UNCHANGED goal
  /\ UNCHANGED <<turn, turns, src, badTurn, stopped>>

MessageResumeWrite ==
  /\ msg = "claimed"
  /\ msg' = "running"
  /\ goal' = IF ResumedBy(shown.status) # shown.status
               THEN [status |-> ResumedBy(shown.status), rev |-> goal.rev + 1]
               ELSE goal
  /\ UNCHANGED <<turn, turns, src, shown, badTurn, stopped>>

MessageEnds ==
  /\ msg = "running"
  /\ msg' = "done"
  /\ UNCHANGED <<goal, turn, turns, src, shown, badTurn, stopped>>

UserStops ==
  /\ goal.status # "stopped"
  /\ goal' = [status |-> "stopped", rev |-> goal.rev + 1]
  /\ stopped' = TRUE
  /\ UNCHANGED <<turn, turns, src, msg, shown, badTurn>>

Next ==
  \/ AgentBlock \/ TurnEnds \/ SourceRead \/ SourceClaim
  \/ UserSends \/ MessageClaim \/ MessageResumeWrite \/ MessageEnds
  \/ UserStops

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ goal.status \in {"active", "blocked", "stopped"}
  /\ turn \in {"none", "running"}
  /\ msg \in {"none", "queued", "claimed", "running", "done"}

\* No continuation turn is published for a Goal that is blocked or stopped.
NoTurnUnlessActive == ~badTurn

\* A stopped Goal stays stopped: neither a message nor the source revives it.
StopIsFinal == stopped => goal.status = "stopped"

====
