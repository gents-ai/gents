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
Native profiles encode that absence as an empty model selection. Their disabled
backend remains unavailable while the runtime serves provider setup. -/
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

/-- A disabled backend with no selected model is unfinished setup, not a broken
runtime configuration. Enabling that same selection without choosing a model
is invalid. Neither state permits inference or invents a model selection. -/
def unselectedModelReason (backendEnabled : Bool) (model : Option String) : Option String :=
  if model.isSome then none
  else some (if backendEnabled then "inference_profile_invalid" else "backend_disabled")

theorem unselected_disabled_backend_is_unavailable :
    unselectedModelReason false none = some "backend_disabled" := rfl

theorem unselected_enabled_backend_is_invalid :
    unselectedModelReason true none = some "inference_profile_invalid" := rfl

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
