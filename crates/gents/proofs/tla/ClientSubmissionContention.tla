---- MODULE ClientSubmissionContention ----
\* A User's signed client submission racing its session's active turn.
\*
\* The client submission (`request.client.submit`, `submit_client_request`)
\* is one DefraDB transaction run by the User through the signed client
\* endpoint: it reads the target `AgentSession` document (requester, selected
\* Agent, `relationships_pending`) and the selected `Agent`, then creates the
\* User-signed `AgentRequest` and stages its relationships. While the
\* session's turn runs, its working Agent publishes each message
\* (`tool_call.complete_delivery`, assistant publication) in a transaction
\* that also refreshes the `AgentSession` observation (`patch_session_in_txn`,
\* `last_activity_at`). That refresh changes the document the submission
\* read, so the submission's commit aborts with a DefraDB transaction
\* conflict (traced in the `mailbox_tool_turn` busy-session test).
\*
\* Keys: "sess" is the `AgentSession` document; the submission's other reads
\* (the Agent document) have no concurrent writer during a turn. DefraDB is
\* optimistic: a commit aborts when a key it read changed after its snapshot.
\*
\* A client-signed write may be rebuilt only after the native owner confirms
\* the attempt did not commit and the caller authorizes replay of a
\* transaction-only operation (Lean `Identity.Configuration
\* .clientSignedWriteMayRetry`). The rebuild re-reads the session, re-plans
\* and signs a fresh graph; the User's request admission signature covers the
\* request content, not the graph, so it is reused. `Redrive` selects that
\* opt-in mode: FALSE is the code before the fix, which surfaced the first
\* confirmed conflict as a rejected submission. A lost commit response is
\* unconfirmed and never rebuilt (`LostResponse`).
\*
\* `Publications` bounds the Agent's session refreshes that land inside one
\* submission's attempt series: the owned loop refreshes the observation once
\* per published message, and the series spans the bounded
\* `transaction_backoff` (100 + 200 + 400 ms). An abort consumes one such
\* refresh, so the series outlasts them only when `Publications < Attempts`.
EXTENDS Naturals

CONSTANTS
  Publications,  \* Agent session refreshes landing in one attempt series
  Attempts,      \* bounded attempts (TRANSACT_CONFLICT_MAX_RETRIES + 1)
  Redrive,       \* signed-conflict rebuild of the submission
  LostResponse   \* a commit response may be lost (unconfirmed)

ASSUME /\ Publications \in Nat
       /\ Attempts \in Nat \ {0}
       /\ Redrive \in BOOLEAN
       /\ LostResponse \in BOOLEAN

VARIABLES
  sessVer,   \* committed version of the AgentSession document
  agentLeft, \* Agent session refreshes still to land
  client,    \* "idle" | "open" | "admitted" | "unconfirmed" | "lost"
  snap,      \* the session version the open attempt read
  tries,     \* attempts begun
  created    \* committed AgentRequest documents for this submission

vars == <<sessVer, agentLeft, client, snap, tries, created>>

Init ==
  /\ sessVer = 0
  /\ agentLeft = Publications
  /\ client = "idle"
  /\ snap = 0
  /\ tries = 0
  /\ created = 0

\* The working Agent publishes a message and refreshes the observation.
AgentPublish ==
  /\ agentLeft > 0
  /\ sessVer' = sessVer + 1
  /\ agentLeft' = agentLeft - 1
  /\ UNCHANGED <<client, snap, tries, created>>

\* The submission opens its first attempt and reads the session.
ClientBegin ==
  /\ client = "idle"
  /\ client' = "open"
  /\ snap' = sessVer
  /\ tries' = 1
  /\ UNCHANGED <<sessVer, agentLeft, created>>

\* Commit: success when the read session is unchanged; otherwise a confirmed
\* abort, which the rebuild re-drives by re-reading while attempts remain.
ClientCommit ==
  /\ client = "open"
  /\ IF snap = sessVer
       THEN /\ client' = "admitted"
            /\ created' = created + 1
            /\ UNCHANGED <<snap, tries>>
       ELSE IF Redrive /\ tries < Attempts
              THEN /\ snap' = sessVer
                   /\ tries' = tries + 1
                   /\ UNCHANGED <<client, created>>
              ELSE /\ client' = "lost"
                   /\ UNCHANGED <<snap, tries, created>>
  /\ UNCHANGED <<sessVer, agentLeft>>

\* The commit lands but its response is lost: finality is unknown, so the
\* submission reports it and never rebuilds a second graph.
ClientCommitUnconfirmed ==
  /\ LostResponse
  /\ client = "open"
  /\ snap = sessVer
  /\ client' = "unconfirmed"
  /\ created' = created + 1
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries>>

Next ==
  \/ AgentPublish
  \/ ClientBegin
  \/ ClientCommit
  \/ ClientCommitUnconfirmed

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(ClientBegin)
  /\ WF_vars(ClientCommit)

TypeOK ==
  /\ sessVer \in 0..Publications
  /\ agentLeft \in 0..Publications
  /\ client \in {"idle", "open", "admitted", "unconfirmed", "lost"}
  /\ tries \in 0..Attempts
  /\ created \in 0..1

\* A valid submission during a turn is never lost to the turn's own writes.
NoConflictLoss == client # "lost"

\* Rebuilding never creates a second request, even after a lost response.
CreatedOnce == created <= 1

\* Every submission settles: admitted, or reported unconfirmed.
EverySubmissionSettles == <>(client \in {"admitted", "unconfirmed"})

====
