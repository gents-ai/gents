import Proofs.Store.Identity.EnrollmentGrant
import Proofs.Conformance.Family

namespace Conformance.EnrollmentGrantTraces

open Lean Identity.EnrollmentGrant

structure StepInput where
  action : String
  request : String
  member : String
  tuple : String
  issued : Nat
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
  owed : Bool
  restore : Bool
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
  else if step.action == "revoke_member" then revokeMember s step.member
  else if step.action == "grant_interrupted" then
    grantInterrupted s step.request step.member step.issued
  else if step.action == "abandon" then abandon s step.request
  else if step.action == "owe_revoke" then revokeBy nodeStanding s step.request
  else if step.action == "owe_revoke_member" then revokeMemberBy nodeStanding s step.member
  else if step.action == "settle" then settle .owner s step.member
  else if step.action == "settle_by_node" then settle nodeStanding s step.member
  else if step.action == "grant_other" then grantOther s ⟨step.member, tupleOf step.tuple⟩
  else if step.action == "expire" then Step.apply s (.expire step.member)
  else if step.action == "overtaking_removal" then overtakingRemoval s step.member
  else if step.action == "restore" then restoreBy .owner s step.member
  else revoke s step.request

def observe (subjects : List String) (s : State) : StepObservation :=
  { holdings := subjects.map fun subject => { subject, tuples := heldOf s subject }
    receipts := s.receipts.reverse.map fun r =>
      { request := r.request, member := r.member, tuples := r.tuples.map Tuple.name
        withdrawn := r.withdrawn, owed := r.owed, restore := r.restore } }

def trace (subjects : List String) (s : State) : List StepInput → List StepObservation
  | [] => []
  | step :: rest =>
      let next := applyStep s step
      observe subjects next :: trace subjects next rest

def grantStep (request member : String) : StepInput :=
  { action := "grant", request, member, tuple := "", issued := 0 }

def independentStep (member tuple : String) : StepInput :=
  { action := "independent", request := "", member, tuple, issued := 0 }

def revokeStep (request : String) : StepInput :=
  { action := "revoke", request, member := "", tuple := "", issued := 0 }

def revokeMemberStep (member : String) : StepInput :=
  { action := "revoke_member", request := "", member, tuple := "", issued := 0 }

def interruptedStep (request member : String) (issued : Nat) : StepInput :=
  { action := "grant_interrupted", request, member, tuple := "", issued }

def abandonStep (request : String) : StepInput :=
  { action := "abandon", request, member := "", tuple := "", issued := 0 }

def oweRevokeStep (request : String) : StepInput :=
  { action := "owe_revoke", request, member := "", tuple := "", issued := 0 }

def oweRevokeMemberStep (member : String) : StepInput :=
  { action := "owe_revoke_member", request := "", member, tuple := "", issued := 0 }

def settleStep (member : String) : StepInput :=
  { action := "settle", request := "", member, tuple := "", issued := 0 }

def settleByNodeStep (member : String) : StepInput :=
  { action := "settle_by_node", request := "", member, tuple := "", issued := 0 }

def grantOtherStep (member tuple : String) : StepInput :=
  { action := "grant_other", request := "", member, tuple, issued := 0 }

def overtakingRemovalStep (member : String) : StepInput :=
  { action := "overtaking_removal", request := "", member, tuple := "", issued := 0 }

def restoreStep (member : String) : StepInput :=
  { action := "restore", request := "", member, tuple := "", issued := 0 }

def expireStep (member : String) : StepInput :=
  { action := "expire", request := "", member, tuple := "", issued := 0 }

/-- An approval failing at each point its issuing can fail at, abandoned, then
retried. -/
def abandonedAtEveryPoint : List (String × List StepInput) :=
  issuePoints.map fun k =>
    (s!"approval-abandoned-after-{k}-tuples-then-retried",
      [independentStep "member" "read-document", grantStep "other-request" "other",
       interruptedStep "a" "member" k, abandonStep "a", grantStep "a" "member", revokeStep "a"])

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
      [independentStep "member" "update-document", revokeStep "a"]),
   ("revoking-a-renewed-membership-withdraws-every-generation",
      [independentStep "other" "node-administrator", grantStep "first" "member",
       grantStep "renewal" "member", revokeMemberStep "member"]),
   ("revoking-a-membership-preserves-independent-and-other-members",
      [independentStep "member" "read-document", grantStep "a" "member",
       grantStep "b" "other", revokeMemberStep "member"]),
   ("abandon-keeps-another-approvals-tuples",
      [grantStep "a" "member", interruptedStep "b" "member" 3, abandonStep "b", revokeStep "b",
       revokeStep "a"]),
   ("abandoned-retry-of-a-live-receipt-reissues",
      [interruptedStep "a" "member" 1, abandonStep "a", interruptedStep "a" "member" 2,
       abandonStep "a", grantStep "a" "member"]),
   ("abandon-without-receipt-changes-nothing",
      [independentStep "member" "update-document", abandonStep "a"]),
   ("revoked-request-is-not-reissued",
      [grantStep "a" "member", revokeStep "a", interruptedStep "a" "member" 2,
       grantStep "a" "member"])]
    ++ abandonedAtEveryPoint

