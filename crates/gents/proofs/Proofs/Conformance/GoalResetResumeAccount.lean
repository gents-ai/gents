import Proofs.Automation.GoalAutomation.ResetResume
import Proofs.Conformance.GoalResetResume
import Proofs.Conformance.Family

/-! A limited Goal whose reset has been reported and whose sign-in may have been
removed. Times are seconds after 2030-01-01T00:00:00Z. -/
namespace Conformance.GoalResetResumeAccount

open Lean GoalAutomation.OperatorResume
open Conformance.GoalResetResumeContracts (limited request)

structure ResetInput where
  accountPresent : Bool
  now : Nat
  deriving ToJson

structure ResumeOutput where
  created : Bool
  goalUsageLimited : Bool
  childCount : Nat
  deriving ToJson

def resetFacts (i : ResetInput) : ResetFacts :=
  { optedIn := true, resetAt := some 3600, limitStartedAt := 0, now := i.now,
    profileNamesAccount := true, accountEnabled := true, backendEnabled := true,
    accountPresent := i.accountPresent }

@[conformance_family]
def resumes : Conformance.Family ResetInput ResumeOutput where
  inputs :=
    [ { accountPresent := false, now := 3600 }
    , { accountPresent := false, now := 36000 }
    , { accountPresent := true, now := 3599 }
    , { accountPresent := true, now := 3600 } ]
  model := fun i =>
    let result := resumeBy limited (.resetReached (resetFacts i)) request true
    { created := decide (result.2 = .created)
      goalUsageLimited := decide (result.1.goal.status = .usageLimited)
      childCount := result.1.children.length }

end Conformance.GoalResetResumeAccount
