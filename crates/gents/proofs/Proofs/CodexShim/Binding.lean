import Proofs.Basic
import Proofs.RuntimeReconcile.State

namespace CodexShim.Binding

inductive UnboundReason
  | dependencyMissing
  | hostResource
  deriving DecidableEq, Repr

inductive ShimState
  | unbound (reason : UnboundReason)
  | bound
  deriving DecidableEq, Repr

namespace ShimState

def isBound : ShimState → Bool
  | .bound => true
  | .unbound _ => false

end ShimState

structure Shim where
  boundBehavior : BehaviorId
  state : ShimState
  deriving DecidableEq, Repr

namespace Shim

def observePublish
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool) : Shim :=
  match s.state with
  | .bound => s
  | .unbound .hostResource => s
  | .unbound .dependencyMissing =>
      if s.boundBehavior ∈ snap.runnable then
        if hostCanListen then { s with state := .bound }
        else { s with state := .unbound .hostResource }
      else s

theorem converges_when_dependency_published
    (s : Shim) (snap : ActiveRuntimeSnapshot)
    (hUnbound : s.state = .unbound .dependencyMissing)
    (hRunnable : s.boundBehavior ∈ snap.runnable) :
    (s.observePublish snap true).state = .bound := by
  unfold observePublish
  rw [hUnbound]
  simp [hRunnable]

theorem listen_failure_degrades_to_host_resource
    (s : Shim) (snap : ActiveRuntimeSnapshot)
    (hUnbound : s.state = .unbound .dependencyMissing)
    (hRunnable : s.boundBehavior ∈ snap.runnable) :
    (s.observePublish snap false).state = .unbound .hostResource := by
  unfold observePublish
  rw [hUnbound]
  simp [hRunnable]

theorem never_binds_unrunnable
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hNotBound : s.state ≠ .bound)
    (hNotRunnable : s.boundBehavior ∉ snap.runnable) :
    (s.observePublish snap hostCanListen).state ≠ .bound := by
  unfold observePublish
  cases hs : s.state with
  | bound => exact absurd hs hNotBound
  | unbound reason =>
      cases reason with
      | dependencyMissing => simp [hs, hNotRunnable]
      | hostResource => simp [hs]

theorem host_resource_is_fixpoint
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hHost : s.state = .unbound .hostResource) :
    s.observePublish snap hostCanListen = s := by
  unfold observePublish
  rw [hHost]

theorem bound_never_unbinds
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hBound : s.state = .bound) :
    s.observePublish snap hostCanListen = s := by
  unfold observePublish
  rw [hBound]

theorem observePublish_idempotent
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool) :
    (s.observePublish snap hostCanListen).observePublish snap hostCanListen
      = s.observePublish snap hostCanListen := by
  unfold observePublish
  cases hs : s.state with
  | bound => simp [hs]
  | unbound reason =>
      cases reason with
      | hostResource => simp [hs]
      | dependencyMissing =>
          by_cases hr : s.boundBehavior ∈ snap.runnable <;>
            cases hostCanListen <;> simp [hs, hr]

theorem bound_is_absorbing
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hBound : s.state = .bound) :
    (s.observePublish snap hostCanListen).state = .bound := by
  rw [bound_never_unbinds s snap hostCanListen hBound, hBound]

theorem observePublish_preserves_target
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool) :
    (s.observePublish snap hostCanListen).boundBehavior = s.boundBehavior := by
  unfold observePublish
  cases hs : s.state with
  | bound => simp [hs]
  | unbound reason =>
      cases reason with
      | hostResource => simp [hs]
      | dependencyMissing =>
          by_cases hr : s.boundBehavior ∈ snap.runnable <;>
            cases hostCanListen <;> simp [hs, hr]

def coherentWith (s : Shim) (snap : ActiveRuntimeSnapshot) : Prop :=
  s.state = .bound → s.boundBehavior ∈ snap.runnable

theorem observePublish_coherent
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hCoherent : s.coherentWith snap) :
    (s.observePublish snap hostCanListen).coherentWith snap := by
  unfold coherentWith observePublish at *
  cases hs : s.state with
  | bound =>
      intro _
      exact hCoherent hs
  | unbound reason =>
      cases reason with
      | hostResource => simp [hs]
      | dependencyMissing =>
          by_cases hr : s.boundBehavior ∈ snap.runnable
          · cases hostCanListen <;> simp [hs, hr]
          · simp [hs, hr]

end Shim

end CodexShim.Binding
