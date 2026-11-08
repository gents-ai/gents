use serde_json::{json, Value};

use crate::store::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, NodePermission,
};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::GrantSet;

const RECEIPT_COLLECTION: &str = gents_protocol::schemas::NETWORK_ENROLLMENT_GRANT_NAME;
const NODE_ADMINISTRATOR_RELATION: &str = "administrator";

/// One native tuple an explicit enrollment grant derives for its member
/// (Lean `Identity.EnrollmentGrant.Tuple`): the Node document's
/// `administrator` relation, or one direct `operator-user` node permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentGrantTuple {
    NodeAdministrator,
    Permission(NodePermission),
}

impl EnrollmentGrantTuple {
    pub fn name(self) -> &'static str {
        match self {
            Self::NodeAdministrator => "node-administrator",
            Self::Permission(permission) => permission.as_str(),
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::derived()
            .into_iter()
            .find(|tuple| tuple.name() == name)
    }

    /// Exactly the authority one grant derives (Lean `derived`).
    pub fn derived() -> Vec<Self> {
        std::iter::once(Self::NodeAdministrator)
            .chain(
                GrantSet::OperatorUser
                    .permissions()
                    .iter()
                    .copied()
                    .map(Self::Permission),
            )
            .collect()
    }
}

/// The durable receipt one grant writes before issuing any tuple (Lean
/// `Receipt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentGrantReceipt {
    pub doc_id: String,
    pub request_id: String,
    pub member_did: String,
    pub tuples: Vec<EnrollmentGrantTuple>,
    pub withdrawn: bool,
}

impl EnrollmentGrantReceipt {
    /// Lean `accounts`.
    pub fn accounts(&self, member: &str, tuple: EnrollmentGrantTuple) -> bool {
        !self.withdrawn && self.member_did == member && self.tuples.contains(&tuple)
    }
}

/// The derived tuples a new grant for `member` records (Lean `record`): every
/// one except those held independently, that is, natively present and
/// accounted for by no live receipt.
pub fn plan_grant(
    member: &str,
    held: &[EnrollmentGrantTuple],
    receipts: &[EnrollmentGrantReceipt],
) -> Vec<EnrollmentGrantTuple> {
    EnrollmentGrantTuple::derived()
        .into_iter()
        .filter(|tuple| {
            !held.contains(tuple)
                || receipts
                    .iter()
                    .any(|receipt| receipt.accounts(member, *tuple))
        })
        .collect()
}

/// The tuples revoking `revoked` withdraws (Lean `removedBy`): those it
/// recorded that no other live receipt still accounts for.
pub fn plan_withdrawal(
    revoked: &EnrollmentGrantReceipt,
    receipts: &[EnrollmentGrantReceipt],
) -> Vec<EnrollmentGrantTuple> {
    revoked
        .tuples
        .iter()
        .copied()
        .filter(|tuple| {
            !receipts.iter().any(|receipt| {
                receipt.request_id != revoked.request_id
                    && receipt.accounts(&revoked.member_did, *tuple)
            })
        })
        .collect()
}

fn receipts_query(member: &str) -> String {
    format!(
        r#"{{ {RECEIPT_COLLECTION}(filter: {{ member_did: {{ _eq: "{}" }} }}) {{ _docID request_id member_did tuples withdrawn_at }} }}"#,
        escape_graphql_string(member)
    )
}

fn decode_receipts(response: &Value) -> Result<Vec<EnrollmentGrantReceipt>, EnrollmentGrantError> {
    let malformed = |reason: String| EnrollmentGrantError::MalformedReceipt(reason);
    gents_protocol::graphql::graphql_rows_from_response(response, RECEIPT_COLLECTION)
        .iter()
        .map(|row| {
            let field = |name: &str| {
                row[name]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| malformed(format!("no {name}")))
            };
            let tuples = match &row["tuples"] {
                Value::Null => Vec::new(),
                Value::Array(names) => names
                    .iter()
                    .map(|name| {
                        name.as_str()
                            .and_then(EnrollmentGrantTuple::parse)
                            .ok_or_else(|| malformed(format!("unknown tuple {name}")))
                    })
                    .collect::<Result<_, _>>()?,
                other => return Err(malformed(format!("tuples are {other}"))),
            };
            Ok(EnrollmentGrantReceipt {
                doc_id: field("_docID")?,
                request_id: field("request_id")?,
                member_did: field("member_did")?,
                tuples,
                withdrawn: !row["withdrawn_at"].is_null(),
            })
        })
        .collect()
}

/// Every receipt recorded for `member`, as `access`'s actor reads them.
pub async fn load_enrollment_grant_receipts(
    access: &ConfigAccess,
    member: &str,
) -> Result<Vec<EnrollmentGrantReceipt>, EnrollmentGrantError> {
    let response = access
        .execute(&receipts_query(member))
        .await
        .map_err(EnrollmentGrantError::Read)?;
    decode_receipts(&response)
}

