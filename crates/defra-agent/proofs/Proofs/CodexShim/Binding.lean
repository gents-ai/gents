import Proofs.Basic
import Proofs.RuntimeReconcile.State

/-!
# Codex Shim Binding

The Codex shim is a **runnable-gated subsystem**: it can only serve a behavior
that the reconciler has published as `runnable`. The runtime already re-derives
exactly this kind of enablement on every published generation — that is the
`dispatchers = runnable` clause of `ActiveRuntimeSnapshot.wellFormed`, which
`RuntimeReconcile` re-establishes at each `publish`.

The shim is a *second* consumer of that same conclusion, and defra-agent#699 is
what happens when a subsystem is gated on `runnable` but never consumes it: the
shim sampled the control documents once, at boot, on an empty store. The bound
behavior did not exist yet, so the shim disabled itself permanently. A later
`config apply` made the behavior runnable and the runtime converged — but the
shim, holding a boot-time verdict, kept its port closed until the whole process
was restarted. All 19 fleet agents came up that way.

So the shim must re-derive its binding from each published generation, exactly
as the dispatchers do. The essential subtlety is that **not every reason to be
unbound is one the control plane can retract**:

* `dependencyMissing` — the bound behavior is absent or not runnable. The
  control plane *supplies* this: writing the behavior document fixes it, and a
  later generation will carry it. This must converge.
* `hostResource` — the port is taken, or the bind address was refused. No
  document can fix it, so republishing must not resurrect the shim, and the
  runtime must not spin retrying it. This is the same shape as
  `PairingReconcile.dial_failure_is_nonconverging_fixpoint`.

Collapsing those two classes is what makes a retry loop either useless (never
retrying the fixable case, i.e. #699) or noisy (retrying the unfixable one
forever).
-/

namespace CodexShim.Binding

/-- Why the shim is not currently serving. -/
inductive UnboundReason
  /-- The bound behavior is not runnable. The control plane can supply it. -/
  | dependencyMissing
  /-- A host resource the control plane cannot supply (port taken, bind address
      refused). No document retracts this. -/
  | hostResource
  deriving DecidableEq, Repr

/-- Whether the shim is currently serving its bound behavior. -/
inductive ShimState
  | unbound (reason : UnboundReason)
  | bound
  deriving DecidableEq, Repr

namespace ShimState

/-- The shim is listening on its port. -/
def isBound : ShimState → Bool
  | .bound => true
  | .unbound _ => false

end ShimState

/-- The shim, as the host holds it: which behavior it serves, and whether it is
currently serving. -/
structure Shim where
  boundBehavior : BehaviorId
  state : ShimState
  deriving DecidableEq, Repr

namespace Shim

/-- The shim observes a published generation and re-derives its binding.

`hostCanListen` is the host's answer at the moment of the attempt: acquiring the
listener can still fail (the port may have been taken since boot). Taking the
listen *inside* the transition is what keeps `bound` meaning "serving" — if the
grant were recorded before the socket existed, reality would have to walk
`bound → unbound`, which `bound_never_unbinds` forbids.

This is the whole fix for #699, and it is deliberately total: every published
generation is observed, and the shim's binding is a function of that generation
rather than of a boot-time sample. -/
def observePublish
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool) : Shim :=
  match s.state with
  -- Already serving: a republish must never tear a live listener down.
  | .bound => s
  -- Not ours to fix; no generation retracts a taken port.
  | .unbound .hostResource => s
  -- The control plane has supplied the dependency. Serve iff the host can listen.
  | .unbound .dependencyMissing =>
      if s.boundBehavior ∈ snap.runnable then
        if hostCanListen then { s with state := .bound }
        else { s with state := .unbound .hostResource }
      else s

/-- **The #699 theorem.** A shim disabled *solely* because its bound behavior was
missing binds as soon as a generation publishes that behavior as runnable — with
no restart transition anywhere in the model. -/
theorem converges_when_dependency_published
    (s : Shim) (snap : ActiveRuntimeSnapshot)
    (hUnbound : s.state = .unbound .dependencyMissing)
    (hRunnable : s.boundBehavior ∈ snap.runnable) :
    (s.observePublish snap true).state = .bound := by
  unfold observePublish
  rw [hUnbound]
  simp [hRunnable]

/-- If the dependency arrives but the host cannot acquire the listener, the shim
degrades to the *non-converging* class — a taken port is not something a later
generation can retract, so it must not be retried forever. -/
theorem listen_failure_degrades_to_host_resource
    (s : Shim) (snap : ActiveRuntimeSnapshot)
    (hUnbound : s.state = .unbound .dependencyMissing)
    (hRunnable : s.boundBehavior ∈ snap.runnable) :
    (s.observePublish snap false).state = .unbound .hostResource := by
  unfold observePublish
  rw [hUnbound]
  simp [hRunnable]

/-- Soundness: the shim never serves a behavior the runtime has not published as
runnable. Binding is granted by the generation, never assumed. -/
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

/-- A host-resource failure is a non-converging fixpoint: no published generation
resurrects it, so the runtime must not spin retrying a taken port. -/
theorem host_resource_is_fixpoint
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hHost : s.state = .unbound .hostResource) :
    s.observePublish snap hostCanListen = s := by
  unfold observePublish
  rw [hHost]

/-- No flap: a live listener is never torn down by a later generation. -/
theorem bound_never_unbinds
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hBound : s.state = .bound) :
    s.observePublish snap hostCanListen = s := by
  unfold observePublish
  rw [hBound]

/-- Observing the same generation twice changes nothing: rebinding is driven by
the generation, not by how many times it was seen. -/
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

/-- Binding is monotone in a run: once bound, always bound. -/
theorem bound_is_absorbing
    (s : Shim) (snap : ActiveRuntimeSnapshot) (hostCanListen : Bool)
    (hBound : s.state = .bound) :
    (s.observePublish snap hostCanListen).state = .bound := by
  rw [bound_never_unbinds s snap hostCanListen hBound, hBound]

/-- The bound behavior is stable: observing generations never re-targets the shim
at a different behavior. -/
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

/-- The invariant that ties the shim to the runtime, and the exact analogue of
`dispatchers = runnable`: a serving shim's behavior is runnable in the generation
it last observed. -/
def coherentWith (s : Shim) (snap : ActiveRuntimeSnapshot) : Prop :=
  s.state = .bound → s.boundBehavior ∈ snap.runnable

/-- Observing a generation establishes coherence with it, from any starting state
that was coherent with it. Together with `bound_is_absorbing` this is the shim's
counterpart of `RuntimeReconcile.coherent_preserved`. -/
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
