//! HTTP for a plugin that holds no socket.
//!
//! A plugin's guest never runs with network authority (WASI preview 1 has no
//! sockets, and [`super::PluginRunner`] strips the `net` axis before a run).
//! A plugin whose granted manifold carries an `OutboundHttp` allow-list
//! instead answers a call with a batch of HTTP requests; the host performs
//! the ones the grant admits and resumes the plugin with the responses,
//! through the same round loop as model calls ([`super::rounds`]).
//!
//! The wire:
//!
//! - input: the caller's input plus `"http_calls": true`, then on later
//!   rounds `"http_results": {id: {"status", "headers": {name: value},
//!   "body" | "body_base64"} | {"error": ...}}` and the `"state"` the plugin
//!   last returned. `body` is used when the response is UTF-8, `body_base64`
//!   otherwise.
//! - output: `{"http_calls": {"requests": [{"id", "method", "url", "headers":
//!   {name: value}, "body" | "body_base64"}], "state": ...}}`.
//!
//! Allow-list entries are `host`, `*.domain` (strict subdomains), an IPv4
//! literal or a bracketed IPv6 literal, optionally prefixed `http://` to also
//! allow plaintext and suffixed `:port`; an entry without a port admits only
//! the scheme's default port. A grant of any host (`OutboundHttp(null)`)
//! admits any public host over HTTPS on port 443. The admission rule is
//! `ToolPolicy.PluginNetwork.allowed`, bound by
//! `generated_plugin_network_cases_drive_admission`. The host never follows
//! a redirect: a 3xx comes back to the plugin with its `location` header, and
//! following it is a new request, admitted like any other. A request carries
//! only what the plugin put in it (no proxy, cookie store or credentials of
//! the host's).
//!
//! Bounds beyond the round loop's: [`MAX_REQUESTS_PER_ROUND`] requests a
//! round, [`MAX_IN_FLIGHT`] of them at once, [`MAX_REQUESTS_PER_CALL`]
//! requests and [`MAX_RESPONSE_BYTES_PER_CALL`] response bytes over the call
//! (charged as they are read, whether or not the response is then refused;
//! a request past either gets an error result), [`MAX_REQUEST_BODY_BYTES`]
//! a request body, [`MAX_RESPONSE_BYTES`] a response body, and
//! [`REQUEST_TIMEOUT`] a request
//! (lowered by the manifold's `http_timeout_ms`, which a manifest may not
//! set to 0), inside the call's wall clock.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use afterburner_core::manifold::{Manifold, NetAccess};
use anyhow::{Context, Result};
use base64::Engine;
use futures::future::BoxFuture;
use futures::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{Method, Url};
use serde_json::{json, Map, Value};

/// Requests one round may carry.
pub const MAX_REQUESTS_PER_ROUND: usize = 16;
/// Requests of one round in flight at once.
pub const MAX_IN_FLIGHT: usize = 8;
/// Requests one call may send over all its rounds.
pub const MAX_REQUESTS_PER_CALL: usize = 256;
/// Bytes of one request body.
pub const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
/// Bytes of one response body handed back to the plugin.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Response bytes one call may take in over all its rounds.
pub const MAX_RESPONSE_BYTES_PER_CALL: usize = 16 * 1024 * 1024;
/// Longest one request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Headers the host owns: framing, connection management and proxying.
const HOST_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "expect",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetHost {
    Name(String),
    Ip(IpAddr),
}

impl std::fmt::Display for TargetHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
            Self::Ip(ip) => write!(f, "{ip}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostPattern {
    Exact(TargetHost),
    Suffix(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pattern: HostPattern,
    plaintext: bool,
    port: Option<u16>,
}

/// What a granted `net` axis lets the host reach for a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NetGrant {
    Sealed,
    AnyHost,
    Hosts(Vec<Entry>),
}

/// The grant `net` describes, or why the host cannot enforce it.
pub(crate) fn net_grant(net: &NetAccess) -> Result<NetGrant, String> {
    match net {
        NetAccess::None => Ok(NetGrant::Sealed),
        NetAccess::OutboundHttp(None) => Ok(NetGrant::AnyHost),
        NetAccess::OutboundHttp(Some(hosts)) if hosts.is_empty() => Ok(NetGrant::Sealed),
        NetAccess::OutboundHttp(Some(hosts)) => hosts
            .iter()
            .map(|host| parse_entry(host))
            .collect::<Result<Vec<_>, _>>()
            .map(NetGrant::Hosts),
        NetAccess::OutboundFull(_) => Err(
            "raw TCP (OutboundFull) is never offered to a plugin; declare OutboundHttp, which the host serves"
                .to_owned(),
        ),
    }
}

