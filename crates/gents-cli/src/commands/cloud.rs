//! `gents cloud login`: device-code sign-in against a gents cloud pod.
//!
//! Shaped like [`crate::commands::claude_login`],
//! [`crate::commands::codex_login`] and [`crate::commands::grok_login`]:
//! another device does the authenticating, this process polls, and the
//! credential lands in the same `OAuthCredential` document those three
//! write. Nothing here ever sees a password. The cloud authenticates the
//! person on the console page they approve the code on, and hands back a
//! workspace bearer token once they have.
//!
//! Invariants: the token is written to the credential document and never
//! printed, logged, or put in the result JSON; the poll loop waits the
//! interval the server asked for and stops at the server's own expiry.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::cli::args::{CloudCommand, CloudLoginArgs};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_agent_did, resolve_config_access};

/// Per-request ceiling on a single call to the cloud. Long enough for a
/// cold pod, short enough that a black-holed host fails while the person
/// is still watching.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Floor under the poll interval the server asks for. A server that
/// answered `interval_secs: 0` would otherwise turn this loop into a
/// hot spin against its own public port. Mirrors the same `.max(1)`
/// guard `gents-chatgpt-login` puts on its device-code interval.
const MIN_POLL_INTERVAL_SECS: u64 = 1;

/// Ceiling on how long one `gents cloud login` waits, whatever expiry the
/// server reports. The real resource is the person's terminal: an
/// `expires_in_secs` of a year would leave the command wedged forever.
/// The wait actually used is printed in the give-up message, so a clamp
/// is never silent.
const MAX_LOGIN_WAIT_SECS: u64 = 15 * 60;

/// Ceiling on how much of a server message is echoed to the terminal.
/// The body is network input; this bounds what it can paint on a screen.
const MAX_MESSAGE_CHARS: usize = 200;

/// What the CLI says when the cloud refuses a sign-in and says nothing
/// about why.
const DEFAULT_REFUSAL: &str = "this sign-in was refused; run `gents cloud login` again";

/// A gents cloud workspace token carries no expiry: the pod mints 32
/// random bytes and honours them until that workspace's token is
/// rotated, and the poll response has no expiry to read.
/// `OAuthCredential.access_token_expires_at` is required, so the row
/// records now plus ten years. If the cloud starts expiring workspace
/// tokens, the poll response gains the expiry and this reads it.
const NO_EXPIRY_HORIZON_DAYS: i64 = 3650;

/// Stored in `refresh_token` because that field is required and a blank
/// value fails decoding of every credential for the agent. The device
/// flow has no refresh grant. This is not a token and must not be sent
/// to a token endpoint.
const NO_REFRESH_GRANT: &str = "gents-cloud:no-refresh-grant";

pub(crate) async fn dispatch(command: CloudCommand) -> Result<()> {
    match command {
        CloudCommand::Login(args) => cloud_login(args).await,
    }
}

pub(crate) struct CloudLoginOptions {
    pub(crate) cloud: String,
    pub(crate) provider: String,
}

pub(crate) struct CloudLoginOutcome {
    pub(crate) doc_id: String,
    pub(crate) email: String,
    pub(crate) workspace_id: String,
    pub(crate) credential: gents::oauth_credential::OAuthCredential,
}

pub(crate) async fn cloud_login(args: CloudLoginArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let agent_did = resolve_agent_did(Some(&home_dir), args.agent_did.as_deref())?;
    let outcome = run_cloud_login(
        &access,
        &agent_did,
        &CloudLoginOptions {
            cloud: args.cloud,
            provider: args.provider,
        },
    )
    .await?;
    print_json(&cloud_login_result_json(&outcome))?;
    Ok(())
}

