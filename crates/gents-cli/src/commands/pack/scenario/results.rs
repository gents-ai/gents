use gents::store::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::super::cli_process::{path_arg, run_cli_json};
use super::manifest::ResultDocumentExpectation;
use super::stages::StageResult;
use crate::graphql_access::post_graphql;
use gents::store::graphql::escape_graphql_string;

pub(super) async fn graphql_rows(
    graphql: &GraphqlEndpoint,
    field: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response = post_graphql(graphql, query).await?;
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            bail!("GraphQL {field} query failed: {errors:?}");
        }
    }
    response
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("GraphQL {field} query returned no row array"))
}

pub(super) async fn load_result_documents(
    graphql: &GraphqlEndpoint,
    expected: &[ResultDocumentExpectation],
    correlation: &str,
) -> Result<BTreeMap<String, Vec<Value>>> {
    let mut documents = BTreeMap::new();
    let correlation = escape_graphql_string(correlation);
    for result in expected {
        let projection = result.fields.join(" ");
        let query = format!(
            r#"{{ {collection}(filter: {{ {correlation_field}: {{ _eq: "{correlation}" }} }}) {{ _docID {projection} }} }}"#,
            collection = result.collection,
            correlation_field = result.correlation_field,
        );
        let mut rows = graphql_rows(graphql, &result.collection, &query).await?;
        rows.sort_by_key(Value::to_string);
        documents.insert(result.collection.clone(), rows);
    }
    Ok(documents)
}

pub(super) async fn render_projection_artifacts(
    bin: &Path,
    home: &Path,
    graphql: &GraphqlEndpoint,
    run_dir: &Path,
    stages: &[StageResult],
    projections: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let artifact_dir = run_dir.join("projections");
    std::fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("creating projection directory {}", artifact_dir.display()))?;
    let mut artifacts = BTreeMap::new();
    for stage in stages {
        let timeline_args = vec![
            "trace".to_string(),
            "timeline".to_string(),
            "--home".to_string(),
            path_arg(home),
            "--graphql".to_string(),
            graphql.to_string(),
            "--request-id".to_string(),
            stage.request_id.clone(),
        ];
        let timeline = run_cli_json(bin, &timeline_args)
            .await
            .with_context(|| format!("projecting timeline for request {}", stage.request_id))?;
        let timeline_path = artifact_dir.join(format!("{}-timeline.json", stage.request_id));
        std::fs::write(&timeline_path, serde_json::to_vec_pretty(&timeline)?)
            .with_context(|| format!("writing {}", timeline_path.display()))?;

        let mut request_artifacts =
            BTreeMap::from([("timeline".to_string(), path_arg(&timeline_path))]);
        for projection in projections {
            let project_args = vec![
                "trace".to_string(),
                "project".to_string(),
                "--home".to_string(),
                path_arg(home),
                "--graphql".to_string(),
                graphql.to_string(),
                "--request-id".to_string(),
                stage.request_id.clone(),
                "--projection".to_string(),
                projection.clone(),
            ];
            let projected = run_cli_json(bin, &project_args).await.with_context(|| {
                format!(
                    "rendering {projection} projection for request {}",
                    stage.request_id
                )
            })?;
            let projection_path = artifact_dir.join(format!(
                "{}-{}.json",
                stage.request_id,
                projection.replace('_', "-")
            ));
            std::fs::write(&projection_path, serde_json::to_vec_pretty(&projected)?)
                .with_context(|| format!("writing {}", projection_path.display()))?;
            request_artifacts.insert(projection.clone(), path_arg(&projection_path));
        }
        artifacts.insert(stage.request_id.clone(), request_artifacts);
    }
    Ok(artifacts)
}

pub(super) async fn count_rows(graphql: &GraphqlEndpoint, collection: &str) -> u64 {
    let query = format!("{{ {collection} {{ _docID }} }}");
    post_graphql(graphql, &query)
        .await
        .ok()
        .and_then(|resp| {
            resp.pointer(&format!("/data/{collection}"))
                .and_then(Value::as_array)
                .map(|rows| rows.len() as u64)
        })
        .unwrap_or(0)
}

pub(super) async fn token_totals(graphql: &GraphqlEndpoint) -> (u64, u64) {
    let query = "{ InferenceCall { prompt_tokens completion_tokens } }";
    let Ok(resp) = post_graphql(graphql, query).await else {
        return (0, 0);
    };
    let Some(rows) = resp
        .pointer("/data/InferenceCall")
        .and_then(Value::as_array)
    else {
        return (0, 0);
    };
    rows.iter().fold((0, 0), |(p, c), row| {
        (
            p + row
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            c + row
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
    })
}
