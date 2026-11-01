//! The Amazon Bedrock runtime pieces of the Messages send path: the model id a
//! request names (the region-prefix rewrite and the account's inference
//! profile), the cached, single-flight `ListInferenceProfiles` call that finds
//! the profile, and the HTTP call that reads them. The event-stream answer becomes Messages
//! SSE bytes in `sse`. The rules themselves are `gents_claude::bedrock`. This
//! module names no Rig type: the transport and the SSE adapter own those.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gents_claude::bedrock as rules;
use gents_claude::headers::HeaderValue;
use gents_claude::sigv4::{escape_uri, SigV4Error};
use gents_loop::claude_messages_body::ClaudeRequestPlan;
use kovan_map::HopscotchMap;
use rapidhash::fast::RandomState;
use tokio::sync::OnceCell;

use super::aws::sign_request;
use super::route::{AwsSigning, BedrockRoute};
use super::transport::{url_parts, ClaudeHttpClient, ClaudeTransportError};

/// How long a listed profile set is trusted before it is read again.
const PROFILE_TTL: Duration = Duration::from_secs(3600);
/// Claude Code's `ListInferenceProfiles` timeout per call.
const PROFILE_TIMEOUT: Duration = Duration::from_secs(8);
/// Pages read before the list is refused as endless.
const MAX_PROFILE_PAGES: usize = 20;
/// Largest page body read; a hostile peer cannot grow memory past it.
const MAX_PROFILE_PAGE_BYTES: usize = 4 * 1024 * 1024;
/// Backends whose profile lists are remembered before the cache starts over.
const PROFILE_CACHE_CAP: usize = 64;

/// Why the inference profiles could not be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProfileError {
    #[error(transparent)]
    Request(#[from] ClaudeTransportError),
    #[error("the request could not be signed: {0}")]
    Sign(#[from] SigV4Error),
    #[error("it did not answer within {0:?}")]
    Timeout(Duration),
    #[error("it listed more than {MAX_PROFILE_PAGES} pages")]
    TooManyPages,
    #[error(transparent)]
    Page(#[from] rules::BedrockError),
}

/// One listing of a backend's profiles; empty when the listing failed, which
/// Claude Code also remembers rather than asking again on every request.
struct Listing {
    ids: Arc<[String]>,
    fetched: Instant,
}

type ProfileCache = HopscotchMap<String, Arc<OnceCell<Listing>>, RandomState>;

static PROFILES: LazyLock<ProfileCache> =
    LazyLock::new(|| HopscotchMap::with_hasher(RandomState::new()));

fn cache_key(bedrock: &BedrockRoute) -> String {
    format!("{}\n{}", bedrock.backend_id, bedrock.control_url)
}

/// The Anthropic inference profiles of the account, listed once per backend
/// while fresh (concurrent first requests share one listing, as Claude Code's
/// cached promise does). A listing that fails is logged and remembered as
/// empty, so the request falls back to the rewritten catalog id.
async fn profiles(
    bedrock: &BedrockRoute,
    signing: Option<&AwsSigning>,
    token: &str,
) -> Arc<[String]> {
    let key = cache_key(bedrock);
    let slot = match PROFILES.get(&key) {
        Some(slot)
            if slot
                .get()
                .is_none_or(|listing| listing.fetched.elapsed() < PROFILE_TTL) =>
        {
            slot
        }
        stale => {
            if stale.is_none() && PROFILES.len() >= PROFILE_CACHE_CAP {
                PROFILES.clear();
            }
            let fresh = Arc::new(OnceCell::new());
            PROFILES.insert(key, fresh.clone());
            fresh
        }
    };
    slot.get_or_init(|| async {
        let ids = fetch_profiles(bedrock, signing, token)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(
                    region = %bedrock.region,
                    error = %error,
                    "Amazon Bedrock inference profiles could not be listed; the catalog model ids are used"
                );
                Vec::new()
            });
        Listing {
            ids: ids.into(),
            fetched: Instant::now(),
        }
    })
    .await
    .ids
    .clone()
}

