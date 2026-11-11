use std::sync::Arc;

use super::effect::{validate, Access, DocRef, Effect, InvalidEffects, Manifest, Named, Row};
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
    /// `message` is the plugin's caller-facing text for the lost fence.
    #[error("{}", conflict_text(plugin, message.as_deref()))]
    Conflict {
        plugin: &'static str,
        message: Option<String>,
    },
    #[error("lifecycle plugin {plugin} requested the unknown core operation {operation}")]
    UnknownOperation {
        plugin: &'static str,
        operation: String,
    },
    #[error(
        "no lifecycle plugins are attached to this node; start it through gents::runtime::node_start, which composes it"
    )]
    NotInstalled,
    #[error(
        "this GraphQL access carries no lifecycle plugins; build its endpoint with gents::runtime::lifecycle_hooks::compose_endpoint"
    )]
    EndpointNotComposed,
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
    /// An earlier attempt of this transaction failed with `PartialStaging`
    /// for the plugin and ran again without it.
    #[error(
        "lifecycle plugin {plugin} failed after part of its staging was applied; the transition ran again without it"
    )]
    RanWithout { plugin: &'static str },
    /// Not transparent: the store's conflict and denial classifiers find the
    /// access error by walking `source()`, which a transparent variant skips.
    #[error("a core operation requested by a lifecycle plugin failed")]
    Operation(#[source] anyhow::Error),
    #[error("a lifecycle hook could not read or write its rows")]
    Store(#[from] gents_store::config_client::ConfigClientError),
    /// A declared revision read failed; the caller sees the read's own error.
    #[error(transparent)]
    Revisions(anyhow::Error),
    /// Lean `Hooks.out_of_bound_history_is_not_answered`.
    #[error(
        "lifecycle plugin {plugin} declared a history read of {collection}, which is answered only for a collection its manifest declares history for and that it owns, or for a document the event names"
    )]
    UnboundedHistory {
        plugin: &'static str,
        collection: String,
    },
    #[error("lifecycle plugin declared the unknown core read {0}")]
    UnknownRead(String),
    /// A declared named core read failed; the caller sees the read's own error.
    #[error(transparent)]
    CoreRead(anyhow::Error),
}

fn conflict_text(plugin: &str, message: Option<&str>) -> String {
    let lost = format!(
        "lifecycle plugin {plugin} staged a write against a row that changed or was not shown"
    );
    match message {
        Some(message) => format!("{message} ({lost})"),
        None => lost,
    }
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
/// of the plugin with `manifest`, at an event that names `named`, from the
/// transaction, `allowed` is document ACP for the transaction's
/// actor, `actor` is that actor, and `apply` applies one plugin's validated
/// effects in the order returned, all or none (Lean `Hooks.applyEffects`).
#[async_trait::async_trait]
pub trait Host: Send + Sync {
    fn actor(&self) -> Option<String>;

    async fn select(
        &self,
        manifest: &Manifest,
        named: &[DocRef],
        query: &Query,
    ) -> Result<Vec<Row>, HookError>;

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
        rows.push(host.select(plugin.manifest(), &[], &query).await?);
    }
    Ok(rows)
}

/// Who a plugin's grants may name: the transaction's actor, the actors the
/// event names, and the documents core showed the plugin (Lean `Hooks.Named`).
fn named<H>(host: &H, actors: &[String], rows: &[Vec<Row>]) -> Named
where
    H: Host,
{
    Named {
        actor: host.actor(),
        actors: actors.to_vec(),
        docs: rows.iter().flatten().map(|row| row.doc.clone()).collect(),
    }
}

async fn validated<H>(
    host: &H,
    plugin: &'static str,
    manifest: &Manifest,
    effects: &[Effect],
    named: &Named,
) -> Result<(), HookError>
where
    H: Host,
{
    validate(manifest, effects, |_, _| true, named)
        .map_err(|cause| HookError::Invalid { plugin, cause })?;
    let mut denied = Vec::new();
    for effect in effects {
        if let Effect::Write(write) = effect {
            if !host.allowed(write.access(), write.doc()).await? {
                denied.push((write.access(), write.doc().clone()));
            }
        }
    }
    validate(
        manifest,
        effects,
        |access, doc| {
            !denied
                .iter()
                .any(|(denied_access, denied_doc)| *denied_access == access && denied_doc == doc)
        },
        named,
    )
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
    stage_naming(host, plugins, event, &[]).await
}

/// `stage` at a point whose event names `actors` a create's grants may name
/// besides the transaction's actor.
pub async fn stage_naming<E, V, H>(
    host: &H,
    plugins: &[Plugin<E, V>],
    event: &E,
    actors: &[String],
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
                let named = named(host, actors, rows);
                validated(host, name, plugin.manifest(), &effects, &named).await?;
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
/// others still stage. A plugin named in `without` is skipped whole and owed
/// unread. The result aligns with the plugin list.
///
/// The one error is a host that could not skip a plugin whole
/// (`ApplyFailure::clean == false`): the model's `applyEffects` leaves the
/// staged state untouched on a miss, so the caller runs its transition again
/// with that plugin in `without`.
pub async fn stage_dropping<E, V, H>(
    host: &H,
    plugins: &[Plugin<E, V>],
    event: &E,
    without: &[&str],
) -> Result<Vec<Staged>, HookError>
where
    H: Host,
{
    let mut shown = Vec::with_capacity(plugins.len());
    for plugin in plugins {
        let name = plugin.manifest().name;
        shown.push(if without.contains(&name) {
            Err(HookError::RanWithout { plugin: name })
        } else {
            snapshot(host, plugin, event).await
        });
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
        let named = named(host, &[], &rows);
        if let Err(cause) = validated(host, name, plugin.manifest(), &effects, &named).await {
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

/// Lean `Hooks.answer` (`Hooks.gate` at `request.claim.gate`): every plugin
/// answers on the transaction's snapshot with a value and no effects; a
/// result that carries any is invalid.
pub async fn gate<E, V, H>(
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
