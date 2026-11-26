---- MODULE ClientSubmissionContention ----
\* lean: TransactionReplay.disposition, Identity.Configuration.clientSignedWriteMayRetry
\* crates: gents-session, gents-store
\* models: TransactionFence.DeclaredFence, TransactionFence.a_bound_session_stays_bound
\* A User's optimistic chat send racing its session's active turn.
\*
\* Client (Lean `ClientOptimisticSend`): the client picks the request
\* identity when the person sends, shows the message pending and keeps its
\* payload. A reply confirms it; a refusal, or no reply by the client's
\* deadline, shows it failed with Retry. Retry resends the same payload under
\* the same identity. The payload is released once the message is confirmed
\* or discarded.
\*
\* Runtime: a chat send (`Reserves` FALSE) is one DefraDB transaction run by
\* the User through the signed client endpoint. Preparing the send only reads
\* the target `AgentSession` and the selected `Agent` (or derives a new
\* session's document id from its signed first block) to bind the physical
\* targets the User signs; the submission (`request.client.submit`,
\* `submit_client_request`) reads the same documents, then creates the
\* session when it is new and the User-signed `AgentRequest` (Lean
\* `AgentSession.admitRequest`). A reserving send (`Reserves` TRUE, the
\* Goal path) first commits the session in a reservation transaction of its
\* own (`request.client.reserve_session`, `reserve_client_targets`) and then
\* submits. While the session's turn runs, its working Agent publishes each
\* message in a transaction that also refreshes the `AgentSession`
\* observation (`patch_session_in_txn`, `last_activity_at`), so a
\* transaction's commit aborts with a DefraDB conflict when that refresh
\* lands after its read. A client-signed write may be rebuilt only after the
\* native owner confirms the attempt did not commit (Lean `Identity
\* .Configuration.clientSignedWriteMayRetry`); `Redrive` and
\* `ReserveRedrive` select that rebuild, FALSE being the code before each
\* fix.
\*
\* The create is idempotent on the request identity (`send_client_request`:
\* the unique key derived from the identity, the held lookup by
\* `request_id`, and `stage_client_request_in_txn`'s held check), so `rows`
\* is a set of identities. Two sends of one identity may be on the wire at
\* once (a retry after a deadline while the first is still committing). The
\* runtime refuses only an identity it does not hold: the held lookup runs
\* before any other admission check (Lean `held_resend_never_refused`). The
\* transport may fail a send before it reaches the runtime, held or not.
\* `Timeout` is the client deadline `CHAT_SEND_DEADLINE_MS`.
\*
\* A submission into a session whose relationships are already published
\* (`PublishedBinding`) reads the session's binding, its requester and Agent,
\* before the transaction (`bound_published_session`), and the transaction
\* reads only the Agent. The Agent's refreshes then never abort it, and the
\* store orders nothing between that read and the commit.
\*
\* Premise (Lean `TransactionFence.DeclaredFence.immutableBinding`, the fence
\* that send names): no writer changes a published session's binding.
\* `requester_did` and `agent_did` are `@immutable`, `relationships_pending`
\* is written true only at a session's create, and no production path
\* deletes a session, so a bound session stays bound whatever its other
\* writers commit (`a_bound_session_stays_bound`). Here `binding` changes only
\* under `BindingChanges`, the RED that removes the premise. Without
\* `PublishedBinding` the transaction reads the session, and a binding change,
\* a write to the session, aborts it like any refresh.
\*
\* `Publications` bounds the Agent's session refreshes that land inside one
\* attempt series, and `MaxSends` bounds the person's retries.
\* REDs: `RetryFreshId` retries under a new identity, which duplicates the turn
\* when the first send landed after its reply was lost; `PayloadLost` lets the
\* client drop the payload of an unconfirmed message, which leaves a failed
\* message with no Retry; `BindingChanges` lets a published session's binding
\* change, which commits a request under the binding it no longer has.
EXTENDS Naturals, Sequences, FiniteSets, LeanDecision_TransactionReplay,
        LeanDecision_Identity_Configuration

CONSTANTS
  Reserves,      \* the send commits a reservation before its submission
  Publications,  \* Agent session refreshes landing in one attempt series
  Attempts,      \* bounded attempts (TRANSACT_CONFLICT_MAX_RETRIES + 1)
  Redrive,       \* signed-conflict rebuild of the submission
  ReserveRedrive,\* signed-conflict rebuild of the target reservation
  LostReply,     \* a reply may be lost on its way back
  Refusals,      \* the runtime or transport may fail a send before it commits
  MaxSends,      \* sends of the one authored message (first plus retries)
  RetryFreshId,  \* RED: retry under a new request identity
  PayloadLost,   \* RED: the client may drop an unconfirmed message's payload
  PublishedBinding, \* the submission reads the binding before its transaction
  BindingChanges \* RED: a published session's binding may change

ASSUME /\ Reserves \in BOOLEAN
       /\ Publications \in Nat
       /\ Attempts \in Nat \ {0}
       /\ Redrive \in BOOLEAN
       /\ ReserveRedrive \in BOOLEAN
       /\ LostReply \in BOOLEAN
       /\ Refusals \in BOOLEAN
       /\ MaxSends \in Nat \ {0}
       /\ RetryFreshId \in BOOLEAN
       /\ PayloadLost \in BOOLEAN
       /\ PublishedBinding \in BOOLEAN
       /\ BindingChanges \in BOOLEAN

Ids == 1..MaxSends

VARIABLES
  sessVer,   \* committed version of the AgentSession document
  agentLeft, \* Agent session refreshes still to land
  msg,       \* "none" | "pending" | "confirmed" | "failed" | "discarded"
  payload,   \* the client still holds the authored payload
  cur,       \* the request identity the client sends under
  sends,     \* sends begun for this authored message
  wire,      \* sends in the runtime: [id, stage, snap, bind, tries]
  replies,   \* replies on the way back: [id, ok]
  rows,      \* committed AgentRequest identities
  conflictLost, \* a send was lost to the turn's own writes
  binding,   \* the session's binding (requester and Agent), by version
  staleBinding \* a request committed under a binding the session no longer has

vars == <<sessVer, agentLeft, msg, payload, cur, sends, wire, replies, rows, conflictLost,
          binding, staleBinding>>

\* Every action but the binding change leaves the binding alone.
Bound == UNCHANGED <<binding, staleBinding>>

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
  /\ binding = 0
  /\ staleBinding = FALSE

Remove(s, i) == SubSeq(s, 1, i - 1) \o SubSeq(s, i + 1, Len(s))

Send(id) ==
  /\ wire' = Append(wire, [id |-> id, stage |-> IF Reserves THEN "reserving" ELSE "open",
                           snap |-> sessVer, bind |-> binding, tries |-> 1])
  /\ sends' = sends + 1

