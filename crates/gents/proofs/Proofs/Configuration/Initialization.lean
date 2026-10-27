import Proofs.Configuration

namespace Configuration.Initialization

structure Input where
  engineer : Bool
  requestedModel : Option String
  preset : Option String
  presetDefault : Option String
  localDefault : String
  concurrent : Option Int
  deriving Repr

structure Plan where
  model : Option String
  enabled : Bool
  concurrent : Option Int
  effectiveConcurrent : Nat
  contextOverride : Option Int := none
  outputOverride : Option Int := none
  recordsProbe : Bool := false
  deriving DecidableEq, Repr

/-- A preset's default is an input from the provider-preset owner. An absent
Engineer selection is deliberately unconfigured, never a guessed local model.
Native profiles encode that absence as an empty model selection, which the
existing runtime resolver refuses until an explicit selection is made. -/
def selectedModel (input : Input) : Option (Option String) :=
  match input.requestedModel with
  | some raw => if raw.trim.isEmpty then none else some (some raw.trim)
  | none => match input.presetDefault with
    | some model => some (some model)
    | none => if input.preset.isSome then none
      else some (if input.engineer then none else some input.localDefault)

/-- Seeding configuration supplies no measured health and pins no model limits.
Only authored concurrency survives; the backend owner retains its safe default.
Discovery and later explicit provider selection keep their existing owners. -/
def seed (input : Input) : Option Plan := do
  let model ← selectedModel input
  let controls ← resolveBackendControls none none input.concurrent none
  pure {
    model := model
    enabled := model.isSome
    concurrent := input.concurrent
    effectiveConcurrent := controls.maxConcurrent
  }

theorem no_selection_stays_disabled (input : Input) (plan : Plan)
    (h : seed input = some plan) (hm : plan.model = none) : plan.enabled = false := by
  simp only [seed, bind, Option.bind] at h
  split at h <;> simp_all
  split at h <;> simp_all
  cases h
  simp_all

theorem seeding_preserves_capability_defaults (input : Input) (plan : Plan)
    (h : seed input = some plan) :
    plan.contextOverride = none ∧ plan.outputOverride = none ∧ plan.recordsProbe = false := by
  simp only [seed, bind, Option.bind] at h
  split at h <;> simp_all
  split at h <;> simp_all
  cases h
  simp

end Configuration.Initialization
