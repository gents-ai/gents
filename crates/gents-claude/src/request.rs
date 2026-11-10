//! The HTTP/1.1 request head exactly as Bun 1.4.4's fetch writes it for
//! Claude Code, the body padding and the request-body gzip decision.

use std::fmt;

/// WebKit's common header names in canonical spelling, one per line of
/// `src/jsc/bindings/webcore/HTTPHeaderNames.in` (Bun tag bun-v1.4.4).
pub const COMMON_HEADER_NAMES: &[&str] = &[
    "Accept",
    "Accept-Charset",
    "Accept-Language",
    "Accept-Encoding",
    "Accept-Ranges",
    "Access-Control-Allow-Credentials",
    "Access-Control-Allow-Headers",
    "Access-Control-Allow-Methods",
    "Access-Control-Allow-Origin",
    "Access-Control-Expose-Headers",
    "Access-Control-Max-Age",
    "Access-Control-Request-Headers",
    "Access-Control-Request-Method",
    "Age",
    "Authorization",
    "Cache-Control",
    "Connection",
    "Content-Disposition",
    "Content-Encoding",
    "Content-Language",
    "Content-Length",
    "Content-Location",
    "Content-Security-Policy",
    "Content-Security-Policy-Report-Only",
    "Content-Type",
    "Content-Range",
    "Cookie",
    "Cookie2",
    "Cross-Origin-Embedder-Policy",
    "Cross-Origin-Embedder-Policy-Report-Only",
    "Cross-Origin-Opener-Policy",
    "Cross-Origin-Opener-Policy-Report-Only",
    "Cross-Origin-Resource-Policy",
    "Date",
    "DNT",
    "Default-Style",
    "ETag",
    "Expect",
    "Expires",
    "Host",
    "If-Match",
    "If-Modified-Since",
    "If-None-Match",
    "If-Range",
    "If-Unmodified-Since",
    "Keep-Alive",
    "Last-Event-ID",
    "Last-Modified",
    "Link",
    "Location",
    "Origin",
    "Ping-From",
    "Ping-To",
    "Purpose",
    "Pragma",
    "Proxy-Authorization",
    "Proxy-Connection",
    "Range",
    "Referer",
    "Referrer-Policy",
    "Refresh",
    "Report-To",
    "Sec-Fetch-Dest",
    "Sec-Fetch-Mode",
    "Sec-WebSocket-Accept",
    "Sec-WebSocket-Extensions",
    "Sec-WebSocket-Key",
    "Sec-WebSocket-Protocol",
    "Sec-WebSocket-Version",
    "Server-Timing",
    "Service-Worker",
    "Service-Worker-Allowed",
    "Service-Worker-Navigation-Preload",
    "Set-Cookie",
    "Set-Cookie2",
    "SourceMap",
    "TE",
    "Timing-Allow-Origin",
    "Trailer",
    "Transfer-Encoding",
    "Upgrade",
    "Upgrade-Insecure-Requests",
    "User-Agent",
    "Vary",
    "Via",
    "X-Content-Type-Options",
    "X-DNS-Prefetch-Control",
    "X-Frame-Options",
    "X-SourceMap",
    "X-XSS-Protection",
    "X-Temp-Tablet",
    "Icy-MetaInt",
    "Icy-Metadata",
];

/// Smallest body, in UTF-16 units, that Claude Code gzips (`szt`).
pub const GZIP_MIN_UTF16_UNITS: usize = 4096;

/// The only host whose requests Claude Code gzips.
const GZIP_HOST: &str = "api.anthropic.com";

/// Bodies the API answers with a JSON parse failure when it cannot read gzip.
const GZIP_REJECTION_MESSAGES: [&str; 2] = [
    "The request body is not valid JSON",
    "Failed to parse request: could not parse request body as JSON",
];

/// The canonical spelling of a common header name, else `name` itself.
pub fn wire_name(name: &str) -> &str {
    COMMON_HEADER_NAMES
        .iter()
        .find(|common| common.eq_ignore_ascii_case(name))
        .copied()
        .unwrap_or(name)
}

fn is_http_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// One header of a head, with the credential flag.
#[derive(Clone, PartialEq, Eq)]
struct Pair {
    name: String,
    value: String,
    sensitive: bool,
}

/// A request line and its ordered headers, without `Content-Length`.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestHead {
    request_line: String,
    pairs: Vec<Pair>,
}

impl fmt::Debug for RequestHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        for pair in &self.pairs {
            if pair.sensitive {
                list.entry(&format_args!("{}: <redacted>", pair.name));
            } else {
                list.entry(&format_args!("{}: {}", pair.name, pair.value));
            }
        }
        list.finish()?;
        write!(f, " {}", self.request_line)
    }
}

