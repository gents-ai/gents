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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimUnboundReason {
    DependencyMissing,
    /// A host resource the control plane cannot supply — the port is taken, or
    /// the bind address was refused. No document retracts this, so no
    /// generation may resurrect the shim and the runtime must not spin on it.
    HostResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimBindingState {
    Unbound(ShimUnboundReason),
    Bound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimBinding {
    bound_behavior: String,
    state: ShimBindingState,
}

impl ShimBinding {
    pub fn unbound(bound_behavior: impl Into<String>, reason: ShimUnboundReason) -> Self {
        Self {
            bound_behavior: bound_behavior.into(),
            state: ShimBindingState::Unbound(reason),
        }
    }

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

    pub fn is_waiting_for_dependency(&self) -> bool {
        matches!(
            self.state,
            ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing)
        )
    }

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

    pub fn grants_listen<'a>(&self, runnable_behaviors: impl IntoIterator<Item = &'a str>) -> bool {
        match self.state {
            ShimBindingState::Bound => false,
            ShimBindingState::Unbound(ShimUnboundReason::HostResource) => false,
            ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing) => runnable_behaviors
                .into_iter()
                .any(|id| id == self.bound_behavior),
        }
    }

    pub fn settle_listen(&mut self, listened: bool) {
        self.state = if listened {
            ShimBindingState::Bound
        } else {
            ShimBindingState::Unbound(ShimUnboundReason::HostResource)
        };
    }
}
