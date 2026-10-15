//! Host-driven HTTP runtime for the three portable web tools.
//!
//! The guest has no socket API. Every network operation is emitted as a
//! bounded `http_calls` batch and resumed with host-provided results.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};

use base64::Engine as _;
use donsetch_core::{
    crawl::{ROBOTS_PRODUCT_TOKEN, Robots},
    extract::ExtractOptions,
    search::{
        engines,
        intent::{self, Intent},
        query, rank,
    },
};
use serde_json::{Value, json};
use url::Url;

mod compact;

const MAX_ROUND_REQUESTS: usize = 16;
const MAX_ROUNDS: usize = 64;
const MAX_REQUESTS: usize = 256;
const MAX_BODY: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_CALL_CHARS: usize = 500_000;

#[derive(Clone)]
pub(crate) struct Task {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) purpose: String,
    pub(crate) engine: Option<String>,
    pub(crate) query: Option<String>,
    pub(crate) depth: usize,
    pub(crate) redirects: usize,
}

impl Task {
    pub(crate) fn value(&self) -> Value {
        json!({"id":self.id,"url":self.url,"purpose":self.purpose,"engine":self.engine,
            "query":self.query,"depth":self.depth,"redirects":self.redirects})
    }
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            id: v.get("id")?.as_str()?.to_owned(),
            url: v.get("url")?.as_str()?.to_owned(),
            purpose: v.get("purpose")?.as_str()?.to_owned(),
            engine: v.get("engine").and_then(Value::as_str).map(str::to_owned),
            query: v.get("query").and_then(Value::as_str).map(str::to_owned),
            depth: v.get("depth").and_then(Value::as_u64).unwrap_or(0) as usize,
            redirects: v.get("redirects").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
    }
}