/// One allow-list entry, in the grammar of this module's doc.
fn parse_entry(text: &str) -> Result<Entry, String> {
    let invalid = |why: &str| format!("network allow-list entry {text:?} {why}");
    let lower = text.trim().to_ascii_lowercase();
    let (plaintext, rest) = match (
        lower.strip_prefix("http://"),
        lower.strip_prefix("https://"),
    ) {
        (Some(rest), _) => (true, rest),
        (_, Some(rest)) => (false, rest),
        _ => (false, lower.as_str()),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && (!host.contains(':') || host.ends_with(']')) =>
        {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| invalid("has a port outside 1-65535"))?;
            (host, Some(port))
        }
        _ => (rest, None),
    };
    let (wildcard, host) = match host.strip_prefix("*.") {
        Some(domain) => (true, domain),
        None => (false, host),
    };
    if host.is_empty() || host.contains(['/', '@', '?', '#', '*']) {
        return Err(invalid(
            "must be a host, *.domain or IP literal, optionally with http:// and :port",
        ));
    }
    let parsed = Url::parse(&format!("https://{host}/"))
        .ok()
        .and_then(|url| target_host(&url))
        .ok_or_else(|| invalid("does not name a host"))?;
    let pattern = match (wildcard, parsed) {
        (false, host) => HostPattern::Exact(host),
        (true, TargetHost::Name(domain)) => HostPattern::Suffix(domain),
        (true, TargetHost::Ip(_)) => return Err(invalid("puts a wildcard on an IP literal")),
    };
    Ok(Entry {
        pattern,
        plaintext,
        port,
    })
}

/// Refuses, at manifest load, a declared network axis the host cannot serve.
pub(crate) fn validate_declared(manifold: &Manifold) -> Result<(), String> {
    if manifold.http_timeout_ms == Some(0) {
        return Err("http_timeout_ms must be at least 1; omit it for the default".to_owned());
    }
    net_grant(&manifold.net).map(|_| ())
}

fn target_host(url: &Url) -> Option<TargetHost> {
    // `Url` serialises an IP host canonically (IPv6 bracketed), so the
    // string form is the parsed address, not the authored spelling.
    let host = url.host_str()?;
    if let Some(v6) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return v6.parse().ok().map(|ip| TargetHost::Ip(IpAddr::V6(ip)));
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Some(TargetHost::Ip(IpAddr::V4(ip)));
    }
    let domain = host.trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty()).then_some(TargetHost::Name(domain))
}

/// Whether an address is outside every internal range: the classification
/// is `ToolPolicy.PluginNetwork.ipPublic`'s.
pub(crate) fn ip_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_public(ip),
        IpAddr::V6(ip) => v6_public(ip),
    }
}

fn v4_public(ip: &Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 224)
}

fn v6_public(ip: &Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_public(&v4);
    }
    let segments = ip.segments();
    if segments[0] == 0x2002 {
        // 6to4 carries the IPv4 address it routes to.
        let [hi, lo] = [segments[1].to_be_bytes(), segments[2].to_be_bytes()];
        return v4_public(&Ipv4Addr::new(hi[0], hi[1], lo[0], lo[1]));
    }
    (segments[0] & 0xe000) == 0x2000
        && !(segments[0] == 0x2001
            && (segments[1] == 0 || segments[1] == 0x0db8 || (0x10..0x30).contains(&segments[1])))
}

/// How a target was admitted: whether an entry naming its IP literal admits
/// it, and the literal itself when the URL names one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Admission {
    explicit: bool,
    literal: Option<IpAddr>,
}

