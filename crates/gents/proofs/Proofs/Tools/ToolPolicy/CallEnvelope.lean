import Proofs.Tools.ToolPolicy.PluginCalls

namespace ToolPolicy.CallEnvelope

/-- The owner that builds the context a callee runs under. `foreground` is the
loop re-entering the admitted request scope for a model call; `child` is a
plugin's child call through the loop's dispatcher; `background` is a
`spawn_process` target and `handoff` a foreground native process the
background owner takes over, both in a task of their own; `plugin` is the
plugin host serving a guest; `taskHook` is a configured hook command at
request finish, outside every tool call. -/
inductive Path where
  | foreground
  | child
  | background
  | handoff
  | plugin
  | taskHook
  deriving DecidableEq, Repr

/-- Where the callee's deadline comes from: none, its parent's, or the lifetime
its owner admitted for this execution. -/
inductive Source where
  | absent
  | parent
  | admitted
  deriving DecidableEq, Repr

/-- Where the callee's cancellation comes from. Tokens have no identity the
runtime can compare, so this is observed by propagation: the parent's own
token stops both, a child token of the parent's stops only the callee, and a
fresh token is independent of the parent. -/
inductive Cancel where
  | absent
  | parent
  | childOfParent
  | fresh
  deriving DecidableEq, Repr

/-- What a callee observes of the call it serves. -/
structure Seen where
  deadline : Source
  cancel : Cancel
  background : Bool
  dispatcher : Bool
  interactive : Bool
  approval : Bool
  recorder : Bool
  deriving DecidableEq, Repr

/-- The context of a call is a function of the call alone: its path, the
parent's context (`hasScope` is whether the parent runs under a request
execution scope) and what the owner admitted for it (`offered`: the owner
offers a tool dispatcher; `process`: the target is a native process). Nothing
else, in particular nothing the task it happens to run on carries, enters it.

A child and a handoff need the parent's request scope and are refused without
one. A task spawned for background work starts empty: it keeps only the
request's `interactive` and approval context, and its owner supplies the
admitted deadline, a fresh cancellation, its own process recorder and, for a
composing non-process target, a dispatcher. -/
def derive (path : Path)
    (hasScope dispatcher interactive approval recorder offered process : Bool) :
    Option Seen :=
  let inherited (deadline : Source) (cancel : Cancel) (dispatcher : Bool) : Seen :=
    { deadline, cancel, background := false, dispatcher, interactive, approval, recorder }
  match path with
  | .foreground =>
      some (if hasScope then inherited .parent .childOfParent offered
        else inherited .absent .absent offered)
  | .child =>
      if hasScope then some (inherited .parent .parent PluginCalls.childDispatcher) else none
  | .background =>
      some { deadline := .admitted, cancel := .fresh, background := true,
             dispatcher := offered && !process, interactive, approval, recorder := true }
  | .handoff =>
      if hasScope then
        some { deadline := .parent, cancel := .fresh, background := false,
               dispatcher := false, interactive, approval, recorder := true }
      else none
  | .plugin =>
      some (if hasScope then inherited .parent .parent dispatcher
        else inherited .absent .absent dispatcher)
  | .taskHook =>
      some { deadline := .admitted, cancel := .fresh, background := false,
             dispatcher := false, interactive := false, approval := false, recorder := true }

theorem child_runs_under_parent_envelope
    (dispatcher interactive approval recorder offered process : Bool) :
    ∃ seen, derive .child true dispatcher interactive approval recorder offered process = some seen ∧
      seen.deadline = .parent ∧ seen.cancel = .parent := by
  exact ⟨_, rfl, rfl, rfl⟩

theorem child_requires_parent_scope
    (dispatcher interactive approval recorder offered process : Bool) :
    derive .child false dispatcher interactive approval recorder offered process = none := rfl

/-- A child never receives a dispatcher, whatever its parent had: the owner of
`PluginCalls.children_cannot_delegate`. -/
theorem child_has_no_dispatcher
    (hasScope dispatcher interactive approval recorder offered process selected : Bool)
    (seen : Seen)
    (h : derive .child hasScope dispatcher interactive approval recorder offered process = some seen) :
    PluginCalls.available selected seen.dispatcher = false := by
  cases hasScope <;> simp [derive] at h
  subst h
  exact PluginCalls.children_cannot_delegate selected

theorem spawned_gets_admitted_deadline_and_fresh_cancel
    (hasScope dispatcher interactive approval recorder offered process : Bool) :
    derive .background hasScope dispatcher interactive approval recorder offered process =
      some { deadline := .admitted, cancel := .fresh, background := true,
             dispatcher := offered && !process, interactive, approval, recorder := true } := rfl

/-- The request's interactive flag and approval context are the only values a
spawned task keeps from its parent. -/
theorem request_constants_cross_spawn
    (path : Path) (h : path = .background ∨ path = .handoff)
    (dispatcher interactive approval recorder offered process : Bool) (seen : Seen)
    (hs : derive path true dispatcher interactive approval recorder offered process = some seen) :
    seen.interactive = interactive ∧ seen.approval = approval := by
  rcases h with rfl | rfl <;> simp [derive] at hs <;> subst hs <;> exact ⟨rfl, rfl⟩

/-- A spawned task never inherits the parent's dispatcher or process recorder. -/
theorem spawned_ignores_parent_dispatcher_and_recorder
    (path : Path) (h : path = .background ∨ path = .handoff)
    (hasScope dispatcher dispatcher' interactive approval recorder recorder' offered process : Bool) :
    derive path hasScope dispatcher interactive approval recorder offered process =
      derive path hasScope dispatcher' interactive approval recorder' offered process := by
  rcases h with rfl | rfl <;> cases hasScope <;> rfl

theorem process_target_never_delegates
    (hasScope dispatcher interactive approval recorder offered : Bool) (seen : Seen)
    (h : derive .background hasScope dispatcher interactive approval recorder offered true = some seen) :
    seen.dispatcher = false := by
  simp [derive] at h
  subst h
  simp

/-- The plugin host passes its guest exactly the call it serves. -/
theorem plugin_host_forwards_its_call
    (dispatcher interactive approval recorder offered process : Bool) :
    derive .plugin true dispatcher interactive approval recorder offered process =
      some { deadline := .parent, cancel := .parent, background := false,
             dispatcher, interactive, approval, recorder } := rfl

/-- A task hook runs outside every tool call: it sees only its own deadline,
cancellation and recorder. -/
theorem task_hook_sees_no_request_context
    (hasScope dispatcher interactive approval recorder offered process : Bool) (seen : Seen)
    (h : derive .taskHook hasScope dispatcher interactive approval recorder offered process = some seen) :
    seen.interactive = false ∧ seen.approval = false ∧ seen.dispatcher = false := by
  simp [derive] at h
  subst h
  exact ⟨rfl, rfl, rfl⟩

end ToolPolicy.CallEnvelope
