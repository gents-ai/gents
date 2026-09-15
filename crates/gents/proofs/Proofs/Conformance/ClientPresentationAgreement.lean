import Proofs.ClientShell.PresentationAgreement

namespace Conformance.ClientPresentationAgreement

open ClientShell
open ClientShell.PresentationAgreement

structure PresentationCase where
  name : String
  draftNonEmpty : Bool
  canonical : SendDecision
  frontendReason : Option String

def blockers : List (String × SendBlockedReason) :=
  [ ("clientOffline", .clientOffline)
  , ("agentNotSelected", .agentNotSelected)
  , ("composerEmpty", .composerEmpty)
  , ("submittingRequest", .mutationInFlight)
  , ("waitingForRequestObservation", .awaitingObservation)
  , ("awaitingTurnTerminality", .awaitingTurnTerminality .streaming)
  , ("behaviorUnavailable", .sessionBehaviorMismatch)
  , ("sessionMissingFromSnapshot", .sessionAbsent)
  , ("inconsistentTurnObservation", .inconsistentObservation)
  , ("routeNotReady", .workflowBlocked)
  ]

def cases : List PresentationCase :=
  [false, true].flatMap fun draftNonEmpty =>
    { name := s!"draft_{draftNonEmpty}_ready"
    , draftNonEmpty
    , canonical := .ready
    , frontendReason := none
    } :: blockers.map fun (frontendReason, reason) =>
      { name := s!"draft_{draftNonEmpty}_{frontendReason}"
      , draftNonEmpty
      , canonical := .blocked reason
      , frontendReason := some frontendReason
      }

def jsonString (value : String) : String :=
  "\"" ++ value ++ "\""

def optionJson : Option String → String
  | none => "null"
  | some value => jsonString value

def expectedReason (c : PresentationCase) : Option String :=
  match adaptLocalDraft c.draftNonEmpty c.canonical with
  | .ready => none
  | .blocked .composerEmpty => some "composerEmpty"
  | .blocked _ => c.frontendReason

def expectedKind (c : PresentationCase) : String :=
  match adaptLocalDraft c.draftNonEmpty c.canonical with
  | .ready => "ready"
  | .blocked _ => "disabled"

def PresentationCase.toJson (c : PresentationCase) : String :=
  "{" ++
    "\"name\":" ++ jsonString c.name ++ "," ++
    "\"draft_non_empty\":" ++ toString c.draftNonEmpty ++ "," ++
    "\"canonical_reason\":" ++ optionJson c.frontendReason ++ "," ++
    "\"expected_kind\":" ++ jsonString (expectedKind c) ++ "," ++
    "\"expected_reason\":" ++ optionJson (expectedReason c) ++
  "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (cases.map PresentationCase.toJson) ++ "]"

end Conformance.ClientPresentationAgreement

def main : IO Unit :=
  IO.println Conformance.ClientPresentationAgreement.casesJson
