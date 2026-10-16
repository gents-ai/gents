//! Runnable-gated binding for the Codex shim; modeled in
//! `proofs/Proofs/CodexShim/Binding.lean`, fenced by
//! `conformance::generated_codex_shim_binding_cases_pin_runnable_gated_binding`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimUnboundReason {
    DependencyMissing,
    /// A host resource the control plane cannot supply (port taken, bind
    /// refused). No document retracts this, so no generation may resurrect
    /// the shim and the runtime must not spin on it.
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
        runnable_agents: impl IntoIterator<Item = &'a str>,
        listen: impl FnOnce() -> bool,
    ) -> ShimBindingState {
        if self.grants_listen(runnable_agents) {
            self.settle_listen(listen());
        }
        self.state
    }

    pub fn grants_listen<'a>(&self, runnable_agents: impl IntoIterator<Item = &'a str>) -> bool {
        match self.state {
            ShimBindingState::Bound => false,
            ShimBindingState::Unbound(ShimUnboundReason::HostResource) => false,
            ShimBindingState::Unbound(ShimUnboundReason::DependencyMissing) => runnable_agents
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