impl RequestHead {
    /// Renders the head Bun's fetch writes: user headers as WebKit stores and sorts them, then Bun's defaults.
    pub fn new<'a, I>(method: &str, path: &str, host: &str, headers: I, gzip: bool) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str, bool)>,
    {
        let mut pairs: Vec<Pair> = Vec::new();
        // FetchHeaders.cpp collectEntries and HTTPHeaderMap.cpp: a repeated name replaces only the
        // value and keeps the spelling it was first stored with.
        for (name, value, sensitive) in headers {
            let name = wire_name(name);
            let value = value.trim_matches(is_http_space);
            match pairs.iter_mut().find(|p| p.name.eq_ignore_ascii_case(name)) {
                Some(pair) => {
                    value.clone_into(&mut pair.value);
                    pair.sensitive = sensitive;
                }
                None => pairs.push(Pair {
                    name: name.to_owned(),
                    value: value.to_owned(),
                    sensitive,
                }),
            }
        }
        // http/lib.rs build_request always drops the user's framing headers.
        pairs.retain(|p| {
            !p.name.eq_ignore_ascii_case("Content-Length")
                && !p.name.eq_ignore_ascii_case("Transfer-Encoding")
        });
        // FetchHeaders.cpp collectEntries sorts by code point over the emitted names.
        pairs.sort_unstable_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        let has =
            |pairs: &[Pair], name: &str| pairs.iter().any(|p| p.name.eq_ignore_ascii_case(name));
        let mut defaults: Vec<(&str, &str)> = Vec::with_capacity(6);
        // runtime/webcore/fetch.rs adds Content-Encoding before http/lib.rs build_request's defaults.
        if gzip && !has(&pairs, "Content-Encoding") {
            defaults.push(("Content-Encoding", "gzip"));
        }
        if !has(&pairs, "Connection") {
            defaults.push(("Connection", "keep-alive"));
        }
        if !has(&pairs, "User-Agent") {
            defaults.push(("User-Agent", "Bun/1.4.4"));
        }
        if !has(&pairs, "Accept") {
            defaults.push(("Accept", "*/*"));
        }
        if !has(&pairs, "Host") {
            defaults.push(("Host", host));
        }
        if !has(&pairs, "Accept-Encoding") {
            defaults.push(("Accept-Encoding", "gzip, deflate, br, zstd"));
        }
        pairs.extend(defaults.into_iter().map(|(name, value)| Pair {
            name: name.to_owned(),
            value: value.to_owned(),
            sensitive: false,
        }));

        Self {
            request_line: format!("{method} {path} HTTP/1.1\r\n"),
            pairs,
        }
    }

    /// The exact head bytes, ending with `Content-Length` and the blank line.
    pub fn encode(&self, content_length: usize) -> Vec<u8> {
        const LENGTH_LINE: &str = "Content-Length: ";
        // usize::MAX has 20 digits.
        let size = self.request_line.len()
            + self
                .pairs
                .iter()
                .map(|p| p.name.len() + p.value.len() + 4)
                .sum::<usize>()
            + LENGTH_LINE.len()
            + 20
            + 4;
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(self.request_line.as_bytes());
        for pair in &self.pairs {
            out.extend_from_slice(pair.name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(pair.value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(LENGTH_LINE.as_bytes());
        out.extend_from_slice(content_length.to_string().as_bytes());
        out.extend_from_slice(b"\r\n\r\n");
        out
    }

    /// The headers in wire order, without `Content-Length`.
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs
            .iter()
            .map(|p| (p.name.as_str(), p.value.as_str()))
    }
}

/// How many padding characters to append for `random` in `[0, 1)` (`floor(random * 257)`).
pub fn padding_len(random: f64) -> usize {
    (random * 257.0) as usize
}

/// Appends `\n` and `len` padding characters, a space when `space()` is true else a tab (`VTt`).
pub fn pad_body<F>(body: &mut Vec<u8>, len: usize, mut space: F)
where
    F: FnMut() -> bool,
{
    body.reserve(len + 1);
    body.push(b'\n');
    body.extend((0..len).map(|_| if space() { b' ' } else { b'\t' }));
}

/// The JavaScript `.length` of `s`: its UTF-16 code unit count.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Whether Claude Code gzips the body (`szt`): first-party host, big enough, direct, not latched off.
pub fn gzip_body(host: &str, body_utf16_len: usize, proxied: bool, latched_off: bool) -> bool {
    host == GZIP_HOST && body_utf16_len >= GZIP_MIN_UTF16_UNITS && !proxied && !latched_off
}

/// Whether the API rejected a gzip body as unparseable JSON.
///
/// On `true` the caller latches gzip off for the process and resends the same request uncompressed.
pub fn gzip_rejected(
    status: u16,
    request_id: Option<&str>,
    has_cf_ray: bool,
    error_message: &str,
) -> bool {
    let from_anthropic = has_cf_ray || request_id.is_some_and(|id| id.starts_with("req_"));
    status == 400
        && from_anthropic
        && (GZIP_REJECTION_MESSAGES.contains(&error_message) || is_bad_json(error_message))
}

/// `/^bad json: (?:invalid character |unexpected EOF$|unexpected end of JSON input$|EOF$)/`.
fn is_bad_json(message: &str) -> bool {
    let Some(rest) = message.strip_prefix("bad json: ") else {
        return false;
    };
    rest.starts_with("invalid character ")
        || matches!(
            rest,
            "unexpected EOF" | "unexpected end of JSON input" | "EOF"
        )
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
