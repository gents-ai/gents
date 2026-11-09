---- MODULE ClientSubmissionContention ----
\* A User's optimistic chat send racing its session's active turn.
\*
\* Client (Lean `ClientOptimisticSend`): the client picks the request
\* identity when the person sends, shows the message pending and keeps its
\* payload. A reply confirms it; a refusal, or no reply by the client's
\* deadline, shows it failed with Retry. Retry resends the same payload under
\* the same identity. The payload is released once the message is confirmed
\* or discarded.
\*
\* Runtime: each send is two DefraDB transactions run by the User through the
\* signed client endpoint. The reservation (`request.client.reserve_session`,
\* `reserve_client_targets`) reads the target `AgentSession` document and the
\* selected `Agent` to bind the physical targets the User then signs. The
\* submission (`request.client.submit`, `submit_client_request`) reads the
\* same documents, then creates the User-signed `AgentRequest`. While the
\* session's turn runs, its working Agent publishes each message in a
\* transaction that also refreshes the `AgentSession` observation
\* (`patch_session_in_txn`, `last_activity_at`), so either transaction's
\* commit aborts with a DefraDB conflict when that refresh lands after its
\* read. A client-signed write may be rebuilt only after the native owner
\* confirms the attempt did not commit (Lean `Identity.Configuration
\* .clientSignedWriteMayRetry`); `Redrive` and `ReserveRedrive` select that
\* rebuild, FALSE being the code before each fix.
\*
\* The create is idempotent on the request identity: the runtime's unique
\* request key (`AgentRequest.retry_key`, `@index(unique: true)`;
\* `send_client_request`) makes a repeat of one identity the same request, so
\* `rows` is a set of identities. Two sends of one identity may be on the wire
\* at once (a retry after a deadline while the first is still committing).
\*
\* `Publications` bounds the Agent's session refreshes that land inside one
\* attempt series, and `MaxSends` bounds the person's retries.
\* REDs: `RetryFreshId` retries under a new identity, which duplicates the turn
\* when the first send landed after its reply was lost; `PayloadLost` lets the
\* client drop the payload of an unconfirmed message, which leaves a failed
\* message with no Retry.
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Publications,  \* Agent session refreshes landing in one attempt series
  Attempts,      \* bounded attempts (TRANSACT_CONFLICT_MAX_RETRIES + 1)
  Redrive,       \* signed-conflict rebuild of the submission
  ReserveRedrive,\* signed-conflict rebuild of the target reservation
  LostReply,     \* a reply may be lost on its way back
  Refusals,      \* the runtime or transport may fail a send before it commits
  MaxSends,      \* sends of the one authored message (first plus retries)
  RetryFreshId,  \* RED: retry under a new request identity
  PayloadLost    \* RED: the client may drop an unconfirmed message's payload

ASSUME /\ Publications \in Nat
       /\ Attempts \in Nat \ {0}
       /\ Redrive \in BOOLEAN
       /\ ReserveRedrive \in BOOLEAN
       /\ LostReply \in BOOLEAN
       /\ Refusals \in BOOLEAN
       /\ MaxSends \in Nat \ {0}
       /\ RetryFreshId \in BOOLEAN
       /\ PayloadLost \in BOOLEAN

Ids == 1..MaxSends

VARIABLES
  sessVer,   \* committed version of the AgentSession document
  agentLeft, \* Agent session refreshes still to land
  msg,       \* "none" | "pending" | "confirmed" | "failed" | "discarded"
  payload,   \* the client still holds the authored payload
  cur,       \* the request identity the client sends under
  sends,     \* sends begun for this authored message
  wire,      \* sends in the runtime: [id, stage, snap, tries]
  replies,   \* replies on the way back: [id, ok]
  rows,      \* committed AgentRequest identities
  conflictLost \* a send was lost to the turn's own writes

vars == <<sessVer, agentLeft, msg, payload, cur, sends, wire, replies, rows, conflictLost>>

Init ==
  /\ sessVer = 0
  /\ agentLeft = Publications
  /\ msg = "none"
  /\ payload = FALSE
  /\ cur = 1
  /\ sends = 0
  /\ wire = <<>>
  /\ replies = <<>>
  /\ rows = {}
  /\ conflictLost = FALSE

Remove(s, i) == SubSeq(s, 1, i - 1) \o SubSeq(s, i + 1, Len(s))

Send(id) ==
  /\ wire' = Append(wire, [id |-> id, stage |-> "reserving", snap |-> sessVer, tries |-> 1])
  /\ sends' = sends + 1

\* The working Agent publishes a message and refreshes the observation.
AgentPublish ==
  /\ agentLeft > 0
  /\ sessVer' = sessVer + 1
  /\ agentLeft' = agentLeft - 1
  /\ UNCHANGED <<msg, payload, cur, sends, wire, replies, rows, conflictLost>>

\* The person sends: pending at once, the payload kept, one signed create.
Compose ==
  /\ msg = "none"
  /\ msg' = "pending"
  /\ payload' = TRUE
  /\ Send(cur)
  /\ UNCHANGED <<sessVer, agentLeft, cur, replies, rows, conflictLost>>

\* A reservation commit binds the targets when the read session is unchanged,
\* and the submission then reads it; a confirmed abort is rebuilt while
\* attempts remain, and otherwise the send fails back to the client.
ReserveCommit(i) ==
  LET w == wire[i] IN
  /\ w.stage = "reserving"
  /\ IF w.snap = sessVer
       THEN /\ wire' = [wire EXCEPT ![i] = [w EXCEPT !.stage = "open", !.snap = sessVer, !.tries = 1]]
            /\ UNCHANGED <<replies, conflictLost>>
       ELSE IF ReserveRedrive /\ w.tries < Attempts
              THEN /\ wire' = [wire EXCEPT ![i] = [w EXCEPT !.snap = sessVer, !.tries = w.tries + 1]]
                   /\ UNCHANGED <<replies, conflictLost>>
              ELSE /\ wire' = Remove(wire, i)
                   /\ replies' = Append(replies, [id |-> w.id, ok |-> FALSE])
                   /\ conflictLost' = TRUE
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, rows>>

