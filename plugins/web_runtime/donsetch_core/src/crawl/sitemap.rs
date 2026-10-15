//! Portable robots.txt rules and sitemap inventory parsing.
//! Network requests and traversal are handled by the pack runtime.

/// Product token sent in the crawler HTTP User-Agent.
pub const ROBOTS_PRODUCT_TOKEN: &str = "GentsWebRuntime";

/// Parsed access rules, sitemap hints, and optional delay.
#[derive(Default, Debug, Clone)]
pub struct Robots {
    pub sitemaps: Vec<String>,
    /// `Disallow:` patterns for the selected agent groups. Longest-match wins.
    pub disallow: Vec<String>,
    /// `Allow:` prefixes (override Disallow on longest match).
    pub allow: Vec<String>,
    /// Site-declared request delay seconds, if any: finite,
    /// non-negative, at most [`MAX_CRAWL_DELAY_SECS`].
    pub crawl_delay: Option<f64>,
    /// True when the file could not be read (RFC 9309
    /// "unreachable") and these rules are the complete disallow that
    /// stands in for it, rather than rules the origin published.
    pub unreachable: bool,
}

/// Upper bound for parsed delay metadata; scheduling belongs to the caller.
pub const MAX_CRAWL_DELAY_SECS: f64 = 60.0;

impl Robots {
    /// `Disallow: /` for every agent: what a reachable file that
    /// closes the whole site would say. RFC 9309 §2.3.1.4: a
    /// robots.txt that cannot be reached ("unreachable") means the
    /// crawler must assume it may access no resource.
    pub fn disallow_all() -> Self {
        Self {
            disallow: vec!["/".into()],
            ..Self::default()
        }
    }