async fn fetch_profiles(
    bedrock: &BedrockRoute,
    signing: Option<&AwsSigning>,
    token: &str,
) -> Result<Vec<String>, ProfileError> {
    let client = ClaudeHttpClient::new(None);
    let mut ids = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PROFILE_PAGES {
        let page = tokio::time::timeout(
            PROFILE_TIMEOUT,
            fetch_page(&client, bedrock, signing, token, next.as_deref()),
        )
        .await
        .map_err(|_| ProfileError::Timeout(PROFILE_TIMEOUT))??;
        ids.extend(page.ids);
        match page.next_token {
            Some(token) => next = Some(token),
            None => return Ok(ids),
        }
    }
    Err(ProfileError::TooManyPages)
}

async fn fetch_page(
    client: &ClaudeHttpClient,
    bedrock: &BedrockRoute,
    signing: Option<&AwsSigning>,
    token: &str,
    next: Option<&str>,
) -> Result<rules::ProfilePage, ProfileError> {
    let base = bedrock
        .control_url
        .strip_suffix('/')
        .unwrap_or(&bedrock.control_url);
    // The SDK's query builder sorts the names: `nextToken` before `type`.
    let url = match next {
        Some(next) => format!(
            "{base}/inference-profiles?nextToken={}&type=SYSTEM_DEFINED",
            escape_uri(next)
        ),
        None => format!("{base}/inference-profiles?type=SYSTEM_DEFINED"),
    };
    let mut headers: Vec<(String, String, bool)> =
        vec![("Accept".into(), "application/json".into(), false)];
    if let Some(signing) = signing {
        // The control plane is its own service in the same region.
        let control = AwsSigning {
            service: "bedrock",
            ..signing.clone()
        };
        let plain: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value, _)| (name.as_str(), value.as_str()))
            .collect();
        let added: Vec<(&'static str, HeaderValue)> =
            sign_request(&control, "GET", &url_parts(&url)?, &plain, b"")?;
        headers.extend(
            added
                .into_iter()
                .map(|(name, header)| (name.to_owned(), header.value, header.sensitive)),
        );
    } else {
        headers.push(("Authorization".into(), format!("Bearer {token}"), true));
    }
    let bytes = client
        .get_limited(&url, &headers, MAX_PROFILE_PAGE_BYTES)
        .await?;
    Ok(rules::parse_profile_page(&bytes)?)
}

/// The model id of the request's path. A model the caller named (an inference
/// profile id or ARN, or any string the catalog does not know) goes as given. A
/// catalog model takes the account's listed inference profile for the region's
/// prefix, else the catalog id with that prefix (`ED`), which is also the id
/// when the listing fails.
pub(super) async fn resolve_model(
    bedrock: &BedrockRoute,
    signing: Option<&AwsSigning>,
    plan: &ClaudeRequestPlan,
    token: &str,
    discover: bool,
) -> String {
    let model = plan.body.model.as_str();
    let Some(first_party) = plan.translated_from else {
        return model.to_owned();
    };
    let prefix = rules::region_prefix(&bedrock.region);
    let rewritten = rules::rewrite_model_id(model, prefix);
    if !discover {
        return rewritten;
    }
    let listed = profiles(bedrock, signing, token).await;
    rules::select_profile(&listed, first_party, prefix).map_or(rewritten, str::to_owned)
}

/// The request URL for `model` under the runtime base `endpoint`.
pub(super) fn invoke_uri(endpoint: &str, model: &str) -> Result<String, rules::BedrockError> {
    Ok(format!(
        "{}{}",
        endpoint.strip_suffix('/').unwrap_or(endpoint),
        rules::invoke_path(model)?
    ))
}

#[cfg(test)]
pub(super) fn clear_profile_cache_for_test() {
    PROFILES.clear();
}
