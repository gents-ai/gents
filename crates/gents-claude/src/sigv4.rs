//! AWS Signature Version 4 request signing, as the AWS SDK for JavaScript
//! bundled in Claude Code 2.1.311 does it (`chunk-qz9v3d54.js`, `SignatureV4`).
//!
//! [`Signer::sign`] takes the request exactly as the transport will write it
//! (the path as it sits on the wire, the headers the client built, the body
//! bytes) and returns the headers the signature adds: `x-amz-date`,
//! `x-amz-security-token` when a session token exists, `x-amz-content-sha256`
//! and `authorization`. The signed header set is every given header plus `host`
//! and the added `x-amz-*` ones, minus the SDK's unsignable names (and the
//! `proxy-*` and `sec-*` prefixes). Headers a client adds after signing, such
//! as `x-client-request-id`, are not part of the request given here and so are
//! not signed. The canonical path is the normalized wire path escaped once
//! more (so a `%2F` in a model ARN signs as `%252F`), the canonical query is
//! escaped, sorted and joined. The derived signing key is cached per
//! (date, region, service) in a lock-free map bounded at [`KEY_CACHE_CAP`].
//! The HMAC and SHA-256 primitives are the `hmac` and `sha2` crates.

use std::fmt;
use std::sync::Arc;

use hmac::{Hmac, Mac};
use kovan_map::HopscotchMap;
use rapidhash::fast::RandomState;
use sha2::{Digest, Sha256};

use crate::headers::HeaderValue;

/// Most derived signing keys kept per signer; one is derived per day, region and service.
pub const KEY_CACHE_CAP: usize = 8;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const EMPTY_BODY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Headers the SDK never signs (`ALWAYS_UNSIGNABLE_HEADERS`).
const UNSIGNABLE: [&str; 15] = [
    "authorization",
    "cache-control",
    "connection",
    "expect",
    "from",
    "keep-alive",
    "max-forwards",
    "pragma",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "x-amzn-trace-id",
];

/// Why a request could not be signed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SigV4Error {
    /// The secret key could not seed the HMAC.
    #[error("the AWS secret access key cannot be used to sign")]
    Key,
    /// The access key id or the secret access key is empty.
    #[error("the AWS credentials need an access key id and a secret access key")]
    EmptyCredentials,
}

/// Static AWS credentials. The secrets never print.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl Credentials {
    /// Credentials for `access_key_id`; a blank session token counts as none.
    pub fn new(
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
    ) -> Result<Self, SigV4Error> {
        if access_key_id.trim().is_empty() || secret_access_key.trim().is_empty() {
            return Err(SigV4Error::EmptyCredentials);
        }
        Ok(Self {
            access_key_id: access_key_id.trim().to_owned(),
            secret_access_key: secret_access_key.trim().to_owned(),
            session_token: session_token
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_owned),
        })
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// The region and service a signature is scoped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope<'a> {
    /// AWS region, such as `us-west-2`.
    pub region: &'a str,
    /// Signing service name, such as `bedrock`.
    pub service: &'a str,
}

/// The request to sign, as the transport will write it.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// The path as it sits on the wire (already percent-encoded once).
    pub path: &'a str,
    /// The decoded query pairs.
    pub query: &'a [(&'a str, &'a str)],
    /// The `Host` header value: the authority without userinfo.
    pub host: &'a str,
    /// The headers the client built, before signing.
    pub headers: &'a [(&'a str, &'a str)],
    /// The body bytes.
    pub body: &'a [u8],
}

type KeyCache = HopscotchMap<(String, String, String), Arc<[u8; 32]>, RandomState>;

