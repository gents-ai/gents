use std::sync::Arc;

use super::claim::ClaimVerdict;
use super::effect::{validate, Access, DocRef, Effect, InvalidEffects, Manifest, Row};
use super::hook::{Hook, Outcome, Query};

/// Lean `Hooks.Refusal`, with the store failures a native host adds. A
/// plugin's refusal reaches the caller as the plugin's own reason, so the text
/// a caller saw before the plugin moved behind the seam is unchanged.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("{reason}")]
    Refused {
        plugin: &'static str,
        reason: String,
    },
    #[error("lifecycle plugin {plugin} returned an invalid result: {cause}")]
    Invalid {
        plugin: &'static str,
        #[source]
        cause: InvalidEffects,
    },
    #[error("lifecycle plugin {plugin} returned effects where only a verdict is accepted")]
    GateEffects { plugin: &'static str },
    #[error(
        "lifecycle plugin {plugin} staged a write against a row that changed or was not shown"
    )]
    Conflict { plugin: &'static str },
    #[error("lifecycle plugin {plugin} requested the unknown core operation {operation}")]
    UnknownOperation {
        plugin: &'static str,
        operation: String,
    },
    #[error("no lifecycle plugins are installed in this process; the runtime was composed without them")]
    NotInstalled,
    #[error("lifecycle plugin declared an unreadable query: {0}")]
    Query(String),
    #[error(
        "lifecycle plugin {plugin} failed after part of its staging was applied; the transition cannot commit without it"
    )]
    PartialStaging {
        plugin: &'static str,
        #[source]
        source: Box<HookError>,
    },
    /// Not transparent: the store's conflict and denial classifiers find the
    /// access error by walking `source()`, which a transparent variant skips.
    #[error("a core operation requested by a lifecycle plugin failed")]
    Operation(#[source] anyhow::Error),
    #[error("a lifecycle hook could not read or write its rows")]
    Store(#[from] gents_store::config_client::ConfigClientError),
}

/// Why one plugin's effects were not applied. `clean` is whether the staged
/// state is exactly what it was before the plugin's first effect; only then
/// can the plugin be skipped whole.
#[derive(Debug)]
pub struct ApplyFailure {
    pub clean: bool,
    pub error: HookError,
}

/// Lean `Hooks.Host`: what core supplies. `select` answers a declared query
/// from the transaction, `allowed` is document ACP for the transaction's
/// actor, and `apply` applies one plugin's validated effects in the order
/// returned, all or none (Lean `Hooks.applyEffects`).
#[async_trait::async_trait]
pub trait Host: Send + Sync {
    async fn select(&self, query: &Query) -> Result<Vec<Row>, HookError>;

    async fn allowed(&self, access: Access, doc: &DocRef) -> Result<bool, HookError>;

    async fn apply(&self, manifest: &Manifest, effects: &[Effect]) -> Result<(), ApplyFailure>;
}

/// A plugin at a point, as the composition root registers it.
pub type Plugin<E, V> = Arc<dyn Hook<E, Value = V>>;

async fn snapshot<E, V, H>(
    host: &H,
    plugin: &Plugin<E, V>,
    event: &E,
) -> Result<Vec<Vec<Row>>, HookError>
where
    H: Host,
{
    let mut rows = Vec::new();
    for query in plugin.reads(event) {
        rows.push(host.select(&query).await?);
    }
    Ok(rows)
}

async fn validated<H>(
    host: &H,
    plugin: &'static str,
    manifest: &Manifest,
    effects: &[Effect],
) -> Result<(), HookError>
where
    H: Host,
{
    validate(manifest, effects, |_, _| true)
        .map_err(|cause| HookError::Invalid { plugin, cause })?;
    let mut denied = Vec::new();
    for effect in effects {
        if let Effect::Write(write) = effect {
            if !host.allowed(write.access(), write.doc()).await? {
                denied.push((write.access(), write.doc().clone()));
            }
        }
    }
    validate(manifest, effects, |access, doc| {
        !denied
            .iter()
            .any(|(denied_access, denied_doc)| *denied_access == access && denied_doc == doc)
    })
    .map_err(|cause| HookError::Invalid { plugin, cause })
}

/// Lean `Hooks.stage`: staging at a point where a plugin may refuse. Every
/// plugin decides against the rows read before any effect is applied; results
/// are validated and applied in list order. Any refusal, invalid result or
/// miss is an error and the caller's transaction must not commit.
pub async fn stage<E, V, H>(
    host: &H,
    plugins: &[Plugin<E, V>],
    event: &E,
) -> Result<Vec<V>, HookError>
where
    H: Host,
{
    let mut shown = Vec::with_capacity(plugins.len());
    for plugin in plugins {
        shown.push(snapshot(host, plugin, event).await?);
    }
    let mut values = Vec::with_capacity(plugins.len());
    for (plugin, rows) in plugins.iter().zip(&shown) {
        let name = plugin.manifest().name;
        match plugin.decide(event, rows) {
            Outcome::Refuse(reason) => {
                return Err(HookError::Refused {
                    plugin: name,
                    reason,
                })
            }
            Outcome::Proceed { value, effects } => {
                validated(host, name, plugin.manifest(), &effects).await?;
                host.apply(plugin.manifest(), &effects)
                    .await
                    .map_err(|failure| failure.error)?;
                values.push(value);
            }
        }
    }
    Ok(values)
}

/// One plugin's result at a point whose transition must commit.
#[derive(Debug)]
pub enum Staged {
    Applied,
    /// The plugin's staging was dropped whole and is owed as a delivery.
    Owed {
        plugin: &'static str,
        cause: HookError,
    },
}

impl Staged {
    pub fn is_owed(&self) -> bool {
        matches!(self, Self::Owed { .. })
    }
}

/// Lean `Hooks.stageDropping`: staging where a plugin cannot stop the
/// transition. A plugin whose rows could not be read, that refuses, whose
/// result is invalid or whose effects miss is skipped whole and owed; the
/// others still stage. The result aligns with the plugin list.
///
/// The one error is a host that could not skip a plugin whole
/// (`ApplyFailure::clean == false`): the model's `applyEffects` leaves the
/// staged state untouched on a miss, and a host without that property cannot
/// honour the drop.
pub async fn stage_dropping<E, V, H>(
    host: &H,
    plugins: &[Plugin<E, V>],
    event: &E,
) -> Result<Vec<Staged>, HookError>
where
    H: Host,
{
    let mut shown = Vec::with_capacity(plugins.len());
    for plugin in plugins {
        shown.push(snapshot(host, plugin, event).await);
    }
    let mut staged = Vec::with_capacity(plugins.len());
    for (plugin, rows) in plugins.iter().zip(shown) {
        let name = plugin.manifest().name;
        let owed = |cause| Staged::Owed {
            plugin: name,
            cause,
        };
        let rows = match rows {
            Ok(rows) => rows,
            Err(cause) => {
                staged.push(owed(cause));
                continue;
            }
        };
        let effects = match plugin.decide(event, &rows) {
            Outcome::Refuse(reason) => {
                staged.push(owed(HookError::Refused {
                    plugin: name,
                    reason,
                }));
                continue;
            }
            Outcome::Proceed { effects, .. } => effects,
        };
        if let Err(cause) = validated(host, name, plugin.manifest(), &effects).await {
            staged.push(owed(cause));
            continue;
        }
        match host.apply(plugin.manifest(), &effects).await {
            Ok(()) => staged.push(Staged::Applied),
            Err(failure) if failure.clean => staged.push(owed(failure.error)),
            Err(failure) => {
                return Err(HookError::PartialStaging {
                    plugin: name,
                    source: Box::new(failure.error),
                })
            }
        }
    }
    Ok(staged)
}

/// Lean `Hooks.owed`: the deliveries a must-commit transition owes, one per
/// dropped plugin.
pub fn owed(staged: &[Staged]) -> Vec<&'static str> {
    staged
        .iter()
        .filter_map(|entry| match entry {
            Staged::Owed { plugin, .. } => Some(*plugin),
            Staged::Applied => None,
        })
        .collect()
}

/// Lean `Hooks.gate`: every plugin answers on the transaction's snapshot with
/// a verdict and no effects; a result that carries any is invalid.
pub async fn gate<E, H>(
    host: &H,
    plugins: &[Plugin<E, ClaimVerdict>],
    event: &E,
) -> Result<Vec<ClaimVerdict>, HookError>
where
    H: Host,
{
    let mut shown = Vec::with_capacity(plugins.len());
    for plugin in plugins {
        shown.push(snapshot(host, plugin, event).await?);
    }
    let mut verdicts = Vec::with_capacity(plugins.len());
    for (plugin, rows) in plugins.iter().zip(&shown) {
        let name = plugin.manifest().name;
        match plugin.decide(event, rows) {
            Outcome::Refuse(reason) => {
                return Err(HookError::Refused {
                    plugin: name,
                    reason,
                })
            }
            Outcome::Proceed { value, effects } if effects.is_empty() => verdicts.push(value),
            Outcome::Proceed { .. } => return Err(HookError::GateEffects { plugin: name }),
        }
    }
    Ok(verdicts)
}
