import Proofs.Session.AgentSession
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Family

namespace Conformance.SessionObservation

open Lean AgentSession

inductive StepKind where
  | ensure | create | finish | advance | refresh | close | reopen | stale | latest
  deriving ToJson, DecidableEq, Repr

structure SessionWire where
  node : Nat
  session : Nat
  requester : Option Nat
  agent : Nat
  deriving ToJson

/-- `createdAt` is whole seconds; every other time is milliseconds. -/
structure RowWire where
  node : Nat
  session : Nat
  requester : Option Nat
  agent : Nat
  requestId : Nat
  createdAt : Nat
  state : RequestState
  deriving ToJson

/-- `target` names a request by its creation index; `terminal` marks a refresh
that supplies the request's final reply. -/
structure Step where
  kind : StepKind
  row : Option RowWire := none
  target : Nat := 0
  state : RequestState := .completed
  terminalizedAt : Option Nat := none
  terminal : Bool := false
  reply : Option String := none
  preview : String := ""
  now : Nat := 0
  tags : List String := []
  allRequesters : Bool := false
  requester : Option Nat := none
  deriving ToJson

structure Trace where
  session : SessionWire
  steps : List Step
  deriving ToJson

structure HeadWire where
  request : Nat
  requestId : Nat
  state : RequestState
  deriving ToJson

structure Observed where
  applied : Option Bool
  latest : Option Nat
  head : Option HeadWire
  replyPreview : Option String
  closed : Bool
  createdAt : Option Nat
  tags : List String
  deriving ToJson

structure State where
  session : Option Document := none
  rows : List RequestFact := []

def SessionWire.document (w : SessionWire) (now : Time) : Document :=
  { scope := ⟨w.node, w.session, w.requester⟩, agent := w.agent, createdAt := now }

def RowWire.fact (w : RowWire) (doc : Nat) : RequestFact :=
  { purpose := .normal, scope := ⟨w.node, w.session, w.requester⟩, agent := w.agent,
    createdAt := w.createdAt, observed := ⟨doc, w.requestId, w.state⟩ }

def finishRow (target : Nat) (state : RequestState) (at? : Option Time)
    (r : RequestFact) : RequestFact :=
  if r.observed.docId = target then
    { r with observed := { r.observed with state }, terminalizedAt := at? }
  else r

def stalePoint (s : Document) (current : RequestObservation) : Document :=
  { s with observation := s.observation.map fun o => { o with latest := some current } }

def apply (w : SessionWire) (st : State) (step : Step) : State × Option Bool × Option Nat :=
  let target := st.rows.find? (·.observed.docId == step.target)
  match step.kind, st.session, target with
  | .ensure, current, _ => ({ st with session := ensure? current (w.document step.now) }, none, none)
  | .create, _, _ => match step.row with
    | some row => ({ st with rows := st.rows ++ [row.fact st.rows.length] }, none, none)
    | none => (st, none, none)
  | .finish, _, _ =>
      ({ st with rows := st.rows.map (finishRow step.target step.state step.terminalizedAt) },
        none, none)
  | .advance, some s, some r =>
      ({ st with session := some (advance s st.rows r step.preview step.now) },
        some (advances s st.rows r), none)
  | .refresh, some s, some r =>
      ({ st with session := some (refreshReply s st.rows r.observed
          (if step.terminal then some step.reply else none) step.now) },
        some (refreshes s st.rows r.observed), none)
  | .close, some s, _ =>
      ({ st with session := some { s with closedAt := some step.now, tags := step.tags } },
        none, none)
  | .reopen, some s, _ =>
      let (next, reopened) := reopen s step.now
      ({ st with session := some next }, some reopened, none)
  | .stale, some s, some r =>
      ({ st with session := some (stalePoint s { r.observed with state := step.state }) },
        none, none)
  | .latest, _, _ =>
      (st, none, (AgentSession.latest st.rows w.node w.session
        (if step.allRequesters then none else some step.requester)).map (·.observed.docId))
  | _, _, _ => (st, none, none)