/// Execute one invocation of a host-driven web tool.
///
/// First call returns an `http_calls` request batch. Subsequent calls consume
/// `http_results` and the prior `state`, emitting more requests or a final
/// result. The caller's original arguments remain the only durable input.
pub fn run(tool: &str, input: Value) -> Value {
    if !matches!(tool, "web_fetch" | "web_search" | "web_crawl") {
        return error("unsupported_tool", format!("unsupported web tool {tool:?}"));
    }
    let args = input.get("arguments").cloned().unwrap_or_else(|| {
        let mut o = input.as_object().cloned().unwrap_or_default();
        for k in ["state", "http_results", "http_calls"] {
            o.remove(k);
        }
        Value::Object(o)
    });
    if let Err(e) = validate_args(tool, &args) {
        return error("invalid_arguments", e);
    }
    if input.get("http_calls").and_then(Value::as_bool) != Some(true) {
        return error(
            "http_unavailable",
            "host did not grant the HTTP call protocol".into(),
        );
    }
    let mut state = input
        .get("state")
        .cloned()
        .unwrap_or_else(|| initial_state(tool, args.clone()));
    if state.get("tool").and_then(Value::as_str) != Some(tool) {
        return error("invalid_state", "state belongs to a different tool".into());
    }
    let round = state.get("round").and_then(Value::as_u64).unwrap_or(0) as usize;
    let deadline_expired = state
        .get("deadline_unix_ms")
        .and_then(Value::as_u64)
        .is_some_and(|d| now_ms() > d);

    let results = input.get("http_results").and_then(Value::as_object);
    let pending = state
        .get("pending")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut queue: Vec<Value> = state
        .get("queue")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut done: Vec<Value> = state
        .get("done")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut errors: Vec<Value> = state
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut req_count = state
        .get("request_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let mut next_id = state.get("next_id").and_then(Value::as_u64).unwrap_or(1);
    let mut depth_limited = state
        .get("depth_limited")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let previous_done_len = done.len();

    if let Some(results) = results {
        for raw in pending {
            let Some(task) = Task::from_value(&raw) else {
                continue;
            };
            let Some(response) = results.get(&task.id) else {
                errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":"host omitted a response"}));
                continue;
            };
            if let Some(e) = response.get("error").and_then(Value::as_str) {
                errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":e}));
                continue;
            }
            let status = response.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
            let headers = response
                .get("headers")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if (300..400).contains(&status) {
                if task.redirects >= MAX_REDIRECTS {
                    errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":"redirect limit exceeded"}));
                    continue;
                }
                let location = header(&headers, "location");
                let next = location.and_then(|loc| Url::parse(&task.url).ok()?.join(loc).ok());
                if let Some(next) = next {
                    if let Err(e) = validate_url(next.as_str()) {
                        errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":format!("unsafe redirect: {e}")}));
                    } else {
                        if tool == "web_crawl" {
                            let seed = args
                                .get("url")
                                .and_then(Value::as_str)
                                .and_then(|s| Url::parse(s).ok());
                            if seed.as_ref().is_some_and(|s| s.origin() != next.origin()) {
                                errors.push(json!({"url":task.url,"purpose":task.purpose,"error":"crawl redirect left the seed origin"}));
                                continue;
                            }
                            if task.purpose == "crawl_page"
                                && args.get("respect_robots").and_then(Value::as_bool)
                                    != Some(false)
                            {
                                let doc = done.iter().find(|d| {
                                    d.pointer("/task/purpose").and_then(Value::as_str)
                                        == Some("robots")
                                });
                                let rules = doc
                                    .map(compact::robots)
                                    .unwrap_or_else(Robots::disallow_all);
                                if !rules.allows_url(&next) {
                                    errors.push(json!({"url":next.as_str(),"purpose":task.purpose,"error":"robots.txt disallows redirect target"}));
                                    continue;
                                }
                            }
                        }
                        let mut redirected = task.clone();
                        redirected.url = next.to_string();
                        redirected.redirects += 1;
                        queue.push(redirected.value());
                    }
                } else {
                    errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":"redirect response has invalid or missing Location"}));
                }
                continue;
            }
            let body = match response_body(response) {
                Ok(body) => body,
                Err(e) => {
                    errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":e}));
                    continue;
                }
            };
            if body.len() > MAX_BODY {
                errors.push(json!({"url":task.url,"purpose":task.purpose,"engine":task.engine,"query":task.query,"error":"response exceeded 1 MiB host limit"}));
                continue;
            }
            let effective_args = compact_args_for_task(&args, &done, &task);
            let mut compacted = compact::record(&task, status, &headers, &body, &effective_args);
            if task.purpose == "sitemap" {
                let cap = bounded_num(&args, "map_limit", 120, 1000);
                let seed_origin = args
                    .get("url")
                    .and_then(Value::as_str)
                    .and_then(|s| Url::parse(s).ok())
                    .map(|u| u.origin());
                let mut eligible_pages = done
                    .iter()
                    .flat_map(|d| {
                        d.get("sitemap_entries")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                    })
                    .filter(|e| e.get("is_index").and_then(Value::as_bool) != Some(true))
                    .filter_map(|e| e.get("loc").and_then(Value::as_str))
                    .filter(|url| eligible_inventory_url(url, seed_origin.as_ref()))
                    .map(str::to_owned)
                    .collect::<HashSet<_>>();
                let mut index_urls = done
                    .iter()
                    .flat_map(|d| {
                        d.get("sitemap_entries")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                    })
                    .filter(|e| e.get("is_index").and_then(Value::as_bool) == Some(true))
                    .filter_map(|e| e.get("loc").and_then(Value::as_str))
                    .filter(|url| eligible_inventory_url(url, seed_origin.as_ref()))
                    .map(str::to_owned)
                    .collect::<HashSet<_>>();
                let mut truncated = compacted
                    .get("sitemap_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(entries) = compacted
                    .get_mut("sitemap_entries")
                    .and_then(Value::as_array_mut)
                {
                    entries.retain(|entry| {
                        let Some(url) = entry.get("loc").and_then(Value::as_str) else {
                            return false;
                        };
                        if url.len() > 2048 {
                            truncated = true;
                            return false;
                        }
                        if !eligible_inventory_url(url, seed_origin.as_ref()) {
                            return false;
                        }
                        if entry.get("is_index").and_then(Value::as_bool) == Some(true) {
                            if index_urls.contains(url) {
                                return false;
                            }
                            if index_urls.len() < MAX_REQUESTS {
                                index_urls.insert(url.to_owned());
                                return true;
                            }
                            truncated = true;
                            return false;
                        }
                        if eligible_pages.contains(url) {
                            return false;
                        }
                        if eligible_pages.len() < cap {
                            eligible_pages.insert(url.to_owned());
                            true
                        } else {
                            truncated = true;
                            false
                        }
                    });
                    if truncated {
                        compacted["sitemap_truncated"] = Value::Bool(true);
                    }
                }
            }
            done.push(compacted);
        }
    }

    // Crawl turns successful page responses into a bounded same-origin BFS.
    if tool == "web_crawl" {
        let mut queued: HashSet<String> = queue
            .iter()
            .filter_map(|v| v.get("url")?.as_str().map(str::to_owned))
            .collect();
        let visited: HashSet<String> = done
            .iter()
            .filter_map(|v| v.pointer("/task/url")?.as_str().map(str::to_owned))
            .collect();
        let seed = args.get("url").and_then(Value::as_str).unwrap_or_default();
        let seed_url = Url::parse(seed).ok();
        let max_pages = bounded_num(&args, "max_pages", 10, 200);
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).min(8))
            .unwrap_or(2);
        let pages_done = done
            .iter()
            .filter(|v| v.pointer("/task/purpose").and_then(Value::as_str) == Some("crawl_page"))
            .count();
        let robots_doc = done
            .iter()
            .find(|v| v.pointer("/task/purpose").and_then(Value::as_str) == Some("robots"));
        let robots = robots_doc.map(compact::robots).unwrap_or_else(|| {
            if args.get("respect_robots").and_then(Value::as_bool) != Some(false) {
                Robots::disallow_all()
            } else {
                Robots::default()
            }
        });
        let crawl_delay_blocked = args.get("respect_robots").and_then(Value::as_bool)
            != Some(false)
            && robots.crawl_delay.is_some_and(|delay| delay > 0.0);
        if crawl_delay_blocked {
            queue.clear();
            errors.push(json!({
                "code":"robots_crawl_delay_unsupported",
                "error":format!("robots.txt specifies a positive Crawl-delay ({} seconds); this runtime cannot wait between requests", robots.crawl_delay.unwrap())
            }));
        } else if (robots_doc.is_some()
            || args.get("respect_robots").and_then(Value::as_bool) == Some(false))
            && args.get("mode").and_then(Value::as_str) != Some("content")
        {
            // The default sitemap is requested only after robots.txt is known,
            // so a Crawl-delay directive can stop follow-up network work.
            if let Some(origin) = seed_url.as_ref().map(|u| u.origin().ascii_serialization()) {
                let sitemap = format!("{origin}/sitemap.xml");
                let already_done = done.iter().any(|d| {
                    d.pointer("/task/url").and_then(Value::as_str) == Some(sitemap.as_str())
                }) || errors
                    .iter()
                    .any(|e| e.get("url").and_then(Value::as_str) == Some(sitemap.as_str()));
                if !already_done && !queued.contains(&sitemap) {
                    let t = Task {
                        id: format!("r{}", next_id),
                        url: sitemap.clone(),
                        purpose: "sitemap".into(),
                        engine: None,
                        query: None,
                        depth: 0,
                        redirects: 0,
                    };
                    next_id += 1;
                    queued.insert(sitemap);
                    queue.push(t.value());
                }
            }
        }
        let mut done_urls: HashSet<String> = done
            .iter()
            .filter_map(|d| d.pointer("/task/url")?.as_str().map(str::to_owned))
            .collect();
        done_urls.extend(
            errors
                .iter()
                .filter_map(|e| e.get("url")?.as_str().map(str::to_owned)),
        );
        for sitemap in robots.sitemaps.iter().filter(|_| {
            !crawl_delay_blocked && args.get("mode").and_then(Value::as_str) != Some("content")
        }) {
            let same_origin = seed_url
                .as_ref()
                .and_then(|seed| {
                    Url::parse(sitemap)
                        .ok()
                        .map(|u| seed.origin() == u.origin())
                })
                .unwrap_or(false);
            if same_origin
                && validate_url(sitemap).is_ok()
                && !done_urls.contains(sitemap)
                && !queued.contains(sitemap)
            {
                let t = Task {
                    id: format!("r{}", next_id),
                    url: sitemap.clone(),
                    purpose: "sitemap".into(),
                    engine: None,
                    query: None,
                    depth: 0,
                    redirects: 0,
                };
                next_id += 1;
                queued.insert(sitemap.clone());
                queue.push(t.value());
            }
        }
        let entries = done
            .iter()
            .flat_map(|d| {
                d.get("sitemap_entries")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .collect::<Vec<_>>();
        let mut sitemap_urls = Vec::new();
        for entry in entries {
            let Some(sitemap) = entry.get("loc").and_then(Value::as_str) else {
                continue;
            };
            let same_origin = seed_url
                .as_ref()
                .and_then(|seed| {
                    Url::parse(sitemap)
                        .ok()
                        .map(|u| seed.origin() == u.origin())
                })
                .unwrap_or(false);
            if !same_origin || validate_url(sitemap).is_err() {
                continue;
            }
            if entry.get("is_index").and_then(Value::as_bool) == Some(true) {
                if !queued.contains(sitemap) && !done_urls.contains(sitemap) {
                    let t = Task {
                        id: format!("r{}", next_id),
                        url: sitemap.to_owned(),
                        purpose: "sitemap".into(),
                        engine: None,
                        query: None,
                        depth: 0,
                        redirects: 0,
                    };
                    next_id += 1;
                    queued.insert(sitemap.to_owned());
                    queue.push(t.value());
                }
            } else {
                sitemap_urls.push(sitemap.to_owned());
            }
        }
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("full");
        let has_crawl_page = done
            .iter()
            .any(|d| d.pointer("/task/purpose").and_then(Value::as_str) == Some("crawl_page"));
        let seed_seen = done
            .iter()
            .any(|d| d.pointer("/task/url").and_then(Value::as_str) == Some(seed));
        let sitemap_waiting = queue
            .iter()
            .any(|v| v.get("purpose").and_then(Value::as_str) == Some("sitemap"));
        if !crawl_delay_blocked
            && matches!(mode, "full" | "map" | "content")
            && (mode != "map" || sitemap_urls.is_empty() || args.get("focus").is_some())
            && !sitemap_waiting
            && !has_crawl_page
            && !seed_seen
        {
            enqueue_crawl_url(
                &args,
                seed_url.as_ref(),
                &robots,
                seed,
                0,
                &mut queue,
                &mut queued,
                &visited,
                pages_done,
                max_pages,
                &mut next_id,
            );
        }
        if !crawl_delay_blocked && mode == "full" {
            for url in sitemap_urls {
                enqueue_crawl_url(
                    &args,
                    seed_url.as_ref(),
                    &robots,
                    &url,
                    0,
                    &mut queue,
                    &mut queued,
                    &visited,
                    pages_done,
                    max_pages,
                    &mut next_id,
                );
            }
        }
        if !crawl_delay_blocked && args.get("mode").and_then(Value::as_str) != Some("map") {
            for d in done.iter().skip(previous_done_len) {
                let Some(t) = d.get("task").and_then(Task::from_value) else {
                    continue;
                };
                if t.purpose != "crawl_page" || t.depth >= max_depth || pages_done >= max_pages {
                    if t.purpose == "crawl_page"
                        && t.depth >= max_depth
                        && d.get("links")
                            .and_then(Value::as_array)
                            .is_some_and(|a| !a.is_empty())
                    {
                        depth_limited = true;
                    }
                    continue;
                }
                for url in d
                    .get("links")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    enqueue_crawl_url(
                        &args,
                        seed_url.as_ref(),
                        &robots,
                        &url,
                        t.depth + 1,
                        &mut queue,
                        &mut queued,
                        &visited,
                        pages_done,
                        max_pages,
                        &mut next_id,
                    );
                }
            }
        }
        if mode != "map" {
            for record in done.iter_mut().skip(previous_done_len) {
                if record.get("links").is_some() {
                    record["links"] = json!([]);
                }
            }
        }
    }

    if deadline_expired {
        errors.push(json!({"code":"deadline","error":"tool deadline elapsed; returning only completed results"}));
        state = json!({"tool":tool,"args":args,"round":round,"pending":[],"queue":queue,"done":done,"errors":errors,"request_count":req_count,"next_id":next_id,"deadline_unix_ms":state.get("deadline_unix_ms"),"depth_limited":depth_limited});
        return finalize(tool, &args, done, errors, &state);
    }

    if req_count >= MAX_REQUESTS {
        state = json!({"tool":tool,"args":args,"round":round,"pending":[],"queue":queue,"done":done,"errors":errors,"request_count":req_count,"next_id":next_id,"deadline_unix_ms":state.get("deadline_unix_ms"),"depth_limited":depth_limited});
        return finalize(tool, &args, done, errors, &state);
    }
    let mut requests = Vec::new();
    let mut new_pending = Vec::new();
    while requests.len() < MAX_ROUND_REQUESTS && req_count < MAX_REQUESTS {
        let Some(raw) = queue.first().cloned() else {
            break;
        };
        queue.remove(0);
        let Some(task) = Task::from_value(&raw) else {
            continue;
        };
        if let Err(e) = validate_url(&task.url) {
            errors.push(json!({"url":task.url,"purpose":task.purpose,"error":e}));
            continue;
        }
        let user_agent = if task.engine.as_deref() == Some("google") {
            "Nokia6230/2.0 (03.15) Profile/MIDP-2.0 Configuration/CLDC-1.1".to_owned()
        } else {
            format!("Mozilla/5.0 (compatible; {ROBOTS_PRODUCT_TOKEN}/1.0; +https://gents.ai)")
        };
        requests.push(json!({"id":task.id,"method":"GET","url":task.url,"headers":{
            "user-agent":user_agent,
            "accept":"text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.5"
        }}));
        new_pending.push(task.value());
        req_count += 1;
    }
    if !requests.is_empty() {
        if round >= MAX_ROUNDS {
            queue.extend(new_pending);
            errors.push(json!({"code":"round_limit","error":"HTTP round limit exhausted"}));
            state = json!({"tool":tool,"args":args,"round":round,"pending":[],"queue":queue,"done":done,"errors":errors,"request_count":req_count.saturating_sub(requests.len()),"next_id":next_id,"deadline_unix_ms":state.get("deadline_unix_ms"),"depth_limited":depth_limited});
            return finalize(tool, &args, done, errors, &state);
        }
        state = json!({"tool":tool,"args":args,"round":round+1,"pending":new_pending,"queue":queue,
            "done":done,"errors":errors,"request_count":req_count,"next_id":next_id,
            "deadline_unix_ms":state.get("deadline_unix_ms"),"depth_limited":depth_limited});
        return json!({"http_calls":{"requests":requests,"state":state}});
    }
    state = json!({"tool":tool,"args":args,"round":round,"pending":[],"queue":queue,"done":done,"errors":errors,"request_count":req_count,"next_id":next_id,"deadline_unix_ms":state.get("deadline_unix_ms"),"depth_limited":depth_limited});
    finalize(tool, &args, done, errors, &state)
}

/// WASI command entry point. Input is bounded to 24 MiB and output to the
/// request-state bound plus result metadata.
pub fn main_for(tool: &str) {
    let mut bytes = Vec::new();
    const MAX_INPUT: u64 = 24 * 1024 * 1024;
    let read = std::io::stdin().take(MAX_INPUT + 1).read_to_end(&mut bytes);
    let output = match read {
        Err(e) => error("invalid_input", format!("could not read JSON input: {e}")),
        Ok(_) if bytes.len() as u64 > MAX_INPUT => {
            error("input_too_large", "JSON input exceeded 24 MiB".into())
        }
        Ok(_) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(input) => run(tool, input),
            Err(e) => error("invalid_input", format!("could not read JSON input: {e}")),
        },
    };
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &output);
    let _ = stdout.write_all(b"\n");
}

