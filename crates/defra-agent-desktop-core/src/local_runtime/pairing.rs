use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::time::{sleep, Instant};

use super::http::{http_post_json, p2p_api_base};

const PAIRING_RETRY_TIMEOUT: Duration = Duration::from_secs(20);
const PAIRING_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn complete_runtime_pairing(
    graphql: &str,
    desktop_listen_address: &str,
    collections: Vec<String>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("building local runtime pairing HTTP client")?;
    let api_base = p2p_api_base(graphql)?;
    let connect_addrs = vec![desktop_listen_address.to_string()];
    let replicator = P2pReplicatorRequest {
        addresses: vec![desktop_listen_address.to_string()],
        collections: collections.clone(),
    };
    let deadline = Instant::now() + PAIRING_RETRY_TIMEOUT;

    loop {
        let result = async {
            http_post_json(&client, &format!("{api_base}/p2p/connect"), &connect_addrs).await?;
            http_post_json(
                &client,
                &format!("{api_base}/p2p/collections"),
                &collections,
            )
            .await?;
            http_post_json(&client, &format!("{api_base}/p2p/replicators"), &replicator).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error).with_context(|| {
                        format!(
                            "timed out pairing desktop listen address {} with local runtime {}",
                            desktop_listen_address, graphql
                        )
                    });
                }
                sleep(PAIRING_RETRY_BACKOFF).await;
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct P2pReplicatorRequest {
    #[serde(rename = "Collections")]
    pub(crate) collections: Vec<String>,
    #[serde(rename = "Addresses")]
    pub(crate) addresses: Vec<String>,
}
