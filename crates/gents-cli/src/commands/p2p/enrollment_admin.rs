use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQuery,
    EnrollmentOperatorQueryCommand, ENROLLMENT_PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::cli::args::{P2pAccessArgs, P2pEnrollmentDecisionArgs, P2pEnrollmentGrantArgs};
use crate::{
    load_initialized_home_identity, print_json, read_init_config, resolve_graphql_endpoint,
    resolve_home_dir,
};

pub(crate) fn resolve_home_identity(
    home: Option<&std::path::Path>,
) -> Result<std::sync::Arc<dyn gents::NodeIdentity>> {
    let home_dir = resolve_home_dir(home);
    let config = read_init_config(&home_dir)?.with_context(|| {
        format!(
            "no init config found in {}; run `gents init` first",
            home_dir.display()
        )
    })?;
    load_initialized_home_identity(&home_dir, &config)
}

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

/// A live runtime receives the signed decision; bound waiting on its endpoint
/// so an unavailable server cannot leave the command pending indefinitely.
pub(crate) async fn submit_enrollment_decision(
    home: &std::path::Path,
    graphql: &str,
    request_id: &str,
    action: EnrollmentOperatorAction,
    lease_seconds: u64,
) -> Result<serde_json::Value> {
    let request_id = request_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must not be empty");
    let identity = resolve_home_identity(Some(home))
        .context("loading operator identity for enrollment decision")?;
    let mut url = reqwest::Url::parse(graphql).context("parsing runtime GraphQL endpoint")?;
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
        .context("signing enrollment operator command")?;

    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(url)
        .json(&command)
        .send()
        .await
        .context("submitting signed enrollment decision to live runtime")?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("decoding enrollment decision response")?;
    if !status.is_success() {
        return Err(EnrollmentDecisionRejected {
            status,
            message: body
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
            code: body
                .get("code")
                .cloned()
                .and_then(|code| serde_json::from_value(code).ok()),
        }
        .into());
    }
    Ok(body)
}

/// The runtime refused a signed enrollment decision; `code` says why when the
/// projection refused it.
#[derive(Debug, thiserror::Error)]
#[error("runtime rejected enrollment decision ({status}): {message}")]
pub struct EnrollmentDecisionRejected {
    pub status: reqwest::StatusCode,
    pub message: String,
    pub code: Option<gents_protocol::enrollment::EnrollmentDecisionRefusalCode>,
}

/// Pending enrollment discovery uses a bounded wait on the live runtime.
pub(super) async fn pending_enrollments(args: P2pAccessArgs) -> Result<()> {
    let home = resolve_home_dir(args.home.as_deref());
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let body = load_pending_enrollments(&home, graphql.url()).await?;
    print_json(&body)
}

pub(crate) async fn load_pending_enrollments(
    home: &std::path::Path,
    graphql: &str,
) -> Result<serde_json::Value> {
    load_enrollment_query(home, graphql, EnrollmentOperatorQuery::Pending).await
}