    pub fn parse(body: &str, base_origin: &str) -> Self {
        let mut r = Robots::default();
        let mut groups = Vec::new();
        let mut group = Robots::default();
        let mut priority = 0;
        let mut has_rules = false;
        let mut seen_agent = false;
        for raw in body.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" => {
                    // Consecutive agent lines share rules. A later agent line
                    // after rules starts a new group, even without a blank line.
                    if has_rules {
                        groups.push((priority, std::mem::take(&mut group)));
                        priority = 0;
                        has_rules = false;
                    }
                    seen_agent = true;
                    priority = priority.max(if value.eq_ignore_ascii_case(ROBOTS_PRODUCT_TOKEN) {
                        2
                    } else if value == "*" {
                        1
                    } else {
                        0
                    });
                }
                "allow" | "disallow" | "crawl-delay" => {
                    has_rules = true;
                    if !seen_agent {
                        priority = 1;
                    }
                    match key.as_str() {
                        "allow" if !value.is_empty() => group.allow.push(value.to_owned()),
                        "disallow" if !value.is_empty() => group.disallow.push(value.to_owned()),
                        "crawl-delay" => {
                            if let Some(delay) = value
                                .parse::<f64>()
                                .ok()
                                .filter(|d| d.is_finite() && *d >= 0.0)
                            {
                                group.crawl_delay = Some(
                                    group
                                        .crawl_delay
                                        .unwrap_or(0.0)
                                        .max(delay.min(MAX_CRAWL_DELAY_SECS)),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                "sitemap" => {
                    // Sitemap hints apply independently of user-agent groups.
                    if value.starts_with("http") {
                        r.sitemaps.push(value.to_owned());
                    } else if value.starts_with('/') {
                        r.sitemaps.push(format!("{base_origin}{value}"));
                    }
                }
                _ => {}
            }
        }
        groups.push((priority, group));
        // Named groups replace wildcard groups; repeated matching groups merge.
        let selected = groups.iter().map(|(p, _)| *p).max().unwrap_or(0);
        for (_, group) in groups.into_iter().filter(|(p, _)| *p > 0 && *p == selected) {
            r.allow.extend(group.allow);
            r.disallow.extend(group.disallow);
            if let Some(delay) = group.crawl_delay {
                r.crawl_delay = Some(r.crawl_delay.unwrap_or(0.0).max(delay));
            }
        }
        r
    }

    /// `allowed` for a parsed URL: the rules see the path and, when
    /// there is one, `?` and the query, which is what a rule such as
    /// `Disallow: /*?` is written against.
    pub fn allows_url(&self, url: &url::Url) -> bool {
        match url.query() {
            Some(q) => self.allowed(&format!("{}?{q}", url.path())),
            None => self.allowed(url.path()),
        }
    }

    /// Longest-match rule evaluation over a path, or a path with its
    /// `?query`. Allow beats Disallow at equal length (RFC 9309).
    pub fn allowed(&self, path: &str) -> bool {
        let mut best_dis = 0usize;
        let mut best_allow = 0usize;
        for d in &self.disallow {
            if rule_matches(d, path) && d.len() > best_dis {
                best_dis = d.len();
            }
        }
        for a in &self.allow {
            if rule_matches(a, path) && a.len() > best_allow {
                best_allow = a.len();
            }
        }
        best_allow >= best_dis
    }
}

/// RFC 9309 §2.2.3 rule matching: the rule is a path prefix, `*`
/// matches any run of characters, and a trailing `$` anchors the
/// end. Plain prefix matching read `/*.pdf$` and `/*?` literally, so
/// they matched nothing and respect_robots fetched what the site
/// had disallowed. Iterative two-pointer wildcard match: O(n·m) on a
/// hostile rule, never exponential (both strings are attacker text).
fn rule_matches(rule: &str, path: &str) -> bool {
    // An unanchored rule is a prefix: the same as `rule*` matched
    // against the whole path.
    let pat: Vec<u8> = match rule.strip_suffix('$') {
        Some(r) => r.as_bytes().to_vec(),
        None => {
            let mut v = rule.as_bytes().to_vec();
            v.push(b'*');
            v
        }
    };
    let (p, t) = (pat.as_slice(), path.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    // The last `*` seen and the text position it is currently
    // assumed to cover up to; on a mismatch the star eats one more.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, ti));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// One sitemap URL entry.
#[derive(Debug, Clone)]
pub struct SitemapEntry {
    pub loc: String,
    pub lastmod: Option<String>,
    /// Sitemap-declared priority (0.0-1.0). Used to seed
    /// the frontier relevance score.
    pub priority: Option<f32>,
    /// True when the entry came from a `<sitemap>` index block
    /// (a CHILD sitemap to fetch), false for `<url>` (a page).
    pub is_index: bool,
}

/// Streaming sitemap parser: finds `<url>`/`<sitemap>` blocks
/// and lifts `<loc>` + `<lastmod>` from each. Tolerates stray
/// namespace prefixes and junk between tags.
/// sitemaps.org caps a <loc> at 2048 characters; a lastmod is a W3C
/// datetime. Anything past that is not a sitemap entry, and one
/// 64 MiB <loc> per file × 32 files was 2 GiB of Strings held by the
/// map, the frontier, the IDF table and the resume token.
const MAX_LOC_LEN: usize = 2048;
const MAX_LASTMOD_LEN: usize = 64;

pub fn parse_sitemap(xml: &str, out: &mut Vec<SitemapEntry>, cap: usize) {
    let b = xml.as_bytes();
    let mut pos = 0usize;
    while out.len() < cap {
        let Some((tag_off, close)) = next_block_open(b, pos) else {
            break;
        };
        let Some(end) = find_from(b, close.as_bytes(), tag_off) else {
            break;
        };
        let block = &xml[tag_off..end];
        if let Some(loc) = extract_tag(block, "loc").filter(|l| l.len() <= MAX_LOC_LEN) {
            let lastmod = extract_tag(block, "lastmod").filter(|l| l.len() <= MAX_LASTMOD_LEN);
            // A hostile <priority>NaN</priority> must not reach the
            // frontier score: serde_json serializes non-finite floats
            // as null, and a resume token carrying one would then
            // fail to load back as "expired or unknown" forever.
            let priority = extract_tag(block, "priority").and_then(|s| {
                s.trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|f| f.is_finite() && *f >= 0.0 && *f <= 1.0)
            });
            let is_index = close == "</sitemap>";
            out.push(SitemapEntry {
                loc,
                lastmod,
                priority,
                is_index,
            });
        }
        pos = end + close.len();
    }
}

/// Find the next `<url>` or `<sitemap>` open tag : skipping the
/// `<urlset>`/`<sitemapindex>` containers. Returns
/// (content_offset, close_tag). Tag-name based: immune to
/// prefix collisions like `<urlset>` matching `<url`.
fn next_block_open(b: &[u8], from: usize) -> Option<(usize, String)> {
    let mut pos = from;
    loop {
        let i = find_from(b, b"<", pos)?;
        let name_start = i + 1;
        if name_start >= b.len() {
            return None;
        }
        let name_end = b[name_start..]
            .iter()
            .position(|&c| !c.is_ascii_alphabetic())
            .map(|p| p + name_start)?;
        let name = &b[name_start..name_end];
        let (is_block, close) = match name {
            b"url" => (true, "</url>"),
            b"sitemap" => (true, "</sitemap>"),
            _ => (false, ""),
        };
        let gt = find_from(b, b">", name_end)? + 1;
        pos = gt;
        if is_block {
            return Some((gt, close.to_string()));
        }
    }
}

fn find_from(b: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Extract the text of the first `<tag>` in `block`, trimming.
fn extract_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let s = block.find(&open)?;
    let after = block[s + open.len()..].find('>')? + s + open.len() + 1;
    let e = block[after..].find(&close)? + after;
    let v = xml_text(block[after..e].trim());
    (!v.is_empty()).then_some(v)
}

/// Element text → its value: CDATA unwrapped, the XML entities
/// decoded. The sitemap protocol requires `&` in a `<loc>` to be
/// written `&amp;`, so taking the text literally fetched every
/// URL with a query string as `?q=x&amp;page=2` (an `amp;page`
/// parameter), and a CDATA-wrapped loc (WordPress/Yoast emit
/// them) failed to parse as a URL and silently vanished.
fn xml_text(raw: &str) -> String {
    let raw = raw
        .trim()
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
        .map(str::trim)
        .unwrap_or(raw);
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // Look for the ';' inside the entity window only. Searching
        // the whole remainder and filtering afterwards rescanned to
        // the end on every '&' that has no ';' behind it, O(n²) on a
        // <loc> made of '&'s: a 64 MiB gz-bombed sitemap held the
        // worker for hours. ';' is ASCII, so the index is a boundary.
        let Some(semi) = rest.as_bytes().iter().take(11).position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decompress a gzip'd sitemap body (many sites ship .xml.gz).
pub fn maybe_gunzip(body: &[u8]) -> Vec<u8> {
    if body.len() > 2 && body[0] == 0x1f && body[1] == 0x8b {
        use std::io::Read;
        // Same 64 MiB cap as the fetch decompressor : a malicious
        // .xml.gz sitemap must not OOM the daemon via unbounded
        // decompression.
        const MAX_SITEMAP_DECOMPRESSED: usize = 64 << 20;
        let mut out = Vec::new();
        let dec = flate2::read::GzDecoder::new(body);
        let mut limited = dec.take((MAX_SITEMAP_DECOMPRESSED + 1) as u64);
        if limited.read_to_end(&mut out).is_ok() {
            if out.len() > MAX_SITEMAP_DECOMPRESSED {
                // Bomb: return the raw bytes; XML parse of gzip
                // garbage fails honestly downstream.
                return body.to_vec();
            }
            return out;
        }
    }
    body.to_vec()
}

/// The origin key for robots lookups: `scheme://host[:port]`
/// (RFC 9309 §2.3 puts a URL's rules at its own service's origin).
/// Default ports are already elided by the URL parser, so the key
/// matches the canonical form of the seed's own origin.
pub fn origin_of(url: &url::Url) -> String {
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(p) => format!("{}://{}:{}", url.scheme(), host, p),
        None => format!("{}://{}", url.scheme(), host),
    }
}

pub fn robots_for_origin(status: u16, body: &[u8], origin: &str) -> Robots {
    if status == 200 {
        Robots::parse(&String::from_utf8_lossy(&maybe_gunzip(body)), origin)
    } else if (400..500).contains(&status) {
        Robots::default()
    } else {
        Robots {
            unreachable: true,
            ..Robots::disallow_all()
        }
    }
}
