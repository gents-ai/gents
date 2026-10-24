use serde_json::Value;

use crate::cli::args::{FleetCommand, FleetSlotsArgs};
use crate::{http_get_json, print_json, resolve_graphql_endpoint};

#[derive(Debug, thiserror::Error)]
pub(crate) enum FleetError {
    #[error(transparent)]
    Cli(#[from] crate::CliError),
    #[error(transparent)]
    HomeState(#[from] crate::home_state::HomeStateError),
    #[error(transparent)]
    RequestHelpers(#[from] crate::request_helpers::RequestHelpersError),
    #[error("building HTTP client")]
    HttpClient(#[source] reqwest::Error),
    #[error("parsing GraphQL endpoint URL")]
    ParseEndpoint(#[source] url::ParseError),
}

type Result<T, E = FleetError> = std::result::Result<T, E>;

pub(crate) async fn dispatch(command: FleetCommand) -> Result<()> {
    match command {
        FleetCommand::Slots(args) => fleet_slots(args).await,
    }
}

async fn fleet_slots(args: FleetSlotsArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let url = runtime_fleet_slots_url(graphql.url())?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(FleetError::HttpClient)?;
    let snapshot: Value = http_get_json(client.get(&url)).await?;
    print_json(&snapshot)?;
    Ok(())
}

fn runtime_fleet_slots_url(graphql: &str) -> Result<String> {
    let mut url = url::Url::parse(graphql).map_err(FleetError::ParseEndpoint)?;
    url.set_path("/fleet/slots");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}
