use anyhow::{Context, Result};
use gents_protocol::enrollment::EnrollmentOperatorAction;
use gents_service::enrollment_admin::{
    grant_verified_client, load_pending_enrollments, submit_enrollment_decision,
};
use gents_service::home_state::{resolve_graphql_endpoint, resolve_home_dir};

use crate::cli::args::{P2pAccessArgs, P2pEnrollmentDecisionArgs, P2pEnrollmentGrantArgs};
use crate::print_json;

pub(super) async fn decide_enrollment(
    args: P2pEnrollmentDecisionArgs,
    action: EnrollmentOperatorAction,
) -> Result<()> {
    let request_id = args.request_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must not be empty");
    let home = resolve_home_dir(args.home.as_deref());
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let body = submit_enrollment_decision(
        &home,
        graphql.url(),
        request_id,
        action.clone(),
        if action == EnrollmentOperatorAction::Approve {
            args.lease_seconds
        } else {
            0
        },
    )
    .await?;
    print_json(&body)
}

/// Pending enrollment discovery uses a bounded wait on the live runtime.
pub(super) async fn pending_enrollments(args: P2pAccessArgs) -> Result<()> {
    let home = resolve_home_dir(args.home.as_deref());
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let body = load_pending_enrollments(&home, graphql.url()).await?;
    print_json(&body)
}

/// This explicit grant is separate from route admission. Its receipt commits
/// before any tuple is issued, so revoking the enrollment withdraws exactly
/// what it derived (Lean `Identity.EnrollmentGrant`); a failed grant is
/// finished by retrying it.
pub(super) async fn grant_pending_client(args: P2pEnrollmentGrantArgs) -> Result<()> {
    let endpoint = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let home = resolve_home_dir(args.home.as_deref());
    let result = grant_verified_client(&home, endpoint, &args.request_id, None)
        .await?
        .context("expected one verified pending enrollment")?;
    print_json(&result)
}

