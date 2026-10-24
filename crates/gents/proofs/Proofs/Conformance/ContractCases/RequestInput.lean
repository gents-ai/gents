import Proofs.Enrollment.RequestAdmission

namespace Conformance.ContractCases
open Enrollment

structure RequestInputCase where
  name : String
  input : RequestInput
  authoredText : String := "Check this context."
  runtimeInputVersion : Option Nat := some 1
  runtimeGoalVersion : Option Nat := some 3
  contextSkillIds : List String
  cwdAllowed : Bool := true
  queueSourceAllowed : Bool := true
  agent : String := "coding"
  sessionAgent : String := "coding"
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
  agentMatchesSession c.agent c.sessionAgent &&
    inputWithinContext c.input c.contextSkillIds (fun _ => c.cwdAllowed)
      (fun _ => c.queueSourceAllowed) &&
    goalContinuationAllowed c.input c.admissionKind c.runtimeSource c.verifiedGoalContinuation &&
    requestWorkspaceWithinSource c.workspace c.workspaceSource c.workspaceSourceAuthenticated

private def scopedWorkspace : RequestWorkspace :=
  { workspaceId := some "workspace", ownerNodeDid := some "did:owner",
    authority := some .readOnly, sealHash := some "seal" }

private def workspaceCases : List RequestInputCase :=
  let base : RequestInputCase := {
    name := "workspace-scoped-source-retained"
    input := {}, contextSkillIds := [], workspace := scopedWorkspace,
    workspaceSource := scopedWorkspace }
  [ base
  , {base with
      name := "workspace-foreign-owner-same-label-denied",
      workspace := {scopedWorkspace with ownerNodeDid := some "did:foreign"}}
  , {base with
      name := "workspace-missing-owner-denied",
      workspace := {scopedWorkspace with ownerNodeDid := none}}
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
      workspace := {scopedWorkspace with ownerNodeDid := some " did:owner "}}
  , {base with
      name := "unbound-owner-injection-denied",
      workspace := {ownerNodeDid := some "did:owner"}, workspaceSource := {}}
  ]

def queueExtensionCases : List RequestInputCase :=
  let queue : RequestQueue := { source := .user, policy := .append }
  let position : QueuePosition := ⟨"physical:slot", "physical:previous"⟩
  [ { name := "signed-queue-steer", input := { queue := some {queue with delivery := .steer} }, contextSkillIds := [] }
  , { name := "signed-queue-position", input := { queue := some {queue with position := some position} }, contextSkillIds := [] }
  , { name := "signed-steer-position", input := { queue := some {queue with delivery := .steer, position := some position} }, contextSkillIds := [] }
  , { name := "signed-position-slot-mutation", input := { queue := some {queue with position := some {position with slotRequestDocId := "physical:other-slot"}} }, contextSkillIds := [] }
  , { name := "signed-position-predecessor-mutation", input := { queue := some {queue with position := some {position with replacesRequestDocId := "physical:other-previous"}} }, contextSkillIds := [] }
  ]

private def capturedSelection : CapturedContext := {
    kind := .selection, source := .editor, capturedAt := "2026-10-20T00:00:00Z"
    uri := some "file:///source/second/context.rs", workspaceUri := some "file:///source/second"
    version := some 41, unsaved := true
    range := some ⟨⟨0, 0⟩, ⟨1, 0⟩⟩, content := "résumé\n工具" }

