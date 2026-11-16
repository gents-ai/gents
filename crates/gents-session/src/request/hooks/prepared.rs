use std::sync::Arc;

/// Lean `Hooks.Binding`: what a prepared decision is about. The host captures
/// every field; a plugin never supplies or echoes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding<S> {
    pub actor: String,
    pub point: &'static str,
    pub subject: S,
    /// A digest of the canonical inputs the plugin was shown.
    pub inputs: String,
    pub configuration: String,
}

/// Lean `Hooks.Verdict`. `Unavailable` is an answer, not an absence; neither
/// it nor `Deny` authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict<P> {
    Allow(P),
    Deny(String),
    Unavailable,
}

/// Lean `Hooks.Prepared`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared<S, P> {
    pub binding: Binding<S>,
    pub verdict: Verdict<P>,
}

/// Lean `Hooks.Stale`: why a prepared decision did not admit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Stale {
    #[error("the actor is not authorized for this lifecycle step")]
    Unauthorized,
    #[error("the lifecycle step was cancelled")]
    Cancelled,
    #[error("no prepared decision was supplied for this lifecycle step")]
    Missing,
    #[error("the prepared decision was made for inputs that have since changed")]
    Stale,
    #[error("{0}")]
    Denied(String),
    #[error("the plugin could not decide this lifecycle step")]
    Unavailable,
}

/// Lean `Hooks.prepare`: the host pairs the binding it captured with the
/// plugin's answer on what it was shown.
pub fn prepare<S, P, G, F>(captured: Binding<S>, plugin: F, shown: &G) -> Prepared<S, P>
where
    F: FnOnce(&G) -> Verdict<P>,
{
    Prepared {
        binding: captured,
        verdict: plugin(shown),
    }
}

/// Lean `Hooks.admit`: re-validation against core's own authorization, the
/// cancellation flag and the binding recaptured from the rows the caller now
/// observes. A missing, stale, denied or unavailable decision never admits.
pub fn admit<S, P>(
    authorized: bool,
    cancelled: bool,
    current: &Binding<S>,
    prepared: Option<Prepared<S, P>>,
) -> Result<P, Stale>
where
    S: PartialEq,
{
    if !authorized {
        return Err(Stale::Unauthorized);
    }
    if cancelled {
        return Err(Stale::Cancelled);
    }
    let Some(decision) = prepared else {
        return Err(Stale::Missing);
    };
    if &decision.binding != current {
        return Err(Stale::Stale);
    }
    match decision.verdict {
        Verdict::Allow(payload) => Ok(payload),
        Verdict::Deny(reason) => Err(Stale::Denied(reason)),
        Verdict::Unavailable => Err(Stale::Unavailable),
    }
}

/// A plugin's prepared decision at one point. `applies` is decided from the
/// event alone, before core reads anything for the plugin; `decide` is a pure
/// function of what core showed it. Neither holds a transaction or a handle.
pub trait Decide<E, G>: Send + Sync {
    type Payload;

    fn name(&self) -> &'static str;

    /// The plugin's selected configuration, captured in the binding. A
    /// bundled plugin without configuration answers the empty string.
    fn configuration(&self) -> &str;

    fn applies(&self, event: &E) -> bool;

    fn decide(&self, shown: &G) -> Verdict<Self::Payload>;
}

/// A prepared-decision plugin at a point, as the composition root registers it.
pub type Decider<E, G, P> = Arc<dyn Decide<E, G, Payload = P>>;

/// The canonical digest of what a plugin was shown, for its binding.
pub fn inputs_digest<G>(shown: &G) -> Result<String, serde_json::Error>
where
    G: serde::Serialize,
{
    use sha2::{Digest, Sha256};
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(shown)?)
    ))
}
