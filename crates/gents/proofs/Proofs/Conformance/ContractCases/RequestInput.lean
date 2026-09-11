import Proofs.Enrollment.RequestAdmission

namespace Conformance.ContractCases
open Enrollment

/-- Path and origin observations are supplied by their existing validators;
these cases do not replace those validators with string-prefix authorization. -/
structure RequestInputCase where
  name : String
  input : RequestInput
  contextSkillIds : List String
  cwdAllowed : Bool := true
  queueSourceAllowed : Bool := true
  behavior : String := "coding"
  sessionBehavior : String := "coding"
  sessionExists : Bool := true
  currentTitle : Option AgentSession.Title := none
  admissionKind : AgentRequestAdmissionKind := .localSelf
  runtimeSource : RuntimeInternalSourceKind := .localControl
  verifiedGoalContinuation : Option GoalContinuationInput := none
  workspace : RequestWorkspace := {}
  workspaceSource : RequestWorkspace := {}
  workspaceSourceAuthenticated : Bool := true
  deriving Repr

def RequestInputCase.accepted (c : RequestInputCase) : Bool :=
  behaviorMatchesSession c.behavior c.sessionBehavior &&
    inputWithinContext c.input c.contextSkillIds (fun _ => c.cwdAllowed)
      (fun _ => c.queueSourceAllowed) &&
    goalContinuationAllowed c.input c.admissionKind c.runtimeSource c.verifiedGoalContinuation &&
    requestWorkspaceWithinSource c.workspace c.workspaceSource c.workspaceSourceAuthenticated

private def scopedWorkspace : RequestWorkspace :=
  { workspaceId := some "workspace", ownerAgentDid := some "did:owner",
    authority := some .readOnly, sealHash := some "seal" }

private def workspaceCases : List RequestInputCase :=
  let base : RequestInputCase := {
    name := "workspace-scoped-source-retained"
    input := {}, contextSkillIds := [], workspace := scopedWorkspace,
    workspaceSource := scopedWorkspace }
  [ base
  , {base with
      name := "workspace-foreign-owner-same-label-denied",
      workspace := {scopedWorkspace with ownerAgentDid := some "did:foreign"}}
  , {base with
      name := "workspace-missing-owner-denied",
      workspace := {scopedWorkspace with ownerAgentDid := none}}
  , {base with
      name := "workspace-missing-id-denied",
      workspace := {scopedWorkspace with workspaceId := none}}
  , {base with
      name := "workspace-forged-source-denied", workspaceSourceAuthenticated := false}
  , {base with
      name := "workspace-authority-widening-denied",
      workspace := {scopedWorkspace with authority := some .readWrite}}
  , {base with
      name := "workspace-authority-attenuation-allowed",
      workspaceSource := {scopedWorkspace with authority := some .readWrite}}
  , {base with
      name := "workspace-seal-substitution-denied",
      workspace := {scopedWorkspace with sealHash := some "forged"}}
  , {base with
      name := "workspace-padded-owner-denied",
      workspace := {scopedWorkspace with ownerAgentDid := some " did:owner "}}
  , {base with
      name := "unbound-owner-injection-denied",
      workspace := {ownerAgentDid := some "did:owner"}, workspaceSource := {}}
  ]

