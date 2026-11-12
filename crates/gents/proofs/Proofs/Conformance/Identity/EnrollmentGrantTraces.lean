import Proofs.Store.Identity.EnrollmentGrant
import Proofs.Conformance.Family

namespace Conformance.EnrollmentGrantTraces

open Lean Identity.EnrollmentGrant

structure StepInput where
  action : String
  request : String
  member : String
  tuple : String
  deriving ToJson

structure CaseInput where
  name : String
  subjects : List String
  steps : List StepInput
  deriving ToJson

structure Holding where
  subject : String
  tuples : List String
  deriving ToJson

structure ReceiptObservation where
  request : String
  member : String
  tuples : List String
  withdrawn : Bool
  deriving ToJson

structure StepObservation where
  holdings : List Holding
  receipts : List ReceiptObservation
  deriving ToJson

structure TraceOutput where
  derived : List String
  steps : List StepObservation
  deriving ToJson

def tupleOf (name : String) : Tuple :=
  if name == "node-administrator" then .nodeAdministrator else .permission name

def applyStep (s : State) (step : StepInput) : State :=
  if step.action == "grant" then grant s step.request step.member
  else if step.action == "independent" then grantIndependent s ⟨step.member, tupleOf step.tuple⟩
  else revoke s step.request

def observe (subjects : List String) (s : State) : StepObservation :=
  { holdings := subjects.map fun subject => { subject, tuples := heldOf s subject }
    receipts := s.receipts.reverse.map fun r =>
      { request := r.request, member := r.member, tuples := r.tuples.map Tuple.name
        withdrawn := r.withdrawn } }

def trace (subjects : List String) (s : State) : List StepInput → List StepObservation
  | [] => []
  | step :: rest =>
      let next := applyStep s step
      observe subjects next :: trace subjects next rest

def grantStep (request member : String) : StepInput :=
  { action := "grant", request, member, tuple := "" }

def independentStep (member tuple : String) : StepInput :=
  { action := "independent", request := "", member, tuple }

def revokeStep (request : String) : StepInput :=
  { action := "revoke", request, member := "", tuple := "" }

def traceCases : List (String × List StepInput) :=
  [("grant-then-revoke-restores",
      [independentStep "other" "node-administrator", grantStep "a" "member", revokeStep "a"]),
   ("prior-independent-survives",
      [independentStep "member" "read-document", independentStep "member" "node-administrator",
       grantStep "a" "member", revokeStep "a"]),
   ("shared-tuple-waits-for-last",
      [grantStep "a" "member", grantStep "b" "member", revokeStep "a", revokeStep "b"]),
   ("revoke-is-idempotent", [grantStep "a" "member", revokeStep "a", revokeStep "a"]),
   ("grant-is-idempotent", [grantStep "a" "member", grantStep "a" "member", revokeStep "a"]),
   ("revoke-without-receipt-changes-nothing",
      [independentStep "member" "update-document", revokeStep "a"])]

@[conformance_family]
def cases : Family CaseInput TraceOutput where
  inputs := traceCases.map fun (name, steps) => { name, subjects := ["member", "other"], steps }
  model := fun i =>
    { derived := derived.map Tuple.name
      steps := trace i.subjects { held := [], receipts := [] } i.steps }

end Conformance.EnrollmentGrantTraces
