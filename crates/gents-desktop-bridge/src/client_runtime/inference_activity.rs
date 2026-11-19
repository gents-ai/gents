use anyhow::Context;
use gents_inference::inference_observation::{InferenceKey, InferenceSnapshot};

impl crate::client_runtime::ClientRuntime {
    pub async fn read_inference_activity(
        &self,
        key: &InferenceKey,
    ) -> anyhow::Result<Option<crate::client_runtime::InferenceActivity>> {
        let target = crate::client_runtime::InferenceTarget {
            node: key.node.clone(),
            session: key.session.clone(),
            request: key.request.clone(),
            generation: key.generation.clone(),
        };
        let epoch = self
            .begin_inference_observation(&target)
            .context("inference observation capacity reached")?;
        let started = std::time::Instant::now();
        let endpoint = self
            .operator_observation_graphql(&key.node)
            .context("runtime connection is unavailable")?;
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
            gents_inference::inference_observation::INFERENCE_ACTIVITY_PATH
        ));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2))
            .build()?;
        let mut response = endpoint
            .send_resolving(|endpoint| endpoint.authorize(client.post(url.clone()).json(key)))
            .await?
            .map_err(|error| {
                let message = error.to_string();
                gents_store::client_error::BridgeError::runtime_http_error(error, message)
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            if let Some(update) =
                gents_store::client_error::BridgeError::home_refusal(status, &body)
            {
                return Err(update.into());
            }
            return Err(gents_store::client_error::BridgeError::new(
                gents_store::client_error::BridgeErrorCode::PermissionDenied,
                "Access was denied. Check this client's permissions with the runtime owner.",
            )
            .into());
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
            gents_store::client_error::BridgeError::runtime_http_error(error, message)
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
            gents_inference::inference_observation::InferenceActivity::AwaitingProvider => {
                crate::client_runtime::InferenceActivity::AwaitingProvider
            }
            gents_inference::inference_observation::InferenceActivity::Working => {
                crate::client_runtime::InferenceActivity::Working
            }
            gents_inference::inference_observation::InferenceActivity::Waiting => {
                crate::client_runtime::InferenceActivity::Waiting
            }
            gents_inference::inference_observation::InferenceActivity::Retrying => {
                crate::client_runtime::InferenceActivity::Retrying
            }
        };
        let observation = crate::client_runtime::InferenceObservation {
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
