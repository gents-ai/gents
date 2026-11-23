use anyhow::{Context, Result};
use gents_protocol::enrollment::EnrollmentOperatorAction;
use gents_service::enrollment_admin::{
    approve_verified_client, grant_verified_client, load_pending_enrollments,
    submit_enrollment_decision,
};
use gents_service::home_state::{resolve_graphql_endpoint, resolve_home_dir};

use crate::cli::args::{
    DeviceGroupMemberArgs, P2pAccessArgs, P2pEnrollmentDecisionArgs, P2pEnrollmentGrantArgs,
};
use crate::print_json;

pub(super) async fn decide_enrollment(
    args: P2pEnrollmentDecisionArgs,
    action: EnrollmentOperatorAction,
) -> Result<()> {
    let request_id = args.request_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must not be empty");
    let home = resolve_home_dir(args.home.as_deref());
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let body = if action == EnrollmentOperatorAction::Approve {
        approve_verified_client(&home, graphql, request_id, args.lease_seconds, None)
            .await?
            .with_context(|| {
                format!("enrollment request {request_id} is not pending; nothing was granted or approved")
            })?
    } else {
        submit_enrollment_decision(&home, graphql.url(), request_id, action, 0).await?
    };
    print_json(&body)
}

/// Pending enrollment discovery uses a bounded wait on the live runtime.
pub(super) async fn pending_enrollments(args: P2pAccessArgs) -> Result<()> {
    let home = resolve_home_dir(args.home.as_deref());
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let body = load_pending_enrollments(&home, graphql.url()).await?;
    print_json(&body)
}

/// This explicit grant is separate from route admission. Its receipt commits
/// before any tuple is issued, so revoking the enrollment withdraws exactly
/// what it derived (Lean `Identity.EnrollmentGrant`); a failed grant is
/// finished by retrying it.
pub(super) async fn grant_pending_client(args: P2pEnrollmentGrantArgs) -> Result<()> {
    let endpoint = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let home = resolve_home_dir(args.home.as_deref());
    let result = grant_verified_client(&home, endpoint, &args.request_id, None)
        .await?
        .context("expected one verified pending enrollment")?;
    print_json(&result)
}

pub(super) async fn device_membership(args: DeviceGroupMemberArgs, admit: bool) -> Result<()> {
    use gents_protocol::schemas::ACCOUNT_DEVICE_GROUP_NAME;
    use gents_store::config_client::ConfigAccess;

    let endpoint = resolve_graphql_endpoint(None, args.home.as_deref()).await?;
    let owner = ConfigAccess::Graphql(endpoint);
    if owner.home_policy_ids().await?
        == gents_protocol::schemas::HomePolicyIds::embedded_vera_transport_v2()
    {
        let node = owner
            .set_shared_device_membership(&args.group, &args.device, admit)
            .await?;
        return print_json(&serde_json::json!({
            "group": args.group, "device": args.device.as_str(), "member": admit,
            "publisher_node_id": node,
        }));
    }
    if admit {
        owner
            .add_actor_relationship(
                ACCOUNT_DEVICE_GROUP_NAME,
                &args.group,
                "member",
                &args.device,
            )
            .await?;
    } else {
        owner
            .delete_actor_relationship(
                ACCOUNT_DEVICE_GROUP_NAME,
                &args.group,
                "member",
                &args.device,
            )
            .await?;
    }
    print_json(&serde_json::json!({
        "group": args.group,
        "device": args.device.as_str(),
        "member": admit,
    }))
}
