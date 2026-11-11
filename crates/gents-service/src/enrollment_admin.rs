use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQuery,
    EnrollmentOperatorQueryCommand, ENROLLMENT_PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::home_state::{load_initialized_home_identity, read_init_config, resolve_home_dir};

pub fn resolve_home_identity(
    home: Option<&std::path::Path>,
) -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
    let home_dir = resolve_home_dir(home);
    let config = read_init_config(&home_dir)?.with_context(|| {
        format!(
            "no init config found in {}; run `gents init` first",
            home_dir.display()
        )
    })?;
    load_initialized_home_identity(&home_dir, &config)
}

/// A live runtime receives the signed decision; bound waiting on its endpoint
/// so an unavailable server cannot leave the command pending indefinitely.
pub async fn submit_enrollment_decision(
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

pub async fn load_pending_enrollments(
    home: &std::path::Path,
    graphql: &str,
) -> Result<serde_json::Value> {
    load_enrollment_query(home, graphql, EnrollmentOperatorQuery::Pending).await
}

pub async fn load_enrollment_query(
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

/// The signed pending request and the two accesses one approval runs with:
/// the home's User, which ACP checks for the Node `administrator` grant, and
/// the runtime principal, which records the receipt and node permissions.
struct PendingApproval {
    runtime: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    approver: gents_store::config_client::ConfigAccess,
    runtime_access: gents_store::config_client::ConfigAccess,
    request: gents_protocol::enrollment::EnrollmentRequestRecord,
}

impl PendingApproval {
    async fn load(
        home: &std::path::Path,
        endpoint: gents_store::config_client::GraphqlEndpoint,
        request_id: &str,
    ) -> Result<Option<Self>> {
        use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
        let runtime = resolve_home_identity(Some(home))?;
        let pending = load_pending_enrollments(home, endpoint.url()).await?;
        let matches: Vec<_> = pending["pending"]
            .as_array()
            .context("invalid pending enrollment response")?
            .iter()
            .filter(|row| row["request_id"].as_str() == Some(request_id))
            .collect();
        let [row] = matches.as_slice() else {
            anyhow::ensure!(
                matches.is_empty(),
                "expected one verified pending enrollment for {request_id}"
            );
            return Ok(None);
        };
        let request: gents_protocol::enrollment::EnrollmentRequestRecord =
            serde_json::from_value(row["signed_request"].clone())
                .context("pending enrollment has no signed request")?;
        anyhow::ensure!(
            request.request_id == request_id,
            "signed enrollment request does not match the requested ID"
        );
        let runtime_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(endpoint.url(), runtime.did())
                .with_client_signing_capability(runtime.client_signing_capability()?)?,
        );
        Ok(Some(Self {
            runtime,
            approver: ConfigAccess::Graphql(endpoint),
            runtime_access,
            request,
        }))
    }

    fn approval<'a>(
        &'a self,
        expected: Option<(&'a str, &'a str)>,
    ) -> gents_store::identity::EnrollmentApproval<'a> {
        gents_store::identity::EnrollmentApproval {
            approver: &self.approver,
            runtime: &self.runtime_access,
            node: self.runtime.as_ref(),
            request: &self.request,
            expected,
        }
    }
}

fn granted_json(
    request_id: &str,
    candidate: &str,
    node_did: &str,
    outcome: &gents_store::identity::EnrollmentGrantOutcome,
) -> serde_json::Value {
    let recorded: Vec<_> = outcome.recorded.iter().map(|tuple| tuple.name()).collect();
    serde_json::json!({"request_id": request_id, "candidate_did": candidate, "node_did": node_did, "node_administrator_granted": true, "recorded": recorded, "reissued": outcome.reissued})
}

/// Grant a pending client enrollment's Node administration without approving
/// it, to finish a grant whose approval did not commit. `expected` binds
/// managed pairing to the target runtime and held desktop User.
pub async fn grant_verified_client(
    home: &std::path::Path,
    endpoint: gents_store::config_client::GraphqlEndpoint,
    request_id: &str,
    expected: Option<(&str, &str)>,
) -> Result<Option<serde_json::Value>> {
    let request_id = request_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must not be empty");
    let Some(pending) = PendingApproval::load(home, endpoint, request_id).await? else {
        return Ok(None);
    };
    let outcome = gents_store::identity::grant_as_approver(&pending.approval(expected)).await?;
    Ok(Some(granted_json(
        request_id,
        &pending.request.candidate_did,
        pending.runtime.did(),
        &outcome,
    )))
}

/// Approve a pending client enrollment as the home's User through the one
/// approval handler every surface runs (`approve_enrollment`): its Node
/// administration is granted under the User's identity, then the runtime
/// approves the signed operator command. `None` when it is not pending.
pub async fn approve_verified_client(
    home: &std::path::Path,
    endpoint: gents_store::config_client::GraphqlEndpoint,
    request_id: &str,
    lease_seconds: u64,
    expected: Option<(&str, &str)>,
) -> Result<Option<serde_json::Value>> {
    let request_id = request_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must not be empty");
    let graphql = endpoint.url().to_owned();
    let Some(pending) = PendingApproval::load(home, endpoint, request_id).await? else {
        return Ok(None);
    };
    let (outcome, mut decided) =
        gents_store::identity::approve_enrollment(pending.approval(expected), || {
            submit_enrollment_decision(
                home,
                &graphql,
                request_id,
                EnrollmentOperatorAction::Approve,
                lease_seconds,
            )
        })
        .await?;
    decided["granted"] = granted_json(
        request_id,
        &pending.request.candidate_did,
        pending.runtime.did(),
        &outcome,
    );
    Ok(Some(decided))
}