/// Signs requests with one set of credentials.
pub struct Signer {
    credentials: Credentials,
    apply_checksum: bool,
    keys: KeyCache,
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signer")
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// A signer that adds `x-amz-content-sha256`, as the SDK's default does.
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            apply_checksum: true,
            keys: HopscotchMap::with_hasher(RandomState::new()),
        }
    }

    /// Whether to add (and sign) the `x-amz-content-sha256` header.
    #[must_use]
    pub fn with_checksum(mut self, apply: bool) -> Self {
        self.apply_checksum = apply;
        self
    }

    /// The derived signing key of `date` (`YYYYMMDD`) in `scope`.
    fn signing_key(&self, date: &str, scope: &Scope<'_>) -> Result<Arc<[u8; 32]>, SigV4Error> {
        let key = (
            date.to_owned(),
            scope.region.to_owned(),
            scope.service.to_owned(),
        );
        if let Some(cached) = self.keys.get(&key) {
            return Ok(cached);
        }
        let mut derived = hmac_sha256(
            format!("AWS4{}", self.credentials.secret_access_key).as_bytes(),
            date.as_bytes(),
        )?;
        for part in [scope.region, scope.service, "aws4_request"] {
            derived = hmac_sha256(&derived, part.as_bytes())?;
        }
        let derived = Arc::new(derived);
        if self.keys.len() >= KEY_CACHE_CAP {
            self.keys.clear();
        }
        self.keys.insert(key, derived.clone());
        Ok(derived)
    }

    /// The headers the signature adds to `request`, in the SDK's insertion
    /// order. `unix_seconds` is the signing time.
    pub fn sign(
        &self,
        request: &Request<'_>,
        scope: &Scope<'_>,
        unix_seconds: u64,
    ) -> Result<Vec<(&'static str, HeaderValue)>, SigV4Error> {
        let (long_date, short_date) = amz_dates(unix_seconds);
        // getPayloadHash: a supplied content hash is taken as the payload hash.
        let supplied_checksum = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-amz-content-sha256"))
            .map(|(_, value)| *value);
        let payload_hash = match supplied_checksum {
            Some(value) => value.to_owned(),
            None if request.body.is_empty() => EMPTY_BODY_SHA256.to_owned(),
            None => hex(&Sha256::digest(request.body)),
        };
        let supplied_checksum = supplied_checksum.is_some();

        // prepareRequest: the client's own authorization and date headers never survive.
        let mut signable: Vec<(String, String)> = Vec::with_capacity(request.headers.len() + 4);
        let mut put = |name: &str, value: &str| {
            let name = name.to_ascii_lowercase();
            match signable.iter_mut().find(|(have, _)| *have == name) {
                Some(entry) => value.clone_into(&mut entry.1),
                None => signable.push((name, value.to_owned())),
            }
        };
        for (name, value) in request.headers {
            if !matches!(
                name.to_ascii_lowercase().as_str(),
                "connection" | "authorization" | "x-amz-date" | "date" | "host"
            ) {
                put(name, value);
            }
        }
        put("host", request.host);
        put("x-amz-date", &long_date);
        if let Some(token) = &self.credentials.session_token {
            put("x-amz-security-token", token);
        }
        if self.apply_checksum && !supplied_checksum {
            put("x-amz-content-sha256", &payload_hash);
        }

        let mut canonical_headers: Vec<(String, String)> = signable
            .iter()
            .filter(|(name, _)| !unsignable(name))
            .map(|(name, value)| (name.clone(), normalize_value(value)))
            .collect();
        canonical_headers.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let signed_headers = canonical_headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");

        let mut canonical = String::with_capacity(256);
        canonical.push_str(request.method);
        canonical.push('\n');
        canonical.push_str(&canonical_path(request.path));
        canonical.push('\n');
        canonical.push_str(&canonical_query(request.query));
        canonical.push('\n');
        for (name, value) in &canonical_headers {
            canonical.push_str(name);
            canonical.push(':');
            canonical.push_str(value);
            canonical.push('\n');
        }
        canonical.push('\n');
        canonical.push_str(&signed_headers);
        canonical.push('\n');
        canonical.push_str(&payload_hash);

        let credential_scope = format!(
            "{short_date}/{}/{}/aws4_request",
            scope.region, scope.service
        );
        let to_sign = format!(
            "{ALGORITHM}\n{long_date}\n{credential_scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let key = self.signing_key(&short_date, scope)?;
        let signature = hex(&hmac_sha256(key.as_slice(), to_sign.as_bytes())?);

        let plain = |value: String| HeaderValue {
            value,
            sensitive: false,
        };
        let secret = |value: String| HeaderValue {
            value,
            sensitive: true,
        };
        let mut added = Vec::with_capacity(4);
        added.push(("x-amz-date", plain(long_date)));
        if let Some(token) = &self.credentials.session_token {
            added.push(("x-amz-security-token", secret(token.clone())));
        }
        if self.apply_checksum && !supplied_checksum {
            added.push(("x-amz-content-sha256", plain(payload_hash)));
        }
        added.push((
            "authorization",
            secret(format!(
                "{ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
                self.credentials.access_key_id
            )),
        ));
        Ok(added)
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<[u8; 32], SigV4Error> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| SigV4Error::Key)?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().into())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn unsignable(name: &str) -> bool {
    UNSIGNABLE.contains(&name) || name.starts_with("proxy-") || name.starts_with("sec-")
}

/// JavaScript's `\s`: the characters `String#trim` and `/\s+/` treat as space.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `value.trim().replace(/\s+/g, " ")`.
fn normalize_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut gap = false;
    for c in value.trim_matches(js_space).chars() {
        if js_space(c) {
            gap = true;
        } else {
            if gap {
                out.push(' ');
                gap = false;
            }
            out.push(c);
        }
    }
    out
}

/// The SDK's `escapeUri`: `encodeURIComponent` plus `!'()*`, so only
/// `A-Za-z0-9-_.~` stay literal.
pub fn escape_uri(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// `getCanonicalPath` with `uriEscapePath`: dot segments resolved, then the
/// path escaped with its slashes kept.
fn canonical_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut normalized = String::with_capacity(path.len());
    if path.starts_with('/') {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    if !segments.is_empty() && path.ends_with('/') {
        normalized.push('/');
    }
    escape_uri(&normalized).replace("%2F", "/")
}

/// The pairs of a raw URL query decoded as `URLSearchParams` decodes them
/// (`+` is a space, `%XX` a byte), which is what the SDK signs.
pub fn query_pairs(raw: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// `getCanonicalQuery`: escaped pairs ordered by escaped name, then by pair.
fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(name, value)| {
            let name = escape_uri(name);
            let pair = format!("{name}={}", escape_uri(value));
            (name, pair)
        })
        .collect();
    pairs.sort_unstable();
    pairs
        .into_iter()
        .map(|(_, pair)| pair)
        .collect::<Vec<_>>()
        .join("&")
}

/// `(YYYYMMDDTHHMMSSZ, YYYYMMDD)` of a Unix time in seconds.
pub fn amz_dates(unix_seconds: u64) -> (String, String) {
    let days = unix_seconds / 86_400;
    let rest = unix_seconds % 86_400;
    // Howard Hinnant's civil-from-days, over days since 1970-01-01.
    let z = i64::try_from(days).unwrap_or(i64::MAX / 2) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let short = format!("{year:04}{month:02}{day:02}");
    let long = format!(
        "{short}T{:02}{:02}{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    );
    (long, short)
}

#[cfg(test)]
#[path = "sigv4_tests.rs"]
mod tests;
