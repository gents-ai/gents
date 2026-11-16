import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.SessionPublication

open Lean Identity.Conformance

deriving instance ToJson for Identity.Automation.TaskSessionPublication

structure TaskInput where
  name : String
  before : Identity.Automation.TaskSessionPublication
  fresh : Bool
  newSession : Bool
  committed : Bool
  deriving ToJson

/-- One Task fire on one served node: a delivery aborted after staging, the
committed delivery, then a duplicate delivery after the working Agent grant was
revoked, in that order; each case's `before` is the state the previous one left. -/
@[conformance_family]
def task : Family TaskInput Identity.Automation.TaskSessionPublication where
  inputs :=
    let empty : Identity.Automation.TaskSessionPublication := ⟨false, false, false, false⟩
    let revoked : Identity.Automation.TaskSessionPublication := ⟨true, true, true, false⟩
    [ { name := "aborted", before := empty, fresh := true, newSession := true, committed := false }
    , { name := "created", before := empty, fresh := true, newSession := true, committed := true }
    , { name := "duplicate-after-revocation", before := revoked, fresh := false,
        newSession := false, committed := true } ]
  model := fun i => Identity.Automation.publishTaskSession i.before i.fresh i.newSession i.committed

structure ReservationState where
  session : Bool
  parent : Bool
  workingAgent : Bool
  deriving ToJson

structure ReservationInput where
  name : String
  before : ReservationState
  fresh : Bool
  commit : Bool
  deriving ToJson

def reservationParent : Identity.Household.Relation :=
  ⟨"sessions", "session", "parent", .object "agents" "agent"⟩

def reservationWorker : Identity.Household.Relation :=
  ⟨"sessions", "session", "working_agent", .actor agentDid⟩

def ReservationState.toState (s : ReservationState) : Identity.Household.State :=
  { objects := if s.session then [⟨"sessions", "session", userDid⟩] else []
    relations := (if s.parent then [reservationParent] else []) ++
      (if s.workingAgent then [reservationWorker] else [])
    enrolled := [] }

def ReservationState.ofState (s : Identity.Household.State) : ReservationState :=
  { session := s.objects.any (fun o => o.resource == "sessions" && o.id == "session")
    parent := s.relations.contains reservationParent
    workingAgent := s.relations.contains reservationWorker }

/-- One client session on one served node: a reservation aborted after its
relationships were staged, the committed reservation, then a duplicate
reservation after the working Agent grant was revoked, in that order. -/
@[conformance_family]
def clientReservation : Family ReservationInput ReservationState where
  inputs :=
    let empty : ReservationState := ⟨false, false, false⟩
    let revoked : ReservationState := ⟨true, true, false⟩
    [ { name := "aborted", before := empty, fresh := true, commit := false }
    , { name := "created", before := empty, fresh := true, commit := true }
    , { name := "duplicate-after-revocation", before := revoked, fresh := false, commit := true } ]
  model := fun i => .ofState (Identity.Household.reserveUserSession i.before.toState "session"
    userDid agentDid "agent" i.fresh i.commit)

example : (ReservationState.toState ⟨true, true, false⟩) =
    ⟨[⟨"sessions", "session", userDid⟩], [reservationParent], []⟩ := by rfl

end Conformance.SessionPublication
