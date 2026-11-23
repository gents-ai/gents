use std::sync::Arc;

use anyhow::{bail, Context as _};
use gents_protocol::schemas::{
    identity_policy_resource, is_replicated_protocol_collection, BuiltInPolicy, HomePolicyIds,
};
use serde::{Deserialize, Serialize};

use super::{
    delegation, ClientSigningCapability, RequestBoundAudience, VERA_REQUEST_BOUND_LIFETIME,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    account: String,
    collection: String,
    parent: String,
    token: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherApproval {
    account: String,
    parent: String,
    publisher: String,
    token: String,
}

fn publisher_command<'a>(
    parent: &'a str,
    subject: &'a vera_delegation::SubjectRef,
) -> vera_delegation::PolicyCommand<'a> {
    vera_delegation::PolicyCommand {
        policy_id: HomePolicyIds::embedded_vera_transport_v2().id(BuiltInPolicy::Household),
        resource: "nodes",
        object_id: parent,
        action: vera_delegation::PolicyAction::SetRelationship {
            relation: "transport_publisher",
            subject: vera_delegation::PolicySubject::Actor(subject),
        },
    }
}

pub fn prepare_publisher(
    account: &ClientSigningCapability,
    audience: &RequestBoundAudience,
    parent: &str,
    publisher: &identity::Did,
) -> anyhow::Result<PublisherApproval> {
    if parent.is_empty() || publisher.is_wildcard() {
        bail!("an individual publisher and parent are required");
    }
    let subject = vera_delegation::SubjectRef::Actor(publisher.to_string());
    let token = delegation::mint_request_bound(
        account,
        audience,
        &publisher_command(parent, &subject),
        VERA_REQUEST_BOUND_LIFETIME,
    )?;
    Ok(PublisherApproval {
        account: account.did().to_owned(),
        parent: parent.to_owned(),
        publisher: publisher.to_string(),
        token,
    })
}

pub async fn submit_publisher(
    node: &Arc<defra_node::Node>,
    expected_account: &str,
    approval: PublisherApproval,
) -> anyhow::Result<()> {
    if approval.account != expected_account || approval.parent.is_empty() {
        bail!("publisher signer differs from the anchored account");
    }
    let account = identity::Did::new(&approval.account)?;
    let publisher = identity::Did::new(&approval.publisher)?;
    if publisher.is_wildcard() {
        bail!("publisher must be an individual");
    }
    let policy = HomePolicyIds::embedded_vera_transport_v2().id(BuiltInPolicy::Household);
    let provider = node.document_acp();
    if provider
        .get_doc_owner(policy, "nodes", &approval.parent)
        .await?
        .as_ref()
        != Some(&account)
    {
        bail!("publisher Node is not owned by the anchored account");
    }
    let context = defra_core::RequestContext::default()
        .with_request_delegation(approval.account, approval.token);
    acp::bound_to_request(&provider, &context)
        .add_relationship(
            &account,
            acp::Subject::entity(publisher),
            policy,
            "nodes",
            &approval.parent,
            "transport_publisher",
            &[],
        )
        .await?;
    Ok(())
}

fn command<'a>(
    collection: &str,
    parent: &'a str,
) -> anyhow::Result<vera_delegation::PolicyCommand<'a>> {
    if !is_replicated_protocol_collection(collection) || parent.is_empty() {
        bail!("invalid transport sentinel selection");
    }
    let ids = HomePolicyIds::embedded_vera_transport_v2();
    let resource = identity_policy_resource(collection).context("unknown transport resource")?;
    let child = crate::schema::catalog::runtime_catalog(ids)
        .baseline
        .iter()
        .find(|entry| entry.name == collection)
        .and_then(|entry| entry.expected_version)
        .context("missing canonical transport collection identity")?;
    Ok(vera_delegation::PolicyCommand {
        policy_id: ids.id(BuiltInPolicy::Household),
        resource,
        object_id: child,
        action: vera_delegation::PolicyAction::RegisterUnderParent {
            resource: "nodes",
            object_id: parent,
        },
    })
}

pub fn prepare(
    account: &ClientSigningCapability,
    audience: &RequestBoundAudience,
    collection: &str,
    parent: &str,
) -> anyhow::Result<Approval> {
    let operation = command(collection, parent)?;
    let token =
        delegation::mint_request_bound(account, audience, &operation, VERA_REQUEST_BOUND_LIFETIME)?;
    Ok(Approval {
        account: account.did().to_owned(),
        collection: collection.to_owned(),
        parent: parent.to_owned(),
        token,
    })
}

pub async fn submit(
    node: &Arc<defra_node::Node>,
    expected_account: &str,
    approval: Approval,
) -> anyhow::Result<()> {
    if approval.account != expected_account {
        bail!("sentinel signer differs from the anchored account");
    }
    let operation = command(&approval.collection, &approval.parent)?;
    let account = identity::Did::new(&approval.account)?;
    let provider = node.document_acp();
    if provider
        .get_doc_owner(operation.policy_id, "nodes", &approval.parent)
        .await?
        .as_ref()
        != Some(&account)
    {
        bail!("sentinel parent is not owned by the anchored account");
    }
    let parent = provider
        .registration_parent(operation.policy_id, operation.resource, &approval.parent)
        .await?;
    if parent.resource != "nodes" || parent.object_id != approval.parent {
        bail!("sentinel parent does not match the approved command");
    }
    let context = defra_core::RequestContext::default()
        .with_request_delegation(approval.account, approval.token);
    acp::bound_to_request(&provider, &context)
        .register_under_parent(
            &account,
            operation.policy_id,
            operation.resource,
            operation.object_id,
            &parent,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_signature_binds_the_exact_sentinel_and_deployment() {
        let account = ClientSigningCapability::from_ed25519_key(&[7; 32]).unwrap();
        let audience = RequestBoundAudience {
            audience: gents_protocol::delegation_audience::DelegationAudience {
                worker: account.did().to_owned(),
                deployment: 42,
                genesis_id: Some(format!("0x{}", "ab".repeat(32))),
            },
            issued_at: 1000,
        };
        let approval = prepare(&account, &audience, "PeerRegistry", "node-a").unwrap();
        let claims = vera_crypto::jwt::verify_bearer_token(&approval.token).unwrap();
        assert_eq!(claims.iss, account.did());
        assert_eq!(claims.sub, audience.audience.worker);
        assert_eq!(claims.aud, "vera:42");
        let request = claims.request.unwrap();
        assert_eq!(request.genesis_id, [0xab; 32]);
        assert_eq!(
            request.digest,
            vera_delegation::request_digest(&command("PeerRegistry", "node-a").unwrap()).unwrap()
        );
        assert_ne!(
            request.digest,
            vera_delegation::request_digest(&command("PeerRegistry", "node-b").unwrap()).unwrap()
        );
        assert_ne!(
            request.digest,
            vera_delegation::request_digest(&command("Network", "node-a").unwrap()).unwrap()
        );
        assert!(prepare(&account, &audience, "Node", "node-a").is_err());
        let publisher = identity::Did::new(account.did()).unwrap();
        let grant = prepare_publisher(&account, &audience, "node-a", &publisher).unwrap();
        let grant_claims = vera_crypto::jwt::verify_bearer_token(&grant.token).unwrap();
        let subject = vera_delegation::SubjectRef::Actor(publisher.to_string());
        let digest =
            vera_delegation::request_digest(&publisher_command("node-a", &subject)).unwrap();
        assert_eq!(grant_claims.request.unwrap().digest, digest);
        assert!(
            prepare_publisher(&account, &audience, "node-a", &identity::Did::wildcard()).is_err()
        );
    }
}
