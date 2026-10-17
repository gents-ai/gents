import Proofs.GoalAutomation.ClientStart
import Proofs.Conformance.Contracts.Json.SessionDocuments

namespace Conformance.GoalClientStartContracts

open GoalAutomation.ClientStart Conformance.Contracts Lean

def requested : AgentSession.Document :=
  { scope := ⟨1, 10, some 1⟩, agent := 3, createdAt := 5 }
def empty : State := ⟨none, ⟨false, false, false, false⟩⟩
def facts : Facts := ⟨⟨true, true, true, false, false⟩, false, 2, 10, true, false, none, none, none⟩

structure Case where
  name : String
  before : State := empty
  objective : String := " ship "
  budget : Option Int := some 1000
  facts : Facts := GoalClientStartContracts.facts
  commit : Bool := true
  deriving DecidableEq, Repr

instance : Inhabited Case := ⟨{ name := "" }⟩

def existing : AgentSession.Document :=
  { requested with
    createdAt := 1
    closedAt := some 4
    title := some ⟨"Preserved", .user⟩
    tags := ["kept"] }
def boundFacts : Facts :=
  { facts with
    currentSessionDocument := some "session-document"
    expectedSessionDocument := some "session-document" }

def cases : List Case :=
  [ { name := "fresh_session" }
  , { name := "reuse_session", before := { empty with session := some existing }, facts := boundFacts }
  , { name := "foreign_agent", before := { empty with session := some { existing with agent := 4 } }, facts := boundFacts }
  , { name := "foreign_requester", before := { empty with session := some { existing with scope.requester := some 9 } },
      facts := { boundFacts with authority.sessionOwned := false } }
  , { name := "invalid_first_request", facts := { facts with firstRequestValid := false } }
  , { name := "invalid_command_signature", facts := { facts with authority.signatureValid := false } }
  , { name := "expired_command", facts := { facts with now := 10 } }
  , { name := "busy_session", facts := { boundFacts with unfinishedRequest := true }, before := { empty with session := some existing } }
  , { name := "recreated_session", facts := { boundFacts with expectedSessionDocument := some "old-header" }, before := { empty with session := some existing } }
  , { name := "unexpected_session", facts := { boundFacts with expectedSessionDocument := none }, before := { empty with session := some existing } }
  , { name := "existing_goal", before := { empty with submission.durableGoal := true } }
  , { name := "blank_objective", objective := "\u3000\u00A0" }
  , { name := "zero_budget", budget := some 0 }
  , { name := "negative_budget", budget := some (-1) }
  , { name := "unbudgeted", budget := none }
  , { name := "maximum_budget", budget := some GoalAutomation.maxTokenBudget }
  , { name := "rollback", commit := false }
  , { name := "matching_creation_claim", facts := { facts with existingClaim := some ⟨"1", "10", "ship", some 1000⟩ } }
  , { name := "conflicting_creation_claim", facts := { facts with existingClaim := some ⟨"1", "10", "earlier", some 1000⟩ } }
  ]

def sessionJson (session : Option AgentSession.Document) : Json :=
  session.map documentJson |>.getD Json.null

def caseJson (c : Case) : Json :=
  let after := start? c.before requested c.objective c.budget c.facts c.commit
  Json.mkObj [ ("name", toJson c.name), ("session", sessionJson c.before.session),
    ("requested", documentJson requested), ("existing_goal", toJson c.before.submission.durableGoal),
    ("objective", toJson c.objective), ("budget", toJson c.budget),
    ("signature_valid", toJson c.facts.authority.signatureValid),
    ("first_request_valid", toJson c.facts.firstRequestValid),
    ("now", toJson c.facts.now), ("expires_at", toJson c.facts.expiresAt),
    ("busy", toJson c.facts.unfinishedRequest),
    ("current_session_document", toJson c.facts.currentSessionDocument),
    ("expected_session_document", toJson c.facts.expectedSessionDocument),
    ("commit", toJson c.commit), ("published", toJson after.isSome),
    ("claim", c.facts.existingClaim.map (fun claim => Json.mkObj
      [("owner", toJson claim.owner), ("session", toJson claim.session),
        ("objective", toJson claim.objective), ("token_budget", toJson claim.tokenBudget)]) |>.getD Json.null),
    ("expected_session", after.map (fun s => sessionJson s.session) |>.getD Json.null) ]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : (cases[1]!).before.session = (start? (cases[1]!).before requested "ship" (some 1000)
    boundFacts true).bind (·.session) := by native_decide

end Conformance.GoalClientStartContracts
