//! Portable Google WML SERP parser derived from upstream DonSeTch 4.10.1.
use super::{Hit, sel, text};
use scraper::{ElementRef, Html};

pub fn url(query: &str) -> String {
    let mut url = url::Url::parse("https://www.google.com/wml/search").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("q", query),
        ("sca_esv", "1"),
        ("hl", "en"),
        ("ie", "utf-8"),
        ("oe", "utf-8"),
    ]);
    url.into()
}

fn target(href: &str) -> Option<String> {
    if !(href.starts_with("https://") || href.starts_with("http://") || href.starts_with("/url?")) {
        return None;
    }
    let base = url::Url::parse("https://www.google.com").unwrap();
    let wrapped = base.join(href).ok()?;
    let target = if matches!(wrapped.host_str(), Some("google.com" | "www.google.com"))
        && wrapped.path() == "/url"
    {
        let (_, value) = wrapped
            .query_pairs()
            .find(|(key, _)| key == "q" || key == "url")?;
        url::Url::parse(&value).ok()?
    } else {
        wrapped
    };
    if !matches!(target.scheme(), "http" | "https")
        || target.host_str().is_none()
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return None;
    }
    if matches!(target.host_str(), Some("google.com" | "www.google.com"))
        && matches!(
            target.path(),
            "/search" | "/wml/search" | "/url" | "/preferences" | "/sorry" | "/sorry/"
        )
    {
        return None;
    }
    Some(target.into())
}

pub fn parse(doc: &Html) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let link = sel("a.fuLhoc[href]");
    let title = sel("span.CVA68e");
    let snippet = sel("div.taTFJ span.FrIlee");
    for block in doc.select(&sel("div.zMzFAb")) {
        let Some(a) = block.select(&link).next() else {
            continue;
        };
        let Some(t) = a.select(&title).next() else {
            continue;
        };
        let body = block
            .select(&snippet)
            .map(text)
            .collect::<Vec<_>>()
            .join(" ");
        push(&mut hits, &mut seen, a, text(t), body);
    }
    // Structural fallback used by DDGS: title div followed by a table-bearing
    // snippet div. Do not fall back to all anchors (navigation is not a result).
    let span = sel("span");
    let table = sel("table");
    for block in doc.select(&sel("div")) {
        let children: Vec<_> = block.children().filter_map(ElementRef::wrap).collect();
        if children.len() < 2
            || children[0].value().name() != "div"
            || children[1].value().name() != "div"
            || children[1].select(&table).next().is_none()
        {
            continue;
        }
        let Some(a) = children[0]
            .children()
            .filter_map(ElementRef::wrap)
            .find(|el| el.value().name() == "a" && el.value().attr("href").is_some())
        else {
            continue;
        };
        // Keep the title inside the result anchor, not a neighbouring UI label.
        let Some(t) = a.select(&span).next() else {
            continue;
        };
        push(&mut hits, &mut seen, a, text(t), text(children[1]));
    }
    hits
}

fn push(
    hits: &mut Vec<Hit>,
    seen: &mut std::collections::HashSet<String>,
    a: ElementRef<'_>,
    title: String,
    snippet: String,
) {
    let Some(url) = a.value().attr("href").and_then(target) else {
        return;
    };
    if title.is_empty() || !seen.insert(url.clone()) {
        return;
    }
    hits.push(Hit {
        title,
        url,
        snippet,
        rank: hits.len(),
        published: None,
    });
}
