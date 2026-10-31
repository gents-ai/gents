use gents::store::config_client::GraphqlEndpoint;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::manifest::FanInExpectation;
use super::results::graphql_rows;
use gents::store::graphql::{escape_graphql_string, validate_collection_identifier};

#[derive(Debug, Clone)]
pub(super) struct FanInEvidence {
    pub(super) correlation: String,
    pub(super) expected_count: usize,
    pub(super) member_count: usize,
    pub(super) result_count: usize,
    pub(super) consumer_request_id: String,
    pub(super) report_count: usize,
    pub(super) candidate_count: Option<usize>,
    pub(super) confirmed_count: Option<usize>,
    pub(super) refuted_count: Option<usize>,
    pub(super) decision_count: Option<usize>,
    pub(super) verification_summary_count: Option<usize>,
    pub(super) final_consumer_request_id: Option<String>,
}

pub(super) async fn verify_fan_in(
    graphql: &GraphqlEndpoint,
    expected: &FanInExpectation,
    correlation: &str,
    node_did: &str,
) -> Result<Option<FanInEvidence>> {
    for collection in [
        &expected.member_collection,
        &expected.result_collection,
        &expected.report_collection,
    ] {
        validate_collection_identifier(collection)?;
    }
    for field in [&expected.correlation_field, &expected.expected_count_field] {
        gents::store::graphql::validate_graphql_name(field)?;
    }
    for field in &expected.member_required_fields {
        gents::store::graphql::validate_graphql_name(field)?;
    }
    if let Some(verification) = &expected.verification {
        for collection in [
            &verification.candidate_collection,
            &verification.decision_collection,
            &verification.summary_collection,
            &verification.confirmed_collection,
        ] {
            validate_collection_identifier(collection)?;
        }
        for field in [
            &verification.finding_id_field,
            &verification.verdict_field,
            &verification.evidence_field,
            &verification.confirmed_count_field,
            &verification.refuted_count_field,
        ] {
            gents::store::graphql::validate_graphql_name(field)?;
        }
    }

    let escaped_correlation = escape_graphql_string(correlation);
    let required_member_projection = expected.member_required_fields.join(" ");
    let load_members = |collection: &str, required_projection: &str| {
        format!(
            r#"{{ {collection}(filter: {{ {correlation_field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID {correlation_field} {expected_count_field} {required_projection} }} }}"#,
            correlation_field = expected.correlation_field,
            expected_count_field = expected.expected_count_field,
        )
    };
    let member_rows = graphql_rows(
        graphql,
        &expected.member_collection,
        &load_members(&expected.member_collection, &required_member_projection),
    )
    .await?;
    if member_rows.is_empty() {
        bail!("fan-in produced no {} rows", expected.member_collection);
    }
    let expected_count = member_rows.len();
    if expected
        .min_expected_count
        .is_some_and(|minimum| expected_count < minimum)
    {
        bail!(
            "fan-in chose {expected_count} members, below minimum {}",
            expected
                .min_expected_count
                .expect("guard establishes minimum")
        );
    }
    if expected
        .max_expected_count
        .is_some_and(|maximum| expected_count > maximum)
    {
        bail!(
            "fan-in chose {expected_count} members, above maximum {}",
            expected
                .max_expected_count
                .expect("guard establishes maximum")
        );
    }
    for row in &member_rows {
        let count = row
            .get(&expected.expected_count_field)
            .and_then(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .or_else(|| value.as_str()?.parse::<usize>().ok())
            })
            .context("fan-in member has no valid expected count")?;
        if count != expected_count {
            bail!(
                "fan-in {} rows disagree with closed-set count: row says {}, actual {}",
                expected.member_collection,
                count,
                expected_count
            );
        }
        for field in &expected.member_required_fields {
            if !row
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
            {
                bail!("fan-in member has no non-empty required field {field}");
            }
        }
    }

    let result_rows = graphql_rows(
        graphql,
        &expected.result_collection,
        &load_members(&expected.result_collection, ""),
    )
    .await?;
    if result_rows.len() != expected_count {
        bail!(
            "fan-in expected {} correlated {} rows, found {}",
            expected_count,
            expected.result_collection,
            result_rows.len()
        );
    }
    for row in &result_rows {
        let count = row
            .get(&expected.expected_count_field)
            .and_then(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .or_else(|| value.as_str()?.parse::<usize>().ok())
            })
            .context("fan-in result has no valid expected count")?;
        if count != expected_count {
            bail!("fan-in result cardinality snapshot drifted");
        }
    }

    let request_query = format!(
        r#"{{ AgentRequest(filter: {{
            node_did: {{ _eq: "{}" }},
            caused_by_trigger_id: {{ _eq: "{}" }},
            caused_by_trigger_kind: {{ _eq: "event" }},
            caused_by_correlation: {{ _eq: "{}" }}
        }}) {{ request_id }} }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(&expected.consumer_trigger_id),
        escaped_correlation,
    );
    let request_rows = graphql_rows(graphql, "AgentRequest", &request_query).await?;
    if request_rows.len() != 1 {
        bail!(
            "fan-in consumer {} expected exactly one correlated AgentRequest, found {}",
            expected.consumer_trigger_id,
            request_rows.len()
        );
    }
    let consumer_request_id = request_rows[0]
        .get("request_id")
        .and_then(Value::as_str)
        .context("fan-in consumer request has no request_id")?
        .to_string();

    let report_projection = expected.verification.as_ref().map_or_else(
        || "_docID".to_string(),
        |verification| {
            format!(
                "_docID {} {}",
                verification.confirmed_count_field, verification.refuted_count_field
            )
        },
    );
    let report_query = format!(
        r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{escaped_correlation}" }} }}) {{ {report_projection} }} }}"#,
        collection = expected.report_collection,
        field = expected.correlation_field,
    );
    let report_rows = graphql_rows(graphql, &expected.report_collection, &report_query).await?;
    if report_rows.len() != 1 {
        bail!(
            "fan-in expected exactly one correlated {}, found {}",
            expected.report_collection,
            report_rows.len()
        );
    }

    let mut candidate_count = None;
    let mut confirmed_count = None;
    let mut refuted_count = None;
    let mut decision_count = None;
    let mut verification_summary_count = None;
    let mut final_consumer_request_id = None;
    if let Some(verification) = &expected.verification {
        let candidate_query = format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID {finding_id} }} }}"#,
            collection = verification.candidate_collection,
            field = expected.correlation_field,
            finding_id = verification.finding_id_field,
        );
        let candidates = graphql_rows(
            graphql,
            &verification.candidate_collection,
            &candidate_query,
        )
        .await?;
        let decision_query = format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID {finding_id} {verdict} {evidence} }} }}"#,
            collection = verification.decision_collection,
            field = expected.correlation_field,
            finding_id = verification.finding_id_field,
            verdict = verification.verdict_field,
            evidence = verification.evidence_field,
        );
        let decisions =
            graphql_rows(graphql, &verification.decision_collection, &decision_query).await?;
        let candidate_ids = candidates
            .iter()
            .map(|row| {
                row.get(&verification.finding_id_field)
                    .and_then(Value::as_str)
                    .context("candidate finding has no finding id")
            })
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        let decision_ids = decisions
            .iter()
            .map(|row| {
                row.get(&verification.finding_id_field)
                    .and_then(Value::as_str)
                    .context("verification decision has no finding id")
            })
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        if candidate_ids.len() != candidates.len() {
            bail!("fan-in candidate finding ids are not unique");
        }
        if decision_ids.len() != decisions.len() {
            bail!("fan-in verification decision ids are not unique");
        }
        if candidate_ids != decision_ids {
            bail!("fan-in verification decisions do not cover the exact candidate set");
        }
        let mut verified_confirmed = 0usize;
        let mut verified_refuted = 0usize;
        for row in &decisions {
            match row.get(&verification.verdict_field).and_then(Value::as_str) {
                Some("confirmed") => verified_confirmed += 1,
                Some("refuted") => verified_refuted += 1,
                verdict => bail!("verification decision has invalid verdict {verdict:?}"),
            }
            if !row
                .get(&verification.evidence_field)
                .and_then(Value::as_str)
                .is_some_and(|evidence| !evidence.trim().is_empty())
            {
                bail!("verification decision has no fresh evidence");
            }
        }
        let summary_query = format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID {confirmed_count} {refuted_count} }} }}"#,
            collection = verification.summary_collection,
            field = expected.correlation_field,
            confirmed_count = verification.confirmed_count_field,
            refuted_count = verification.refuted_count_field,
        );
        let summaries =
            graphql_rows(graphql, &verification.summary_collection, &summary_query).await?;
        if summaries.len() != 1 {
            bail!(
                "fan-in expected exactly one correlated {}, found {}",
                verification.summary_collection,
                summaries.len()
            );
        }
        let parse_count = |row: &Value, field: &str| -> Result<usize> {
            row.get(field)
                .and_then(|value| {
                    value
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                        .or_else(|| value.as_str()?.parse::<usize>().ok())
                })
                .with_context(|| format!("row has no valid {field}"))
        };
        if parse_count(&summaries[0], &verification.confirmed_count_field)? != verified_confirmed
            || parse_count(&summaries[0], &verification.refuted_count_field)? != verified_refuted
        {
            bail!("verification summary does not match the durable decision ledger");
        }

        let final_request_query = format!(
            r#"{{ AgentRequest(filter: {{
                node_did: {{ _eq: "{}" }},
                caused_by_trigger_id: {{ _eq: "{}" }},
                caused_by_trigger_kind: {{ _eq: "event" }},
                caused_by_correlation: {{ _eq: "{}" }}
            }}) {{ request_id }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(&verification.final_consumer_trigger_id),
            escaped_correlation,
        );
        let final_requests = graphql_rows(graphql, "AgentRequest", &final_request_query).await?;
        if final_requests.len() != 1 {
            bail!(
                "final consumer {} expected exactly one correlated AgentRequest, found {}",
                verification.final_consumer_trigger_id,
                final_requests.len()
            );
        }
        final_consumer_request_id = Some(
            final_requests[0]
                .get("request_id")
                .and_then(Value::as_str)
                .context("final consumer request has no request_id")?
                .to_string(),
        );
        let confirmed_query = format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID {verdict} {evidence} }} }}"#,
            collection = verification.confirmed_collection,
            field = expected.correlation_field,
            verdict = verification.verdict_field,
            evidence = verification.evidence_field,
        );
        let confirmed = graphql_rows(
            graphql,
            &verification.confirmed_collection,
            &confirmed_query,
        )
        .await?;
        for row in &confirmed {
            if row.get(&verification.verdict_field).and_then(Value::as_str) != Some("confirmed") {
                bail!(
                    "fan-in promoted {} row has a verdict other than confirmed",
                    verification.confirmed_collection
                );
            }
            if !row
                .get(&verification.evidence_field)
                .and_then(Value::as_str)
                .is_some_and(|evidence| !evidence.trim().is_empty())
            {
                bail!(
                    "fan-in promoted {} row has no verification evidence",
                    verification.confirmed_collection
                );
            }
        }
        let parse_report_count = |field: &str| -> Result<usize> {
            report_rows[0]
                .get(field)
                .and_then(|value| {
                    value
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                        .or_else(|| value.as_str()?.parse::<usize>().ok())
                })
                .with_context(|| format!("fan-in report has no valid {field}"))
        };
        let reported_confirmed = parse_report_count(&verification.confirmed_count_field)?;
        let reported_refuted = parse_report_count(&verification.refuted_count_field)?;
        if reported_confirmed != confirmed.len() {
            bail!(
                "fan-in report says {} confirmed findings, found {}",
                reported_confirmed,
                confirmed.len()
            );
        }
        if reported_confirmed != verified_confirmed || reported_refuted != verified_refuted {
            bail!("final report counts do not match the verification decision ledger");
        }
        if reported_confirmed + reported_refuted != candidates.len() {
            bail!(
                "fan-in verification ledger is unbalanced: {} confirmed + {} refuted != {} candidates",
                reported_confirmed,
                reported_refuted,
                candidates.len()
            );
        }
        candidate_count = Some(candidates.len());
        confirmed_count = Some(reported_confirmed);
        refuted_count = Some(reported_refuted);
        decision_count = Some(decisions.len());
        verification_summary_count = Some(summaries.len());
    }

    Ok(Some(FanInEvidence {
        correlation: correlation.to_string(),
        expected_count,
        member_count: member_rows.len(),
        result_count: result_rows.len(),
        consumer_request_id,
        report_count: report_rows.len(),
        candidate_count,
        confirmed_count,
        refuted_count,
        decision_count,
        verification_summary_count,
        final_consumer_request_id,
    }))
}