\* The submission commits the idempotent create when the read session is
\* unchanged; a confirmed abort is rebuilt while attempts remain.
SubmitCommit(i) ==
  LET w == wire[i] IN
  /\ w.stage = "open"
  /\ IF w.snap = sessVer
       THEN /\ wire' = Remove(wire, i)
            /\ rows' = rows \cup {w.id}
            /\ replies' = Append(replies, [id |-> w.id, ok |-> TRUE])
            /\ UNCHANGED conflictLost
       ELSE IF Redrive /\ w.tries < Attempts
              THEN /\ wire' = [wire EXCEPT ![i] = [w EXCEPT !.snap = sessVer, !.tries = w.tries + 1]]
                   /\ UNCHANGED <<rows, replies, conflictLost>>
              ELSE /\ wire' = Remove(wire, i)
                   /\ replies' = Append(replies, [id |-> w.id, ok |-> FALSE])
                   /\ conflictLost' = TRUE
                   /\ UNCHANGED rows
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends>>

\* The runtime refuses the send, or the transport fails before it commits.
Refuse(i) ==
  /\ Refusals
  /\ wire' = Remove(wire, i)
  /\ replies' = Append(replies, [id |-> wire[i].id, ok |-> FALSE])
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, rows, conflictLost>>

LoseReply(j) ==
  /\ LostReply
  /\ replies' = Remove(replies, j)
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, wire, rows, conflictLost>>

\* A reply is applied even after the deadline: a committed reply confirms.
Receive(j) ==
  LET r == replies[j] IN
  /\ replies' = Remove(replies, j)
  /\ IF r.ok
       THEN IF msg = "discarded"
              THEN UNCHANGED <<msg, payload>>
              ELSE /\ msg' = "confirmed"
                   /\ payload' = FALSE
       ELSE IF msg = "pending"
              THEN /\ msg' = "failed"
                   /\ UNCHANGED payload
              ELSE UNCHANGED <<msg, payload>>
  /\ UNCHANGED <<sessVer, agentLeft, cur, sends, wire, rows, conflictLost>>

\* The client's deadline passes with no reply.
Timeout ==
  /\ msg = "pending"
  /\ msg' = "failed"
  /\ UNCHANGED <<sessVer, agentLeft, payload, cur, sends, wire, replies, rows, conflictLost>>

\* Retry resends the kept payload under the same identity.
Retry ==
  /\ msg = "failed"
  /\ payload
  /\ sends < MaxSends
  /\ msg' = "pending"
  /\ cur' = IF RetryFreshId THEN cur + 1 ELSE cur
  /\ Send(cur')
  /\ UNCHANGED <<sessVer, agentLeft, payload, replies, rows, conflictLost>>

Discard ==
  /\ msg = "failed"
  /\ msg' = "discarded"
  /\ payload' = FALSE
  /\ UNCHANGED <<sessVer, agentLeft, cur, sends, wire, replies, rows, conflictLost>>

\* RED: the client drops the payload of an unconfirmed message.
DropPayload ==
  /\ PayloadLost
  /\ msg \in {"pending", "failed"}
  /\ payload
  /\ payload' = FALSE
  /\ UNCHANGED <<sessVer, agentLeft, msg, cur, sends, wire, replies, rows, conflictLost>>

Runtime ==
  \/ \E i \in 1..Len(wire) : ReserveCommit(i) \/ SubmitCommit(i)
  \/ \E i \in 1..Len(wire) : Refuse(i)

Next ==
  \/ AgentPublish
  \/ Compose
  \/ Runtime
  \/ \E j \in 1..Len(replies) : LoseReply(j) \/ Receive(j)
  \/ Timeout
  \/ Retry
  \/ Discard
  \/ DropPayload

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(Compose)
  /\ WF_vars(\E i \in 1..Len(wire) : ReserveCommit(i) \/ SubmitCommit(i))
  /\ WF_vars(\E j \in 1..Len(replies) : Receive(j))
  /\ WF_vars(Timeout)

TypeOK ==
  /\ sessVer \in 0..Publications
  /\ agentLeft \in 0..Publications
  /\ msg \in {"none", "pending", "confirmed", "failed", "discarded"}
  /\ payload \in BOOLEAN
  /\ cur \in Ids
  /\ sends \in 0..MaxSends
  /\ Len(wire) <= MaxSends
  /\ Len(replies) <= MaxSends
  /\ rows \subseteq Ids
  /\ conflictLost \in BOOLEAN

\* A valid send during a turn is never lost to the turn's own writes.
NoConflictLoss == ~conflictLost

\* No interleaving of send, lost reply, deadline and retry creates a second
\* request for one authored message.
CreatedOnce == Cardinality(rows) <= 1

\* The client keeps the payload of every unconfirmed message.
RetainedUntilConfirmed == msg \in {"pending", "failed"} => payload

\* A failed message always offers Retry: nothing is stuck.
FailedOffersRetry == msg = "failed" => payload

\* Pending always resolves: confirmed, or failed with Retry.
PendingResolves == [](msg = "pending" => <>(msg \in {"confirmed", "failed"}))

\* The authored message leaves pending.
EverySendSettles == <>(msg \in {"confirmed", "failed", "discarded"})

====
