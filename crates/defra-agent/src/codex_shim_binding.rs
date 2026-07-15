//! Runnable-gated binding for the Codex shim (#699).
//!
//! The Codex shim may only serve a behavior the reconciler has published as
//! runnable. The runtime already re-derives that kind of enablement on every
//! published generation — the behavior dispatchers are rebuilt from the active
//! snapshot's runnable set each time. The shim is a *second* consumer of the
//! same conclusion, and #699 is what happened when it did not consume it: the
//! shim sampled the control documents once at boot, found no behavior on an
//! empty store, disabled itself permanently, and kept its port closed even
//! after `config apply` made the behavior runnable and the runtime converged.
//!
//! The model is `proofs/Proofs/CodexShim/Binding.lean`; the case table it emits
//! is fenced by `conformance::generated_codex_shim_binding_cases_pin_runnable_gated_binding`.

/// Why the shim is not currently serving.
///
/// The split is load-bearing, not cosmetic: it decides whether a later
/// generation can revive the shim. Collapsing the two classes makes a retry
/// loop either useless (never retrying the fixable case — #699) or noisy
/// (retrying a taken port forever).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimUnboundReason {
    /// The bound behavior is absent or not runnable. The control plane supplies
    /// this: write the behavior document and a later generation will carry it.
    DependencyMissing,
    /// A host resource the control plane cannot supply — the port is taken, or
    /// the bind address was refused. No document retracts this, so no
    /// generation may resurrect the shim and the runtime must not spin on it.
    HostResource,
}

/// Whether the shim is serving its bound behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimBindingState {
    Unbound(ShimUnboundReason),
    Bound,
}

/// The shim's binding, as the host holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimBinding {
    bound_behavior: String,
    state: ShimBindingState,
}

impl ShimBinding {
    /// A shim that is not yet serving, for the stated reason.
    pub fn unbound(bound_behavior: impl Into<String>, reason: ShimUnboundReason) -> Self {
        Self {
            bound_behavior: bound_behavior.into(),
            state: ShimBindingState::Unbound(reason),
        }
    }

    /// A shim that is already serving.
    pub fn bound(bound_behavior: impl Into<String>) -> Self {
        Self {
            bound_behavior: bound_behavior.into(),
            state: ShimBindingState::Bound,
        }
    }

    pub fn state(&self) -> ShimBindingState {
        self.state
    }

    pub fn is_bound(&self) -> bool {
        matches!(self.state, ShimBindingState::Bound)
    }

    pub fn bound_behavior(&self) -> &str {
        &self.bound_behavior
    }

    /// True when this shim is waiting on a behavior the control plane can still
    /// supply — the only state a published generation is allowed to revive.
    pub fn is_waiting_for_dependency(&self) -> bool {
        matches!(
            self.state,
            ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing)
        )
    }

    /// Observe a published generation and re-derive the binding.
    ///
    /// Mirrors `CodexShim.Binding.Shim.observePublish`. `listen` is only invoked
    /// when the generation authorizes serving; it acquires the listener and
    /// reports whether it succeeded. Taking the listen *inside* the transition is
    /// what keeps `Bound` meaning "serving": if the grant were recorded before
    /// the socket existed, a failed listen would force a `Bound -> Unbound` walk,
    /// which `bound_never_unbinds` forbids.
    ///
    /// A listen that fails here is a *host resource* failure (the port was taken
    /// between boot and the behavior arriving), so the shim degrades to the
    /// non-converging class rather than spinning on it.
    pub fn observe_publish<'a>(
        &mut self,
        runnable_behaviors: impl IntoIterator<Item = &'a str>,
        listen: impl FnOnce() -> bool,
    ) -> ShimBindingState {
        if self.grants_listen(runnable_behaviors) {
            self.settle_listen(listen());
        }
        self.state
    }

    /// Does this published generation authorize the shim to start serving?
    ///
    /// The guard half of [`Self::observe_publish`], split out because acquiring a
    /// real listener is async. Hosts that must `await` the bind call this, then
    /// report the outcome to [`Self::settle_listen`]; both paths share this one
    /// definition so they cannot drift.
    pub fn grants_listen<'a>(&self, runnable_behaviors: impl IntoIterator<Item = &'a str>) -> bool {
        match self.state {
            // Already serving, or waiting on something no document can supply.
            ShimBindingState::Bound => false,
            ShimBindingState::Unbound(ShimUnboundReason::HostResource) => false,
            ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing) => runnable_behaviors
                .into_iter()
                .any(|id| id == self.bound_behavior),
        }
    }

    /// Record the outcome of the listen a grant authorized.
    ///
    /// A failed listen is a host-resource failure — the port went away between
    /// boot and the behavior arriving — so the shim degrades to the
    /// non-converging class instead of spinning on it.
    pub fn settle_listen(&mut self, listened: bool) {
        self.state = if listened {
            ShimBindingState::Bound
        } else {
            ShimBindingState::Unbound(ShimUnboundReason::HostResource)
        };
    }
}