def requestInputCases : List RequestInputCase :=
  [ { name := "empty-input-inherits-context", input := {}, contextSkillIds := ["rust"] }
  , { name := "explicit-skill-within-whitelist", input := { selectedSkillIds := ["rust"] },
      contextSkillIds := ["rust"] }
  , { name := "principal-skill-outside-context-denied", input := { selectedSkillIds := ["admin"] },
      contextSkillIds := ["rust"] }
  , { name := "cwd-owner-rejects-escape", input := { cwd := some "/outside" },
      contextSkillIds := [], cwdAllowed := false }
  , { name := "forged-background-source-confers-no-authority",
      input := { queue := some { source := .backgroundCompletion, policy := .coalesce } },
      contextSkillIds := [], queueSourceAllowed := false }
  , { name := "blank-behavior-no-principal-fallback", input := {}, contextSkillIds := [],
      behavior := "  " }
  , { name := "mismatched-behavior-cannot-rebind-session", input := {}, contextSkillIds := [],
      behavior := "review" }
  , { name := "initial-title-only-on-creation",
      input := { initialTitle := some ⟨"Task title", .task⟩ }, contextSkillIds := [],
      sessionExists := false }
  , { name := "existing-untitled-session-not-retitled-by-old-input",
      input := { initialTitle := some ⟨"Task title", .task⟩ }, contextSkillIds := [] }
  , { name := "user-title-preserved-on-reuse",
      input := { initialTitle := some ⟨"Task title", .task⟩ }, contextSkillIds := [],
      currentTitle := some ⟨"My title", .user⟩ }
  , { name := "goal-source-needs-owned-admission",
      input := { queue := some { source := .goal, policy := .coalesce } },
      contextSkillIds := [], queueSourceAllowed := false }
  , { name := "signed-owner-cannot-author-goal-runtime-facts",
      input := { goalContinuation := some ⟨1, false⟩ }, contextSkillIds := [],
      verifiedGoalContinuation := some ⟨1, false⟩ }
  , { name := "runtime-goal-without-verified-physical-edge-denied",
      input := { goalContinuation := some ⟨1, false⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal }
  , { name := "runtime-goal-original-non-wrapup-preserved",
      input := { goalContinuation := some ⟨1, false⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal, verifiedGoalContinuation := some ⟨1, false⟩ }
  , { name := "runtime-goal-original-wrapup-preserved",
      input := { goalContinuation := some ⟨2, true⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal, verifiedGoalContinuation := some ⟨2, true⟩ }
  , { name := "goal-queue-with-missing-original-facts-denied",
      input := { queue := some { source := .goal, policy := .coalesce } },
      contextSkillIds := [], admissionKind := .runtimeInternal }
  , { name := "goal-negative-sequence-denied",
      input := { goalContinuation := some ⟨-1, false⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal, verifiedGoalContinuation := some ⟨-1, false⟩ }
  , { name := "goal-zero-sequence-denied",
      input := { goalContinuation := some ⟨0, false⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal, verifiedGoalContinuation := some ⟨0, false⟩ }
  , { name := "wrapup-cannot-be-substituted-after-signing",
      input := { goalContinuation := some ⟨2, true⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal, verifiedGoalContinuation := some ⟨2, false⟩ }
  , { name := "other-runtime-source-is-not-goal-control",
      input := { goalContinuation := some ⟨1, false⟩ }, contextSkillIds := [],
      admissionKind := .runtimeInternal, runtimeSource := .localChild,
      verifiedGoalContinuation := some ⟨1, false⟩ }

  , { name := "encoding-all-queue-fields-and-unicode-before-admission"
      input := {
        selectedSkillIds := ["résumé", "a,b"]
        cwd := some "/tmp/工具"
        initialTitle := some ⟨"Résumé", .user⟩
        queue := some {
          source := .backgroundCompletion
          policy := .coalesce
          key := some "wake:key"
          queuedAfterRequestId := some "parent"
          interruptedRequestId := some "interrupted"
          backgroundCompletionWakeVersion := some 1 } }
      contextSkillIds := ["résumé", "a,b"]
      queueSourceAllowed := false }
  , { name := "encoding-goal-queue-retains-explicit-false"
      input := {
        queue := some {
          source := .goal
          policy := .coalesce
          key := some "goal:key"
          queuedAfterRequestId := some "parent" }
        goalContinuation := some ⟨3, false⟩ }
      contextSkillIds := []
      admissionKind := .runtimeInternal
      verifiedGoalContinuation := some ⟨3, false⟩ }

  ] ++ workspaceCases

theorem input_boundary_cases_pinned : requestInputCases.map RequestInputCase.accepted =
    [true, true, false, false, false, false, false, true, true, true, false,
     false, false, true, true, false, false, false, false, false, false, true,
     true, false, false, false, false, false, true, false, false, false] := by native_decide

theorem title_creation_cases_pinned :
    (requestInputCases.drop 7 |>.take 3).map
      (fun c => materializedTitle c.sessionExists c.currentTitle c.input) =
      [some ⟨"Task title", .task⟩, none, some ⟨"My title", .user⟩] := by decide

/-- Canonical input coverage: each optional subtree participates in the signed
field list, and a one-element delimiter-containing skill is not two skills. -/
theorem input_encoding_distinguishes_semantics :
    requestInputFields {} ≠ requestInputFields { cwd := some "" } ∧
    requestInputFields {} ≠ requestInputFields { initialTitle := some ⟨"", .user⟩ } ∧
    requestInputFields {} ≠ requestInputFields
      { queue := some { source := .user, policy := .append } } ∧
    requestInputFields { selectedSkillIds := ["a,b"] } ≠
      requestInputFields { selectedSkillIds := ["a", "b"] } ∧
    requestInputFields {} ≠ requestInputFields { goalContinuation := some ⟨1, false⟩ } ∧
    requestInputFields { goalContinuation := some ⟨1, false⟩ } ≠
      requestInputFields { goalContinuation := some ⟨1, true⟩ } := by decide

end Conformance.ContractCases
