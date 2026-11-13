import Proofs.Automation.GoalAutomation.OperatorResume
import Proofs.Conformance.Family
import Proofs.Conformance.GoalOperatorResume
import Lean.Data.Json.FromToJson

namespace Conformance.GoalOperatorResumeProfiles

open Lean GoalAutomation.OperatorResume Conformance.GoalOperatorResumeContracts

structure RetryInput where
  compaction : Option String
  operatorMoveTo : Option String
  deriving ToJson

structure RetryOutput where
  firstProfile : String
  retrySwitched : Bool
  retryCreated : Bool
  sameRequest : Bool
  childIsFirst : Bool
  profile : String
  compaction : Option String
  deriving ToJson, DecidableEq

def retryObservation (i : RetryInput) : RetryOutput :=
  let start : OperatorProfiles := ⟨"a", i.compaction⟩
  let first := accountResume initial start request "b"
  let moved := operatorMove first.2.1 i.operatorMoveTo
  let retry := accountResume first.1 moved request "b"
  { firstProfile := first.2.1.limited
    retrySwitched := switchesProfile first.1 request
    retryCreated := retry.2.2 == .created
    sameRequest := retry.1.latestRequest == first.1.latestRequest
    childIsFirst := retry.1.children == first.1.children
    profile := retry.2.1.limited
    compaction := retry.2.1.compaction }

@[conformance_family]
def resumeOnRetry : Family RetryInput RetryOutput where
  inputs :=
    [ { compaction := none, operatorMoveTo := some "a" }
    , { compaction := some "a", operatorMoveTo := none } ]
  model := retryObservation

theorem completed_resume_retry_does_not_switch_a_profile_moved_again :
    retryObservation { compaction := none, operatorMoveTo := some "a" } =
      { firstProfile := "b", retrySwitched := false, retryCreated := false,
        sameRequest := true, childIsFirst := true, profile := "a", compaction := none } := by
  decide

theorem resume_on_retry_leaves_the_compaction_profile :
    retryObservation { compaction := some "a", operatorMoveTo := none } =
      { firstProfile := "b", retrySwitched := false, retryCreated := false,
        sameRequest := true, childIsFirst := true, profile := "b", compaction := some "a" } := by
  decide

end Conformance.GoalOperatorResumeProfiles
