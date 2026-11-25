import Proofs.Documents.Configuration

namespace InferenceSetup

/-- The providers `gents init`, the shell and the desktop set a backend up
for. -/
inductive Provider where
  | openai
  | anthropic
  | grok
  | «local»
  | openrouter
  deriving DecidableEq, Repr

/-- Decision: the concurrent model calls setup writes into the backend it
creates. A local server gets the default of a backend written without the
field (`Configuration.defaultBackendControls`). An Agent's request workers are
not sized from it (`CanonicalOutput.Execution.WorkerLevels`), so the number
bounds only the calls the server runs at once, which vLLM and Ollama batch; a
server with fewer parallel slots queues the rest itself. A hosted account gets
eight; a rate limit it reaches is met by the completion retry's backoff. -/
def recommendedMaxConcurrent : Provider → Nat
  | .«local» => Configuration.defaultBackendControls.maxConcurrent
  | .openai | .anthropic | .grok | .openrouter => 8

theorem local_recommendation_is_the_unset_default :
    recommendedMaxConcurrent .«local» = Configuration.defaultBackendControls.maxConcurrent :=
  rfl

theorem every_recommendation_admits_a_call (provider : Provider) :
    0 < recommendedMaxConcurrent provider := by
  cases provider <;> decide

end InferenceSetup