\* The working Agent publishes a message and refreshes the observation.
AgentPublish ==
  /\ agentLeft > 0
  /\ sessVer' = sessVer + 1
  /\ agentLeft' = agentLeft - 1
  /\ UNCHANGED <<msg, payload, cur, sends, wire, replies, rows, conflictLost>>
  /\ Bound

\* RED: the session's binding changes, a write to the session document.
BindingChange ==
  /\ BindingChanges
  /\ binding = 0
  /\ binding' = 1
  /\ sessVer' = sessVer + 1
  /\ UNCHANGED <<agentLeft, msg, payload, cur, sends, wire, replies, rows, conflictLost,
                 staleBinding>>

\* The person sends: pending at once, the payload kept, one signed create.
Compose ==
  /\ msg = "none"
  /\ msg' = "pending"
  /\ payload' = TRUE
  /\ Send(cur)
  /\ UNCHANGED <<sessVer, agentLeft, cur, replies, rows, conflictLost>>
  /\ Bound

\* A commit that aborted on the store's conflict, its signed write sent and
\* the abort confirmed, is decided by Lean `TransactionReplay.disposition`;
\* without the rebuild the entry is an ordinary conflict retry.
Rebuilds(rebuild, w) ==
  disposition(IF rebuild THEN "signed_conflict_rebuild" ELSE "conflict_retry",
              "commit", "conflict", TRUE, TRUE, w.tries < Attempts) = "retry"

