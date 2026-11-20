use serde_json::{json, Value};

use crate::config_client::{ConfigAccess, ConfigApplyTxn, ConfigClientError, NodePermission};
use crate::identity::{GrantSet, Recipient};

#[path = "enrollment_grant/settlement.rs"]
mod settlement;

pub use settlement::{
    after_removal, lapse_clock, membership_lapsed, owed_removals, permission_grant_admitted,
    restoration_step, settle_owed_withdrawals, settlement_step, AfterRemoval, RestorationStep,
    SettlementStep,
};
pub(crate) use settlement::{close_owed_removal, owed_removal_receipts};

const RECEIPT_COLLECTION: &str = gents_protocol::schemas::NETWORK_ENROLLMENT_GRANT_NAME;
pub(crate) const NODE_ADMINISTRATOR_RELATION: &str = "administrator";

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
                GrantSet::for_recipient(Recipient::EnrolledUser)
                    .direct
                    .iter()
                    .flat_map(|set| set.permissions())
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
    /// The withdrawal of the member's Node `administrator` relation is owed
    /// (Lean `Receipt.owed`).
    pub owed: bool,
    /// The relation's restoration is owed after a takeover (Lean
    /// `Receipt.restore`).
    pub restore: bool,
}

impl EnrollmentGrantReceipt {
    /// Lean `accounts`.
    pub fn accounts(&self, member: &str, tuple: EnrollmentGrantTuple) -> bool {
        !self.withdrawn && self.member_did == member && self.tuples.contains(&tuple)
    }
}

/// Lean `owedFor`.
pub fn administration_owed(member: &str, receipts: &[EnrollmentGrantReceipt]) -> bool {
    receipts
        .iter()
        .any(|receipt| receipt.owed && receipt.member_did == member)
}

/// The derived tuples a new grant for `member` records (Lean `record`): every
/// one except those held independently, that is, natively present, accounted
/// for by no live receipt and not an owed withdrawal.
pub fn plan_grant(
    member: &str,
    held: &[EnrollmentGrantTuple],
    receipts: &[EnrollmentGrantReceipt],
) -> Vec<EnrollmentGrantTuple> {
    let owed = administration_owed(member, receipts);
    EnrollmentGrantTuple::derived()
        .into_iter()
        .filter(|tuple| {
            !held.contains(tuple)
                || receipts
                    .iter()
                    .any(|receipt| receipt.accounts(member, *tuple))
                || (owed && *tuple == EnrollmentGrantTuple::NodeAdministrator)
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

/// The tuples revoking `member`'s membership withdraws (Lean `revokeMember`):
/// every tuple any of its live receipts recorded.
pub fn plan_membership_withdrawal(
    member: &str,
    receipts: &[EnrollmentGrantReceipt],
) -> Vec<EnrollmentGrantTuple> {
    EnrollmentGrantTuple::derived()
        .into_iter()
        .filter(|tuple| {
            receipts
                .iter()
                .any(|receipt| receipt.accounts(member, *tuple))
        })
        .collect()
}

/// The receipts a withdrawal the runtime runs leaves owing the Node
/// `administrator` relation's withdrawal (Lean `oweRevoke`,
/// `oweRevokeMember`): those of `retired` that accounted for it, when the
/// withdrawal removes it.
pub fn plan_owing(
    member: &str,
    retired: &[&EnrollmentGrantReceipt],
    removed: &[EnrollmentGrantTuple],
) -> Vec<String> {
    if !removed.contains(&EnrollmentGrantTuple::NodeAdministrator) {
        return Vec::new();
    }
    retired
        .iter()
        .filter(|receipt| receipt.accounts(member, EnrollmentGrantTuple::NodeAdministrator))
        .map(|receipt| receipt.doc_id.clone())
        .collect()
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
                owed: !row["withdrawal_owed_at"].is_null(),
                restore: !row["restoration_owed_at"].is_null(),
            })
        })
        .collect()
}

/// Every receipt recorded for `member`, as `access`'s actor reads them.
pub async fn load_enrollment_grant_receipts(
    access: &ConfigAccess,
    member: &str,
) -> Result<Vec<EnrollmentGrantReceipt>, EnrollmentGrantError> {
    let rendered = receipt_query(member);
    let response = access
        .read_for_decision(&rendered)
        .await
        .map_err(EnrollmentGrantError::Read)?;
    decode_receipts(&response)
}

