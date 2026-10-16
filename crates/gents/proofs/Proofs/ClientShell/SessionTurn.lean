import Proofs.ClientShell.Types

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
  retryParentDoc : Option Nat
  completed   : Bool := false
  publicInput : Bool := true
  authoredInputMaterialized : Bool := false
  deriving DecidableEq, Repr

def findDoc (rows : List Row) (requester doc : Nat) : Option Row :=
  rows.find? (fun r => r.doc == doc && r.requester == requester)

def findRequest (rows : List Row) (requester : Nat) (request : RequestId) : Option Row :=
  rows.find? (fun r => r.request == request && r.requester == requester)

def retrySuccessor (rows : List Row) (row : Row) : Option Row :=
  rows.find? (fun r => r.retryParent == some row.request && r.requester == row.requester)

def resolveSeen (rows : List Row) : Nat → List Nat → Row → Row
  | 0, _, row => row
  | fuel + 1, seen, row =>
    if row.doc ∈ seen then row else
    let seen := row.doc :: seen
    match row.foldedInto with
    | some head =>
      match findDoc rows row.requester head with
      | some owner => resolveSeen rows fuel seen owner
      | none => row
    | none =>
      match row.state with
      | .terminal =>
        match retrySuccessor rows row with
        | some next => resolveSeen rows fuel seen next
        | none => row
      | .active => row
      | .unclaimed =>
        match row.queuedAfter.bind (findRequest rows row.requester) with
        | some ahead =>
          let turn := resolveSeen rows fuel seen ahead
          if turn.state == .terminal then row else turn
        | none => row

def resolve (rows : List Row) (row : Row) : Row :=
  resolveSeen rows (rows.length + 1) [] row

def turnOf (rows : List Row) (newest : Row) : Row :=
  resolve rows newest

def queuedBehind (rows : List Row) (turn : Row) : List Row :=
  rows.filter (fun r =>
    r.state == .unclaimed && r.doc != turn.doc && (turnOf rows r).doc == turn.doc)

/-- The runtime supplies physical requests in receipt-validated native arrival
order, including requests already taken from its pending listing. The client
does not infer a replacement's position from timestamps or unsigned slots.
Missing arrivals provide no ordering evidence and are not projected. -/
def orderedQueued (queued : List Row) (arrivals : List Nat) : List Row :=
  arrivals.filterMap (fun doc => queued.find? (fun r => r.doc == doc))

theorem unqueued_arrival_is_ignored (queued : List Row) (doc : Nat) (rest : List Nat)
    (h : queued.find? (fun r => r.doc == doc) = none) :
    orderedQueued queued (doc :: rest) = orderedQueued queued rest := by
  simp [orderedQueued, h]

theorem available_arrival_keeps_its_place (queued : List Row) (doc : Nat)
    (rest : List Nat) (row : Row)
    (h : queued.find? (fun r => r.doc == doc) = some row) :
    orderedQueued queued (doc :: rest) = row :: orderedQueued queued rest := by
  simp [orderedQueued, h]

def foldedIn (rows : List Row) (requester : Nat) : List Row :=
  rows.filter (fun r => r.foldedInto.isSome && r.requester == requester)

def resolutionDependencies (rows : List Row) (row : Row) : List Row :=
  match row.foldedInto with
  | some ownerDoc => rows.filter (fun candidate =>
      candidate.doc == ownerDoc && candidate.requester == row.requester)
  | none =>
      match row.state with
      | .terminal => rows.filter (fun candidate =>
          candidate.retryParent == some row.request && candidate.requester == row.requester)
      | .active => []
      | .unclaimed =>
          match row.queuedAfter with
          | some predecessor => rows.filter (fun candidate =>
              candidate.request == predecessor && candidate.requester == row.requester)
          | none => []

def retryParentDependency (rows : List Row) (row : Row) : List Row :=
  row.retryParentDoc.toList.flatMap (fun doc =>
      rows.filter (fun candidate => candidate.doc == doc && candidate.requester == row.requester))

