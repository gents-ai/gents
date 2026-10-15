import Proofs.ClientShell.Types

/-!
# The turn a session is on

A client holds a session's request rows and must name the request whose turn
the session is on, starting from its newest request. A claim selects queued
messages (`SessionQueue.claimFolding`) and each is superseded by the claiming
request only in the transaction that publishes it as that request's folded
entry, so a folded row is answered by that request and is never a turn. A
selected message not yet published is still an unclaimed row queued behind
the running request, and returns to the queue if that request ends first.
An unclaimed request queued behind another waits until the request it was
queued after — resolved through folds and retries — is terminal. A retry
replaces a terminal attempt with its successor. Every step stays within the
starting row's requester scope; physical identity (`doc`) names fold owners,
logical identity (`request`) names queue predecessors and retry parents.

`SessionObservation.latestObservedRequest`, `queuedRequests` and
`foldedRequests` are computed here from rows rather than assumed.
-/

namespace ClientShell.SessionTurn

inductive RowState where
  | unclaimed
  | active
  | terminal
  deriving DecidableEq, Repr

structure Row where
  doc         : Nat
  request     : RequestId
  requester   : Nat
  state       : RowState
  foldedInto  : Option Nat
  queuedAfter : Option RequestId
  retryParent : Option RequestId
  deriving DecidableEq, Repr

def findDoc (rows : List Row) (requester doc : Nat) : Option Row :=
  rows.find? (fun r => r.doc == doc && r.requester == requester)

def findRequest (rows : List Row) (requester : Nat) (request : RequestId) : Option Row :=
  rows.find? (fun r => r.request == request && r.requester == requester)

def retrySuccessor (rows : List Row) (row : Row) : Option Row :=
  rows.find? (fun r => r.retryParent == some row.request && r.requester == row.requester)

/-- `fuel` bounds the walk; `rows.length + 1` covers every acyclic chain. -/
def resolve (rows : List Row) : Nat → Row → Row
  | 0, row => row
  | fuel + 1, row =>
    match row.foldedInto with
    | some head =>
      match findDoc rows row.requester head with
      | some owner => resolve rows fuel owner
      | none => row
    | none =>
      match row.state with
      | .terminal =>
        match retrySuccessor rows row with
        | some next => resolve rows fuel next
        | none => row
      | .active => row
      | .unclaimed =>
        match row.queuedAfter.bind (findRequest rows row.requester) with
        | some ahead =>
          let turn := resolve rows fuel ahead
          if turn.state == .terminal then row else turn
        | none => row

def turnOf (rows : List Row) (newest : Row) : Row :=
  resolve rows (rows.length + 1) newest

/-- Unclaimed rows waiting behind `turn`, in arrival (list) order. -/
def queuedBehind (rows : List Row) (turn : Row) : List Row :=
  rows.filter (fun r =>
    r.state == .unclaimed && r.doc != turn.doc && (turnOf rows r).doc == turn.doc)

def foldedIn (rows : List Row) (requester : Nat) : List Row :=
  rows.filter (fun r => r.foldedInto.isSome && r.requester == requester)

def turnState (row : Row) : ClientTurnState :=
  match row.state with
  | .unclaimed => .waitingForClaim
  | .active    => .running
  | .terminal  => if row.foldedInto.isSome then .superseded else .completed

def observe (sid : SessionId) (node : NodeDid) (agent : Option AgentId)
    (rows : List Row) (newest : Row) : SessionObservation :=
  let turn := turnOf rows newest
  { sessionId := sid
  , nodeDid := node
  , agentId := agent
  , latestObservedRequest := some turn.request
  , latestTurn := some (turnState turn)
  , queuedRequests := (queuedBehind rows turn).map (·.request)
  , foldedRequests := (foldedIn rows newest.requester).map (·.request) }

theorem resolve_follows_fold_owner (rows : List Row) (fuel : Nat) (row owner : Row)
    (head : Nat) (hfold : row.foldedInto = some head)
    (hown : findDoc rows row.requester head = some owner) :
    resolve rows (fuel + 1) row = resolve rows fuel owner := by
  simp [resolve, hfold, hown]

/-- A message queued behind a row that was folded waits behind the turn that
row was folded into while that turn runs. -/
theorem queued_waits_behind_resolved_predecessor (rows : List Row) (fuel : Nat)
    (row ahead : Row) (request : RequestId)
    (hunfolded : row.foldedInto = none) (hunclaimed : row.state = .unclaimed)
    (hafter : row.queuedAfter = some request)
    (hahead : findRequest rows row.requester request = some ahead)
    (hrunning : (resolve rows fuel ahead).state ≠ .terminal) :
    resolve rows (fuel + 1) row = resolve rows fuel ahead := by
  simp [resolve, hunfolded, hunclaimed, hafter, hahead, hrunning]

theorem find_preserves_requester {rows : List Row} {requester : Nat} {p : Row → Bool}
    {r : Row} (hp : ∀ x, p x = true → x.requester = requester)
    (hfind : rows.find? p = some r) : r.requester = requester :=
  hp r (List.find?_some hfind)

/-- Resolution never leaves the starting row's requester scope. -/
theorem resolve_stays_in_requester_scope (rows : List Row) :
    ∀ (fuel : Nat) (row : Row), (resolve rows fuel row).requester = row.requester := by
  intro fuel
  induction fuel with
  | zero => intro row; rfl
  | succ n ih =>
    intro row
    unfold resolve
    split
    · rename_i head _
      split
      · rename_i owner hown
        rw [ih owner]
        exact find_preserves_requester (by intro x hx; simp at hx; exact hx.2) hown
      · rfl
    · split
      · split
        · rename_i next hnext
          rw [ih next]
          exact find_preserves_requester (by intro x hx; simp at hx; exact hx.2) hnext
        · rfl
      · rfl
      · split
        · rename_i ahead hahead
          simp only
          split
          · rfl
          · rw [ih ahead]
            cases hq : row.queuedAfter with
            | none => simp [hq] at hahead
            | some req =>
              simp [hq] at hahead
              exact find_preserves_requester
                (by intro x hx; simp at hx; exact hx.2) hahead
        · rfl

end ClientShell.SessionTurn