pub(crate) async fn run_cloud_login(
    access: &ConfigAccess,
    agent_did: &str,
    opts: &CloudLoginOptions,
) -> Result<CloudLoginOutcome> {
    let base = cloud_base_url(&opts.cloud)?;
    let provider = cloud_provider(&opts.provider)?;
    // A redirect would forward the device-code body, and later the poll,
    // off the host the operator named. The 3xx comes back as the response.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("building HTTP client")?;

    let start = request_device_code(&http, &base).await?;
    eprintln!(
        "Open {} and enter code: {}",
        start.verification_uri, start.user_code
    );

    let polls = CloudDevicePolls {
        http,
        poll_url: cloud_endpoint(&base, "/auth/device/poll"),
    };
    let session = wait_for_approval(&polls, &start).await?;
    eprintln!("Signed in as {}", session.email);
    eprintln!("Workspace {} is ready.", session.workspace_id);

    let credential = credential_from_session(agent_did, provider, &session, Utc::now());
    let mutation = gents::oauth_credential::oauth_credential_upsert_mutation(&credential);
    let response = access
        .write("cli.cloud_login.credential", &mutation)
        .await?;
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;

    Ok(CloudLoginOutcome {
        doc_id,
        email: session.email,
        workspace_id: session.workspace_id,
        credential,
    })
}

pub(crate) fn cloud_login_result_json(outcome: &CloudLoginOutcome) -> Value {
    let credential = &outcome.credential;
    json!({
        "login": "completed",
        "doc_id": outcome.doc_id,
        "credential_id": credential.credential_id,
        "agent_did": credential.agent_did,
        "provider": credential.provider,
        "email": outcome.email,
        "workspace_id": outcome.workspace_id,
        "access_token_expires_at": credential.access_token_expires_at,
        "last_refresh": credential.last_refresh,
        "enabled": credential.enabled,
        "access_token": "<redacted>",
    })
}

/// What `POST /auth/device/start` answers: the secret this device polls
/// with, the short code the person reads out, and the schedule to keep.
#[derive(Deserialize)]
struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval_secs: u64,
    expires_in_secs: u64,
}

/// Hand-written so no `{:?}` anywhere can print the device code. That
/// value is what the poll exchanges for the workspace token.
impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceStart")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("interval_secs", &self.interval_secs)
            .field("expires_in_secs", &self.expires_in_secs)
            .finish()
    }
}

/// What `POST /auth/device/poll` answers once a person has approved.
#[derive(Deserialize)]
struct SignedIn {
    email: String,
    workspace_id: String,
    token: String,
}

/// Hand-written so no `{:?}` anywhere can print the workspace token.
impl std::fmt::Debug for SignedIn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignedIn")
            .field("email", &self.email)
            .field("workspace_id", &self.workspace_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// One answer to one poll.
#[derive(Debug)]
enum PollStep {
    /// Nobody has approved the code yet.
    Pending,
    /// Approved: this is the one and only time the token is handed over.
    SignedIn(SignedIn),
    /// Denied, expired, or already spent, with the cloud's own reason.
    Refused(String),
}

/// The one thing the poll loop does to the outside world. Behind a trait
/// so the loop's schedule (the interval it honours, the deadline it gives
/// up at, the refusal it stops on) is testable without a server.
trait DevicePolls {
    async fn poll_once(&self, device_code: &str) -> Result<PollStep>;
}

struct CloudDevicePolls {
    http: reqwest::Client,
    poll_url: String,
}

impl DevicePolls for CloudDevicePolls {
    async fn poll_once(&self, device_code: &str) -> Result<PollStep> {
        let response = self
            .http
            .post(&self.poll_url)
            .json(&json!({ "device_code": device_code }))
            .send()
            .await
            .map_err(|error| unreachable_cloud(&self.poll_url, &error))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| unreachable_cloud(&self.poll_url, &error))?;
        classify_poll(status, &body)
    }
}