fn receipt_query(member: &str) -> gents_protocol::document_query::Rendered {
    use gents_protocol::document_query::{
        fields::network_enrollment_grant as fields, Filter, Query, Selection,
    };
    Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_ID)
            .field(fields::MEMBER_DID)
            .field(fields::TUPLES)
            .field(fields::WITHDRAWN_AT)
            .field(fields::WITHDRAWAL_OWED_AT)
            .field(fields::RESTORATION_OWED_AT),
    )
    .filter(Filter::eq(fields::MEMBER_DID, member))
    .render()
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
        "issuing {tuple} failed; the authority this grant had issued was removed and its \
         receipt kept, so approving the request again reissues it"
    )]
    Issue {
        tuple: &'static str,
        #[source]
        source: ConfigClientError,
    },
    #[error(
        "issuing {tuple} failed, and removing {removing} afterwards failed too; the receipt \
         is kept, so approving the request again reissues every tuple and denying or revoking \
         it withdraws them"
    )]
    IssueNotAbandoned {
        tuple: &'static str,
        removing: &'static str,
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

impl crate::config_client::TransactionFailure for EnrollmentGrantError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Read(error)
            | Self::Observe(error)
            | Self::Record(error)
            | Self::Retire(error)
            | Self::Issue { source: error, .. }
            | Self::IssueNotAbandoned { source: error, .. }
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
/// `runtime` is the runtime principal that writes the receipt; `nac_owner`
/// issues the node permissions as the NAC owner, the only actor native admits
/// for the admin-class ones (`Identity.NodeProvisioning.nacOwner`): the
/// runtime principal in process, whose NAC changes are issued as the owner, or
/// the owning User's own client. The receipt is committed before any tuple is
/// issued, so an interrupted grant never leaves authority that revocation
/// cannot find; a repeated grant of a request with a live receipt reissues
/// exactly its tuples.
pub async fn grant_enrollment_authority(
    owner: &ConfigAccess,
    runtime: &ConfigAccess,
    nac_owner: &ConfigAccess,
    node_doc_id: &str,
    request_id: &str,
    member: &identity::Did,
) -> Result<EnrollmentGrantOutcome, EnrollmentGrantError> {
    let tuples = NativeTuples {
        owner,
        nac_owner,
        node_doc_id,
        member,
    };
    grant_with(&tuples, owner, runtime, node_doc_id, request_id, member).await
}

/// One member's native tuples on one Node: the Node `administrator`
/// relation, written by the Node's owner, and node permissions, written by
/// the NAC owner.
pub(crate) trait TupleChanges {
    async fn issue(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError>;
    async fn remove(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError>;
}

pub(crate) struct NativeTuples<'a> {
    pub(crate) owner: &'a ConfigAccess,
    pub(crate) nac_owner: &'a ConfigAccess,
    pub(crate) node_doc_id: &'a str,
    pub(crate) member: &'a identity::Did,
}

impl TupleChanges for NativeTuples<'_> {
    async fn issue(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        match tuple {
            EnrollmentGrantTuple::NodeAdministrator => {
                self.owner
                    .add_actor_relationship(
                        "Node",
                        self.node_doc_id,
                        NODE_ADMINISTRATOR_RELATION,
                        self.member,
                    )
                    .await
            }
            EnrollmentGrantTuple::Permission(permission) => {
                self.nac_owner
                    .grant_node_permission(self.member, permission)
                    .await
            }
        }
    }

    async fn remove(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        match tuple {
            EnrollmentGrantTuple::NodeAdministrator => {
                self.owner
                    .delete_actor_relationship(
                        "Node",
                        self.node_doc_id,
                        NODE_ADMINISTRATOR_RELATION,
                        self.member,
                    )
                    .await
            }
            EnrollmentGrantTuple::Permission(permission) => {
                self.nac_owner
                    .revoke_node_permission(self.member, permission)
                    .await
            }
        }
    }
}

/// [`grant_enrollment_authority`] issuing through `tuples`.
pub(crate) async fn grant_with<T>(
    tuples: &T,
    owner: &ConfigAccess,
    runtime: &ConfigAccess,
    node_doc_id: &str,
    request_id: &str,
    member: &identity::Did,
) -> Result<EnrollmentGrantOutcome, EnrollmentGrantError>
where
    T: TupleChanges,
{
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
        issue_or_abandon(tuples, existing, &receipts).await?;
        return Ok(EnrollmentGrantOutcome {
            recorded: existing.tuples.clone(),
            reissued: true,
        });
    }
    let mut held = Vec::new();
    let administration = owner
        .observe_actor_relationship("Node", node_doc_id, NODE_ADMINISTRATOR_RELATION, member)
        .await
        .map_err(EnrollmentGrantError::Observe)?;
    if administration {
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
    let accounted = plan_grant(member.as_str(), &held, &receipts);
    held.retain(|tuple| !accounted.contains(tuple));
    let query = receipt_query(member.as_str());
    let (doc_id, recorded, mut receipts) = runtime
        .transact_signed_conflict_rebuild("enrollment_grant.record", |txn| {
            let (query, held) = (&query, &held);
            Box::pin(async move {
                let receipts = decode_receipts(&txn.run(query).await?)?;
                let recorded = plan_grant(member.as_str(), held, &receipts);
                let names: Vec<&str> = recorded.iter().map(|tuple| tuple.name()).collect();
                let input = json!({
                    "request_id": request_id,
                    "member_did": member.as_str(),
                    "tuples": if names.is_empty() { Value::Null } else { json!(names) },
                    "granted_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "node_doc_id": node_doc_id,
                    "parent_linked": true,
                });
                let taken_over: Vec<&str> = receipts
                    .iter()
                    .filter(|receipt| receipt.owed)
                    .map(|receipt| receipt.doc_id.as_str())
                    .collect();
                take_over_in_txn(txn, &taken_over).await?;
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
                Ok::<_, EnrollmentGrantError>((doc, recorded, receipts))
            })
        })
        .await?;
    let receipt = EnrollmentGrantReceipt {
        doc_id,
        request_id: request_id.to_owned(),
        member_did: member.as_str().to_owned(),
        tuples: recorded.clone(),
        withdrawn: false,
        owed: false,
        restore: false,
    };
    receipts.push(receipt.clone());
    issue_or_abandon(tuples, &receipt, &receipts).await?;
    Ok(EnrollmentGrantOutcome {
        recorded,
        reissued: false,
    })
}

/// Issue `receipt`'s tuples one at a time. When one fails, remove every
/// tuple the receipt alone accounts for and keep the receipt live (Lean
/// `Identity.EnrollmentGrant.abandon`), so a failed grant leaves no authority it
/// issued and a retried grant reissues all of it. When a removal fails too, the
/// receipt still accounts for whatever remains, which approving the request again
/// reissues and denying or revoking it withdraws.
async fn issue_or_abandon<T>(
    tuples: &T,
    receipt: &EnrollmentGrantReceipt,
    receipts: &[EnrollmentGrantReceipt],
) -> Result<(), EnrollmentGrantError>
where
    T: TupleChanges,
{
    for tuple in &receipt.tuples {
        if let Err(source) = tuples.issue(*tuple).await {
            for removed in plan_withdrawal(receipt, receipts) {
                if let Err(removal) = tuples.remove(removed).await {
                    tracing::warn!(
                        tuple = tuple.name(),
                        removing = removed.name(),
                        error = %removal,
                        "an abandoned enrollment grant kept authority it had issued"
                    );
                    return Err(EnrollmentGrantError::IssueNotAbandoned {
                        tuple: tuple.name(),
                        removing: removed.name(),
                        source,
                    });
                }
            }
            return Err(EnrollmentGrantError::Issue {
                tuple: tuple.name(),
                source,
            });
        }
    }
    Ok(())
}

/// The withdrawal one revocation owes (Lean `revoke`): the live receipt it
/// retires and the tuples only that receipt accounted for. When the runtime
/// runs it, the retired receipts in `owing_doc_ids` owe the Node
/// `administrator` relation's withdrawal instead (Lean `oweRevoke`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentGrantWithdrawal {
    pub receipt_doc_ids: Vec<String>,
    pub owing_doc_ids: Vec<String>,
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
        let tuples = plan_withdrawal(revoked, receipts);
        Ok(Some(Self {
            receipt_doc_ids: vec![revoked.doc_id.clone()],
            owing_doc_ids: plan_owing(&revoked.member_did, &[revoked], &tuples),
            member: identity::Did::new(&revoked.member_did)
                .map_err(EnrollmentGrantError::MemberIdentity)?,
            tuples,
        }))
    }

    /// Plan the withdrawal a membership's revocation owes (Lean
    /// `revokeMember`): every live receipt of `member`, whichever request or
    /// renewal issued it, and every tuple they recorded.
    pub fn plan_membership(
        member: &str,
        receipts: &[EnrollmentGrantReceipt],
    ) -> Result<Option<Self>, EnrollmentGrantError> {
        let live: Vec<_> = receipts
            .iter()
            .filter(|receipt| receipt.member_did == member && !receipt.withdrawn)
            .collect();
        if live.is_empty() {
            return Ok(None);
        }
        let tuples = plan_membership_withdrawal(member, receipts);
        Ok(Some(Self {
            receipt_doc_ids: live.iter().map(|receipt| receipt.doc_id.clone()).collect(),
            owing_doc_ids: plan_owing(member, &live, &tuples),
            member: identity::Did::new(member).map_err(EnrollmentGrantError::MemberIdentity)?,
            tuples,
        }))
    }

    /// Native node permissions are not transactional, so they are revoked
    /// before the revision commits: a failure leaves the member with less
    /// authority, never more, and the receipt stays live for a retry. They are
    /// revoked as the NAC owner, as they were issued.
    pub async fn revoke_node_permissions(
        &self,
        nac_owner: &ConfigAccess,
    ) -> Result<(), EnrollmentGrantError> {
        for tuple in &self.tuples {
            if let EnrollmentGrantTuple::Permission(permission) = tuple {
                nac_owner
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

    /// Retire the receipts in the runtime's transaction that publishes the
    /// denial or revocation, those that accounted for the Node
    /// `administrator` relation owing its withdrawal (Lean `oweRevoke`): the
    /// Node may not remove a relation on the User-owned Node document
    /// (`node_alone_neither_grants_nor_withdraws_administration`), so the
    /// owner's client issues it (`ClientAccessOwner::settle_withdrawals`).
    pub async fn retire_owing_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<(), EnrollmentGrantError> {
        self.retire_receipts_in_txn(txn, &self.owing_doc_ids).await
    }

    pub async fn withdraw_as(
        &self,
        approver: &ConfigAccess,
        runtime: &ConfigAccess,
        nac_owner: &ConfigAccess,
        node_doc_id: &str,
    ) -> Result<(), EnrollmentGrantError> {
        let tuples = NativeTuples {
            owner: approver,
            nac_owner,
            node_doc_id,
            member: &self.member,
        };
        for tuple in &self.tuples {
            tuples
                .remove(*tuple)
                .await
                .map_err(|source| EnrollmentGrantError::Revoke {
                    tuple: tuple.name(),
                    source,
                })?;
        }
        runtime
            .transact_write("client_access.retire_grant", |txn| {
                Box::pin(async move { self.retire_receipts_in_txn(txn, &[]).await })
            })
            .await
    }

    async fn retire_receipts_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        owing: &[String],
    ) -> Result<(), EnrollmentGrantError> {
        use gents_protocol::document_query::{fields::network_enrollment_grant as fields, Input};
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        for receipt_doc_id in &self.receipt_doc_ids {
            let input = Input::new().set(fields::WITHDRAWN_AT, now.as_str());
            let input = match owing.contains(receipt_doc_id) {
                true => input.set(fields::WITHDRAWAL_OWED_AT, now.as_str()),
                false => input,
            };
            update_receipt_in_txn(txn, receipt_doc_id, input).await?;
        }
        Ok(())
    }
}

async fn update_receipt_in_txn(
    txn: &ConfigApplyTxn<'_>,
    receipt_doc_id: &str,
    input: gents_protocol::document_query::Input<
        gents_protocol::document_query::fields::network_enrollment_grant::Doc,
    >,
) -> Result<(), EnrollmentGrantError> {
    let rendered = gents_protocol::document_query::Update::doc(receipt_doc_id, input).render();
    let response = txn
        .run(&rendered)
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
    Ok(())
}

/// Take the owed withdrawals of `receipt_doc_ids` over (Lean `takeOver`):
/// they stop owing the removal and owe the relation's restoration instead,
/// in the same write.
pub(crate) async fn take_over_in_txn(
    txn: &ConfigApplyTxn<'_>,
    receipt_doc_ids: &[&str],
) -> Result<(), EnrollmentGrantError> {
    use gents_protocol::document_query::{fields::network_enrollment_grant as fields, Input};
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for receipt_doc_id in receipt_doc_ids {
        update_receipt_in_txn(
            txn,
            receipt_doc_id,
            Input::new()
                .set_null(fields::WITHDRAWAL_OWED_AT)
                .set(fields::RESTORATION_OWED_AT, now.as_str()),
        )
        .await?;
    }
    Ok(())
}

/// Close the owed withdrawals of `receipt_doc_ids` once issued (Lean
/// `clearOwed`).
pub(crate) async fn close_owed_in_txn(
    txn: &ConfigApplyTxn<'_>,
    receipt_doc_ids: &[&str],
) -> Result<(), EnrollmentGrantError> {
    use gents_protocol::document_query::{fields::network_enrollment_grant as fields, Input};
    for receipt_doc_id in receipt_doc_ids {
        update_receipt_in_txn(
            txn,
            receipt_doc_id,
            Input::new().set_null(fields::WITHDRAWAL_OWED_AT),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "enrollment_grant_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "enrollment_grant/withdrawal_tests.rs"]
mod withdrawal_tests;