/-- A completed public request can leave the repeated turn projection only
when its exact authored input is known durable and it is not a resumed retry.
The runtime binds this fact by publishing fresh authored input before a
successful non-retry completion; retry rows remain because they can inherit
history without publishing a prompt under their own physical document. -/
def mayOmitCompletedPublicInput (row : Row) : Bool :=
  row.completed && row.publicInput && row.authoredInputMaterialized &&
    row.retryParentDoc.isNone

def expandReadRows (all current : List Row) : List Row :=
  let neededDocs := current.map (·.doc) ++
    (current.flatMap (fun row =>
      resolutionDependencies all row ++ retryParentDependency all row)).map (·.doc)
  all.filter (fun candidate => neededDocs.contains candidate.doc)

def closeReadRows (all : List Row) : Nat → List Row → List Row
  | 0, current => current
  | fuel + 1, current => closeReadRows all fuel (expandReadRows all current)

/-- The request rows needed for the existing session-turn projection. Every
unclaimed row is retained because all queued siblings are displayed; every
folded row is retained because `foldedIn` is the complete acknowledgement
list. The selected and observed latest physical rows are explicit seeds. The
closure follows only references used by `resolve` (physical fold owners,
logical queue predecessors, retry successors, and physical retry parents),
preserving duplicate logical IDs and source order. Failed/dead/interrupted
public rows may still own placeholders and therefore remain output-sensitive.
Completed public requests have crossed the authored-input publication boundary
before successful completion and may be omitted unless needed as a dependency.
Fold history is output-sensitive and must be read in full pages by its owner;
it is not subject to a fixed row cap. -/
def requestReadSeeds (all : List Row) (requester selectedDoc latestDoc : Nat)
    (pageRequestDocs : List Nat) : List Row :=
  all.filter (fun row => row.requester == requester &&
    (row.doc == selectedDoc || row.doc == latestDoc ||
      pageRequestDocs.contains row.doc || row.state == .unclaimed ||
      (row.publicInput && !mayOmitCompletedPublicInput row) ||
      row.retryParentDoc.isSome ||
      row.foldedInto.isSome))

def requestReadRows (all : List Row) (requester selectedDoc latestDoc : Nat)
    (pageRequestDocs : List Nat) : List Row :=
  closeReadRows all (all.length + 1)
    (requestReadSeeds all requester selectedDoc latestDoc pageRequestDocs)

theorem expandReadRows_keeps_current {all current : List Row} {row : Row}
    (hrow : row ∈ current) (hall : row ∈ all) :
    row ∈ expandReadRows all current := by
  apply List.mem_filter.mpr
  refine ⟨hall, ?_⟩
  simp [expandReadRows, List.mem_append, List.mem_map]
  exact Or.inl ⟨row, hrow, rfl⟩

theorem closeReadRows_keeps_current (all : List Row) :
    ∀ (fuel : Nat) (current : List Row) {row : Row},
      row ∈ current → row ∈ all → row ∈ closeReadRows all fuel current := by
  intro fuel
  induction fuel with
  | zero => intro current row hrow hall; exact hrow
  | succ fuel ih =>
      intro current row hrow hall
      change row ∈ closeReadRows all fuel (expandReadRows all current)
      exact ih (expandReadRows all current)
        (expandReadRows_keeps_current hrow hall) hall

theorem requestReadRows_keeps_page_request (all : List Row) (requester selected latest : Nat)
    (pageDocs : List Nat) {row : Row}
    (hall : row ∈ all) (hrequester : row.requester = requester)
    (hpage : row.doc ∈ pageDocs) :
    row ∈ requestReadRows all requester selected latest pageDocs := by
  have hseed : row ∈ requestReadSeeds all requester selected latest pageDocs := by
    apply List.mem_filter.mpr
    refine ⟨hall, ?_⟩
    simp [requestReadSeeds, hrequester, hpage]
  exact closeReadRows_keeps_current all (all.length + 1)
    (requestReadSeeds all requester selected latest pageDocs) hseed hall