async fn request_device_code(http: &reqwest::Client, base: &reqwest::Url) -> Result<DeviceStart> {
    let url = cloud_endpoint(base, "/auth/device/start");
    let response = http
        .post(&url)
        .json(&json!({}))
        .send()
        .await
        .map_err(|error| unreachable_cloud(&url, &error))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| unreachable_cloud(&url, &error))?;
    classify_start(status, &body)
}

fn classify_start(status: StatusCode, body: &str) -> Result<DeviceStart> {
    if status != StatusCode::OK {
        return Err(cloud_said_no("start this sign-in", status, body));
    }
    serde_json::from_str(body).map_err(|_| {
        anyhow::anyhow!(
            "the gents cloud answered the sign-in request with something this version does not understand; upgrade gents or check --cloud"
        )
    })
}

fn classify_poll(status: StatusCode, body: &str) -> Result<PollStep> {
    match status {
        StatusCode::OK => serde_json::from_str(body).map(PollStep::SignedIn).map_err(|_| {
            anyhow::anyhow!(
                "the gents cloud approved this sign-in but answered with something this version does not understand; upgrade gents or check --cloud"
            )
        }),
        StatusCode::PRECONDITION_REQUIRED => Ok(PollStep::Pending),
        StatusCode::FORBIDDEN => Ok(PollStep::Refused(
            message_sentence(body).unwrap_or_else(|| DEFAULT_REFUSAL.to_string()),
        )),
        other => Err(cloud_said_no("complete this sign-in", other, body)),
    }
}

/// Polls until somebody approves the code, the cloud refuses it, or the
/// code runs out of time.
///
/// The schedule is the server's: it asks how often to poll and how long
/// the code lives, and both are honoured rather than guessed at. Both are
/// still bounded here, because they arrive over the network:
/// [`MIN_POLL_INTERVAL_SECS`] stops a hot loop and [`MAX_LOGIN_WAIT_SECS`]
/// stops a wedged terminal. The wait actually used is in the give-up
/// message, so neither bound is silent.
async fn wait_for_approval(polls: &impl DevicePolls, start: &DeviceStart) -> Result<SignedIn> {
    let interval = Duration::from_secs(start.interval_secs.max(MIN_POLL_INTERVAL_SECS));
    let wait_secs = start.expires_in_secs.min(MAX_LOGIN_WAIT_SECS);
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        match polls.poll_once(&start.device_code).await? {
            PollStep::SignedIn(session) => return Ok(session),
            PollStep::Refused(reason) => anyhow::bail!("{reason}"),
            PollStep::Pending => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!(
                "this sign-in was not approved within {wait_secs} seconds; run `gents cloud login` again"
            );
        }
        tokio::time::sleep(interval.min(remaining)).await;
    }
}

fn credential_from_session(
    agent_did: &str,
    provider: &str,
    session: &SignedIn,
    now: DateTime<Utc>,
) -> gents::oauth_credential::OAuthCredential {
    gents::oauth_credential::OAuthCredential {
        doc_id: None,
        credential_id: gents::oauth_credential::oauth_credential_id(agent_did, provider),
        agent_did: agent_did.to_string(),
        provider: provider.to_string(),
        access_token: session.token.clone(),
        refresh_token: NO_REFRESH_GRANT.to_string(),
        id_token: None,
        // The account the cloud verified through Google. The workspace it
        // opens is not stored beside it: the token already names the
        // workspace to the cloud (`GET /api/v1/workspace`), and a second
        // copy here is a copy that can drift.
        account_id: Some(session.email.clone()),
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: no_expiry_horizon(now),
        last_refresh: Some(now),
        enabled: true,
    }
}

