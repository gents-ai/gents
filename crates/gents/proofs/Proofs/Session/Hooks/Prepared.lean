import Proofs.Session.Hooks.Effect

namespace Hooks

/-- What a decision prepared outside the transaction is about. The host
captures every field: the actor of the lifecycle step, the hook point, the
physical subject (a request, a tool call), a digest of the canonical inputs
the plugin was shown and the plugin's selected configuration. A plugin never
supplies or echoes a binding. -/
structure Binding (Subject : Type) where
  actor : Actor
  point : String
  subject : Subject
  inputs : String
  configuration : String
  deriving DecidableEq, Repr

/-- A plugin's answer. `allow` may carry data for the transaction (the result
of an external effect whose outcome selects a terminal state). `unavailable`
is an answer, not an absence: neither authorizes. -/
inductive Verdict (Payload : Type) where
  | allow (payload : Payload)
  | deny (reason : String)
  | unavailable
  deriving DecidableEq, Repr

structure Prepared (Subject Payload : Type) where
  binding : Binding Subject
  verdict : Verdict Payload
  deriving DecidableEq, Repr

inductive Stale where
  | unauthorized
  | cancelled
  | missing
  | stale
  | denied (reason : String)
  | unavailable
  deriving DecidableEq, Repr

variable {Subject Payload Guest : Type}

/-- The host pairs the binding it captured with whatever the plugin answered.
This is the shape for work that cannot run inside a transaction closure,
because the closure may run more than once: a model call, a host command, a
filesystem or network effect (`request.admit.verify`,
`tool_call.dispatch.decide`, `request.work.complete` in gents-ai/gents#3642). -/
def prepare (captured : Binding Subject) (plugin : Guest → Verdict Payload) (shown : Guest) :
    Prepared Subject Payload :=
  ⟨captured, plugin shown⟩

theorem plugin_cannot_choose_its_binding (captured : Binding Subject)
    (plugin : Guest → Verdict Payload) (shown : Guest) :
    (prepare captured plugin shown).binding = captured := rfl

variable [DecidableEq Subject]

/-- Re-validation inside the lifecycle transaction. `authorized` is core's own
decision for the actor and `current` is the binding recaptured from the rows
the transaction reads, so every retry of the closure checks again. -/
def admit (authorized cancelled : Bool) (current : Binding Subject)
    (prepared : Option (Prepared Subject Payload)) : Except Stale Payload :=
  if !authorized then .error .unauthorized
  else if cancelled then .error .cancelled
  else match prepared with
    | none => .error .missing
    | some decision =>
        if decision.binding = current then
          match decision.verdict with
          | .allow payload => .ok payload
          | .deny reason => .error (.denied reason)
          | .unavailable => .error .unavailable
        else .error .stale

/-- A prepared decision only narrows: without core's own authorization no
verdict admits. -/
theorem verdict_cannot_grant_authority (cancelled : Bool) (current : Binding Subject)
    (prepared : Option (Prepared Subject Payload)) :
    admit false cancelled current prepared = .error .unauthorized := by
  simp [admit]

theorem missing_decision_never_admits (authorized cancelled : Bool)
    (current : Binding Subject) (payload : Payload) :
    admit authorized cancelled current (none : Option (Prepared Subject Payload)) ≠
      .ok payload := by
  cases authorized <;> cases cancelled <;> simp [admit]

theorem changed_binding_never_admits (authorized cancelled : Bool)
    (current : Binding Subject) (decision : Prepared Subject Payload) (payload : Payload)
    (changed : decision.binding ≠ current) :
    admit authorized cancelled current (some decision) ≠ .ok payload := by
  cases authorized <;> cases cancelled <;> simp [admit, changed]

/-- Admission needs all of: core's authorization, no cancellation, and an
`allow` prepared for exactly the binding the transaction now observes. -/
theorem admission_requires_matching_allow (authorized cancelled : Bool)
    (current : Binding Subject) (prepared : Option (Prepared Subject Payload))
    (payload : Payload) (admitted : admit authorized cancelled current prepared = .ok payload) :
    authorized = true ∧ cancelled = false ∧ prepared = some ⟨current, .allow payload⟩ := by
  cases authorized with
  | false => simp [admit] at admitted
  | true =>
    cases cancelled with
    | true => simp [admit] at admitted
    | false =>
      cases prepared with
      | none => simp [admit] at admitted
      | some decision =>
        obtain ⟨binding, verdict⟩ := decision
        by_cases matching : binding = current
        · subst matching
          cases verdict with
          | allow value =>
            simp only [admit, Bool.not_true, Bool.false_eq_true, if_false, if_true,
              Except.ok.injEq] at admitted
            simp [admitted]
          | deny reason => simp [admit] at admitted
          | unavailable => simp [admit] at admitted
        · simp [admit, matching] at admitted

/-- Retry safety: a closure that runs again re-admits against the binding it
observes then. An unchanged binding gives the same answer; a changed one is
stale, whatever the first attempt concluded. -/
theorem readmission_is_decided_by_the_current_binding (authorized cancelled : Bool)
    (first second : Binding Subject) (decision : Prepared Subject Payload)
    (bound : decision.binding = first) (changed : first ≠ second) (payload : Payload) :
    admit authorized cancelled second (some decision) ≠ .ok payload :=
  changed_binding_never_admits authorized cancelled second decision payload (bound ▸ changed)

end Hooks
