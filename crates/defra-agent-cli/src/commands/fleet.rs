use anyhow::{Context, Result};
use serde_json::Value;

use crate::cli::args::{FleetCommand, FleetSlotsArgs};
use crate::{http_get_json, print_json, resolve_graphql_endpoint};

pub(crate) async fn dispatch(command: FleetCommand) -> Result<()> {
    match command {
        FleetCommand::Slots(args) => fleet_slots(args).await,
    }
}

async fn fleet_slots(args: FleetSlotsArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let url = runtime_fleet_slots_url(&graphql)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("building HTTP client")?;
    let snapshot: Value = http_get_json(&client, &url).await?;
    print_json(&snapshot)?;
    Ok(())
}

fn runtime_fleet_slots_url(graphql: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(graphql).context("parsing GraphQL endpoint URL")?;
    url.set_path("/fleet/slots");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_slots_url_uses_runtime_http_root() {
        assert_eq!(
            runtime_fleet_slots_url("http://127.0.0.1:9191/api/v0/graphql").unwrap(),
            "http://127.0.0.1:9191/fleet/slots"
        );
    }
}
