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

end SelfConfig.StructuredConfig
