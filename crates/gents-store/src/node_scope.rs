//! A [`RequestContext`] as `defra_node`'s scoped calls take it.
//!
//! Gents carries who a call acts as in a request context; the node takes it
//! as a scope instead. Queries keep their context in the [`QueryRequest`]
//! they run, so only the other node calls are scoped here.
//!
//! [`QueryRequest`]: defra_node::QueryRequest

use defra_core::{RequestContext, TxnId};
use defra_node::{ActingFor, Node, ScopedNode, TransactionHandle, TxnNode};
use identity::Did;

mod context_calls;

pub use context_calls::ContextCalls;

/// The node acting as `context`'s caller, or as no one when it names none,
/// with the bearer and signing key it carries and the document access
/// control bypass it was granted. A denied bypass belongs to a request and
/// is carried by its [`QueryRequest`](defra_node::QueryRequest).
pub fn scoped<'n>(node: &'n Node, context: &RequestContext) -> defra_node::Result<ScopedNode<'n>> {
    let scope = match caller(context)? {
        Some(caller) => node.with_identity(caller),
        None => node.anonymous(),
    };
    let scope = match bearer(context) {
        Some((token, true)) => scope.with_request_delegation(token),
        Some((token, false)) => scope.with_bearer(token),
        None => scope,
    };
    let scope = match context.signing() {
        Some(signing) => scope.with_signing(signing.clone()),
        None => scope,
    };
    Ok(if context.dac_bypass() {
        scope.with_dac_bypass(true)
    } else {
        scope
    })
}

/// [`scoped`] in the transaction `handle` names.
pub fn in_transaction<'n>(
    node: &'n Node,
    context: &RequestContext,
    handle: &TransactionHandle,
) -> defra_node::Result<TxnNode<'n>> {
    Ok(scoped(node, context)?.with_transaction(transaction(handle)?))
}

/// The node acting for `context`'s caller in its own transaction `handle`,
/// presenting the caller's bearer.
pub fn acting_for<'n>(
    node: &'n Node,
    context: &RequestContext,
    handle: &TransactionHandle,
) -> defra_node::Result<ActingFor<'n>> {
    let actor = caller(context)?.ok_or_else(|| {
        defra_node::Error::Transaction(query::TransactionError::Operation(
            "an acting-for call names its actor".into(),
        ))
    })?;
    let acting = node
        .with_transaction(transaction(handle)?)
        .acting_for(actor);
    Ok(match bearer(context) {
        Some((token, true)) => acting.with_request_delegation(token),
        Some((token, false)) => acting.with_bearer(token),
        None => acting,
    })
}

/// The node, as its own principal, in the transaction `handle` names.
pub fn node_transaction<'n>(
    node: &'n Node,
    handle: &TransactionHandle,
) -> defra_node::Result<TxnNode<'n>> {
    Ok(node.with_transaction(transaction(handle)?))
}

/// The ID of the transaction `handle` names.
pub fn transaction(handle: &TransactionHandle) -> defra_node::Result<TxnId> {
    handle.as_str().parse().map_err(|_| {
        defra_node::Error::Transaction(query::TransactionError::Operation(format!(
            "transaction ID '{handle}' is not a number"
        )))
    })
}

/// The handle Gents keeps for the transaction `id`.
pub fn handle(id: TxnId) -> TransactionHandle {
    TransactionHandle::new(id.to_string())
}

fn caller(context: &RequestContext) -> defra_node::Result<Option<Did>> {
    context
        .identity()
        .map(Did::new)
        .transpose()
        .map_err(defra_node::Error::from)
}

fn bearer(context: &RequestContext) -> Option<(String, bool)> {
    let token = context.bearer_token_for(context.identity()?)?;
    Some((token, context.bearer_names_request()))
}

/// The transaction error a node call failed with, as the transaction calls
/// report it.
pub fn transaction_error(error: defra_node::Error) -> query::TransactionError {
    match error {
        defra_node::Error::Transaction(error) | defra_node::Error::TransactionConflict(error) => {
            error
        }
        error => query::TransactionError::Operation(error.to_string()),
    }
}