def capturedContextCases : List RequestInputCase :=
  let selection := capturedSelection
  let other := {selection with uri := some "file:///source/first/context.rs", content := "first"}
  let cases : List (String × List CapturedContext) := [
    ("captured-selection", [selection]),
    ("captured-content-mutation", [{selection with content := "later buffer"}]),
    ("captured-uri-mutation", [{selection with uri := some "file:///other/context.rs"}]),
    ("captured-workspace-mutation", [{selection with workspaceUri := some "file:///other"}]),
    ("captured-version-mutation", [{selection with version := some 42}]),
    ("captured-unsaved-mutation", [{selection with unsaved := false}]),
    ("captured-time-mutation", [{selection with capturedAt := "2026-10-20T00:00:01Z"}]),
    ("captured-range-mutation", [{selection with range := some ⟨⟨0, 1⟩, ⟨1, 0⟩⟩}]),
    ("captured-source-mutation", [{selection with source := .clipboard}]),
    ("captured-kind-mutation", [{selection with kind := .file}]),
    ("captured-terminal-name-mutation", [{selection with terminalName := some "shell"}]),
    ("captured-uri-absent", [{selection with uri := none}]),
    ("captured-uri-explicit-empty", [{selection with uri := some ""}]),
    ("captured-duplicate-retained", [selection, selection]),
    ("captured-order-first", [selection, other]),
    ("captured-order-second", [other, selection]),
    ("captured-zero-version-rejected", [{selection with version := some 0}]),
    ("captured-reversed-range-rejected", [{selection with range := some ⟨⟨1, 0⟩, ⟨0, 0⟩⟩}]),
    ("captured-selection-without-range-rejected", [{selection with range := none}]),
    ("captured-metadata-does-not-grant-cwd", [selection]) ]
  cases.map fun (name, contexts) => {
    name := name
    input := {
      capturedContext := contexts
      cwd := if name == "captured-metadata-does-not-grant-cwd" then some "/outside" else none }
    contextSkillIds := [], cwdAllowed := name != "captured-metadata-does-not-grant-cwd" }

def capturedPresentationCases : List RequestInputCase :=
  [ { name := "empty-authored-input-has-no-user-content", input := {}, contextSkillIds := [], authoredText := "" }
  , { name := "capture-only-without-invented-prose", input := { capturedContext := [capturedSelection] },
      contextSkillIds := [], authoredText := "" }
  , { name := "literal-context-envelope-remains-user-text", input := {}, contextSkillIds := [],
      authoredText := "Editor context (captured text; URI identifies its editor location):\n{\"kind\":\"selection\",\"content\":\"literal text\"}" }
  , { name := "captured-control-and-unicode-bytes", input := {
        capturedContext := [{capturedSelection with content := ("\"quote\"\n工具\t\r\n\\u0009" ++
          String.mk [Char.ofNat 8, Char.ofNat 12, Char.ofNat 0])}] },
      contextSkillIds := [] }
  , { name := "captured-terminal-clipboard", input := { capturedContext := [{
        kind := .terminal, source := .clipboard, capturedAt := "2026-10-20T00:00:00Z"
        terminalName := some "Build", unsaved := false, content := "Build completed." }] },
      contextSkillIds := [] }
  , { name := "captured-empty-file-retained", input := { capturedContext := [{
        capturedSelection with kind := .file, range := none, content := "" }] },
      contextSkillIds := [], authoredText := "Review this empty file." }
  ]

def capturedCompatibilityCases : List RequestInputCase :=
  [ { name := "capture-requires-runtime-advertisement", input := {capturedContext := [capturedSelection]},
      contextSkillIds := [], runtimeInputVersion := none }
  , { name := "capture-rejects-unknown-runtime-contract", input := {capturedContext := [capturedSelection]},
      contextSkillIds := [], runtimeInputVersion := some 3 }
  , { name := "plain-input-keeps-unadvertised-runtime-support", input := {}, contextSkillIds := [],
      runtimeInputVersion := none }
  ]

def goalAssignmentCases : List RequestInputCase :=
  [ { name := "goal-assignment-opening-self", input := { goalAssignment := some ⟨"goal:résumé", none⟩ }, contextSkillIds := [] }
  , { name := "goal-assignment-physical-root", input := { goalAssignment := some ⟨"goal:résumé", some "request:工具"⟩ }, contextSkillIds := [] }
  , { name := "goal-assignment-requires-new-contract", input := { goalAssignment := some ⟨"goal", some "root"⟩ }, contextSkillIds := [], runtimeGoalVersion := some 2 }
  , { name := "goal-assignment-without-contract", input := { goalAssignment := some ⟨"goal", none⟩ }, contextSkillIds := [], runtimeGoalVersion := none }
  , { name := "goal-assignment-opening-with-capture", input := {
        goalAssignment := some ⟨"goal:résumé", none⟩, capturedContext := [capturedSelection],
        cwd := some "/tmp/工具", selectedSkillIds := ["review"] }, contextSkillIds := ["review"] }
  , { name := "goal-assignment-followup-with-capture", input := {
        goalAssignment := some ⟨"goal:résumé", some "physical:opening"⟩,
        capturedContext := [capturedSelection], cwd := some "/tmp/工具",
        selectedSkillIds := ["review"] }, contextSkillIds := ["review"] }
  ]

