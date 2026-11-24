import Proofs.Runtime.BackendHealth.Properties

/-! What one models-endpoint probe tells the health model, read together with
the calls admitted to the same backend. -/

namespace Proofs.BackendHealth

/-- How one models-endpoint probe ended: the endpoint answered with its model
list, answered with an error or refused the connection, or gave no answer
before the probe's deadline. -/
inductive ProbeOutcome where
  | answered
  | failed
  | timedOut
  deriving DecidableEq, Repr

/-- The health event a probe supplies, given whether a call admitted to the
backend received a response item while the probe ran.

A probe that times out while an admitted call receives response items supplies
no event. The items show the backend serving, so the slow models endpoint is
not evidence of an outage; they show the generation path rather than the
models endpoint, so they are not a success either.

A probe that fails counts whatever the admitted calls do. An error answer or a
refused connection says a new request to the endpoint fails now, which items
arriving on a connection opened earlier cannot contradict; and demotion stops
only new admissions (`InferenceCall.Registry`), so calls already admitted keep
running to their own result either way. -/
def probeEvent (outcome : ProbeOutcome) (admittedCallProgressed : Bool) : Option Event :=
  match outcome, admittedCallProgressed with
  | .answered, _ => some .probeSuccess
  | .failed, _ => some .probeFail
  | .timedOut, true => none
  | .timedOut, false => some .probeFail

/-- The model after one probe: the event's step, or unchanged when the probe
supplies none. -/
def observe (m : Model) (outcome : ProbeOutcome) (progressed : Bool) (K : Threshold) : Model :=
  match probeEvent outcome progressed with
  | some event => step m event K
  | none => m

def observeAll (m : Model) (probes : List (ProbeOutcome × Bool)) (K : Threshold) : Model :=
  probes.foldl (fun acc probe => observe acc probe.1 probe.2 K) m

theorem progressing_timeout_leaves_health_unchanged (m : Model) (K : Threshold) :
    observe m .timedOut true K = m := rfl

/-- A backend whose admitted call receives response items during every probe
is never demoted by those probes timing out, however many there are. -/
theorem progressing_backend_is_never_demoted_by_probe_timeouts
    (m : Model) (n : Nat) (K : Threshold) :
    observeAll m (List.replicate n (.timedOut, true)) K = m := by
  induction n with
  | zero => rfl
  | succ n ih =>
      rw [List.replicate_succ]
      exact ih

theorem idle_timeouts_are_probe_failures (m : Model) (n : Nat) (K : Threshold) :
    observeAll m (List.replicate n (.timedOut, false)) K =
      run m (List.replicate n .probeFail) K := by
  induction n generalizing m with
  | zero => rfl
  | succ n ih =>
      rw [List.replicate_succ, List.replicate_succ, run_cons]
      exact ih (step m .probeFail K)

/-- With no admitted call progressing, `K` probe timeouts demote a backend as
`K` probe failures do. -/
theorem idle_backend_is_demoted_by_K_probe_timeouts (m : Model)
    (hm : m.state.needsAccountRecovery = false) (h0 : m.failureCount = 0) (K : Threshold) :
    (observeAll m (List.replicate K.val (.timedOut, false)) K).state = .unhealthy := by
  rw [idle_timeouts_are_probe_failures]
  exact b1_demotes_at_K m hm h0 K

theorem failed_probe_counts_whatever_admitted_calls_do (m : Model) (progressed : Bool)
    (K : Threshold) :
    observe m .failed progressed K = step m .probeFail K := by
  cases progressed <;> rfl

theorem answered_probe_recovers (m : Model) (progressed : Bool) (K : Threshold) :
    observe m .answered progressed K = { state := .healthy, failureCount := 0 } := by
  cases progressed <;> rfl

end Proofs.BackendHealth