def observe (st : State) (applied : Option Bool) (selected : Option Nat) : Observed :=
  let head := st.session.bind (·.observation) |>.bind (·.latest)
  { applied
    latest := selected
    head := head.map fun h => { request := h.docId, requestId := h.requestId, state := h.state }
    replyPreview := st.session.bind (·.observation) |>.bind (·.replyPreview)
    closed := (st.session.bind (·.closedAt)).isSome
    createdAt := st.session.map (·.createdAt)
    tags := (st.session.map (·.tags)).getD [] }

def run (t : Trace) : List Observed :=
  (t.steps.foldl (fun (acc : State × List Observed) step =>
    let (next, applied, selected) := apply t.session acc.1 step
    (next, acc.2 ++ [observe next applied selected])) ({}, [])).2

def row (session requestId createdAt : Nat) (state : RequestState)
    (requester : Option Nat := none) : RowWire :=
  { node := 1, session, requester, agent := 1, requestId, createdAt, state }

/-- An absent requester refreshes its own request, survives close and reopen, and
refuses an observation that names a request of another session with the same
logical request id. -/
def nullRequesterRefresh : Trace :=
  { session := { node := 1, session := 1, requester := none, agent := 1 }
    steps :=
      [ { kind := .create, row := some (row 1 1 0 .processing) }
      , { kind := .ensure, now := 1000 }
      , { kind := .advance, target := 0, preview := "prompt", now := 1000 }
      , { kind := .finish, target := 0, state := .completed }
      , { kind := .refresh, target := 0, terminal := true, reply := some "Finished", now := 1000 }
      , { kind := .close, now := 2000, tags := ["review"] }
      , { kind := .reopen, now := 3000 }
      , { kind := .create, row := some (row 2 1 4 .failed) }
      , { kind := .stale, target := 1, state := .completed }
      , { kind := .refresh, target := 1, terminal := true, reply := some "Wrong scope",
          now := 1000 } ] }

/-- #3604: the successor is claimed in the second its predecessor finished and
its request id sorts lower. -/
def sameSecondClaim : Trace :=
  { session := { node := 1, session := 1, requester := none, agent := 1 }
    steps :=
      [ { kind := .create, row := some (row 1 9 0 .claimed) }
      , { kind := .ensure, now := 0 }
      , { kind := .advance, target := 0, preview := "first", now := 0 }
      , { kind := .finish, target := 0, state := .completed, terminalizedAt := some 400 }
      , { kind := .refresh, target := 0, terminal := true, reply := some "first reply", now := 400 }
      , { kind := .create, row := some (row 1 4 0 .claimed) }
      , { kind := .advance, target := 1, preview := "second", now := 900 }
      , { kind := .finish, target := 1, state := .completed, terminalizedAt := some 1300 }
      , { kind := .refresh, target := 1, terminal := true, reply := some "second reply",
          now := 1300 }
      , { kind := .latest, requester := none } ] }

/-- A requester-scoped session: ordinary activity keeps the reply, a refresh of a
request the observation does not name is refused, and the scoped and unscoped
heads follow a newer request until it is advanced. -/
def requesterScoped : Trace :=
  { session := { node := 1, session := 1, requester := some 2, agent := 1 }
    steps :=
      [ { kind := .create, row := some (row 1 1 0 .processing (some 2)) }
      , { kind := .ensure, now := 0 }
      , { kind := .advance, target := 0, preview := "one", now := 0 }
      , { kind := .refresh, target := 0, now := 100 }
      , { kind := .reopen, now := 200 }
      , { kind := .create, row := some (row 1 2 1 .pending (some 2)) }
      , { kind := .refresh, target := 1, terminal := true, reply := some "stale", now := 300 }
      , { kind := .latest, requester := some 2 }
      , { kind := .latest, allRequesters := true }
      , { kind := .latest, requester := none }
      , { kind := .advance, target := 1, preview := "two", now := 1500 } ] }

@[conformance_family]
def trace : Family Trace (List Observed) where
  inputs := [nullRequesterRefresh, sameSecondClaim, requesterScoped]
  model := run

end Conformance.SessionObservation
