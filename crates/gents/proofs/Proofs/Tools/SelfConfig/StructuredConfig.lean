import Proofs.Tools.SelfConfig.Apply

/-! ## Structured config: preview and apply

A preview validates a candidate and never enters the write path. An apply
enters it and writes the merged document. `node_did` is never writable, so a
patch naming it is refused and the stored document stays as it was. -/

namespace SelfConfig.StructuredConfig

inductive Call where
  | preview
  | apply
  deriving DecidableEq, Repr

/-- Whether a call enters the write path (`config_execution.mutation_entered`). -/
def mutationEntered : Call → Bool
  | .preview => false
  | .apply => true

/-- A call commits exactly when it enters the write path. -/
abbrev committed (c : Call) : Bool := mutationEntered c

def fieldsEqual (a b : Store) : Bool :=
  (allFields .agentContext).all fun k => decide (a .agentContext k = b .agentContext k)

def promptPatch (prompt : FieldValue) : Patch := [⟨"system_prompt", .set prompt⟩]

def foreignPatch : Patch := [⟨"node_did", .set "foreign"⟩]

def initialStore : Store := fun _ => Doc.ofList []

def previewStore (s : Store) (_p : Patch) : Store := s

def commitStore (s : Store) (p : Patch) : Store × Bool :=
  runStep (fun _ => true) (fun _ => true) .agentContext s p

def storedPrompt (s : Store) : Option FieldValue := s .agentContext "system_prompt"

theorem foreign_patch_refused : admissible .agentContext foreignPatch = false := by
  decide

theorem structured_config_preview_and_apply_round_trip_literal_prompt (s : Store) :
    commitStore s foreignPatch = (s, false) := by
  simp [commitStore, runStep, step, foreign_patch_refused]

/-- Whether a configuration write records the experience attempt it carries
(`Tools.Memory.Experience.AttemptRow`). Decision (arXiv 2611.18856: only the
chosen candidate is recorded): the attempt row is staged in the write's own
transaction, so it exists exactly when the write commits. A preview never
enters the write path and a refused write commits nothing, so neither records
an attempt. -/
def experienceStaged (c : Call) (validate guard : Doc → Bool) (t : Target) (s : Store)
    (p : Patch) : Bool :=
  committed c && (runStep validate guard t s p).2

theorem experience_staged_iff_the_write_commits (c : Call) (validate guard : Doc → Bool)
    (t : Target) (s : Store) (p : Patch) :
    experienceStaged c validate guard t s p = true ↔
      c = .apply ∧ (step validate guard t (s t) p).isSome := by
  cases c <;> cases h : step validate guard t (s t) p <;>
    simp [experienceStaged, committed, mutationEntered, runStep, h]

theorem a_preview_stages_no_experience (validate guard : Doc → Bool) (t : Target) (s : Store)
    (p : Patch) : experienceStaged .preview validate guard t s p = false := rfl

theorem a_refused_write_stages_no_experience (c : Call) (validate guard : Doc → Bool)
    (t : Target) (s : Store) (p : Patch) (h : step validate guard t (s t) p = none) :
    experienceStaged c validate guard t s p = false ∧ (runStep validate guard t s p).1 = s := by
  simp [experienceStaged, runStep, h]

end SelfConfig.StructuredConfig
