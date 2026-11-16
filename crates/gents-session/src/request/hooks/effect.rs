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

/// Lean `Hooks.Subject`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Actor(String),
    Document(DocRef),
}

/// Lean `Hooks.Grant` on the document of the create that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Grant {
    pub relation: String,
    pub subject: Subject,
}

/// Lean `Hooks.Write`. There is no blind write and no upsert. A create's
/// `grants` are published with the document it creates and nowhere else. An
/// update's or delete's `conflict` is what the caller is told when its
/// revision fence is lost; it does not change what is applied.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    Create {
        doc: DocRef,
        value: Value,
        grants: Vec<Grant>,
    },
    Update {
        doc: DocRef,
        expected: u64,
        value: Value,
        conflict: Option<String>,
    },
    Delete {
        doc: DocRef,
        expected: u64,
        conflict: Option<String>,
    },
}

impl Write {
    pub fn doc(&self) -> &DocRef {
        match self {
            Self::Create { doc, .. } | Self::Update { doc, .. } | Self::Delete { doc, .. } => doc,
        }
    }

    /// The caller-facing message for a lost revision fence.
    pub fn conflict(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Update { conflict, .. } | Self::Delete { conflict, .. } => conflict.as_deref(),
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

/// Lean `Hooks.SubjectKind`: an actor, or a document of `collection`, which
/// the ACP policy calls `resource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    Actor,
    Document {
        resource: &'static str,
        collection: &'static str,
    },
}

impl SubjectKind {
    fn admits(&self, subject: &Subject) -> bool {
        match (self, subject) {
            (Self::Actor, Subject::Actor(_)) => true,
            (Self::Document { collection, .. }, Subject::Document(doc)) => {
                doc.collection == *collection
            }
            _ => false,
        }
    }
}

/// Lean `Hooks.Grantable`, for the collection that declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grantable {
    pub relation: &'static str,
    pub subject: SubjectKind,
}

/// Lean `Hooks.sessionChildGrants`: the Household `children` resource's
/// `parent` (a session) and `working_agent` (an actor). The only grants a
/// plugin may publish for now (decision 2026-11-11, maintainer-approved).
pub const SESSION_CHILD_GRANTS: &[Grantable] = &[
    Grantable {
        relation: "parent",
        subject: SubjectKind::Document {
            resource: "sessions",
            collection: "AgentSession",
        },
    },
    Grantable {
        relation: "working_agent",
        subject: SubjectKind::Actor,
    },
];

/// A collection a plugin owns. `schema` is the trusted SDL of a collection
/// whose writes are signed; `grants` are the relations its creates may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnedCollection {
    pub name: &'static str,
    pub schema: Option<&'static str>,
    pub grants: &'static [Grantable],
}

impl OwnedCollection {
    /// The declaration that covers `grant`.
    pub fn grantable(&self, grant: &Grant) -> Option<&Grantable> {
        self.grants.iter().find(|declared| {
            declared.relation == grant.relation && declared.subject.admits(&grant.subject)
        })
    }
}

/// Lean `Hooks.Named`, with the transaction's actor: who a create's grants
/// may name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Named {
    pub actor: Option<String>,
    pub actors: Vec<String>,
    pub docs: Vec<DocRef>,
}

impl Named {
    fn names(&self, subject: &Subject) -> bool {
        match subject {
            Subject::Actor(did) => {
                self.actor.as_deref() == Some(did.as_str()) || self.actors.contains(did)
            }
            Subject::Document(doc) => self.docs.contains(doc),
        }
    }
}

/// Lean `Hooks.Manifest`. Whole collections are the unit of write authority;
/// a core-owned collection is in no manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: &'static str,
    pub owns: Vec<OwnedCollection>,
    pub bound: usize,
    /// The collections whose revision history the plugin declares it reads
    /// (Lean `Hooks.historyPermitted`).
    pub history: Vec<&'static str>,
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
    #[error("a create in {collection} grants {relation}, which its manifest does not declare for that subject")]
    UndeclaredGrant {
        collection: String,
        relation: String,
    },
    #[error("a create in {collection} grants {relation} to a subject the event does not name")]
    UnnamedGrantSubject {
        collection: String,
        relation: String,
    },
    #[error("{collection} document {doc_id} is written more than once; merge its writes into one")]
    RepeatedDocument { collection: String, doc_id: String },
}

/// Lean `Hooks.valid` and `Hooks.grantsValid`: bounded, every write in a
/// collection the plugin owns and permitted to the transaction's actor, no
/// document written twice, every grant declared for its create's collection
/// and naming a subject `named` admits. A requested core operation is
/// admitted here and authorized by core when it performs it.
///
/// A create that names no document ID is given a fresh one by the store, so
/// it writes a document no other write in the result can name.
pub fn validate<F>(
    manifest: &Manifest,
    effects: &[Effect],
    allowed: F,
    named: &Named,
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
    let mut written: Vec<&DocRef> = Vec::new();
    for effect in effects {
        let Effect::Write(write) = effect else {
            continue;
        };
        let doc = write.doc();
        let assigned = matches!(write, Write::Create { .. }) && doc.doc_id.is_empty();
        if !assigned {
            if written.contains(&doc) {
                return Err(InvalidEffects::RepeatedDocument {
                    collection: doc.collection.clone(),
                    doc_id: doc.doc_id.clone(),
                });
            }
            written.push(doc);
        }
        let Some(owned) = manifest.owned(&doc.collection) else {
            return Err(InvalidEffects::ForeignCollection {
                collection: doc.collection.clone(),
            });
        };
        if let Write::Create { grants, .. } = write {
            for grant in grants {
                if owned.grantable(grant).is_none() {
                    return Err(InvalidEffects::UndeclaredGrant {
                        collection: doc.collection.clone(),
                        relation: grant.relation.clone(),
                    });
                }
                if !named.names(&grant.subject) {
                    return Err(InvalidEffects::UnnamedGrantSubject {
                        collection: doc.collection.clone(),
                        relation: grant.relation.clone(),
                    });
                }
            }
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