/-- Owing, settling, restoring and refusing around an owed administration
removal, and expiry. -/
def owedTraceCases : List (String × List StepInput) :=
  [("a-denial-the-node-finds-owes-administration-until-the-owner-settles",
      [grantStep "a" "member", oweRevokeStep "a", settleByNodeStep "member",
       settleStep "member", settleStep "member"]),
   ("a-revocation-the-node-finds-owes-every-generations-administration",
      [independentStep "member" "read-document", grantStep "first" "member",
       grantStep "renewal" "member", oweRevokeMemberStep "member", settleByNodeStep "member",
       settleStep "member"]),
   ("a-denial-sharing-administration-with-a-live-enrollment-owes-nothing",
      [grantStep "a" "member", grantStep "b" "member", oweRevokeStep "a", settleStep "member",
       oweRevokeStep "b", settleStep "member"]),
   ("an-independent-administrator-owes-nothing",
      [independentStep "member" "node-administrator", grantStep "a" "member",
       oweRevokeStep "a", settleStep "member"]),
   ("a-reapproval-takes-the-owed-withdrawal-over",
      [grantStep "a" "member", oweRevokeStep "a", grantStep "b" "member", settleStep "member",
       oweRevokeStep "b", settleStep "member"]),
   ("an-owed-withdrawal-survives-other-steps",
      [grantStep "a" "member", grantStep "c" "other", oweRevokeStep "a", revokeStep "c",
       oweRevokeMemberStep "member", abandonStep "a", settleStep "other", settleStep "member"]),
   ("an-owed-member-is-refused-every-node-permission",
      [grantStep "a" "member", oweRevokeStep "a", grantOtherStep "member" "read-document",
       grantOtherStep "other" "read-document", settleStep "member",
       grantOtherStep "member" "read-document"]),
   ("a-deliberate-grant-closes-the-owed-removal",
      [grantStep "a" "member", oweRevokeStep "a", grantOtherStep "member" "node-administrator",
       settleStep "member", grantOtherStep "member" "update-document"]),
   ("an-expiry-withdraws-like-a-revocation",
      [grantStep "first" "member", grantStep "renewal" "member", expireStep "member",
       settleByNodeStep "member", settleStep "member"]),
   ("a-renewal-after-an-expiry-takes-the-removal-over",
      [grantStep "first" "member", expireStep "member", grantStep "renewal" "member",
       settleStep "member", expireStep "member", settleStep "member"]),
   ("a-removal-overtaking-an-approval-is-restored",
      [grantStep "a" "member", oweRevokeStep "a", grantStep "b" "member",
       overtakingRemovalStep "member", settleStep "member", restoreStep "member",
       restoreStep "member", revokeStep "b"]),
   ("a-removal-overtaking-a-deliberate-grant-is-restored",
      [grantStep "a" "member", oweRevokeStep "a", grantOtherStep "member" "node-administrator",
       overtakingRemovalStep "member", restoreStep "member"]),
   ("a-restoration-superseded-by-a-later-revocation-closes",
      [grantStep "a" "member", oweRevokeStep "a", grantStep "b" "member",
       overtakingRemovalStep "member", oweRevokeStep "b", restoreStep "member",
       settleStep "member"])]

@[conformance_family]
def cases : Family CaseInput TraceOutput where
  inputs := (traceCases ++ owedTraceCases).map fun (name, steps) =>
    { name, subjects := ["member", "other"], steps }
  model := fun i =>
    { derived := derived.map Tuple.name
      steps := trace i.subjects { held := [], receipts := [] } i.steps }

structure LapseInput where
  name : String
  provider : Option Nat
  wall : Nat
  leases : List Nat
  deriving ToJson

/-- Whether a membership lapsed: a provider ahead of, behind or absent from
the wall clock, at and around each lease's end, with a renewal fresh or
lapsed. -/
@[conformance_family]
def lapseCases : Family LapseInput Bool where
  inputs :=
    [("no-provider-before", none, 99, [100]), ("no-provider-at", none, 100, [100]),
     ("provider-ahead", some 100, 90, [100]), ("provider-stale-home-back", some 50, 150, [100]),
     ("provider-behind-live", some 90, 95, [100]), ("no-lease", some 500, 500, []),
     ("renewed-before-expiry", some 120, 120, [100, 200]),
     ("renewal-also-lapsed", some 250, 250, [100, 200])].map
      fun (name, provider, wall, leases) => { name, provider, wall, leases }
  model := fun i => membershipLapsed i.provider i.wall i.leases

end Conformance.EnrollmentGrantTraces