/// What an explicit enrollment grant recorded and issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentGrantOutcome {
    pub recorded: Vec<EnrollmentGrantTuple>,
    /// The request already had a live receipt; its tuples were reissued.
    pub reissued: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EnrollmentGrantError {
    #[error("enrollment {request_id} was revoked; its grant is not reissued")]
    Withdrawn { request_id: String },
    #[error("enrollment grant receipt is malformed: {0}")]
    MalformedReceipt(String),
    #[error("enrollment grant receipt names an invalid member")]
    MemberIdentity(#[source] identity::Error),
    #[error("reading enrollment grant receipts failed")]
    Read(#[source] ConfigClientError),
    #[error("observing what the enrollment member already holds failed; nothing was granted")]
    Observe(#[source] ConfigClientError),
    #[error("recording the enrollment grant receipt failed; nothing was granted")]
    Record(#[source] ConfigClientError),
    #[error("the enrollment grant receipt create returned no document")]
    RecordUnconfirmed(#[source] gents_protocol::graphql::GraphqlError),
    #[error(
        "the enrollment grant receipt is recorded, but issuing {tuple} failed; \
         retry the grant to finish it, or revoke the enrollment to withdraw it"
    )]
    Issue {
        tuple: &'static str,
        #[source]
        source: ConfigClientError,
    },
    #[error("revoking the derived {tuple} failed; the revocation was not published")]
    Revoke {
        tuple: &'static str,
        #[source]
        source: ConfigClientError,
    },
    #[error("retiring the enrollment grant receipt failed")]
    Retire(#[source] ConfigClientError),
    #[error("the enrollment grant receipt was not retired")]
    RetireUnconfirmed,
}

impl crate::store::config_client::TransactionFailure for EnrollmentGrantError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Read(error)
            | Self::Observe(error)
            | Self::Record(error)
            | Self::Retire(error)
            | Self::Issue { source: error, .. }
            | Self::Revoke { source: error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<ConfigClientError> for EnrollmentGrantError {
    fn from(error: ConfigClientError) -> Self {
        Self::Record(error)
    }
}

/// Grant `member` the authority one verified client enrollment derives (Lean
/// `Identity.EnrollmentGrant.grant`). `owner` is the Node document's owner,
/// which alone can observe and add the Node `administrator` relation;
/// `runtime` is the runtime principal that writes the receipt and issues the
/// node permissions. The receipt is committed before any tuple is issued, so
/// an interrupted grant never leaves authority that revocation cannot find; a
/// repeated grant of a request with a live receipt reissues exactly its tuples.
pub async fn grant_enrollment_authority(
    owner: &ConfigAccess,
    runtime: &ConfigAccess,
    node_doc_id: &str,
    request_id: &str,
    member: &identity::Did,
) -> Result<EnrollmentGrantOutcome, EnrollmentGrantError> {
    let receipts = load_enrollment_grant_receipts(runtime, member.as_str()).await?;
    if let Some(existing) = receipts
        .iter()
        .find(|receipt| receipt.request_id == request_id)
    {
        if existing.withdrawn {
            return Err(EnrollmentGrantError::Withdrawn {
                request_id: request_id.to_owned(),
            });
        }
        issue(owner, runtime, node_doc_id, member, &existing.tuples).await?;
        return Ok(EnrollmentGrantOutcome {
            recorded: existing.tuples.clone(),
            reissued: true,
        });
    }
    let mut held = Vec::new();
    if owner
        .list_document_relationships("Node", node_doc_id)
        .await
        .map_err(EnrollmentGrantError::Observe)?
        .iter()
        .any(|relationship| {
            relationship.relation == NODE_ADMINISTRATOR_RELATION
                && relationship.subject == member.as_str()
        })
    {
        held.push(EnrollmentGrantTuple::NodeAdministrator);
    }
    for provenance in runtime
        .explain_node_subject(member)
        .await
        .map_err(EnrollmentGrantError::Observe)?
    {
        if matches!(provenance.via, acp::nac::PermissionVia::Direct) {
            held.push(EnrollmentGrantTuple::Permission(provenance.permission));
        }
    }
    let recorded = plan_grant(member.as_str(), &held, &receipts);
    let names: Vec<&str> = recorded.iter().map(|tuple| tuple.name()).collect();
    let input = json!({
        "request_id": request_id,
        "member_did": member.as_str(),
        "tuples": if names.is_empty() { Value::Null } else { json!(names) },
        "granted_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "node_doc_id": node_doc_id,
        "parent_linked": true,
    });
    runtime
        .transact_signed_conflict_rebuild("enrollment_grant.record", |txn| {
            let input = &input;
            Box::pin(async move {
                let result = txn
                    .execute_client_signed_create(
                        "mutation($input: NetworkEnrollmentGrantMutationInputArg!) { create_NetworkEnrollmentGrant(input: $input) { _docID } }",
                        &json!({ "input": input }),
                        gents_protocol::schemas::NETWORK_ENROLLMENT_GRANT,
                    )
                    .await?;
                let doc =
                    gents_protocol::graphql::extract_mutation_doc_id(&result, RECEIPT_COLLECTION)
                        .map_err(EnrollmentGrantError::RecordUnconfirmed)?;
                txn.add_resource_relationship(RECEIPT_COLLECTION, &doc, "parent", "nodes", node_doc_id)
                    .await?;
                Ok::<_, EnrollmentGrantError>(doc)
            })
        })
        .await?;
    issue(owner, runtime, node_doc_id, member, &recorded).await?;
    Ok(EnrollmentGrantOutcome {
        recorded,
        reissued: false,
    })
}

async fn issue(
    owner: &ConfigAccess,
    runtime: &ConfigAccess,
    node_doc_id: &str,
    member: &identity::Did,
    tuples: &[EnrollmentGrantTuple],
) -> Result<(), EnrollmentGrantError> {
    for tuple in tuples {
        let result = match tuple {
            EnrollmentGrantTuple::NodeAdministrator => {
                owner
                    .add_actor_relationship(
                        "Node",
                        node_doc_id,
                        NODE_ADMINISTRATOR_RELATION,
                        member,
                    )
                    .await
            }
            EnrollmentGrantTuple::Permission(permission) => {
                runtime.grant_node_permission(member, *permission).await
            }
        };
        result.map_err(|source| EnrollmentGrantError::Issue {
            tuple: tuple.name(),
            source,
        })?;
    }
    Ok(())
}

/// The withdrawal one revocation owes (Lean `revoke`): the live receipt it
/// retires and the tuples only that receipt accounted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentGrantWithdrawal {
    pub receipt_doc_id: String,
    pub member: identity::Did,
    pub tuples: Vec<EnrollmentGrantTuple>,
}

impl EnrollmentGrantWithdrawal {
    /// Plan the withdrawal of `request_id`'s live receipt, if it has one.
    pub fn plan(
        request_id: &str,
        receipts: &[EnrollmentGrantReceipt],
    ) -> Result<Option<Self>, EnrollmentGrantError> {
        let Some(revoked) = receipts
            .iter()
            .find(|receipt| receipt.request_id == request_id && !receipt.withdrawn)
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            receipt_doc_id: revoked.doc_id.clone(),
            member: identity::Did::new(&revoked.member_did)
                .map_err(EnrollmentGrantError::MemberIdentity)?,
            tuples: plan_withdrawal(revoked, receipts),
        }))
    }

    /// Native node permissions are not transactional, so they are revoked
    /// before the revision commits: a failure leaves the member with less
    /// authority, never more, and the receipt stays live for a retry.
    pub async fn revoke_node_permissions(
        &self,
        runtime: &ConfigAccess,
    ) -> Result<(), EnrollmentGrantError> {
        for tuple in &self.tuples {
            if let EnrollmentGrantTuple::Permission(permission) = tuple {
                runtime
                    .revoke_node_permission(&self.member, *permission)
                    .await
                    .map_err(|source| EnrollmentGrantError::Revoke {
                        tuple: tuple.name(),
                        source,
                    })?;
            }
        }
        Ok(())
    }

    /// Retire the receipt and the Node `administrator` relation in the
    /// transaction that publishes the revocation. The runtime is the native
    /// NAC owner, so it may remove a relation on the user-owned Node.
    pub async fn stage_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_doc_id: &str,
    ) -> Result<(), EnrollmentGrantError> {
        let withdrawn_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mutation = format!(
            r#"mutation {{ update_{RECEIPT_COLLECTION}(docID: "{}", input: {{ withdrawn_at: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&self.receipt_doc_id),
            escape_graphql_string(&withdrawn_at),
        );
        let response = txn
            .execute(&mutation)
            .await
            .map_err(EnrollmentGrantError::Retire)?;
        let updated = response
            .pointer(&format!("/data/update_{RECEIPT_COLLECTION}"))
            .or_else(|| response.pointer(&format!("/update_{RECEIPT_COLLECTION}")))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if updated != 1 {
            return Err(EnrollmentGrantError::RetireUnconfirmed);
        }
        if self
            .tuples
            .contains(&EnrollmentGrantTuple::NodeAdministrator)
        {
            txn.delete_actor_relationship(
                "Node",
                node_doc_id,
                NODE_ADMINISTRATOR_RELATION,
                &self.member,
            )
            .await
            .map_err(EnrollmentGrantError::Retire)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "enrollment_grant_tests.rs"]
mod tests;
