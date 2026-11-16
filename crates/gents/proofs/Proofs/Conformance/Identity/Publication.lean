import Proofs.Store.Identity.Household
import Proofs.Conformance.Family

namespace Conformance.IdentityPublication

open Lean Identity.Household

structure Step where
  parentOk : Bool
  readerOk : Bool
  revokeParent : Bool
  deriving ToJson

structure Trace where
  name : String
  steps : List Step
  deriving ToJson

def applyStep (state : PendingPublication) (step : Step) : PendingPublication :=
  let swept := sweepPublication state step.parentOk step.readerOk
  if step.revokeParent then ownerRevokesParent swept else swept

def afterSteps : PendingPublication → List Step → List PendingPublication
  | _, [] => []
  | state, step :: rest =>
    let next := applyStep state step
    next :: afterSteps next rest

def traces : List Trace :=
  [⟨"interrupted_then_repaired", [⟨false, false, false⟩, ⟨true, true, false⟩]⟩,
   ⟨"reader_grant_retried", [⟨true, false, false⟩, ⟨true, true, false⟩]⟩,
   ⟨"owner_revokes_after_completion", [⟨true, true, false⟩, ⟨true, true, true⟩]⟩]

/-- A User reads a published row through its parent or its reader
relationship, so a User query observes only their union. -/
structure Observation where
  pending : Bool
  visible : Bool
  deriving ToJson

def Observation.ofState (state : PendingPublication) : Observation :=
  { pending := state.pending, visible := state.parentGranted || state.readerGranted }

structure ReceiptOutput where
  initial : Observation
  after : List Observation
  deriving ToJson

@[conformance_family]
def receipt : Family Trace ReceiptOutput where
  inputs := traces
  model := fun i =>
    { initial := .ofState beginPublication
      after := (afterSteps beginPublication i.steps).map .ofState }

/-- The Task delivery fixture drops an AgentSession's relationships and marks
its publication pending, then runs session recovery once: only the interrupted
trace has that shape. -/
def sessionRecoveryTraces : List Trace :=
  traces.filter fun trace => trace.name == "interrupted_then_repaired"

@[conformance_family]
def sessionRecovery : Family Trace Bool where
  inputs := sessionRecoveryTraces
  model := fun i =>
    ((afterSteps beginPublication i.steps).getLast?.map (·.pending)).getD beginPublication.pending

deriving instance ToJson for ParentPublication

structure ParentStep where
  nativeOwner : Option String
  claimedCreator : Option String
  authenticatedCreator : Option String
  grantOk : Bool
  markOk : Bool
  revokeParent : Bool
  deriving ToJson

structure ParentTrace where
  name : String
  steps : List ParentStep
  deriving ToJson

def applyParentStep (state : ParentPublication) (step : ParentStep) : ParentPublication :=
  let repaired := recoverParent state step.nativeOwner step.claimedCreator
    step.authenticatedCreator step.grantOk step.markOk
  if step.revokeParent then ownerRevokesLinkedParent repaired else repaired

def afterParentSteps : ParentPublication → List ParentStep → List ParentPublication
  | _, [] => []
  | state, step :: rest =>
    let next := applyParentStep state step
    next :: afterParentSteps next rest

def parentTraces : List ParentTrace :=
  [⟨"grant_failure_then_recovery", [⟨some "agent", some "agent", some "agent", false, true, false⟩,
      ⟨some "agent", some "agent", some "agent", true, true, false⟩]⟩,
   ⟨"marker_failure_then_recovery", [⟨some "agent", some "agent", some "agent", true, false, false⟩,
      ⟨some "agent", some "agent", some "agent", false, true, false⟩]⟩,
   ⟨"forged_creator", [⟨some "agent", some "other", some "agent", true, true, false⟩]⟩,
   ⟨"missing_authenticated_creator", [⟨some "agent", some "agent", none, true, true, false⟩]⟩,
   ⟨"revoked_after_completion", [⟨some "agent", some "agent", some "agent", true, true, false⟩,
      ⟨some "agent", some "agent", some "agent", true, true, true⟩]⟩]

structure ParentOutput where
  initial : ParentPublication
  after : List ParentPublication
  deriving ToJson

@[conformance_family]
def parent : Family ParentTrace ParentOutput where
  inputs := parentTraces
  model := fun i =>
    { initial := beginParentPublication
      after := afterParentSteps beginParentPublication i.steps }

end Conformance.IdentityPublication