/// Admits `url` under `grant`, before any address is known; `Err` is why
/// not, as a sentence that names the next step.
pub(crate) fn admit(grant: &NetGrant, url: &Url, coordinate: &str) -> Result<Admission, String> {
    let https = match url.scheme() {
        "https" => true,
        "http" => false,
        other => {
            return Err(format!(
                "plugin {coordinate} may send only http(s) requests, not {other}:"
            ))
        }
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "a plugin request URL may not carry credentials; send them as a header".to_owned(),
        );
    }
    let host = target_host(url).ok_or("the request URL names no host")?;
    let port = url.port_or_known_default().unwrap_or(0);
    let literal = match host {
        TargetHost::Ip(ip) => Some(ip),
        TargetHost::Name(_) => None,
    };
    let default_port = if https { 443 } else { 80 };
    let admitted = match grant {
        NetGrant::Sealed => None,
        NetGrant::AnyHost => (https && port == 443).then_some(false),
        NetGrant::Hosts(entries) => entries
            .iter()
            .filter(|entry| {
                let matches = match &entry.pattern {
                    HostPattern::Exact(exact) => *exact == host,
                    HostPattern::Suffix(domain) => matches!(
                        &host,
                        TargetHost::Name(name) if name.ends_with(&format!(".{domain}"))
                    ),
                };
                matches && (https || entry.plaintext) && port == entry.port.unwrap_or(default_port)
            })
            .map(|entry| matches!(entry.pattern, HostPattern::Exact(TargetHost::Ip(_))))
            .reduce(|a, b| a || b),
    };
    let scheme = url.scheme();
    admitted
        .map(|explicit| Admission { explicit, literal })
        .ok_or_else(|| {
            format!(
                "{scheme}://{host}:{port} is not in plugin {coordinate}'s granted network allow-list; \
                 add it to the plugin's manifold net OutboundHttp allow-list (prefix http:// for \
                 plaintext) and reinstall with --grant-authority"
            )
        })
}

/// Whether one connection may use `addrs`: every address must be public
/// unless an entry naming the IP literal itself admitted the target.
pub(crate) fn addresses_allowed(explicit: bool, addrs: &[IpAddr]) -> bool {
    !addrs.is_empty() && (explicit || addrs.iter().all(ip_public))
}

/// The refusal of `ip` as the target `host` names; `host` is `None` when
/// the URL names `ip` itself.
fn internal_refusal(host: Option<&str>, ip: &IpAddr) -> String {
    match host {
        Some(host) => format!(
            "{host} resolves to the internal address {ip}; a hostname grant reaches public \
             addresses only, so to reach it, name the IP literal in the request URL and grant \
             that literal"
        ),
        None => {
            let literal = TargetHost::Ip(*ip);
            format!(
                "{ip} is an internal address; only an allow-list entry naming that exact IP \
                 literal grants it, so add {literal} to the plugin's allow-list to reach it"
            )
        }
    }
}

/// Resolves a host name for the client.
pub(super) trait Lookup: Send + Sync {
    fn lookup(&self, host: String) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>>;
}

struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: String) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|addr| addr.ip())
                .collect())
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct InternalAddress(String);

/// The client's only resolver: the addresses it returns are the ones the
/// connection uses, so checking them here leaves no second resolution for a
/// rebinding name to answer differently.
struct Guarded(Arc<dyn Lookup>);

