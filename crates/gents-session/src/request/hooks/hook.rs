use serde_json::Value;

use super::effect::{DocRef, Effect, Manifest, Row};

/// A declared read: rows of one collection selected by equality on its
/// fields. A plugin declares its reads from the event alone and gets one
/// round; it cannot issue a read that depends on a row it was shown.
///
/// With `revisions`, the read is of every native revision of the matching
/// documents, deleted ones included: one row per composite commit, carrying
/// `fields` at that commit and the commit's `_revision` (`cid`, `height`,
/// `parents`, `inherited`). `revisions` names the fields a commit may link;
/// a commit with one parent that links only those, none of them in
/// `fields`, carries its parent's values without a reconstruction read
/// (`inherited`). A revision row is history: it is never a write target.
///
/// With `core`, the read is the named core read `collection` with these
/// arguments (Lean `Hooks.Read.core`): core answers it from its own rows, and
/// its rows are never write targets. Decision 2026-11-11, maintainer-approved.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub collection: String,
    pub fields: String,
    pub filter: Vec<(String, Value)>,
    pub order: Vec<(String, Direction)>,
    pub limit: Option<u64>,
    pub revisions: Option<String>,
    pub core: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Query {
    pub fn select(collection: impl Into<String>, fields: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            fields: fields.into(),
            filter: Vec::new(),
            order: Vec::new(),
            limit: None,
            revisions: None,
            core: None,
        }
    }

    /// The named core read `name` with `args`.
    pub fn core(name: impl Into<String>, args: Value) -> Self {
        Self {
            core: Some(args),
            ..Self::select(name, "")
        }
    }

    pub fn eq(mut self, field: impl Into<String>, value: impl Into<Value>) -> Self {
        self.filter.push((field.into(), value.into()));
        self
    }

    pub fn order_by(mut self, field: impl Into<String>, direction: Direction) -> Self {
        self.order.push((field.into(), direction));
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn revisions(mut self, links: impl Into<String>) -> Self {
        self.revisions = Some(links.into());
        self
    }
}

/// Lean `Hooks.historyPermitted`: a revision read is answered only for a
/// collection the manifest declares history for that the plugin owns, or for
/// a read its `_docID` filter pins to a document the event names.
pub fn history_permitted(manifest: &Manifest, named: &[DocRef], query: &Query) -> bool {
    let collection = query.collection.as_str();
    manifest.history.contains(&collection)
        && (manifest.owned(collection).is_some()
            || query.filter.iter().any(|(field, value)| {
                field == "_docID"
                    && value
                        .as_str()
                        .is_some_and(|doc| named.contains(&DocRef::new(collection, doc)))
            }))
}

/// Lean `Hooks.Outcome`: a refusal, or a value for core and the effects the
/// plugin wants. There is no third case and no handle.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<V> {
    Refuse(String),
    Proceed { value: V, effects: Vec<Effect> },
}

impl<V> Outcome<V> {
    pub fn proceed(value: V) -> Self {
        Self::Proceed {
            value,
            effects: Vec::new(),
        }
    }
}

impl Outcome<()> {
    pub fn effects(effects: Vec<Effect>) -> Self {
        Self::Proceed { value: (), effects }
    }
}

/// Lean `Hooks.Plugin` at one point: a manifest and a hook. `decide` is a
/// function of the event and the rows core read for `reads`, and nothing else.
pub trait Hook<E>: Send + Sync {
    type Value;

    fn manifest(&self) -> &Manifest;

    fn reads(&self, event: &E) -> Vec<Query>;

    fn decide(&self, event: &E, rows: &[Vec<Row>]) -> Outcome<Self::Value>;
}