fn initial_state(tool: &str, args: Value) -> Value {
    let mut queue = Vec::<Value>::new();
    let mut next = 1u64;
    let mut push = |url: String,
                    purpose: &str,
                    engine: Option<String>,
                    query: Option<String>,
                    depth: usize| {
        let t = Task {
            id: format!("r{next}"),
            url,
            purpose: purpose.into(),
            engine,
            query,
            depth,
            redirects: 0,
        };
        next += 1;
        queue.push(t.value());
    };
    match tool {
        "web_fetch" => {
            if let Some(urls) = args.get("url").and_then(Value::as_array) {
                for (i, u) in urls.iter().enumerate() {
                    if let Some(u) = u.as_str() {
                        push(u.into(), "fetch", None, None, i);
                    }
                }
            } else if let Some(url) = args.get("url").and_then(Value::as_str) {
                push(url.into(), "fetch", None, None, 0);
            }
        }
        "web_search" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let mut queries = vec![query];
            if let Some(v) = args.get("query_variants").and_then(Value::as_array) {
                queries.extend(v.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            for (qi, q) in queries.into_iter().enumerate() {
                for engine in ["ddg_lite", "bing", "google", "brave", "mojeek", "yahoo"] {
                    let compiled = query::compile(&q);
                    let engine_query = query::for_engine(&compiled, engine);
                    let Some(url) = engines::serp_url(engine, &engine_query) else {
                        continue;
                    };
                    push(url, "search", Some(engine.into()), Some(q.clone()), qi);
                }
            }
        }
        "web_crawl" => {
            if let Some(url) = args.get("url").and_then(Value::as_str) {
                if let Ok(u) = Url::parse(url) {
                    let origin = u.origin().ascii_serialization();
                    let respect_robots =
                        args.get("respect_robots").and_then(Value::as_bool) != Some(false);
                    if respect_robots {
                        push(format!("{origin}/robots.txt"), "robots", None, None, 0);
                    } else if args.get("mode").and_then(Value::as_str) != Some("content") {
                        push(format!("{origin}/sitemap.xml"), "sitemap", None, None, 0);
                    }
                }
                if args.get("mode").and_then(Value::as_str) == Some("content")
                    && args.get("respect_robots").and_then(Value::as_bool) == Some(false)
                {
                    push(url.into(), "crawl_page", None, None, 0);
                }
            }
        }
        _ => {}
    }
    let budget_ms = if tool == "web_crawl" {
        args.get("deadline_s")
            .and_then(Value::as_u64)
            .unwrap_or(120)
            .saturating_mul(1000)
    } else {
        args.get("deadline_ms")
            .and_then(Value::as_u64)
            .unwrap_or(120_000)
    };
    json!({"tool":tool,"args":args,"round":0,"pending":[],"queue":queue,"done":[],"errors":[],"request_count":0,"next_id":next,"deadline_unix_ms":now_ms().saturating_add(budget_ms)})
}

fn finalize(
    tool: &str,
    args: &Value,
    done: Vec<Value>,
    errors: Vec<Value>,
    state: &Value,
) -> Value {
    match tool {
        "web_fetch" => finish_fetch(done, errors),
        "web_search" => finish_search(args, done, errors),
        "web_crawl" => finish_crawl(args, done, errors, state),
        _ => error("unsupported_tool", tool.into()),
    }
}

fn finish_fetch(done: Vec<Value>, errors: Vec<Value>) -> Value {
    let mut out = Vec::new();
    let mut remaining = MAX_CALL_CHARS;
    for d in &done {
        if d.pointer("/task/purpose").and_then(Value::as_str) != Some("fetch") {
            continue;
        }
        let Some(mut row) = d.get("fetch_result").cloned() else {
            continue;
        };
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let original = content.len();
        let mut content = content;
        truncate_utf8(&mut content, remaining);
        remaining = remaining.saturating_sub(content.len());
        if content.len() < original {
            row["content_complete"] = Value::Bool(false);
            row["next_offset"] = Value::Null;
            row["output_truncated"] = Value::Bool(true);
        }
        row["content"] = Value::String(content);
        out.push(row);
    }
    for e in errors {
        out.push(e);
    }
    let ok = out
        .iter()
        .any(|r| r.get("ok").and_then(Value::as_bool) == Some(true));
    json!({"tool":"web_fetch","ok":ok,"results":out,"provenance":"HTTP response body; content_hash is SHA-256 of the exact response bytes"})
}

fn finish_search(args: &Value, done: Vec<Value>, errors: Vec<Value>) -> Value {
    let mut groups: BTreeMap<String, Vec<(String, Vec<donsetch_core::search::engines::Hit>)>> =
        BTreeMap::new();
    let mut health = Vec::new();
    for d in done {
        let Some(t) = d.get("task").and_then(Task::from_value) else {
            continue;
        };
        if t.purpose != "search" {
            continue;
        }
        let engine = t.engine.unwrap_or_default();
        let q = t.query.unwrap_or_default();
        let status = d.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
        let hits: Vec<donsetch_core::search::engines::Hit> = d
            .get("hits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|h| {
                Some(donsetch_core::search::engines::Hit {
                    title: h.get("title")?.as_str()?.to_owned(),
                    url: h.get("url")?.as_str()?.to_owned(),
                    snippet: h
                        .get("snippet")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    rank: h.get("rank").and_then(Value::as_u64).unwrap_or(0) as usize,
                    published: h
                        .get("published")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect();
        health.push(json!({"engine":engine,"query":q,"status":status,"hits":hits.len(),"healthy":hits.len()>=3}));
        groups.entry(q).or_default().push((engine, hits));
    }
    for e in &errors {
        if e.get("purpose").and_then(Value::as_str) == Some("search") {
            health.push(json!({"engine":e.get("engine"),"query":e.get("query"),"healthy":false,"error":e.get("error")}));
        }
    }
    let max = bounded_num(args, "max_results", 7, 12).max(1);
    let requested_intent = args.get("intent").and_then(Value::as_str).unwrap_or("auto");
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let detected = match requested_intent {
        "web" => Intent::Web,
        "code" => Intent::Code,
        "paper" => Intent::Paper,
        "news" => Intent::News,
        "entity" => Intent::Entity,
        _ => intent::detect(query),
    };
    let trust = HashMap::new();
    let mut output = Vec::new();
    for (q, per) in groups {
        let compiled = query::compile(&q);
        let mut fused = rank::merge(
            &per,
            &compiled.text,
            detected,
            &trust,
            max.saturating_mul(3),
        );
        if let Some(site) = compiled.site.as_deref() {
            let site = site.trim_start_matches("www.").to_ascii_lowercase();
            fused.retain(|r| {
                Url::parse(&r.url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                    .is_some_and(|h| h == site || h.ends_with(&format!(".{site}")))
            });
        }
        if let Some(title) = compiled.intitle.as_deref() {
            let title = title.to_ascii_lowercase();
            fused.retain(|r| r.title.to_ascii_lowercase().contains(&title));
        }
        if let Some(ext) = compiled.filetype.as_deref() {
            let suffix = format!(".{}", ext.trim_start_matches('.').to_ascii_lowercase());
            fused.retain(|r| {
                Url::parse(&r.url)
                    .ok()
                    .is_some_and(|u| u.path().to_ascii_lowercase().ends_with(&suffix))
                    || Url::parse(&r.url).ok().is_some_and(|u| {
                        u.path()
                            .rsplit('/')
                            .next()
                            .is_some_and(|s| !s.contains('.'))
                    })
            });
        }
        let merged_total = fused.len();
        fused.truncate(max);
        let weak = rank::is_weak(&fused, merged_total, &compiled.text);
        output.push(json!({"query":q,"weak":weak,"results":fused.iter().enumerate().map(|(i,r)|json!({"rank":i+1,"url":r.url,"title":r.title,"snippet":r.snippet,"sources":r.sources,"score":r.score})).collect::<Vec<_>>()}));
    }
    let healthy = health.iter().any(|h| h["healthy"] == true);
    let degraded = health.iter().any(|h| h["healthy"] != true);
    let oks = healthy
        && output
            .iter()
            .any(|g| g["results"].as_array().is_some_and(|a| !a.is_empty()));
    let weak = output.iter().any(|set| set["weak"] == true);
    json!({"tool":"web_search","ok":oks,"degraded":degraded,"weak":weak,"intent":format!("{:?}",detected).to_lowercase(),"engine_health":health,"query_sets":output,
        "errors":errors,"unsupported_capabilities":["configured providers","vertical search backends","semantic reranker"],
        "provenance":"Search results parsed from the listed live HTTP engine responses; engine_health reports parser degradation."})
}

fn finish_crawl(args: &Value, done: Vec<Value>, mut errors: Vec<Value>, state: &Value) -> Value {
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("full");
    let seed_origin = args
        .get("url")
        .and_then(Value::as_str)
        .and_then(|s| Url::parse(s).ok())
        .map(|u| u.origin());
    let mut pages = Vec::new();
    let mut inventory = Vec::new();
    let mut chars = 0usize;
    let mut content_budget_omitted = false;
    let mut content_truncated = false;
    let mut source_partial = false;
    for d in &done {
        let Some(t) = d.get("task").and_then(Task::from_value) else {
            continue;
        };
        let status = d.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
        if t.purpose == "sitemap" {
            if !(200..300).contains(&status) && status != 404 {
                errors.push(json!({"url":t.url,"error":format!("sitemap HTTP status {status}")}));
            }
            inventory.extend(
                d.get("sitemap_entries")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|e| e.get("is_index").and_then(Value::as_bool) != Some(true))
                    .filter_map(|e| e.get("loc")?.as_str().map(str::to_owned)),
            );
        }
        if t.purpose == "crawl_page" {
            inventory.push(t.url.clone());
            if mode == "map" {
                inventory.extend(
                    d.get("links")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
            }
            if !(200..300).contains(&status) {
                errors.push(json!({"url":t.url,"error":format!("HTTP status {status}")}));
                continue;
            }
            let char_budget = bounded_num(args, "max_total_chars", 60000, MAX_CALL_CHARS);
            if mode != "map" && chars < char_budget {
                if let Some(mut row) = d.get("fetch_result").cloned() {
                    if row.get("ok").and_then(Value::as_bool) != Some(true) {
                        errors.push(json!({"url":t.url,"error":row.get("error").cloned().unwrap_or_else(||json!("page extraction failed"))}));
                        continue;
                    }
                    let page_limit =
                        bounded_num(args, "per_page_max", 8000, 40000).min(char_budget - chars);
                    let original = row
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let mut content = original.clone();
                    truncate_utf8(&mut content, page_limit);
                    let cut = content.len() < original.len();
                    content_truncated |= cut;
                    source_partial |= row.get("next_offset").is_some_and(|v| !v.is_null())
                        || row.get("partial").is_some_and(|v| !v.is_null())
                        || row.get("thin").and_then(Value::as_bool) == Some(true);
                    chars += content.len();
                    row["content"] = Value::String(content);
                    row["depth"] = json!(t.depth);
                    if cut {
                        row["content_complete"] = Value::Bool(false);
                        row["next_offset"] = Value::Null;
                        row["output_truncated"] = Value::Bool(true);
                    }
                    pages.push(row);
                } else {
                    errors.push(
                        json!({"url":t.url,"error":"page response was missing extracted content"}),
                    );
                }
            } else if mode != "map" {
                // The source was fetched, but the aggregate output limit left no
                // room for its extracted content.
                content_budget_omitted = true;
            }
        }
    }
    if let Some(origin) = seed_origin {
        inventory.retain(|url| {
            Url::parse(url)
                .ok()
                .is_some_and(|parsed| parsed.origin() == origin)
        });
    }
    inventory.sort();
    inventory.dedup();
    let inventory_truncated = inventory.len() > bounded_num(args, "map_limit", 120, 1000)
        || done
            .iter()
            .any(|d| d.get("sitemap_truncated").and_then(Value::as_bool) == Some(true))
        || done
            .iter()
            .any(|d| d.get("links_truncated").and_then(Value::as_bool) == Some(true));
    inventory.truncate(bounded_num(args, "map_limit", 120, 1000));
    let remaining = state
        .get("queue")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let page_cap = bounded_num(args, "max_pages", 10, 200);
    let page_tasks = state
        .get("done")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|v| {
                    v.pointer("/task/purpose").and_then(Value::as_str) == Some("crawl_page")
                })
                .count()
        })
        .unwrap_or(pages.len());
    content_truncated |= pages
        .iter()
        .any(|p| p.get("output_truncated").and_then(Value::as_bool) == Some(true));
    source_partial |= pages.iter().any(|p| {
        p.get("next_offset").is_some_and(|v| !v.is_null())
            || p.get("partial").is_some_and(|v| !v.is_null())
            || p.get("thin").and_then(Value::as_bool) == Some(true)
    });
    let stop = if errors
        .iter()
        .any(|e| e.get("code").and_then(Value::as_str) == Some("robots_crawl_delay_unsupported"))
    {
        "robots_crawl_delay"
    } else if inventory_truncated {
        "map_limit"
    } else if remaining {
        "request_or_round_budget"
    } else if content_budget_omitted || content_truncated {
        "content_budget"
    } else if source_partial {
        "partial_content"
    } else if state.get("depth_limited").and_then(Value::as_bool) == Some(true) {
        "depth_limit"
    } else if page_tasks >= page_cap {
        "max_pages"
    } else if errors.is_empty() && pages.is_empty() && inventory.is_empty() {
        "frontier_empty_no_evidence"
    } else if errors.is_empty() {
        "frontier_empty"
    } else {
        "partial_failures"
    };
    let complete = stop == "frontier_empty" && errors.is_empty();
    let ok = if mode == "map" {
        !inventory.is_empty()
    } else {
        !pages.is_empty()
    };
    json!({"tool":"web_crawl","ok":ok,"seed":args.get("url"),"mode":mode,"complete":complete,"stop_reason":stop,
        "pages":pages,"inventory":inventory,"inventory_truncated":inventory_truncated,"errors":errors,"pages_read":pages.len(),"characters":chars,
        "robots":{"requested":args.get("respect_robots").and_then(Value::as_bool)!=Some(false),"best_effort":true},
        "focus_scope":if args.get("focus").is_some(){"URL path token matching before fetch; extracted pages use block relevance"}else{"unfiltered"},
        "deadline_scope":"cooperative between host HTTP rounds; an in-flight host request cannot be interrupted by the guest",
        "unsupported_capabilities":["resume tokens","persistent change fingerprints","browser rendering","path glob filters","cross-host crawl","dataset JSONL"],
        "provenance":"Pages and inventory are derived from fetched HTTP responses; completeness is bounded by the stated crawl stop reason."})
}

fn extract_options(args: &Value) -> ExtractOptions {
    let mut o = ExtractOptions::default();
    o.focus = args.get("focus").and_then(Value::as_str).map(str::to_owned);
    o.selector = args
        .get("selector")
        .and_then(Value::as_str)
        .map(str::to_owned);
    o.section = args
        .get("section")
        .and_then(Value::as_str)
        .map(str::to_owned);
    o.toc = args.get("toc").and_then(Value::as_bool).unwrap_or(false);
    o.include_links = args.get("links").and_then(Value::as_bool).unwrap_or(false);
    o.include_media = args.get("media").and_then(Value::as_bool).unwrap_or(false);
    o.must_contain = args
        .get("must_contain")
        .and_then(Value::as_str)
        .map(str::to_owned);
    o.offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    o.max_chars = Some(
        args.get("max_chars")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or_else(|| match args.get("mode").and_then(Value::as_str) {
                Some("scan") => 800,
                Some("read") => 4000,
                _ => 16000,
            }),
    );
    o
}

fn compact_args_for_task(args: &Value, done: &[Value], task: &Task) -> Value {
    let mut tuned = args.clone();
    let key = if task.purpose == "fetch" {
        "max_chars"
    } else if task.purpose == "crawl_page" {
        "per_page_max"
    } else {
        return tuned;
    };
    let budget = if task.purpose == "fetch" {
        MAX_CALL_CHARS
    } else {
        bounded_num(args, "max_total_chars", 60000, MAX_CALL_CHARS)
    };
    let used = done
        .iter()
        .filter(|d| {
            d.pointer("/task/purpose").and_then(Value::as_str) == Some(task.purpose.as_str())
        })
        .map(|d| {
            d.get("fetch_result")
                .and_then(|r| r.get("content"))
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0)
        })
        .sum::<usize>();
    let default = if task.purpose == "fetch" {
        extract_options(args).max_chars.unwrap_or(16000)
    } else {
        bounded_num(args, "per_page_max", 8000, 40000)
    };
    let requested = args
        .get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(default);
    let cap = requested.min(budget.saturating_sub(used).max(1));
    if let Some(obj) = tuned.as_object_mut() {
        obj.insert(key.to_owned(), json!(cap));
    }
    tuned
}

fn validate_args(tool: &str, args: &Value) -> Result<(), String> {
    let obj = args.as_object().ok_or("arguments must be an object")?;
    let supported: &[&str] = match tool {
        "web_fetch" => &[
            "url",
            "focus",
            "mode",
            "max_chars",
            "offset",
            "section",
            "toc",
            "deadline_ms",
            "must_contain",
            "selector",
            "links",
            "media",
        ],
        "web_search" => &[
            "query",
            "query_variants",
            "max_results",
            "deadline_ms",
            "intent",
        ],
        _ => &[
            "url",
            "mode",
            "focus",
            "max_pages",
            "map_limit",
            "max_depth",
            "max_total_chars",
            "per_page_max",
            "include_paths",
            "exclude_paths",
            "same_host",
            "respect_robots",
            "deadline_s",
            "dataset",
        ],
    };
    for k in obj.keys() {
        if !supported.contains(&k.as_str()) {
            return Err(format!(
                "unsupported parameter {k:?}; this runtime does not implement it"
            ));
        }
    }
    if tool == "web_fetch" {
        let u = obj.get("url").ok_or("url is required")?;
        if !(u.is_string()
            || u.as_array()
                .is_some_and(|a| !a.is_empty() && a.len() <= 12 && a.iter().all(Value::is_string)))
        {
            return Err("url must be a URL string or an array of 1 to 12 URL strings".into());
        }
    }
    if tool == "web_search" {
        if obj
            .get("query")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err("query is required and must be nonempty".into());
        }
        if obj.get("query_variants").is_some_and(|v| {
            !v.is_null()
                && !v
                    .as_array()
                    .is_some_and(|a| a.len() <= 2 && a.iter().all(Value::is_string))
        }) {
            return Err("query_variants must be an array of at most 2 strings".into());
        }
    }
    if tool == "web_crawl" {
        if obj.get("url").and_then(Value::as_str).is_none() {
            return Err("url is required; resume is unsupported".into());
        }
        for k in ["include_paths", "exclude_paths", "dataset"] {
            if obj.contains_key(k) {
                return Err(format!("{k} is not supported by this runtime"));
            }
        }
        if obj
            .get("same_host")
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err("same_host must be a boolean".into());
        }
        if obj.get("same_host").and_then(Value::as_bool) == Some(false) {
            return Err("same_host=false is unsupported; crawl is same-origin only".into());
        }
    }
    let allowed_enum: &[(&str, &[&str])] = match tool {
        "web_fetch" => &[("mode", &["scan", "read", "deep"])],
        "web_search" => &[(
            "intent",
            &["auto", "web", "code", "paper", "news", "entity"],
        )],
        _ => &[("mode", &["full", "map", "content"])],
    };
    for (k, v) in allowed_enum {
        if let Some(value) = obj.get(*k).filter(|v| !v.is_null()) {
            let Some(s) = value.as_str() else {
                return Err(format!("{k} must be a string enum"));
            };
            if !v.contains(&s) {
                return Err(format!("{k} must be one of {}", v.join(", ")));
            }
        }
    }
    let strings: &[&str] = match tool {
        "web_fetch" => &["focus", "section", "must_contain", "selector"],
        "web_crawl" => &["focus"],
        _ => &[],
    };
    for k in strings {
        if obj.get(*k).is_some_and(|v| !v.is_null() && !v.is_string()) {
            return Err(format!("{k} must be a string"));
        }
    }
    let booleans: &[&str] = if tool == "web_fetch" {
        &["toc", "links", "media"]
    } else if tool == "web_crawl" {
        &["respect_robots"]
    } else {
        &[]
    };
    for k in booleans {
        if obj.get(*k).is_some_and(|v| !v.is_null() && !v.is_boolean()) {
            return Err(format!("{k} must be a boolean"));
        }
    }
    let numeric: &[(&str, u64, u64)] = match tool {
        "web_fetch" => &[
            ("max_chars", 200, 500_000),
            ("offset", 0, u64::MAX),
            ("deadline_ms", 500, 120_000),
        ],
        "web_search" => &[("max_results", 1, 12), ("deadline_ms", 500, 120_000)],
        _ => &[
            ("max_pages", 1, 200),
            ("map_limit", 1, 1000),
            ("max_depth", 0, 8),
            ("max_total_chars", 1, 500_000),
            ("per_page_max", 200, 40_000),
            ("deadline_s", 1, 120),
        ],
    };
    for (k, min, max) in numeric {
        if let Some(v) = obj.get(*k).filter(|v| !v.is_null()) {
            let Some(n) = v.as_u64() else {
                return Err(format!("{k} must be an unsigned integer"));
            };
            if n < *min || n > *max {
                return Err(format!("{k} must be between {min} and {max}"));
            }
        }
    }
    Ok(())
}