pub(crate) async fn load_enrollment_query(
    home: &std::path::Path,
    graphql: &str,
    query: EnrollmentOperatorQuery,
) -> Result<serde_json::Value> {
    let identity = resolve_home_identity(Some(home))
        .context("loading operator identity for enrollment query")?;
    let mut url = reqwest::Url::parse(graphql).context("parsing runtime GraphQL endpoint")?;
    url.set_path("/enrollment/pending");
    url.set_query(None);
    url.set_fragment(None);
    let mut command = EnrollmentOperatorQueryCommand {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        query,
        admin_did: identity.did().to_string(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        nonce: Uuid::new_v4().simple().to_string(),
        admin_sig: Vec::new(),
    };
    command.admin_sig = identity
        .sign(&command.signing_payload())
        .await
        .context("signing enrollment query")?;
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(url)
        .json(&command)
        .send()
        .await
        .context("loading enrollment query from live runtime")?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("decoding enrollment query response")?;
    anyhow::ensure!(
        status.is_success(),
        "runtime rejected enrollment query ({status}): {}",
        body.get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
    );
    Ok(body)
}

/// This explicit grant is separate from route admission. Each native operation
/// commits independently; a failure reports which grants may already exist.
pub(super) async fn grant_pending_client(args: P2pEnrollmentGrantArgs) -> Result<()> {
    let endpoint = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let home = resolve_home_dir(args.home.as_deref());
    let result = grant_verified_client(&home, endpoint, &args.request_id, None)
        .await?
        .context("expected one verified pending enrollment")?;
    print_json(&result)
}

/// `expected` binds managed pairing to the target runtime and held desktop User.
pub(crate) async fn grant_verified_client(
    home: &std::path::Path,
    endpoint: gents::store::config_client::GraphqlEndpoint,
    request_id: &str,
    expected: Option<(&str, &str)>,
) -> Result<Option<serde_json::Value>> {
    use gents::store::config_client::ConfigAccess;
    let request_id = request_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must not be empty");
    let runtime = resolve_home_identity(Some(home))?;
    if let Some((node_did, _)) = expected {
        anyhow::ensure!(
            runtime.did() == node_did,
            "managed Node identity does not match the initialized runtime"
        );
    }
    let pending = load_pending_enrollments(home, endpoint.url()).await?;
    let matches: Vec<_> = pending["pending"]
        .as_array()
        .context("invalid pending enrollment response")?
        .iter()
        .filter(|row| row["request_id"].as_str() == Some(request_id))
        .collect();
    if matches.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        matches.len() == 1,
        "expected one verified pending enrollment for {request_id}"
    );
    let request: gents_protocol::enrollment::EnrollmentRequestRecord =
        serde_json::from_value(matches[0]["signed_request"].clone())
            .context("pending enrollment has no signed request")?;
    let offer = gents_protocol::enrollment::decode_offer(&request.offer_token)?;
    request.validate_against_offer(&offer)?;
    anyhow::ensure!(
        request.request_id == request_id,
        "signed enrollment request does not match the requested ID"
    );
    anyhow::ensure!(
        offer.admin_did == runtime.did(),
        "enrollment offer has a different administrator"
    );
    anyhow::ensure!(
        request.profile == "client",
        "only client enrollment can receive Node administration"
    );
    anyhow::ensure!(
        runtime
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?,
        "enrollment offer signature is invalid"
    );
    anyhow::ensure!(
        runtime
            .verify(
                &request.candidate_did,
                &request.signing_payload(),
                &request.candidate_sig
            )
            .await?,
        "enrollment candidate signature is invalid"
    );
    let now = Utc::now();
    anyhow::ensure!(
        chrono::DateTime::parse_from_rfc3339(&request.issued_at)? <= now
            && now <= chrono::DateTime::parse_from_rfc3339(&request.expires_at)?,
        "enrollment request is not current"
    );
    anyhow::ensure!(
        request.owner_node_did == runtime.did(),
        "enrollment belongs to another runtime"
    );
    if let Some((node_did, client_did)) = expected {
        anyhow::ensure!(
            request.owner_node_did == node_did && request.candidate_did == client_did,
            "managed enrollment request does not match the selected Node and desktop User"
        );
    }
    let candidate = identity::Did::new(&request.candidate_did)?;
    let owner = ConfigAccess::Graphql(endpoint.clone());
    let query = format!(
        "{{ Node(filter: {{ node_did: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
        gents::store::graphql::escape_graphql_string(runtime.did())
    );
    let response = owner
        .execute(&query)
        .await
        .context("reading Node as the client owner")?;
    let rows = response["data"]["Node"]
        .as_array()
        .context("Node query returned no rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "expected one Node visible to the client owner"
    );
    let doc_id = rows[0]["_docID"]
        .as_str()
        .context("Node has no document ID")?;
    owner
        .add_actor_relationship("Node", doc_id, "administrator", &candidate)
        .await
        .context("granting Node administrator as the client owner; the outcome may be unknown, so inspect the relationship before retrying")?;
    let runtime_access = ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(endpoint.url(), runtime.did())
            .with_client_signing_capability(runtime.client_signing_capability()?)?,
    );
    gents::store::identity::GrantSet::OperatorUser
        .grant(&runtime_access, &candidate)
        .await
        .context("Node administrator was granted, but its operator node permissions were not")?;
    Ok(Some(
        serde_json::json!({"request_id": request_id, "candidate_did": candidate.as_str(), "node_did": runtime.did(), "node_administrator_granted": true}),
    ))
}
