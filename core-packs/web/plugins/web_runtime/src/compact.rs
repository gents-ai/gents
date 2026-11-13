//! Response bytes are consumed once. Only bounded extraction/search records
//! cross the guest's next host-HTTP round, not copies of fetched documents.

use std::collections::HashSet;

use donsetch_core::{
    crawl::{self, Robots},
    extract,
    search::engines,
};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use super::{Task, extract_options, header};

fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(crate) fn record(
    task: &Task,
    status: u16,
    headers: &Value,
    body: &[u8],
    args: &Value,
) -> Value {
    let mut result = json!({"task":task.value(),"status":status});
    match task.purpose.as_str() {
        "search" => {
            let hits = if (200..300).contains(&status) {
                engines::parse(
                    task.engine.as_deref().unwrap_or(""),
                    &String::from_utf8_lossy(body),
                )
            } else {
                Vec::new()
            };
            let total = hits.len();
            result["hits"] = json!(hits.into_iter().filter(|h| h.url.len() <= 2048).take(20).map(|h| json!({
                "title":cut(&h.title,512),"url":h.url,"snippet":cut(&h.snippet,1500),"rank":h.rank,"published":h.published
            })).collect::<Vec<_>>());
            result["hits_total"] = json!(total);
        }
        "robots" => {
            let origin = Url::parse(&task.url)
                .map(|u| u.origin().ascii_serialization())
                .unwrap_or_default();
            let rules = if status == 429 {
                Robots {
                    unreachable: true,
                    ..Robots::disallow_all()
                }
            } else {
                crawl::robots_for_origin(status, body, &origin)
            };
            result["robots"] = json!({"sitemaps":rules.sitemaps,"allow":rules.allow,"disallow":rules.disallow,"crawl_delay":rules.crawl_delay,"unreachable":rules.unreachable});
        }
        "sitemap" => {
            // map_limit is the page inventory budget. Keep index entries
            // available for bounded traversal even when that budget is tiny.
            const MAX_SITEMAP_ENTRIES: usize = 1000;
            let mut entries = Vec::new();
            let mut overlong_loc = false;
            if (200..300).contains(&status) {
                // A .xml.gz file can be compressed independently of HTTP content encoding.
                let decoded = crawl::maybe_gunzip(body);
                let xml = String::from_utf8_lossy(&decoded);
                overlong_loc = sitemap_has_overlong_loc(&xml);
                crawl::parse_sitemap(&xml, &mut entries, MAX_SITEMAP_ENTRIES + 1);
            }
            result["sitemap_truncated"] =
                json!(overlong_loc || entries.len() > MAX_SITEMAP_ENTRIES);
            result["sitemap_entries"] = json!(
                entries
                    .into_iter()
                    .take(MAX_SITEMAP_ENTRIES)
                    .map(|e| json!({"loc":e.loc,"is_index":e.is_index}))
                    .collect::<Vec<_>>()
            );
        }
        "fetch" | "crawl_page" => {
            result["fetch_result"] = fetch_result(task, status, headers, body, args);
            if task.purpose == "crawl_page" && (200..300).contains(&status) {
                let (links, truncated) = links(body, &task.url);
                result["links"] = json!(links);
                result["links_truncated"] = json!(truncated);
            }
        }
        _ => {}
    }
    result
}

fn sitemap_has_overlong_loc(xml: &str) -> bool {
    let mut rest = xml;
    while let Some(open) = rest.find("<loc") {
        rest = &rest[open + 4..];
        let Some(gt) = rest.find('>') else {
            return false;
        };
        rest = &rest[gt + 1..];
        let Some(end) = rest.find("</loc>") else {
            return false;
        };
        if rest[..end].trim().len() > 2048 {
            return true;
        }
        rest = &rest[end + 6..];
    }
    false
}