fn validate_url(raw: &str) -> Result<(), String> {
    let u = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    if u.scheme() != "https" {
        return Err("only HTTPS URLs are supported".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("URL userinfo is forbidden".into());
    }
    let host = u.host_str().ok_or("URL has no host")?;
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Err("private or local host is forbidden".into());
    }
    let ip_host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = ip_host.parse::<std::net::IpAddr>() {
        if !ip_public(ip) {
            return Err("private or special-use IP literal is forbidden".into());
        }
    }
    Ok(())
}
fn ip_public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(x) => {
            let [a, b, c, _] = x.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..128).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..32).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19))
                || a >= 224)
        }
        std::net::IpAddr::V6(x) => {
            if let Some(v4) = x.to_ipv4_mapped() {
                ip_public(v4.into())
            } else {
                let s = x.segments();
                if s[0] == 0x2002 {
                    let a = s[1].to_be_bytes();
                    let b = s[2].to_be_bytes();
                    return ip_public(std::net::Ipv4Addr::new(a[0], a[1], b[0], b[1]).into());
                }
                (s[0] & 0xe000) == 0x2000
                    && !(s[0] == 0x2001 && (s[1] == 0x0db8 || s[1] == 0 || s[1] == 0x10))
            }
        }
    }
}
fn response_body(v: &Value) -> Result<Vec<u8>, String> {
    if let Some(s) = v.get("body_base64").and_then(Value::as_str) {
        return base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| "host supplied invalid base64 response body".to_owned());
    }
    if let Some(s) = v.get("body").and_then(Value::as_str) {
        return Ok(s.as_bytes().to_vec());
    }
    Err("host response omitted body and body_base64".into())
}
fn header<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    headers
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))?
        .1
        .as_str()
}
fn bounded_num(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, max))
        .unwrap_or(default)
}
fn error(code: &str, message: String) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
fn truncate_utf8(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1
        }
        s.truncate(end)
    }
}

