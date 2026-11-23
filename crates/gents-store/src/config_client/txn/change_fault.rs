use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{ConfigApplyTxn, ConfigClientError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationshipChangeKind {
    Add,
    Delete,
}

/// Selects the `nth` (1-based) relationship change whose collection, relation
/// and kind match every field that is set.
#[derive(Clone, Copy, Debug)]
pub struct RelationshipMatch {
    pub collection: Option<&'static str>,
    pub relation: Option<&'static str>,
    pub kind: Option<RelationshipChangeKind>,
    pub nth: usize,
}

impl RelationshipMatch {
    #[must_use]
    pub fn first(collection: &'static str, relation: &'static str) -> Self {
        Self {
            collection: Some(collection),
            relation: Some(relation),
            kind: None,
            nth: 1,
        }
    }

    fn matches(&self, change: &RelationshipChange) -> bool {
        self.collection.is_none_or(|c| c == change.collection)
            && self.relation.is_none_or(|r| r == change.relation)
            && self.kind.is_none_or(|k| k == change.kind)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationshipChange {
    pub operation: Option<&'static str>,
    pub collection: String,
    pub relation: String,
    pub kind: RelationshipChangeKind,
}

struct RelationshipChangeFault {
    matcher: Option<RelationshipMatch>,
    matched: AtomicUsize,
    trace: Mutex<Vec<RelationshipChange>>,
}

struct AutoCommitFault {
    operation: &'static str,
    fired: AtomicBool,
}

tokio::task_local! {
    static RELATIONSHIP_CHANGE_FAULT: Arc<RelationshipChangeFault>;
    static AUTO_COMMIT_FAULT: Arc<AutoCommitFault>;
}

/// An injected failure aborts the change before either backend sees it.
pub(in crate::config_client) fn before_relationship_change_for_test(
    operation: Option<&'static str>,
    collection: &str,
    relation: &str,
    delete: bool,
) -> Result<(), ConfigClientError> {
    let Ok(fault) = RELATIONSHIP_CHANGE_FAULT.try_with(Arc::clone) else {
        return Ok(());
    };
    let change = RelationshipChange {
        operation,
        collection: collection.to_owned(),
        relation: relation.to_owned(),
        kind: if delete {
            RelationshipChangeKind::Delete
        } else {
            RelationshipChangeKind::Add
        },
    };
    let fail = fault.matcher.is_some_and(|matcher| {
        matcher.matches(&change) && fault.matched.fetch_add(1, Ordering::Relaxed) + 1 == matcher.nth
    });
    fault
        .trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(change);
    if fail {
        return Err(ConfigClientError::InjectedRelationshipFailure {
            collection: collection.to_owned(),
            relation: relation.to_owned(),
        });
    }
    Ok(())
}

pub(in crate::config_client) fn before_auto_commit_for_test(
    operation: &'static str,
) -> Result<(), ConfigClientError> {
    let Ok(fault) = AUTO_COMMIT_FAULT.try_with(Arc::clone) else {
        return Ok(());
    };
    if fault.operation == operation && !fault.fired.swap(true, Ordering::Relaxed) {
        return Err(ConfigClientError::InjectedAutoCommitFailure { operation });
    }
    Ok(())
}

impl ConfigApplyTxn<'_> {
    /// Fails the relationship change `matcher` selects, or none when it is
    /// `None`, and returns every relationship change attempted in `future` in
    /// order. Document mutation counts are unaffected.
    pub async fn with_relationship_change_failure<F>(
        matcher: Option<RelationshipMatch>,
        future: F,
    ) -> (F::Output, Vec<RelationshipChange>)
    where
        F: Future,
    {
        assert!(matcher.is_none_or(|matcher| matcher.nth > 0));
        let fault = Arc::new(RelationshipChangeFault {
            matcher,
            matched: AtomicUsize::new(0),
            trace: Mutex::new(Vec::new()),
        });
        let output = RELATIONSHIP_CHANGE_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        let trace = fault
            .trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        (output, trace)
    }

    /// Fails the first auto-commit write named `operation` in `future` before
    /// it executes, and reports whether it fired.
    pub async fn with_auto_commit_failure<F>(
        operation: &'static str,
        future: F,
    ) -> (F::Output, bool)
    where
        F: Future,
    {
        let fault = Arc::new(AutoCommitFault {
            operation,
            fired: AtomicBool::new(false),
        });
        let output = AUTO_COMMIT_FAULT.scope(Arc::clone(&fault), future).await;
        (output, fault.fired.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
#[path = "change_fault_tests.rs"]
mod tests;
