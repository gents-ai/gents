---- MODULE ShellSendLandings ----
\* lean: none
\* crates: gents-cli
(***************************************************************************)
(* What the CLI shell's source tells after a tab's first send, as the      *)
(* shell tests observe it (`a_first_send_starts_a_session_then_a_turn`,    *)
(* `a_reply_streams_as_deltas`).                                           *)
(*                                                                          *)
(* Actors and their atomic steps, from `commands/shell/source.rs`:         *)
(*  - Source loop: handles one intent or one watched message at a time.   *)
(*    The send handler tells `Started`, awaits the send operation (whose   *)
(*    commits are `Commits` chat-feed changes: the session and the         *)
(*    request), then spawns the work reread for the newly open session and *)
(*    tells `Turn`. No watched message is handled while it awaits.         *)
(*  - Chat feed forwarder: turns each committed change into one            *)
(*    `Changed(Chat)` on the watched channel.                              *)
(*  - Work read (`reread_work`): reads the jobs and background calls and   *)
(*    sends `Read`; one at a time.                                         *)
(*  - Chat read (`reread_chat`): spawned on a `Changed(Chat)`; one at a    *)
(*    time, a change while it is out asks for one more. It reads the store *)
(*    and the open session's subagents and sends `Chatted`.                *)
(* Landing a `Read` tells `Jobs`, `Background` and the session's running   *)
(* rows (`RunningW`). Landing a `Chatted` tells the sessions list when it  *)
(* changed and the session's running rows when its subagents changed       *)
(* (`RunningC`); the first chat read changes both, later ones neither.    *)
(*                                                                          *)
(* The two reads race: nothing orders the work read before the chat read.  *)
(* What the product guarantees is `LandingsWhole`: the loop tells each     *)
(* landing within one handler, so after `Turn` the updates are the two    *)
(* landings, each whole, in either order.                                  *)
(*                                                                          *)
(* `Observer` is what the test accepts after `Turn`: "either" landing      *)
(* first, or "work_first" (the test before the fix, which read the next   *)
(* three updates as the work reread).                                      *)
(*                                                                          *)
(* GREEN `MCShellSendLandings`. RED: `WorkFirst` (the CI failure, a chat   *)
(* landing ahead of the work reread) and `TaskTells` (the reads tell their *)
(* updates themselves instead of through the loop, so landings interleave). *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
  Commits,    \* chat-feed changes the send operation commits
  LoopTells,  \* landings are told by the source loop, one message at a time
  Observer    \* "either" | "work_first"

VARIABLES
  out,        \* updates told to the shell, in order
  queue,      \* the watched channel
  phase,      \* send handler: "idle" | "awaiting" | "returned"
  commits,    \* changes committed by the send operation
  events,     \* committed changes the forwarder has not yet sent
  work,       \* work read: "none" | "reading" | "sent" | "landed"
  chat,       \* chat read: "idle" | "reading" | "sent"
  chatAgain,  \* a change arrived while the chat read was out
  listed,     \* the sessions list told includes the session
  rowsTold,   \* the session's subagent rows have been told
  telling     \* per read, updates it is telling itself (`TaskTells`)

vars == <<out, queue, phase, commits, events, work, chat, chatAgain, listed,
          rowsTold, telling>>

WorkLanding == <<"Jobs", "Background", "RunningW">>
ChatLanding == <<"Sessions", "RunningC">>

Reads == {"work", "chat"}

ChatTells ==
  (IF listed THEN <<>> ELSE <<"Sessions">>) \o
  (IF rowsTold THEN <<>> ELSE <<"RunningC">>)

Init ==
  /\ out = <<>>
  /\ queue = <<>>
  /\ phase = "idle"
  /\ commits = 0
  /\ events = 0
  /\ work = "none"
  /\ chat = "idle"
  /\ chatAgain = FALSE
  /\ listed = FALSE
  /\ rowsTold = FALSE
  /\ telling = [r \in Reads |-> <<>>]

Send ==
  /\ phase = "idle"
  /\ out' = Append(out, "Started")
  /\ phase' = "awaiting"
  /\ UNCHANGED <<queue, commits, events, work, chat, chatAgain, listed, rowsTold, telling>>

Commit ==
  /\ phase = "awaiting" /\ commits < Commits
  /\ commits' = commits + 1
  /\ events' = events + 1
  /\ UNCHANGED <<out, queue, phase, work, chat, chatAgain, listed, rowsTold, telling>>

Return ==
  /\ phase = "awaiting" /\ commits = Commits
  /\ work' = "reading"
  /\ out' = Append(out, "Turn")
  /\ phase' = "returned"
  /\ UNCHANGED <<queue, commits, events, chat, chatAgain, listed, rowsTold, telling>>

Forward ==
  /\ events > 0
  /\ queue' = Append(queue, "changed")
  /\ events' = events - 1
  /\ UNCHANGED <<out, phase, commits, work, chat, chatAgain, listed, rowsTold, telling>>

WorkRead ==
  /\ work = "reading"
  /\ IF LoopTells
       THEN /\ queue' = Append(queue, "read") /\ UNCHANGED telling
       ELSE /\ telling' = [telling EXCEPT !["work"] = @ \o WorkLanding]
            /\ UNCHANGED queue
  /\ work' = IF LoopTells THEN "sent" ELSE "landed"
  /\ UNCHANGED <<out, phase, commits, events, chat, chatAgain, listed, rowsTold>>

ChatRead ==
  /\ chat = "reading"
  /\ IF LoopTells
       THEN /\ queue' = Append(queue, "chatted")
            /\ chat' = "sent"
            /\ UNCHANGED <<telling, listed, rowsTold, chatAgain>>
       ELSE /\ telling' = [telling EXCEPT !["chat"] = @ \o ChatTells]
            /\ listed' = TRUE /\ rowsTold' = TRUE
            /\ chat' = IF chatAgain THEN "reading" ELSE "idle"
            /\ chatAgain' = FALSE
            /\ UNCHANGED queue
  /\ UNCHANGED <<out, phase, commits, events, work>>

\* A read telling its own updates: one update per step, so another read's
\* updates can land between them.
TaskTell ==
  \E r \in Reads :
    /\ telling[r] # <<>>
    /\ out' = Append(out, Head(telling[r]))
    /\ telling' = [telling EXCEPT ![r] = Tail(@)]
  /\ UNCHANGED <<queue, phase, commits, events, work, chat, chatAgain, listed, rowsTold>>

Handle ==
  /\ phase # "awaiting" /\ queue # <<>>
  /\ queue' = Tail(queue)
  /\ CASE Head(queue) = "changed" ->
            /\ IF chat = "idle"
                 THEN chat' = "reading" /\ UNCHANGED chatAgain
                 ELSE chatAgain' = TRUE /\ UNCHANGED chat
            /\ UNCHANGED <<out, work, listed, rowsTold>>
       [] Head(queue) = "read" ->
            /\ out' = out \o WorkLanding
            /\ work' = "landed"
            /\ UNCHANGED <<chat, chatAgain, listed, rowsTold>>
       [] Head(queue) = "chatted" ->
            /\ out' = out \o ChatTells
            /\ listed' = TRUE /\ rowsTold' = TRUE
            /\ chat' = IF chatAgain THEN "reading" ELSE "idle"
            /\ chatAgain' = FALSE
            /\ UNCHANGED work
  /\ UNCHANGED <<phase, commits, events, telling>>

Next == Send \/ Commit \/ Return \/ Forward \/ WorkRead \/ ChatRead \/ TaskTell \/ Handle

Spec == Init /\ [][Next]_vars

Quiescent ==
  /\ phase = "returned" /\ queue = <<>> /\ events = 0
  /\ \A r \in Reads : telling[r] = <<>>
  /\ work = "landed" /\ chat = "idle"

TypeOK ==
  /\ phase \in {"idle", "awaiting", "returned"}
  /\ commits \in 0..Commits
  /\ events \in 0..Commits
  /\ work \in {"none", "reading", "sent", "landed"}
  /\ chat \in {"idle", "reading", "sent"}
  /\ chatAgain \in BOOLEAN
  /\ listed \in BOOLEAN
  /\ rowsTold \in BOOLEAN

After == SubSeq(out, 3, Len(out))

\* After `Started` and `Turn` come exactly the two landings, each whole.
LandingsWhole ==
  Quiescent =>
    /\ SubSeq(out, 1, 2) = <<"Started", "Turn">>
    /\ After \in {WorkLanding \o ChatLanding, ChatLanding \o WorkLanding}

Accepted ==
  CASE Observer = "either" ->
         After \in {WorkLanding \o ChatLanding, ChatLanding \o WorkLanding}
    [] Observer = "work_first" ->
         Len(After) >= 3 /\ SubSeq(After, 1, 3) = WorkLanding

\* The test passes on every schedule the product allows.
ObserverAgrees == Quiescent => Accepted
====
