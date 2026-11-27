//! Whether the body of a placed Agent, its document and the configuration
//! it selects, is authorized on this node (gents#4245).
//!
//! Authorization happens when a change is written. External premise
//! (DefraDB): a node whose document access control is a Vera home holds the
//! verified signer of every replicated update and delete to that home's
//! permission before merging the change, merges neither a refused change nor
//! one to a document that is not registered yet, and takes a first commit's
//! authority from the document's registration. The heads such a node reads
//! are therefore the last authorized version, and nothing here checks a
//! signature again against the current state.

use std::collections::BTreeMap;

/// Lean `RuntimeReconcile.Placement.MergeCheck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeCheck {
    Unchecked,
    /// Every replicated change is held to the policy `home`.
    Strict {
        home: String,
    },
}

/// One document of a body as the view's transaction saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureDocument {
    pub present: bool,
    pub registered: bool,
    /// The document's composite head; empty when it is absent.
    pub head: String,
}

/// Lean `RuntimeReconcile.Placement.Body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyVerification {
    /// This node does not hold replicated changes to the issuer's home.
    NotStrict,
    /// A document of the closure has not arrived or is not registered yet.
    Waiting,
    /// Every document is present and registered, at these heads.
    Verified { heads: Vec<String> },
}

impl BodyVerification {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// The body of every Agent a view read, by Agent `_docID`.
pub type PlacementBodies = BTreeMap<String, BodyVerification>;

/// Lean `RuntimeReconcile.Placement.body`.
pub fn body_verification(
    check: &MergeCheck,
    issuer_home: &str,
    closure: &[ClosureDocument],
) -> BodyVerification {
    match check {
        MergeCheck::Strict { home } if home == issuer_home => {
            if closure
                .iter()
                .all(|document| document.present && document.registered)
            {
                BodyVerification::Verified {
                    heads: closure
                        .iter()
                        .map(|document| document.head.clone())
                        .collect(),
                }
            } else {
                BodyVerification::Waiting
            }
        }
        MergeCheck::Strict { .. } | MergeCheck::Unchecked => BodyVerification::NotStrict,
    }
}

#[cfg(test)]
mod tests;
