use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lean `Hooks.Ref`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocRef {
    pub collection: String,
    pub doc_id: String,
}

impl DocRef {
    pub fn new(collection: impl Into<String>, doc_id: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            doc_id: doc_id.into(),
        }
    }
}

/// Lean `Hooks.Row`. `revision` is the only compare-and-set fence a plugin
/// holds; the host that showed the row decides what it denotes.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub doc: DocRef,
    pub revision: u64,
    pub value: Value,
}

/// Lean `Hooks.Access`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    Create,
    Update,
    Delete,
}

/// Lean `Hooks.Write`. There is no blind write and no upsert.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    Create {
        doc: DocRef,
        value: Value,
    },
    Update {
        doc: DocRef,
        expected: u64,
        value: Value,
    },
    Delete {
        doc: DocRef,
        expected: u64,
    },
}

impl Write {
    pub fn doc(&self) -> &DocRef {
        match self {
            Self::Create { doc, .. } | Self::Update { doc, .. } | Self::Delete { doc, .. } => doc,
        }
    }

    pub fn access(&self) -> Access {
        match self {
            Self::Create { .. } => Access::Create,
            Self::Update { .. } => Access::Update,
            Self::Delete { .. } => Access::Delete,
        }
    }
}

/// A named core operation with its arguments as data. Core performs it under
/// its own rules and the transaction's actor; it reads only core state.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreOp {
    pub name: String,
    pub args: Value,
}

impl CoreOp {
    pub fn new(name: impl Into<String>, args: Value) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

/// Lean `Hooks.Effect`.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Write(Write),
    Core(CoreOp),
}

/// A collection a plugin owns. `schema` is the trusted SDL of a collection
/// whose writes are signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnedCollection {
    pub name: &'static str,
    pub schema: Option<&'static str>,
}

/// Lean `Hooks.Manifest`. Whole collections are the unit of write authority;
/// a core-owned collection is in no manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: &'static str,
    pub owns: Vec<OwnedCollection>,
    pub bound: usize,
}

impl Manifest {
    pub fn owned(&self, collection: &str) -> Option<&OwnedCollection> {
        self.owns.iter().find(|owned| owned.name == collection)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidEffects {
    #[error("{returned} effects exceed the bound of {bound}")]
    OverBound { returned: usize, bound: usize },
    #[error("write to {collection}, which the plugin does not own")]
    ForeignCollection { collection: String },
    #[error("write to {collection} document {doc_id} is not permitted to the transaction's actor")]
    Unauthorized { collection: String, doc_id: String },
}

/// Lean `Hooks.valid`: bounded, every write in a collection the plugin owns
/// and permitted to the transaction's actor. A requested core operation is
/// admitted here and authorized by core when it performs it.
pub fn validate<F>(
    manifest: &Manifest,
    effects: &[Effect],
    allowed: F,
) -> Result<(), InvalidEffects>
where
    F: Fn(Access, &DocRef) -> bool,
{
    if effects.len() > manifest.bound {
        return Err(InvalidEffects::OverBound {
            returned: effects.len(),
            bound: manifest.bound,
        });
    }
    for effect in effects {
        let Effect::Write(write) = effect else {
            continue;
        };
        let doc = write.doc();
        if manifest.owned(&doc.collection).is_none() {
            return Err(InvalidEffects::ForeignCollection {
                collection: doc.collection.clone(),
            });
        }
        if !allowed(write.access(), doc) {
            return Err(InvalidEffects::Unauthorized {
                collection: doc.collection.clone(),
                doc_id: doc.doc_id.clone(),
            });
        }
    }
    Ok(())
}