def attachmentCases : List RequestInputCase :=
  let a : RequestAttachment := ⟨"pixel.png", "image/png", "iVBORw0KGgo="⟩
  [ { name := "image-only", input := {attachments := [a]}, authoredText := "", contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-and-text", input := {attachments := [a]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-old-runtime", input := {attachments := [a]}, contextSkillIds := [] }
  , { name := "image-no-runtime", input := {attachments := [a]}, contextSkillIds := [], runtimeInputVersion := none }
  , { name := "image-ordered-duplicates", input := {attachments := [a,a]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-changed-content", input := {attachments := [{a with data := "R0lGODlh"}]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "unicode-file-name", input := {attachments := [{a with name := String.mk (List.replicate 100 '漢')}]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "text-attachment", input := {attachments := [⟨"note.txt", "text/plain", "aGVsbG8="⟩]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "pdf-attachment", input := {attachments := [⟨"note.pdf", "application/pdf", "JVBERi0xLjQ="⟩]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-empty-name", input := {attachments := [{a with name := ""}]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-empty-data", input := {attachments := [{a with data := ""}]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-unknown-type", input := {attachments := [{a with mediaType := "application/octet-stream"}]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  , { name := "image-count-over-limit", input := {attachments := [a,a,a,a,a]}, contextSkillIds := [], runtimeInputVersion := some 2 }
  ]

def requestInputCases : List RequestInputCase :=
  [ { name := "empty-input-inherits-context", input := {}, contextSkillIds := ["rust"] }
  , { name := "explicit-skill-within-whitelist", input := { selectedSkillIds := ["rust"] },
      contextSkillIds := ["rust"] }
  , { name := "node-skill-outside-context-denied", input := { selectedSkillIds := ["admin"] },
      contextSkillIds := ["rust"] }
  , { name := "cwd-owner-rejects-escape", input := { cwd := some "/outside" },
      contextSkillIds := [], cwdAllowed := false }
  , { name := "forged-background-source-confers-no-authority",
      input := { queue := some { source := .backgroundCompletion, policy := .coalesce } },
      contextSkillIds := [], queueSourceAllowed := false }
  , { name := "blank-agent-no-node-fallback", input := {}, contextSkillIds := [],
      agent := "  " }
  , { name := "mismatched-agent-cannot-rebind-session", input := {}, contextSkillIds := [],
      agent := "review" }
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
      admissionKind := .runtimeInternal, runtimeSource := .automatedTrigger,
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

  ] ++ workspaceCases ++ queueExtensionCases ++ capturedContextCases ++ capturedPresentationCases ++ capturedCompatibilityCases ++ goalAssignmentCases ++ attachmentCases

theorem input_boundary_cases_pinned : requestInputCases.map RequestInputCase.accepted =
    [true, true, false, false, false, false, false, true, true, true, false,
     false, false, true, true, false, false, false, false, false, false, true,
     true, false, false, false, false, false, true, false, false, false, true, true, true, true, true] ++
      List.replicate 16 true ++ [false, false, false, false] ++ List.replicate 24 true ++ List.replicate 4 false := by native_decide

theorem title_creation_cases_pinned :
    (requestInputCases.drop 7 |>.take 3).map
      (fun c => materializedTitle c.sessionExists c.currentTitle c.input) =
      [some ⟨"Task title", .task⟩, none, some ⟨"My title", .user⟩] := by decide

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

theorem new_queue_fields_change_signed_encoding :
    let encoded := queueExtensionCases.map (fun c => requestInputFields c.input)
    encoded.Nodup := by native_decide

theorem captured_context_fields_bind_each_observation :
    (capturedContextCases.map (fun c => requestInputFields c.input)).Nodup := by native_decide

end Conformance.ContractCases