fn no_expiry_horizon(now: DateTime<Utc>) -> DateTime<Utc> {
    now.checked_add_signed(chrono::Duration::days(NO_EXPIRY_HORIZON_DAYS))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// Turns `--cloud` into the base URL of a gents cloud public listener.
///
/// A bare host (`app.dev.gents.xyz`) is HTTPS. `http` is kept only for a
/// loopback listener, which is what a local pod on `127.0.0.1` needs. A
/// workspace token has no expiry, so cleartext to any other host would
/// publish it.
fn cloud_base_url(cloud: &str) -> Result<reqwest::Url> {
    let cloud = cloud.trim();
    if cloud.is_empty() {
        anyhow::bail!("--cloud must name a gents cloud host, for example app.dev.gents.xyz");
    }
    let absolute = if cloud.starts_with("http://") || cloud.starts_with("https://") {
        cloud.to_string()
    } else {
        format!("https://{cloud}")
    };
    let url = reqwest::Url::parse(&absolute)
        .map_err(|_| anyhow::anyhow!("--cloud value {cloud:?} is not a host or a URL"))?;
    if url.host_str().is_none() {
        anyhow::bail!("--cloud value {cloud:?} is not a host or a URL");
    }
    if url.scheme() == "http" && !host_is_loopback(&url) {
        anyhow::bail!(
            "--cloud must use https, or http on a loopback host such as http://127.0.0.1:9192"
        );
    }
    Ok(url)
}

fn host_is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // `host_str` keeps the brackets around an IPv6 address.
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// Cloud login writes one non-refreshing workspace token. The provider
/// names that row. It must not be one of the OAuth providers gents
/// refreshes, or this write would replace that grant with a token those
/// clients then try to refresh.
fn cloud_provider(provider: &str) -> Result<&str> {
    let provider = provider.trim();
    if provider.is_empty() {
        anyhow::bail!("--provider must name the gents cloud credential, for example gents-cloud");
    }
    if matches!(
        provider,
        gents::claude_oauth::CLAUDE_OAUTH_PROVIDER
            | gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER
            | gents::xai_grok_oauth::XAI_OAUTH_PROVIDER
    ) {
        anyhow::bail!(
            "--provider {provider} is an OAuth credential gents refreshes; cloud login stores a workspace token under gents-cloud"
        );
    }
    Ok(provider)
}

/// The URL of one device-auth route on `base`. Mirrors
/// [`crate::commands::fleet`]: take the host from the endpoint the
/// operator gave and put the route's own path on it.
fn cloud_endpoint(base: &reqwest::Url, path: &str) -> String {
    let mut url = base.clone();
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

/// One sentence for a cloud that answered, but said no.
fn cloud_said_no(action: &str, status: StatusCode, body: &str) -> anyhow::Error {
    match message_sentence(body) {
        Some(reason) => anyhow::anyhow!(
            "the gents cloud could not {action} (HTTP {status}: {reason}); run `gents cloud login` again"
        ),
        None => anyhow::anyhow!(
            "the gents cloud could not {action} (HTTP {status}); run `gents cloud login` again"
        ),
    }
}

/// One sentence for a cloud that did not answer at all.
///
/// `reqwest` reports a transport failure as a chain. The outer segment
/// repeats the URL, which this sentence already names, so the causes are
/// joined on one line.
fn unreachable_cloud(url: &str, error: &reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the gents cloud at {url} ({}); check --cloud and that the host is reachable",
        transport_reasons(error)
    )
}

fn transport_reasons(error: &reqwest::Error) -> String {
    let mut reasons = Vec::new();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let text = cause.to_string().replace('\n', "; ");
        if !text.is_empty() {
            reasons.push(text);
        }
        source = cause.source();
    }
    if reasons.is_empty() {
        error.to_string().replace('\n', "; ")
    } else {
        reasons.join("; ")
    }
}

