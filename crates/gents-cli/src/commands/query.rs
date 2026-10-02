use anyhow::{Context, Result};
use gents::config_client::GraphqlEndpoint;
use gents::defra_query::{CollectionScope, DefraQueryParams};
use serde_json::{json, Value};

use crate::cli::args::QueryArgs;
use crate::print_json;

pub(crate) async fn run_defra_query(
    graphql: &GraphqlEndpoint,
    params: &DefraQueryParams,
    scope: &CollectionScope,
) -> Result<Value> {
    gents::defra_query::execute_command(
        &gents::config_client::ConfigAccess::Graphql(graphql.clone()),
        &params.clone().into(),
        scope,
    )
    .await
}

pub(crate) fn params_from_args(args: &QueryArgs) -> Result<(DefraQueryParams, CollectionScope)> {
    let filter = match args
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(serde_json::from_str::<Value>(raw).context("parsing --filter as JSON")?),
        None => None,
    };
    let params = DefraQueryParams {
        collection: args.collection.clone(),
        filter,
        fields: args.fields.clone(),
        limit: args.limit,
    };
    let scope = if args.allow_collections.is_empty() {
        CollectionScope::all()
    } else {
        CollectionScope::restricted(args.allow_collections.clone())
    };
    Ok((params, scope))
}

pub(crate) async fn query(args: QueryArgs) -> Result<()> {
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let (params, scope) = params_from_args(&args)?;
    let mut command: gents::defra_query::QueryParams = params.into();
    if let Some(verb) = args.verb {
        command.argv = vec![verb.clone()];
        if verb == "fields" || verb == "count" {
            command.options.remove("fields");
        }
        if verb == "help" {
            command.collection = None;
            command.options.clear();
        }
    }
    if let Some(mode) = args.mode {
        command.options.insert("mode".into(), json!(mode));
    }
    if let Some(order) = args.order {
        command.options.insert(
            "order".into(),
            serde_json::from_str(&order).context("parsing --order as JSON")?,
        );
    }
    if let Some(offset) = args.offset {
        command.options.insert("offset".into(), json!(offset));
    }
    let output = gents::defra_query::execute_command(&*access, &command, &scope).await?;
    print_json(&output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use gents_protocol::schemas::EVAL_VERDICT_NAME;

    use super::*;

    /// `gents query` with no `--allow-collection` is the widest scope the CLI
    /// offers, and it is the one an operator reaches for by default. A protected
    /// collection must still be refused there, before any request leaves the
    /// process — the endpoint below is never listening, so a refusal that named a
    /// transport failure would not name the collection.
    fn protected_args(fields: Vec<String>) -> QueryArgs {
        QueryArgs {
            verb: None,
            mode: None,
            order: None,
            offset: None,
            home: None,
            graphql: Some("http://127.0.0.1:1/api/v0/graphql".into()),
            collection: EVAL_VERDICT_NAME.into(),
            fields,
            filter: None,
            limit: None,
            allow_collections: Vec::new(),
        }
    }

    #[tokio::test]
    async fn refuses_a_protected_collection_under_the_default_scope() {
        let args = protected_args(vec!["verdict_id".into()]);
        let (params, scope) = params_from_args(&args).expect("args parse");
        assert!(scope.is_unrestricted(), "no --allow-collection means all");

        let error = run_defra_query(
            &gents::config_client::GraphqlEndpoint::anonymous("http://127.0.0.1:1/api/v0/graphql"),
            &params,
            &scope,
        )
        .await
        .expect_err("EvalVerdict must never be readable through `gents query`");
        let message = format!("{error:#}");
        assert!(message.contains(EVAL_VERDICT_NAME), "{message}");
        assert!(message.contains("protected"), "{message}");
    }

    #[tokio::test]
    async fn refuses_discovery_of_a_protected_collection() {
        let args = protected_args(vec!["*".into()]);
        let (params, scope) = params_from_args(&args).expect("args parse");
        assert!(params.is_discovery(), "a lone `*` is the discovery request");

        let error = run_defra_query(
            &gents::config_client::GraphqlEndpoint::anonymous("http://127.0.0.1:1/api/v0/graphql"),
            &params,
            &scope,
        )
        .await
        .expect_err("a protected collection's field inventory must stay unlisted");
        let message = format!("{error:#}");
        assert!(message.contains(EVAL_VERDICT_NAME), "{message}");
        assert!(message.contains("protected"), "{message}");
    }
}
