import Proofs.Identity.EnrollmentGrant
import Lean

namespace Identity.EnrollmentGrantConformance
open Lean Identity.EnrollmentGrant

inductive Step where
  | grant (request member : String)
  | independent (member : String) (tuple : Tuple)
  | revoke (request : String)

def Step.apply (s : State) : Step → State
  | .grant request member => EnrollmentGrant.grant s request member
  | .independent member tuple => grantIndependent s ⟨member, tuple⟩
  | .revoke request => EnrollmentGrant.revoke s request

def Step.json : Step → Json
  | .grant request member => Json.mkObj [("action", toJson "grant"), ("request", toJson request),
      ("member", toJson member)]
  | .independent member tuple => Json.mkObj [("action", toJson "independent"),
      ("member", toJson member), ("tuple", toJson tuple.name)]
  | .revoke request => Json.mkObj [("action", toJson "revoke"), ("request", toJson request)]

def subjects : List String := ["member", "other"]

def observe (s : State) : Json :=
  Json.mkObj [("held", Json.mkObj (subjects.map fun m => (m, toJson (heldOf s m)))),
    ("receipts", toJson (s.receipts.reverse.map fun r => Json.mkObj [("request", toJson r.request),
      ("member", toJson r.member), ("tuples", toJson (r.tuples.map Tuple.name)),
      ("withdrawn", toJson r.withdrawn)]))]

def trace (s : State) : List Step → List Json
  | [] => []
  | step :: rest =>
      let next := step.apply s
      Json.mkObj [("step", step.json), ("expected", observe next)] :: trace next rest

def empty : State := { held := [], receipts := [] }

def cases : List (String × List Step) :=
  [("grant-then-revoke-restores",
      [.independent "other" .nodeAdministrator, .grant "a" "member", .revoke "a"]),
   ("prior-independent-survives",
      [.independent "member" (.permission "read-document"),
       .independent "member" .nodeAdministrator, .grant "a" "member", .revoke "a"]),
   ("shared-tuple-waits-for-last",
      [.grant "a" "member", .grant "b" "member", .revoke "a", .revoke "b"]),
   ("revoke-is-idempotent", [.grant "a" "member", .revoke "a", .revoke "a"]),
   ("grant-is-idempotent", [.grant "a" "member", .grant "a" "member", .revoke "a"]),
   ("revoke-without-receipt-changes-nothing",
      [.independent "member" (.permission "update-document"), .revoke "a"])]

def derivedJson : Json := toJson (derived.map Tuple.name)

def casesJson : String := (Json.mkObj [("derived", derivedJson),
  ("cases", toJson (cases.map fun (name, steps) =>
    Json.mkObj [("name", toJson name), ("steps", toJson (trace empty steps))]))]).compress

end Identity.EnrollmentGrantConformance