impl reqwest::dns::Resolve for Guarded {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = self.0.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = lookup.lookup(host.clone()).await?;
            if let Some(ip) = addrs.iter().find(|ip| !ip_public(ip)) {
                return Err(Box::new(InternalAddress(internal_refusal(Some(&host), ip))) as _);
            }
            if !addresses_allowed(false, &addrs) {
                return Err(format!("{host} did not resolve").into());
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

pub(super) struct Request {
    id: String,
    method: Method,
    url: Url,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Option<Vec<u8>>,
}

pub(super) struct Batch {
    pub(super) requests: Vec<Request>,
    pub(super) state: Option<Value>,
}

/// The requests a plugin's output asks for: `None` when the output is a
/// final result, an error naming what is malformed otherwise.
pub(super) fn parse_batch(output: &Value) -> Result<Option<Batch>, String> {
    let Some(calls) = output
        .as_object()
        .and_then(|object| object.get("http_calls"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let requests = calls
        .get("requests")
        .and_then(Value::as_array)
        .ok_or("http_calls.requests must be an array")?;
    if requests.len() > MAX_REQUESTS_PER_ROUND {
        return Err(format!(
            "http_calls.requests holds {} requests; one round takes at most {MAX_REQUESTS_PER_ROUND}",
            requests.len()
        ));
    }
    let mut ids = BTreeSet::new();
    let requests = requests
        .iter()
        .map(|request| {
            let id = request
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("every http request needs a non-empty string id")?;
            if !ids.insert(id) {
                return Err(format!("http request id {id:?} is used twice in one round"));
            }
            parse_request(id, request).map_err(|why| format!("http request {id:?} {why}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Some(Batch {
        requests,
        state: calls.get("state").filter(|state| !state.is_null()).cloned(),
    }))
}

fn parse_request(id: &str, request: &Value) -> Result<Request, String> {
    let method = match request.get("method") {
        None | Some(Value::Null) => Method::GET,
        Some(Value::String(method)) => match method.to_ascii_uppercase().as_str() {
            "GET" => Method::GET,
            "HEAD" => Method::HEAD,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "DELETE" => Method::DELETE,
            _ => {
                return Err(format!(
                    "has method {method:?}; use GET, HEAD, POST, PUT, PATCH or DELETE"
                ))
            }
        },
        Some(_) => return Err("method must be a string".to_owned()),
    };
    let url = request
        .get("url")
        .and_then(Value::as_str)
        .ok_or("needs a url")?;
    let url = Url::parse(url).map_err(|error| format!("url is not a URL: {error}"))?;
    let headers = match request.get("headers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(headers)) => headers
            .iter()
            .map(|(name, value)| {
                let header = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| format!("header {name:?} is not a header name"))?;
                if HOST_HEADERS.contains(&header.as_str()) || header.as_str().starts_with("proxy-")
                {
                    return Err(format!("header {name:?} is set by the host"));
                }
                let value = value
                    .as_str()
                    .and_then(|value| HeaderValue::from_str(value).ok())
                    .ok_or_else(|| format!("header {name:?} needs a string value"))?;
                Ok((header, value))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("headers must be an object".to_owned()),
    };
    let body = match (request.get("body"), request.get("body_base64")) {
        (None | Some(Value::Null), None | Some(Value::Null)) => None,
        (Some(Value::String(body)), None | Some(Value::Null)) => Some(body.as_bytes().to_vec()),
        (None | Some(Value::Null), Some(Value::String(body))) => Some(
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .map_err(|_| "body_base64 is not base64".to_owned())?,
        ),
        _ => return Err("takes one string body or body_base64".to_owned()),
    };
    if body
        .as_ref()
        .is_some_and(|body| body.len() > MAX_REQUEST_BODY_BYTES)
    {
        return Err(format!(
            "body is larger than {MAX_REQUEST_BODY_BYTES} bytes"
        ));
    }
    Ok(Request {
        id: id.to_owned(),
        method,
        url,
        headers,
        body,
    })
}

/// Why a request is answered with an error without being sent.
#[derive(Clone, Copy)]
enum Refusal {
    RequestLimit,
    ResponseBudget,
}

impl Refusal {
    fn message(self) -> &'static str {
        match self {
            Self::RequestLimit => {
                "this call used up its http request limit; this request was not sent"
            }
            Self::ResponseBudget => {
                "this call used up its http response budget; this request was not sent"
            }
        }
    }
}

/// One plugin's network for the length of one call.
pub(super) struct Session {
    coordinate: String,
    grant: NetGrant,
    client: reqwest::Client,
    request_timeout: Duration,
    requests_sent: usize,
    /// Charged by every concurrent request as its body is read.
    response_bytes: AtomicUsize,
    max_requests: usize,
    max_response_bytes: usize,
}

impl Session {
    /// The session `manifold`'s granted `net` axis allows, or `None` when it
    /// allows nothing and the plugin runs as a sealed one.
    pub(super) fn for_grant(coordinate: &str, manifold: &Manifold) -> Result<Option<Self>> {
        Self::with_lookup(coordinate, manifold, Arc::new(SystemLookup))
    }

    pub(super) fn with_lookup(
        coordinate: &str,
        manifold: &Manifold,
        lookup: Arc<dyn Lookup>,
    ) -> Result<Option<Self>> {
        let grant = net_grant(&manifold.net)
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("plugin {coordinate}'s network grant"))?;
        if grant == NetGrant::Sealed {
            return Ok(None);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(Guarded(lookup)))
            .build()
            .context("building the plugin HTTP client")?;
        let request_timeout = manifold.http_timeout_ms.map_or(REQUEST_TIMEOUT, |ms| {
            Duration::from_millis(ms).min(REQUEST_TIMEOUT)
        });
        Ok(Some(Self {
            coordinate: coordinate.to_owned(),
            grant,
            client,
            request_timeout,
            requests_sent: 0,
            response_bytes: AtomicUsize::new(0),
            max_requests: MAX_REQUESTS_PER_CALL,
            max_response_bytes: MAX_RESPONSE_BYTES_PER_CALL,
        }))
    }

    /// Answers every request of one round; one past the call's request or
    /// response-byte budget gets an error result without being sent.
    pub(super) async fn serve(
        &mut self,
        requests: Vec<Request>,
        deadline: Instant,
    ) -> Map<String, Value> {
        let mut room = self.max_requests.saturating_sub(self.requests_sent);
        let plan: Vec<(Request, Option<Refusal>)> = requests
            .into_iter()
            .map(|request| {
                let refusal = if room == 0 {
                    Some(Refusal::RequestLimit)
                } else {
                    room -= 1;
                    None
                };
                (request, refusal)
            })
            .collect();
        self.requests_sent += plan.iter().filter(|(_, refusal)| refusal.is_none()).count();
        let this = &*self;
        let answers: Vec<(String, Result<Value, String>)> = futures::stream::iter(plan)
            .map(|(request, refusal)| async move {
                let id = request.id.clone();
                // The budget is checked as each request starts: earlier
                // ones of this round may already have used it up.
                let refusal = refusal.or_else(|| {
                    (this.response_bytes.load(Ordering::Relaxed) >= this.max_response_bytes)
                        .then_some(Refusal::ResponseBudget)
                });
                let answer = match refusal {
                    Some(why) => Err(why.message().to_owned()),
                    None => this.fetch(request, deadline).await,
                };
                (id, answer)
            })
            .buffer_unordered(MAX_IN_FLIGHT)
            .collect()
            .await;
        let mut results = Map::new();
        for (id, answer) in answers {
            results.insert(
                id,
                answer.unwrap_or_else(|error| {
                    tracing::debug!(plugin = %self.coordinate, %error, "plugin http request refused or failed");
                    json!({"error": error})
                }),
            );
        }
        results
    }

    async fn fetch(&self, request: Request, deadline: Instant) -> Result<Value, String> {
        let wall = tokio::time::Instant::from_std(deadline);
        let limit = (tokio::time::Instant::now() + self.request_timeout).min(wall);
        match tokio::time::timeout_at(limit, self.exchange(request)).await {
            Ok(result) => result,
            Err(_) => Err("the http request timed out".to_owned()),
        }
    }

    /// Admits `url` and, for an IP literal, the address itself; a name's
    /// addresses are checked by [`Guarded`] as the connection resolves them.
    fn check(&self, url: &Url) -> Result<(), String> {
        let admission = admit(&self.grant, url, &self.coordinate).inspect_err(|why| {
            tracing::warn!(plugin = %self.coordinate, url = %url, "{why}");
        })?;
        if let Some(ip) = admission.literal {
            if !addresses_allowed(admission.explicit, &[ip]) {
                return Err(internal_refusal(None, &ip));
            }
        }
        Ok(())
    }

    async fn exchange(&self, request: Request) -> Result<Value, String> {
        let Request {
            method,
            url,
            headers,
            body,
            ..
        } = request;
        self.check(&url)?;
        let mut call = self.client.request(method, url.clone());
        for (name, value) in headers {
            call = call.header(name, value);
        }
        if let Some(body) = body {
            call = call.body(body);
        }
        let mut response = call
            .send()
            .await
            .map_err(|error| send_error(&url, &error))?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "the http response was cut off".to_owned())?
        {
            let charged = self
                .response_bytes
                .fetch_add(chunk.len(), Ordering::Relaxed);
            if charged + chunk.len() > self.max_response_bytes {
                return Err("this call used up its http response budget".to_owned());
            }
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "the http response body is larger than {MAX_RESPONSE_BYTES} bytes"
                ));
            }
        }
        let mut names = Map::new();
        for (name, value) in response.headers() {
            let Ok(value) = value.to_str() else { continue };
            match names.get_mut(name.as_str()) {
                Some(Value::String(joined)) => {
                    joined.push_str(", ");
                    joined.push_str(value);
                }
                _ => {
                    names.insert(name.as_str().to_owned(), Value::String(value.to_owned()));
                }
            }
        }
        let mut answer = json!({
            "status": status.as_u16(),
            "headers": names,
        });
        match String::from_utf8(bytes) {
            Ok(text) => answer["body"] = Value::String(text),
            Err(error) => {
                answer["body_base64"] = Value::String(
                    base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
                )
            }
        }
        Ok(answer)
    }
}

/// One sentence for a failed send: the guard's own refusal when it refused
/// the resolved addresses.
fn send_error(url: &Url, error: &reqwest::Error) -> String {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(cause) = source {
        if let Some(internal) = cause.downcast_ref::<InternalAddress>() {
            return internal.0.clone();
        }
        source = cause.source();
    }
    let host = url.host_str().unwrap_or_default();
    if error.is_timeout() {
        format!("the http request to {host} timed out")
    } else if error.is_connect() {
        format!("{host} could not be reached")
    } else {
        format!("the http request to {host} failed")
    }
}

#[cfg(test)]
#[path = "http_calls_tests.rs"]
mod tests;
