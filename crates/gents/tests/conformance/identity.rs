//! Defra ACP permission owner coverage after behavior resolution. Scoped
//! behavior selection/ambiguity and catalog well-formedness require a production
//! resolver; constructing a test-local map cannot establish those contracts.
use crate::lean_vocab_test::{lean_identity_permission_cases, LeanIdentityPermissionCase};
use acp::{
    AcpStore, DocumentACP, DocumentPermission, Identity, LocalDocumentACP, MemoryAcpStore,
    RelationTuple, READER_RELATION,
};
use identity::Did;
use std::sync::Arc;

const IDENTITY_PERMISSION_POLICY_ID: &str = "identity-permission-cases";
const IDENTITY_PERMISSION_RESOURCE_NAME: &str = "row";

async fn build_local_acp_from_lean_case(
    case: &LeanIdentityPermissionCase,
) -> anyhow::Result<LocalDocumentACP> {
    assert!(
        case.permission.ends_with(".read"),
        "case {:?}: only .read permission fixtures are supported by this ACP witness, got {:?}",
        case.name,
        case.permission
    );
    assert!(
        case.permission
            .starts_with(format!("row:{}:", case.row_owner).as_str()),
        "case {:?}: permission {:?} must be scoped to row owner {:?}",
        case.name,
        case.permission,
        case.row_owner
    );

    let store = Arc::new(MemoryAcpStore::new());
    let acp = LocalDocumentACP::new(store.clone());
    let row_owner = did_from_lean_case(&case.row_owner, case, "row_owner");

    acp.register_doc_object(
        &row_owner,
        IDENTITY_PERMISSION_POLICY_ID,
        IDENTITY_PERMISSION_RESOURCE_NAME,
        &case.row_owner,
    )
    .await?;

    let namespaced_resource =
        format!("{IDENTITY_PERMISSION_POLICY_ID}:{IDENTITY_PERMISSION_RESOURCE_NAME}");
    for grant in &case.grants {
        assert_eq!(
            grant.permission, case.permission,
            "case {:?}: grant {:?} targets a different permission than the row under test",
            case.name, grant
        );
        let principal = did_from_lean_case(&grant.principal, case, "grant.principal");
        let tuple = RelationTuple::try_new(
            principal,
            READER_RELATION,
            namespaced_resource.as_str(),
            case.row_owner.as_str(),
        )?;
        store.put_tuple(&tuple).await?;
    }

    Ok(acp)
}

fn did_from_lean_case(value: &str, case: &LeanIdentityPermissionCase, field: &str) -> Did {
    Did::new(value).unwrap_or_else(|error| {
        panic!(
            "case {:?}: {field} {:?} is not a valid DefraDB identity DID: {error}",
            case.name, value
        )
    })
}

#[tokio::test]
async fn resolved_identity_permission_cases_drive_defra_acp() -> anyhow::Result<()> {
    for case in lean_identity_permission_cases() {
        // Missing/ambiguous selection must be denied before ACP. This test has
        // no production selection operation and therefore makes no assertion
        // about those cases (recorded as gaps in the conformance handoff).
        if case.expected_actor_principal.is_none() || case.expected_peer_principal.is_none() {
            continue;
        }
        let acp = build_local_acp_from_lean_case(case).await?;
        let actor = Identity::Authenticated(did_from_lean_case(
            &case.actor_principal,
            case,
            "actor_principal",
        ));
        let peer = Identity::Authenticated(did_from_lean_case(
            &case.peer_principal,
            case,
            "peer_principal",
        ));
        let actor_allowed = acp
            .check_doc_access(
                &actor,
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        let peer_allowed = acp
            .check_doc_access(
                &peer,
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        assert_eq!(
            actor_allowed, case.expected_actor_allowed,
            "{}: actor ACP permission",
            case.name
        );
        assert_eq!(
            peer_allowed, case.expected_peer_allowed,
            "{}: peer ACP permission",
            case.name
        );
        assert_eq!(
            actor_allowed == peer_allowed,
            case.expected_decisions_equal,
            "{}: ACP decision equality",
            case.name
        );
    }
    Ok(())
}