theorem requestReadRows_keeps_unclaimed (all : List Row) (requester selected latest : Nat)
    (pageDocs : List Nat) {row : Row}
    (hall : row ∈ all) (hrequester : row.requester = requester)
    (hunclaimed : row.state = .unclaimed) :
    row ∈ requestReadRows all requester selected latest pageDocs := by
  have hseed : row ∈ requestReadSeeds all requester selected latest pageDocs := by
    apply List.mem_filter.mpr
    refine ⟨hall, ?_⟩
    simp [requestReadSeeds, hrequester, hunclaimed]
  exact closeReadRows_keeps_current all (all.length + 1)
    (requestReadSeeds all requester selected latest pageDocs) hseed hall

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
    resolveSeen rows (fuel + 1) [] row = resolveSeen rows fuel [row.doc] owner := by
  simp [resolveSeen, hfold, hown]

theorem queued_waits_behind_resolved_predecessor (rows : List Row) (fuel : Nat)
    (seen : List Nat)
    (row ahead : Row) (request : RequestId)
    (hunfolded : row.foldedInto = none) (hunclaimed : row.state = .unclaimed)
    (hafter : row.queuedAfter = some request)
    (hahead : findRequest rows row.requester request = some ahead)
    (hnotseen : ¬ seen.contains row.doc)
    (hrunning : (resolveSeen rows fuel (row.doc :: seen) ahead).state ≠ .terminal) :
    resolveSeen rows (fuel + 1) seen row = resolveSeen rows fuel (row.doc :: seen) ahead := by
  have hnotmem : row.doc ∉ seen := by
    intro hmem
    have hcontains : seen.contains row.doc = true := by simp [hmem]
    exact hnotseen hcontains
  simp [resolveSeen, hnotmem, hunfolded, hunclaimed, hafter, hahead, hrunning]

theorem find_preserves_requester {rows : List Row} {requester : Nat} {p : Row → Bool}
    {r : Row} (hp : ∀ x, p x = true → x.requester = requester)
    (hfind : rows.find? p = some r) : r.requester = requester :=
  hp r (List.find?_some hfind)

theorem resolve_stays_in_requester_scope (rows : List Row) :
    ∀ (fuel : Nat) (seen : List Nat) (row : Row),
      (resolveSeen rows fuel seen row).requester = row.requester := by
  intro fuel
  induction fuel with
  | zero => intro seen row; rfl
  | succ n ih =>
      intro seen row
      cases hseen : seen.contains row.doc with
      | true =>
        have hmem : row.doc ∈ seen := by simpa using hseen
        simp [resolveSeen, hmem]
      | false =>
        have hnotmem : row.doc ∉ seen := by simpa using hseen
        simp only [resolveSeen, hnotmem, Bool.false_eq_true, ↓reduceIte]
        cases hfold : row.foldedInto with
        | some head =>
          cases howner : findDoc rows row.requester head with
          | none => simp [hfold, howner]
          | some owner =>
            simp only [hfold, howner]
            rw [ih (row.doc :: seen) owner]
            exact find_preserves_requester
              (by intro x hx; simp at hx; exact hx.2) howner
        | none =>
          cases hstate : row.state with
          | terminal =>
            cases hnext : retrySuccessor rows row with
            | none => simp [hfold, hstate, hnext]
            | some next =>
              simp only [hfold, hstate, hnext]
              rw [ih (row.doc :: seen) next]
              exact find_preserves_requester
                (by intro x hx; simp at hx; exact hx.2) hnext
          | active => simp [hfold, hstate]
          | unclaimed =>
            cases hqueued : row.queuedAfter.bind (findRequest rows row.requester) with
            | none => simp [hfold, hstate, hqueued]
            | some ahead =>
              simp only [hfold, hstate, hqueued]
              split
              · rfl
              · rw [ih (row.doc :: seen) ahead]
                cases hq : row.queuedAfter with
                | none => simp [hq] at hqueued
                | some req =>
                  simp [hq] at hqueued
                  exact find_preserves_requester
                    (by intro x hx; simp at hx; exact hx.2) hqueued

def requestReadProjection (all : List Row) (requester selectedDoc latestDoc : Nat)
    (pageRequestDocs : List Nat) : Option SessionObservation :=
  let rows := requestReadRows all requester selectedDoc latestDoc pageRequestDocs
  let newest := rows.find? (fun row => row.doc == latestDoc && row.requester == requester)
  newest.map (observe 0 0 none rows)

end ClientShell.SessionTurn