\* The two Lean owners this spec names agree on the aborts it rebuilds: the
\* rebuild mode is the caller's replay authority.
ASSUME \A rebuild, remain \in BOOLEAN :
         (disposition(IF rebuild THEN "signed_conflict_rebuild" ELSE "conflict_retry",
                      "commit", "conflict", TRUE, TRUE, remain) = "retry")
           = (remain /\ clientSignedWriteMayRetry(TRUE, TRUE, rebuild))

\* A reservation commit binds the targets when the read session is unchanged,
\* and the submission then reads the session and its binding: before its
\* transaction when the binding is published, inside it otherwise. A
\* confirmed abort is rebuilt while attempts remain, and otherwise the send
\* fails back to the client.
ReserveCommit ==
  \E i \in 1..Len(wire) :
  LET w == wire[i] IN
  /\ w.stage = "reserving"
  /\ IF w.snap = sessVer
       THEN /\ wire' = [wire EXCEPT ![i] = [w EXCEPT !.stage = "open", !.snap = sessVer,
                                              !.bind = binding, !.tries = 1]]
            /\ UNCHANGED <<replies, conflictLost>>
       ELSE IF Rebuilds(ReserveRedrive, w)
              THEN /\ wire' = [wire EXCEPT ![i] = [w EXCEPT !.snap = sessVer, !.tries = w.tries + 1]]
                   /\ UNCHANGED <<replies, conflictLost>>
              ELSE /\ wire' = Remove(wire, i)
                   /\ replies' = Append(replies, [id |-> w.id, ok |-> FALSE])
                   /\ conflictLost' = TRUE
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, rows>>
  /\ Bound

\* The submission commits the idempotent create when the session it read in
\* its transaction is unchanged, or at once when it read the published
\* binding before the transaction; a confirmed abort is rebuilt, reading the
\* session again, while attempts remain.
SubmitCommit ==
  \E i \in 1..Len(wire) :
  LET w == wire[i] IN
  /\ w.stage = "open"
  /\ IF PublishedBinding \/ w.snap = sessVer
       THEN /\ wire' = Remove(wire, i)
            /\ rows' = rows \cup {w.id}
            /\ replies' = Append(replies, [id |-> w.id, ok |-> TRUE])
            /\ staleBinding' = (staleBinding \/ w.bind # binding)
            /\ UNCHANGED conflictLost
       ELSE IF Rebuilds(Redrive, w)
              THEN /\ wire' = [wire EXCEPT ![i] = [w EXCEPT !.snap = sessVer, !.bind = binding,
                                                     !.tries = w.tries + 1]]
                   /\ UNCHANGED <<rows, replies, conflictLost, staleBinding>>
              ELSE /\ wire' = Remove(wire, i)
                   /\ replies' = Append(replies, [id |-> w.id, ok |-> FALSE])
                   /\ conflictLost' = TRUE
                   /\ UNCHANGED <<rows, staleBinding>>
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, binding>>

\* The runtime refuses a send of an identity it does not hold.
Refuse ==
  \E i \in 1..Len(wire) :
  /\ Refusals
  /\ wire[i].id \notin rows
  /\ wire' = Remove(wire, i)
  /\ replies' = Append(replies, [id |-> wire[i].id, ok |-> FALSE])
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, rows, conflictLost>>
  /\ Bound

\* The transport fails before the send reaches the runtime.
TransportFail ==
  \E i \in 1..Len(wire) :
  /\ Refusals
  /\ wire' = Remove(wire, i)
  /\ replies' = Append(replies, [id |-> wire[i].id, ok |-> FALSE])
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, rows, conflictLost>>
  /\ Bound