pub(crate) fn robots(record: &Value) -> Robots {
    let Some(r) = record.get("robots") else {
        return Robots {
            unreachable: true,
            ..Robots::disallow_all()
        };
    };
    let strings = |key: &str| {
        r[key]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    Robots {
        sitemaps: strings("sitemaps"),
        allow: strings("allow"),
        disallow: strings("disallow"),
        crawl_delay: r["crawl_delay"].as_f64(),
        unreachable: r["unreachable"].as_bool().unwrap_or(true),
    }
}

fn fetch_result(task: &Task, status: u16, headers: &Value, body: &[u8], args: &Value) -> Value {
    let fail = |code: &str, message: String| json!({"url":task.url,"ok":false,"status":status,"code":code,"error":message,"source":"http","content_complete":false});
    if !(200..300).contains(&status) {
        return fail("http.status", format!("HTTP status {status}"));
    }
    let ct = header(headers, "content-type").unwrap_or("");
    let lower = ct.to_ascii_lowercase();
    let leading = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let html_like = body.get(leading) == Some(&b'<');
    let pdf = body.starts_with(b"%PDF-") || lower.contains("pdf");
    if !pdf
        && !html_like
        && !lower.is_empty()
        && !lower.starts_with("text/")
        && !lower.contains("json")
        && !lower.contains("xml")
    {
        return fail(
            "format.unsupported",
            format!("Cannot extract {ct}; this tool reads HTML, text, JSON, XML, and text PDFs."),
        );
    }
    if body.contains(&0) && !pdf && !lower.starts_with("text/") {
        return fail(
            "format.binary",
            "Response is binary and has no supported text format.".into(),
        );
    }
    let mut options = extract_options(args);
    if task.purpose == "crawl_page" {
        options.max_chars = Some(args["per_page_max"].as_u64().unwrap_or(8000).min(40000) as usize);
    }
    match extract::extract(body, ct, &task.url, &options) {
        Ok(ex) => {
            let projection = options.toc
                || options.section.is_some()
                || options.selector.is_some()
                || options.must_contain.is_some()
                || ex.focus_omitted;
            let complete =
                ex.next_offset.is_none() && !ex.thin && ex.partial.is_none() && !projection;
            let read_status = if options.must_contain.is_some() {
                "probe"
            } else if ex.thin {
                "thin"
            } else if !complete {
                "partial"
            } else {
                "content"
            };
            json!({"url":task.url,"ok":true,"status":status,"title":ex.title,"content":ex.markdown,
                "content_kind":format!("{:?}",ex.content_kind).to_lowercase(),"content_hash":format!("sha256:{:x}",Sha256::digest(body)),
                "content_complete":complete,"read_status":read_status,"next_offset":ex.next_offset,"total_chars":ex.total_chars,
                "thin":ex.thin,"quality":ex.quality,"via":ex.via,"source":"http","depth":task.depth,
                "partial":ex.partial,"focus_omitted":ex.focus_omitted,"byline":ex.byline,"published":ex.published})
        }
        Err(error) => fail("extract.failed", error.to_string()),
    }
}

fn links(body: &[u8], base: &str) -> (Vec<String>, bool) {
    let Ok(base) = Url::parse(base) else {
        return (Vec::new(), false);
    };
    let doc = Html::parse_document(&String::from_utf8_lossy(body));
    let selector = Selector::parse("a[href]").expect("static selector");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for link in doc.select(&selector) {
        let Some(href) = link.value().attr("href") else {
            continue;
        };
        let Ok(mut url) = base.join(href) else {
            continue;
        };
        url.set_fragment(None);
        if url.scheme() != "https"
            || url.origin() != base.origin()
            || url.as_str().len() > 2048
            || !url.username().is_empty()
            || url.password().is_some()
        {
            continue;
        }
        if seen.insert(url.to_string()) {
            if out.len() == 500 {
                return (out, true);
            }
            out.push(url.to_string());
        }
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_are_same_origin_deduplicated_and_have_no_fragments() {
        let (urls, _) = links(
            b"<a href='/a#one'>1</a><a href='/a#two'>2</a><a href='https://other.example/b'>3</a>",
            "https://site.example/start",
        );
        assert_eq!(urls, ["https://site.example/a"]);
    }
    #[test]
    fn robots_429_is_unreachable_not_permission_to_crawl() {
        let task = Task {
            id: "r1".into(),
            url: "https://site.example/robots.txt".into(),
            purpose: "robots".into(),
            engine: None,
            query: None,
            depth: 0,
            redirects: 0,
        };
        let out = record(&task, 429, &json!({}), b"Too many requests", &json!({}));
        assert!(robots(&out).unreachable);
        assert!(!robots(&out).allowed("/"));
    }
}
