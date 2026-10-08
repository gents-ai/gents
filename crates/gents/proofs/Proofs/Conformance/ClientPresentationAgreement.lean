import Proofs.ClientShell.PresentationAgreement
import Proofs.ClientShell.Projection

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
  , ("submittingRequest", .mutationInFlight)
  , ("waitingForRequestObservation", .awaitingObservation)
  , ("behaviorUnavailable", .sessionAgentMismatch)
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
    } ::
    { name := s!"draft_{draftNonEmpty}_queue"
    , draftNonEmpty
    , canonical := .queue .running
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
  | .queue _ => none
  | .blocked .composerEmpty => some "composerEmpty"
  | .blocked _ => c.frontendReason

def expectedKind (c : PresentationCase) : String :=
  match adaptLocalDraft c.draftNonEmpty c.canonical with
  | .ready => "ready"
  | .queue _ => "queue"
  | .blocked _ => "disabled"

def canonicalKind (c : PresentationCase) : String :=
  match c.canonical with
  | .ready => "ready"
  | .queue _ => "queue"
  | .blocked _ => "disabled"

def PresentationCase.toJson (c : PresentationCase) : String :=
  "{" ++
    "\"name\":" ++ jsonString c.name ++ "," ++
    "\"draft_non_empty\":" ++ toString c.draftNonEmpty ++ "," ++
    "\"canonical_kind\":" ++ jsonString (canonicalKind c) ++ "," ++
    "\"canonical_reason\":" ++ optionJson c.frontendReason ++ "," ++
    "\"expected_kind\":" ++ jsonString (expectedKind c) ++ "," ++
    "\"expected_reason\":" ++ optionJson (expectedReason c) ++
  "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (cases.map PresentationCase.toJson) ++ "]"

def recoveryKindJson (kind : RecoveryStatusKind) : String :=
  jsonString (match kind with | .ready => "ready" | .waiting => "waiting" | .blocked => "blocked")

def recoveryActionJson : Option Unit → String
  | none => "null"
  | some _ => jsonString "required"

def recoveryCaseJson (surface : String) (connected routeReady pairingPending : Bool) : String :=
  let projected := if surface = "transport" then projectTransportRecovery connected
    else if surface = "route" then projectRouteRecovery routeReady pairingPending
    else projectConnectionFailure
  "{\"name\":" ++ jsonString s!"{surface}_{connected}_{routeReady}_{pairingPending}" ++
    ",\"surface\":" ++ jsonString surface ++
    ",\"connected\":" ++ toString connected ++
    ",\"routeReady\":" ++ toString routeReady ++
    ",\"pairingPending\":" ++ toString pairingPending ++
    ",\"expected_kind\":" ++ recoveryKindJson projected.kind ++
    ",\"expected_action\":" ++ recoveryActionJson projected.action ++ "}"

def recoveryCasesJson : String :=
  let transport := [false, true].map fun connected =>
    recoveryCaseJson "transport" connected false false
  let route := [false, true].flatMap fun ready => [false, true].map fun pairing =>
    recoveryCaseJson "route" true ready pairing
  "[" ++ String.intercalate "," (transport ++ route ++ ["offline", "failed", "incompatible", "deploymentError"].map fun surface =>
    recoveryCaseJson surface false false false) ++ "]"

def presentationAndRecoveryJson : String :=
  "{\"presentationCases\":" ++ casesJson ++ ",\"recoveryCases\":" ++ recoveryCasesJson ++ "}"

end Conformance.ClientPresentationAgreement

def main : IO Unit :=
  IO.println Conformance.ClientPresentationAgreement.presentationAndRecoveryJson