LoseReply ==
  \E j \in 1..Len(replies) :
  /\ LostReply
  /\ replies' = Remove(replies, j)
  /\ UNCHANGED <<sessVer, agentLeft, msg, payload, cur, sends, wire, rows, conflictLost>>
  /\ Bound

\* A reply is applied even after the deadline: a committed reply confirms.
Receive ==
  \E j \in 1..Len(replies) :
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
  /\ Bound

\* The client's deadline passes with no reply.
Timeout ==
  /\ msg = "pending"
  /\ msg' = "failed"
  /\ UNCHANGED <<sessVer, agentLeft, payload, cur, sends, wire, replies, rows, conflictLost>>
  /\ Bound

\* Retry resends the kept payload under the same identity.
Retry ==
  /\ msg = "failed"
  /\ payload
  /\ sends < MaxSends
  /\ msg' = "pending"
  /\ cur' = IF RetryFreshId THEN cur + 1 ELSE cur
  /\ Send(cur')
  /\ UNCHANGED <<sessVer, agentLeft, payload, replies, rows, conflictLost>>
  /\ Bound

Discard ==
  /\ msg = "failed"
  /\ msg' = "discarded"
  /\ payload' = FALSE
  /\ UNCHANGED <<sessVer, agentLeft, cur, sends, wire, replies, rows, conflictLost>>
  /\ Bound

\* RED: the client drops the payload of an unconfirmed message.
DropPayload ==
  /\ PayloadLost
  /\ msg \in {"pending", "failed"}
  /\ payload
  /\ payload' = FALSE
  /\ UNCHANGED <<sessVer, agentLeft, msg, cur, sends, wire, replies, rows, conflictLost>>
  /\ Bound

Next ==
  \/ AgentPublish
  \/ BindingChange
  \/ Compose
  \/ ReserveCommit
  \/ SubmitCommit
  \/ Refuse
  \/ TransportFail
  \/ LoseReply
  \/ Receive
  \/ Timeout
  \/ Retry
  \/ Discard
  \/ DropPayload

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(Compose)
  /\ WF_vars(ReserveCommit \/ SubmitCommit)
  /\ WF_vars(Receive)
  /\ WF_vars(Timeout)

TypeOK ==
  /\ sessVer \in 0..(Publications + IF BindingChanges THEN 1 ELSE 0)
  /\ agentLeft \in 0..Publications
  /\ msg \in {"none", "pending", "confirmed", "failed", "discarded"}
  /\ payload \in BOOLEAN
  /\ cur \in Ids
  /\ sends \in 0..MaxSends
  /\ Len(wire) <= MaxSends
  /\ Len(replies) <= MaxSends
  /\ rows \subseteq Ids
  /\ conflictLost \in BOOLEAN
  /\ binding \in 0..1
  /\ staleBinding \in BOOLEAN

\* A valid send during a turn is never lost to the turn's own writes.
NoConflictLoss == ~conflictLost

\* Every request is committed under the binding its session has.
SubmittedUnderCurrentBinding == ~staleBinding

\* No interleaving of send, lost reply, deadline and retry creates a second
\* request for one authored message.
CreatedOnce == Cardinality(rows) <= 1

\* The client keeps the payload of every unconfirmed message.
RetainedUntilConfirmed == msg \in {"pending", "failed"} => payload

\* A failed message always offers Retry: nothing is stuck. `sends = MaxSends`
\* is the model's bound on the person's retries, not a client state.
FailedOffersRetry == msg = "failed" => (ENABLED Retry \/ sends = MaxSends)

\* Pending always resolves: confirmed, or failed with Retry.
PendingResolves == [](msg = "pending" => <>(msg \in {"confirmed", "failed"}))

\* The authored message leaves pending.
EverySendSettles == <>(msg \in {"confirmed", "failed", "discarded"})

====