/// The server's own message, made safe to print: the first non-empty
/// line, trimmed and bounded, with the truncation visible when it bites.
/// `None` when the body says nothing.
fn message_sentence(body: &str) -> Option<String> {
    let first = body.lines().map(str::trim).find(|line| !line.is_empty())?;
    let kept: String = first.chars().take(MAX_MESSAGE_CHARS).collect();
    if kept.chars().count() < first.chars().count() {
        Some(format!("{kept} (truncated)"))
    } else {
        Some(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn signed_in() -> SignedIn {
        SignedIn {
            email: "theo@source.network".to_string(),
            workspace_id: "ws-0c21".to_string(),
            token: "workspace-token-SECRET".to_string(),
        }
    }

    fn start_with(interval_secs: u64, expires_in_secs: u64) -> DeviceStart {
        DeviceStart {
            device_code: "device-code-SECRET".to_string(),
            user_code: "WDJB-MJHT".to_string(),
            verification_uri: "https://console.example/login".to_string(),
            interval_secs,
            expires_in_secs,
        }
    }

    /// A cloud that answers from a script, recording when it was asked.
    struct ScriptedPolls {
        steps: Mutex<VecDeque<PollStep>>,
        asked_at: Mutex<Vec<Duration>>,
        started: Instant,
    }

    impl ScriptedPolls {
        /// Must be built inside the test's runtime: `started` anchors the
        /// recorded poll times to the same (paused) clock the loop sleeps on.
        fn new(steps: impl IntoIterator<Item = PollStep>) -> Self {
            Self {
                steps: Mutex::new(steps.into_iter().collect()),
                asked_at: Mutex::new(Vec::new()),
                started: Instant::now(),
            }
        }

        fn asked_at_secs(&self) -> Vec<u64> {
            self.asked_at
                .lock()
                .expect("poll times")
                .iter()
                .map(Duration::as_secs)
                .collect()
        }
    }

    impl DevicePolls for ScriptedPolls {
        async fn poll_once(&self, _device_code: &str) -> Result<PollStep> {
            self.asked_at
                .lock()
                .expect("poll times")
                .push(self.started.elapsed());
            Ok(self
                .steps
                .lock()
                .expect("script")
                .pop_front()
                .unwrap_or(PollStep::Pending))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_poll_loop_waits_the_interval_the_server_asked_for() {
        let polls = ScriptedPolls::new([
            PollStep::Pending,
            PollStep::Pending,
            PollStep::SignedIn(signed_in()),
        ]);
        let session = wait_for_approval(&polls, &start_with(7, 600))
            .await
            .expect("approved");
        assert_eq!(session.email, "theo@source.network");
        assert_eq!(session.workspace_id, "ws-0c21");
        assert_eq!(
            polls.asked_at_secs(),
            vec![0, 7, 14],
            "each poll waits the server's own interval, not a fixed sleep"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_interval_from_the_server_still_leaves_a_gap_between_polls() {
        let polls = ScriptedPolls::new([PollStep::Pending, PollStep::SignedIn(signed_in())]);
        wait_for_approval(&polls, &start_with(0, 600))
            .await
            .expect("approved");
        assert_eq!(
            polls.asked_at_secs(),
            vec![0, MIN_POLL_INTERVAL_SECS],
            "an interval of zero must not become a hot loop"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_poll_loop_gives_up_when_the_code_expires() {
        let polls = ScriptedPolls::new([]);
        let error = wait_for_approval(&polls, &start_with(5, 20))
            .await
            .expect_err("the code expires");
        assert_eq!(
            error.to_string(),
            "this sign-in was not approved within 20 seconds; run `gents cloud login` again"
        );
        assert_eq!(
            polls.asked_at_secs(),
            vec![0, 5, 10, 15, 20],
            "it polls up to the deadline and then stops"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_poll_loop_never_waits_longer_than_the_login_ceiling() {
        let polls = ScriptedPolls::new([]);
        let error = wait_for_approval(&polls, &start_with(MAX_LOGIN_WAIT_SECS, u64::MAX))
            .await
            .expect_err("the ceiling bites");
        assert_eq!(
            error.to_string(),
            format!(
                "this sign-in was not approved within {MAX_LOGIN_WAIT_SECS} seconds; run `gents cloud login` again"
            ),
            "the clamped wait is named in the message rather than applied silently"
        );
        assert_eq!(polls.asked_at_secs(), vec![0, MAX_LOGIN_WAIT_SECS]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_stops_the_poll_loop_with_the_clouds_own_reason() {
        let polls = ScriptedPolls::new([
            PollStep::Pending,
            PollStep::Refused(
                "this sign-in code has expired; run the login command again".to_string(),
            ),
            PollStep::SignedIn(signed_in()),
        ]);
        let error = wait_for_approval(&polls, &start_with(5, 600))
            .await
            .expect_err("refused");
        assert_eq!(
            error.to_string(),
            "this sign-in code has expired; run the login command again"
        );
        assert_eq!(
            polls.asked_at_secs().len(),
            2,
            "a refusal stops the loop instead of polling on to the deadline"
        );
    }

    #[test]
    fn a_forbidden_poll_becomes_a_refusal_carrying_the_servers_sentence() {
        let step = classify_poll(
            StatusCode::FORBIDDEN,
            "this sign-in code has expired; run the login command again",
        )
        .expect("classified");
        match step {
            PollStep::Refused(reason) => assert_eq!(
                reason,
                "this sign-in code has expired; run the login command again"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_forbidden_poll_with_an_empty_body_still_says_what_to_do() {
        let step = classify_poll(StatusCode::FORBIDDEN, "").expect("classified");
        match step {
            PollStep::Refused(reason) => assert_eq!(reason, DEFAULT_REFUSAL),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_428_poll_is_pending_and_a_200_poll_is_a_session() {
        assert!(matches!(
            classify_poll(StatusCode::PRECONDITION_REQUIRED, "waiting for approval").expect("428"),
            PollStep::Pending
        ));
        let step = classify_poll(
            StatusCode::OK,
            r#"{"email":"theo@source.network","workspace_id":"ws-0c21","token":"t"}"#,
        )
        .expect("200");
        match step {
            PollStep::SignedIn(session) => {
                assert_eq!(session.email, "theo@source.network");
                assert_eq!(session.workspace_id, "ws-0c21");
                assert_eq!(session.token, "t");
            }
            other => panic!("expected a session, got {other:?}"),
        }
    }

    /// The exact transport reason differs per host and platform (refused,
    /// unreachable, blocked), so this pins what the operator is promised:
    /// one line, naming the URL, carrying a reason, ending in what to do,
    /// and with no cause chain for `Error: {:?}` to unfold into a stack.
    #[tokio::test]
    async fn an_unreachable_cloud_fails_with_one_plain_sentence() {
        let polls = CloudDevicePolls {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("client"),
            // Port 1 is privileged and never a gents cloud, so the connect
            // fails immediately without this test needing a network.
            poll_url: "http://127.0.0.1:1/auth/device/poll".to_string(),
        };
        let error = polls
            .poll_once("device-code-SECRET")
            .await
            .expect_err("unreachable");
        let message = error.to_string();
        assert!(
            message.starts_with(
                "cannot reach the gents cloud at http://127.0.0.1:1/auth/device/poll ("
            ),
            "{message}"
        );
        assert!(
            message.ends_with("); check --cloud and that the host is reachable"),
            "{message}"
        );
        assert!(!message.contains('\n'), "one line, not a stack: {message}");
        assert_eq!(error.chain().count(), 1, "no cause chain to print");
        assert!(!message.contains("SECRET"), "{message}");
    }

    #[test]
    fn an_unexpected_poll_status_fails_with_one_sentence() {
        let error = classify_poll(StatusCode::BAD_GATEWAY, "upstream is down").expect_err("502");
        assert_eq!(
            error.to_string(),
            "the gents cloud could not complete this sign-in (HTTP 502 Bad Gateway: upstream is down); run `gents cloud login` again"
        );
        assert_eq!(error.chain().count(), 1, "no cause chain to print");
    }

    #[test]
    fn a_refused_start_reports_the_clouds_own_sentence() {
        let error = classify_start(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many sign-ins are in progress; try again in a minute",
        )
        .expect_err("503");
        assert_eq!(
            error.to_string(),
            "the gents cloud could not start this sign-in (HTTP 503 Service Unavailable: too many sign-ins are in progress; try again in a minute); run `gents cloud login` again"
        );
    }

    #[test]
    fn a_start_response_decodes_the_whole_contract() {
        let start = classify_start(
            StatusCode::OK,
            r#"{"device_code":"dc","user_code":"WDJB-MJHT","verification_uri":"https://console.example/login","interval_secs":5,"expires_in_secs":600}"#,
        )
        .expect("start");
        assert_eq!(start.device_code, "dc");
        assert_eq!(start.user_code, "WDJB-MJHT");
        assert_eq!(start.verification_uri, "https://console.example/login");
        assert_eq!(start.interval_secs, 5);
        assert_eq!(start.expires_in_secs, 600);
    }

    #[test]
    fn a_server_message_is_one_bounded_line_and_says_when_it_was_cut() {
        assert_eq!(message_sentence(""), None);
        assert_eq!(message_sentence("  \n \n"), None);
        assert_eq!(
            message_sentence("\n  refused because reasons  \nsecond line\n"),
            Some("refused because reasons".to_string())
        );
        let long = "x".repeat(MAX_MESSAGE_CHARS + 50);
        let bounded = message_sentence(&long).expect("bounded");
        assert_eq!(
            bounded,
            format!("{} (truncated)", "x".repeat(MAX_MESSAGE_CHARS))
        );
    }

    #[test]
    fn a_bare_host_is_https_and_an_explicit_scheme_is_kept() {
        assert_eq!(
            cloud_endpoint(
                &cloud_base_url("app.dev.gents.xyz").expect("host"),
                "/auth/device/start"
            ),
            "https://app.dev.gents.xyz/auth/device/start"
        );
        assert_eq!(
            cloud_endpoint(
                &cloud_base_url("http://127.0.0.1:9192").expect("local"),
                "/auth/device/poll"
            ),
            "http://127.0.0.1:9192/auth/device/poll"
        );
        assert_eq!(
            cloud_endpoint(
                &cloud_base_url("https://app.dev.gents.xyz/ignored").expect("url"),
                "/auth/device/poll"
            ),
            "https://app.dev.gents.xyz/auth/device/poll"
        );
    }

    #[test]
    fn a_query_on_the_cloud_url_does_not_reach_the_endpoint() {
        assert_eq!(
            cloud_endpoint(
                &cloud_base_url("https://app.dev.gents.xyz?q=1").expect("url"),
                "/auth/device/poll"
            ),
            "https://app.dev.gents.xyz/auth/device/poll"
        );
    }

    #[test]
    fn a_fragment_on_the_cloud_url_does_not_reach_the_endpoint() {
        assert_eq!(
            cloud_endpoint(
                &cloud_base_url("https://app.dev.gents.xyz#frag").expect("url"),
                "/auth/device/poll"
            ),
            "https://app.dev.gents.xyz/auth/device/poll"
        );
    }

    #[test]
    fn an_empty_cloud_host_is_refused_with_an_example() {
        let error = cloud_base_url("   ").expect_err("empty");
        assert_eq!(
            error.to_string(),
            "--cloud must name a gents cloud host, for example app.dev.gents.xyz"
        );
    }

    #[test]
    fn the_credential_carries_the_token_the_account_and_no_refresh_grant() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("timestamp");
        let credential =
            credential_from_session("did:key:z6MkTest", "gents-cloud", &signed_in(), now);
        assert_eq!(credential.credential_id, "gents-cloud:did:key:z6MkTest");
        assert_eq!(credential.access_token, "workspace-token-SECRET");
        assert_eq!(credential.refresh_token, NO_REFRESH_GRANT);
        assert_ne!(credential.refresh_token, credential.access_token);
        assert_eq!(
            credential.account_id.as_deref(),
            Some("theo@source.network")
        );
        assert!(credential.id_token.is_none());
        assert!(credential.enabled);
        assert_eq!(credential.last_refresh, Some(now));
        assert!(
            credential.access_token_expires_at > now + chrono::Duration::days(3000),
            "a workspace token does not expire on a schedule this CLI can know"
        );
    }

    #[test]
    fn the_result_json_never_carries_the_token() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("timestamp");
        let session = signed_in();
        let outcome = CloudLoginOutcome {
            doc_id: "bae-1".to_string(),
            email: session.email.clone(),
            workspace_id: session.workspace_id.clone(),
            credential: credential_from_session("did:key:z6MkTest", "gents-cloud", &session, now),
        };
        let json = cloud_login_result_json(&outcome);
        let text = json.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        assert_eq!(json["access_token"], "<redacted>");
        assert_eq!(json["login"], "completed");
        assert_eq!(json["email"], "theo@source.network");
        assert_eq!(json["workspace_id"], "ws-0c21");
    }

    #[test]
    fn debug_on_a_session_redacts_the_token() {
        assert!(!format!("{:?}", signed_in()).contains("SECRET"));
    }

    #[test]
    fn debug_on_a_device_start_redacts_the_device_code() {
        let rendered = format!("{:?}", start_with(5, 600));
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(rendered.contains("WDJB-MJHT"), "{rendered}");
    }

    #[test]
    fn the_stored_credential_decodes_with_the_agents_other_credentials() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("timestamp");
        let credential =
            credential_from_session("did:key:z6MkTest", "gents-cloud", &signed_in(), now);
        let response = json!({
            "data": {
                "OAuthCredential": [{
                    "_docID": "doc-1",
                    "credential_id": credential.credential_id,
                    "agent_did": credential.agent_did,
                    "provider": credential.provider,
                    "access_token": credential.access_token,
                    "refresh_token": credential.refresh_token,
                    "id_token": null,
                    "account_id": credential.account_id,
                    "chatgpt_plan_type": null,
                    "is_fedramp": false,
                    "access_token_expires_at": credential.access_token_expires_at.to_rfc3339(),
                    "last_refresh": now.to_rfc3339(),
                    "enabled": true,
                }]
            }
        });
        let loaded = gents::oauth_credential::oauth_credentials_from_response(&response)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("cloud credential decodes");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].access_token, "workspace-token-SECRET");
        assert_eq!(loaded[0].refresh_token, NO_REFRESH_GRANT);
    }

    #[test]
    fn http_is_only_kept_for_a_loopback_host() {
        assert!(cloud_base_url("http://127.0.0.1:9192").is_ok());
        assert!(cloud_base_url("http://localhost:9192").is_ok());
        assert!(cloud_base_url("http://[::1]:9192").is_ok());
        assert!(cloud_base_url("https://app.dev.gents.xyz").is_ok());
        let error = cloud_base_url("http://app.dev.gents.xyz").expect_err("cleartext");
        assert!(error.to_string().contains("https"), "{error}");
        assert!(cloud_base_url("http://10.1.2.3:9192").is_err());
    }

    #[test]
    fn cloud_login_refuses_to_overwrite_an_oauth_provider_gents_refreshes() {
        for provider in ["claude-subscription", "chatgpt-codex", "xai-oauth"] {
            let error = cloud_provider(provider).expect_err(provider);
            assert!(error.to_string().contains(provider), "{error}");
        }
        assert_eq!(cloud_provider("gents-cloud").expect("cloud"), "gents-cloud");
        assert_eq!(
            cloud_provider("  gents-cloud  ").expect("trimmed"),
            "gents-cloud"
        );
        assert!(cloud_provider("   ").is_err());
    }
}