fn eligible_inventory_url(url: &str, seed_origin: Option<&url::Origin>) -> bool {
    url.len() <= 2048
        && validate_url(url).is_ok()
        && Url::parse(url)
            .ok()
            .is_some_and(|parsed| seed_origin.is_none_or(|origin| &parsed.origin() == origin))
}

fn enqueue_crawl_url(
    args: &Value,
    seed: Option<&Url>,
    robots: &Robots,
    url: &str,
    depth: usize,
    queue: &mut Vec<Value>,
    queued: &mut HashSet<String>,
    visited: &HashSet<String>,
    pages: usize,
    max: usize,
    next: &mut u64,
) {
    let queued_pages = queue
        .iter()
        .filter(|task| task.get("purpose").and_then(Value::as_str) == Some("crawl_page"))
        .count();
    if pages + queued_pages >= max {
        return;
    }
    let Ok(u) = Url::parse(url) else { return };
    if validate_url(url).is_err() {
        return;
    }
    if args.get("respect_robots").and_then(Value::as_bool) != Some(false) && !robots.allows_url(&u)
    {
        return;
    }
    if args.get("same_host").and_then(Value::as_bool) != Some(false)
        && seed.is_some_and(|s| s.origin() != u.origin())
    {
        return;
    }
    if args
        .get("focus")
        .and_then(Value::as_str)
        .is_some_and(|focus| {
            seed.is_none_or(|s| s.as_str() != url) && !focus_matches(focus, u.path())
        })
    {
        return;
    }
    if visited.contains(url) || !queued.insert(url.into()) {
        return;
    }
    let t = Task {
        id: format!("r{}", *next),
        url: url.into(),
        purpose: "crawl_page".into(),
        engine: None,
        query: None,
        depth,
        redirects: 0,
    };
    *next += 1;
    queue.push(t.value());
}
fn focus_matches(focus: &str, path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let terms = focus
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 2)
        .collect::<Vec<_>>();
    terms.is_empty()
        || terms
            .iter()
            .any(|t| lower.contains(&t.to_ascii_lowercase()))
}
