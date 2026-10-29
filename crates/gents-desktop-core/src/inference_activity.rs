use anyhow::Context;
use gents::inference_observation::{InferenceKey, InferenceSnapshot};

impl crate::client::ClientCore {
    pub async fn read_inference_activity(
        &self,
        key: &InferenceKey,
    ) -> anyhow::Result<Option<crate::client::InferenceActivity>> {
        let target = crate::client::InferenceTarget {
            node: key.node.clone(),
            session: key.session.clone(),
            request: key.request.clone(),
            generation: key.generation.clone(),
        };
        let epoch = self
            .begin_inference_observation(&target)
            .context("inference observation capacity reached")?;
        let started = std::time::Instant::now();
        let records = self.peer_records().await;
        let mut matches = records.iter().filter(|record| record.node_did == key.node);
        let record = matches
            .next()
            .filter(|_| matches.next().is_none())
            .context("runtime connection is unavailable")?;
        let operator = self
            .operator_observation_endpoint(&key.node)
            .await
            .filter(|endpoint| !endpoint.is_anonymous());
        let endpoint = if let Some(operator) = operator {
            operator
        } else if record.is_enrollment() {
            let Some(graphql) = record.graphql.as_deref() else {
                return Ok(None);
            };
            gents::config_client::GraphqlEndpoint::as_principal(graphql, self.node_identity().did())
        } else {
            return Ok(None);
        };
        let graphql = endpoint.url();
        let mut url = reqwest::Url::parse(graphql)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid runtime endpoint"
        );
        let base = url
            .path()
            .strip_suffix("/api/v0/graphql")
            .context("invalid runtime GraphQL endpoint")?;
        url.set_path(&format!(
            "{base}{}",
            gents::inference_observation::INFERENCE_ACTIVITY_PATH
        ));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2))
            .build()?;
        let mut response = endpoint
            .authorize(client.post(url).json(key))?
            .send()
            .await
            .map_err(|error| {
                let message = error.to_string();
                crate::error::BridgeError::runtime_http_error(error, message)
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        anyhow::ensure!(
            response.status().is_success(),
            "runtime inference activity is unavailable"
        );
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= 8192),
            "runtime inference activity is too large"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            let message = error.to_string();
            crate::error::BridgeError::runtime_http_error(error, message)
        })? {
            anyhow::ensure!(
                body.len().saturating_add(chunk.len()) <= 8192,
                "runtime inference activity is too large"
            );
            body.extend_from_slice(&chunk);
        }
        let snapshot: InferenceSnapshot = serde_json::from_slice(&body)?;
        anyhow::ensure!(
            snapshot.node == key.node
                && snapshot.session == key.session
                && snapshot.request == key.request
                && snapshot.generation == key.generation,
            "runtime inference activity has a different target"
        );
        let activity = match snapshot.activity {
            gents::inference_observation::InferenceActivity::AwaitingProvider => {
                crate::client::InferenceActivity::AwaitingProvider
            }
            gents::inference_observation::InferenceActivity::Working => {
                crate::client::InferenceActivity::Working
            }
            gents::inference_observation::InferenceActivity::Waiting => {
                crate::client::InferenceActivity::Waiting
            }
            gents::inference_observation::InferenceActivity::Retrying => {
                crate::client::InferenceActivity::Retrying
            }
        };
        let observation = crate::client::InferenceObservation {
            target,
            turn: snapshot.turn as u64,
            attempt: u64::from(snapshot.attempt),
            sequence: snapshot.sequence,
            observed_at: 0,
            activity,
        };
        Ok(self.finish_inference_observation(
            epoch,
            observation,
            started.elapsed().as_millis() as u64,
        ))
    }
}
