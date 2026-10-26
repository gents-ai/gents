use super::error::{P2pCommandError, Result};
use chrono::{SecondsFormat, Utc};
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQuery,
    EnrollmentOperatorQueryCommand, ENROLLMENT_PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::cli::args::{P2pAccessArgs, P2pEnrollmentDecisionArgs};
use crate::{
    load_initialized_home_identity, print_json, read_init_config, resolve_graphql_endpoint,
    resolve_home_dir,
};

pub(crate) fn resolve_home_identity(
    home: Option<&std::path::Path>,
) -> Result<std::sync::Arc<dyn gents::NodeIdentity>, P2pCommandError> {
    let home_dir = resolve_home_dir(home);
    let config = read_init_config(&home_dir)?.ok_or_else(|| P2pCommandError::NoInitConfig {
        home_dir: home_dir.clone(),
    })?;
    Ok(load_initialized_home_identity(&home_dir, &config)?)
}

pub(super) async fn decide_enrollment(
    args: P2pEnrollmentDecisionArgs,
    action: EnrollmentOperatorAction,
) -> Result<()> {
    let request_id = args.request_id.trim();
    if request_id.is_empty() {
        return Err(P2pCommandError::EmptyRequestId);
    }
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
    Ok(print_json(&body)?)
}

pub(crate) async fn submit_enrollment_decision(
    home: &std::path::Path,
    graphql: &str,
    request_id: &str,
    action: EnrollmentOperatorAction,
    lease_seconds: u64,
) -> Result<serde_json::Value> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Err(P2pCommandError::EmptyRequestId);
    }
    let identity = resolve_home_identity(Some(home)).map_err(|source| {
        P2pCommandError::LoadOperatorIdentity {
            purpose: "enrollment decision",
            source: Box::new(source),
        }
    })?;
    let mut url = reqwest::Url::parse(graphql).map_err(P2pCommandError::ParseGraphqlEndpoint)?;
    url.set_path("/enrollment/decisions");
    url.set_query(None);
    url.set_fragment(None);

    let mut command = EnrollmentOperatorDecisionCommand {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        lease_seconds,
        action,
        admin_did: identity.did().to_string(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        nonce: Uuid::new_v4().simple().to_string(),
        admin_sig: Vec::new(),
    };
    command.admin_sig = identity
        .sign(&command.signing_payload())
        .await
        .map_err(|source| P2pCommandError::SignOperatorCommand {
            what: "enrollment operator command",
            source,
        })?;

    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(url)
        .json(&command)
        .send()
        .await
        .map_err(|source| P2pCommandError::OperatorRequest {
            what: "submitting signed enrollment decision to live runtime",
            source,
        })?;
    let status = response.status();
    let body: serde_json::Value =
        response
            .json()
            .await
            .map_err(|source| P2pCommandError::OperatorRequest {
                what: "decoding enrollment decision response",
                source,
            })?;
    if !status.is_success() {
        return Err(P2pCommandError::OperatorRejected {
            what: "enrollment decision",
            status,
            message: body
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
                .to_owned(),
        });
    }
    Ok(body)
}

pub(super) async fn pending_enrollments(args: P2pAccessArgs) -> Result<()> {
    let identity = resolve_home_identity(args.home.as_deref()).map_err(|source| {
        P2pCommandError::LoadOperatorIdentity {
            purpose: "pending enrollments",
            source: Box::new(source),
        }
    })?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let mut url =
        reqwest::Url::parse(graphql.url()).map_err(P2pCommandError::ParseGraphqlEndpoint)?;
    url.set_path("/enrollment/pending");
    url.set_query(None);
    url.set_fragment(None);
    let mut command = EnrollmentOperatorQueryCommand {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        query: EnrollmentOperatorQuery::Pending,
        admin_did: identity.did().to_string(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        nonce: Uuid::new_v4().simple().to_string(),
        admin_sig: Vec::new(),
    };
    command.admin_sig = identity
        .sign(&command.signing_payload())
        .await
        .map_err(|source| P2pCommandError::SignOperatorCommand {
            what: "pending enrollment query",
            source,
        })?;
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(url)
        .json(&command)
        .send()
        .await
        .map_err(|source| P2pCommandError::OperatorRequest {
            what: "loading pending enrollment requests from live runtime",
            source,
        })?;
    let status = response.status();
    let body: serde_json::Value =
        response
            .json()
            .await
            .map_err(|source| P2pCommandError::OperatorRequest {
                what: "decoding pending enrollment response",
                source,
            })?;
    if !status.is_success() {
        return Err(P2pCommandError::OperatorRejected {
            what: "enrollment pending query",
            status,
            message: body
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
                .to_owned(),
        });
    }
    Ok(print_json(&body)?)
}
