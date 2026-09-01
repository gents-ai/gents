use anyhow::{Context, Result};
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

fn resolve_home_identity(
    home: Option<&std::path::Path>,
) -> Result<std::sync::Arc<dyn gents::AgentIdentity>> {
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
    let identity = resolve_home_identity(args.home.as_deref())
        .context("loading operator identity for enrollment decision")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let mut url = reqwest::Url::parse(&graphql).context("parsing runtime GraphQL endpoint")?;
    url.set_path("/enrollment/decisions");
    url.set_query(None);
    url.set_fragment(None);

    let mut command = EnrollmentOperatorDecisionCommand {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        lease_seconds: if action == EnrollmentOperatorAction::Approve {
            args.lease_seconds
        } else {
            0
        },
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
    anyhow::ensure!(
        status.is_success(),
        "runtime rejected enrollment decision ({status}): {}",
        body.get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
    );
    print_json(&body)
}

pub(super) async fn pending_enrollments(args: P2pAccessArgs) -> Result<()> {
    let identity = resolve_home_identity(args.home.as_deref())
        .context("loading operator identity for pending enrollments")?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let mut url = reqwest::Url::parse(&graphql).context("parsing runtime GraphQL endpoint")?;
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
        .context("signing pending enrollment query")?;
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(url)
        .json(&command)
        .send()
        .await
        .context("loading pending enrollment requests from live runtime")?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("decoding pending enrollment response")?;
    anyhow::ensure!(
        status.is_success(),
        "runtime rejected enrollment pending query ({status}): {}",
        body.get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
    );
    print_json(&body)
}
