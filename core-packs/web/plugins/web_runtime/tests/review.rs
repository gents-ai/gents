use serde_json::{Value, json};
use web_runtime::run;

fn drive(tool: &str, args: Value, mut serve: impl FnMut(&str) -> Value) -> (Value, Vec<String>) {
    let mut input = args.clone();
    input["http_calls"] = json!(true);
    let mut urls = Vec::new();
    for _ in 0..65 {
        let output = run(tool, input);
        let Some(batch) = output.get("http_calls") else {
            return (output, urls);
        };
        let requests = batch["requests"].as_array().expect("requests array");
        assert!(!requests.is_empty() && requests.len() <= 16);
        let mut responses = serde_json::Map::new();
        for request in requests {
            let url = request["url"].as_str().unwrap();
            urls.push(url.to_owned());
            responses.insert(request["id"].as_str().unwrap().into(), serve(url));
        }
        input = args.clone();
        input["http_calls"] = json!(true);
        input["state"] = batch["state"].clone();
        input["http_results"] = Value::Object(responses);
    }
    panic!("tool exceeded host round budget");
}

fn html(body: &str) -> Value {
    json!({"status":200,"headers":{"content-type":"text/html; charset=utf-8"},"body":body})
}

#[test]
fn robots_network_failure_does_not_fetch_pages() {
    let (result, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"content"}),
        |_| json!({"error":"DNS lookup failed"}),
    );
    assert!(
        urls.iter().all(|url| url.ends_with("/robots.txt")),
        "{urls:?}"
    );
    assert_ne!(result["complete"], true, "{result}");
    assert_ne!(result["ok"], true, "{result}");
}

#[test]
fn crawl_does_not_follow_cross_origin_redirect() {
    let (result, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"content"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                json!({"status":302,"headers":{"location":"https://other.example/escape"},"body":""})
            }
        },
    );
    assert!(
        urls.contains(&"https://public.example/start".to_string()),
        "{result}"
    );
    assert!(
        !urls.iter().any(|url| url.contains("other.example")),
        "{urls:?}"
    );
    assert_ne!(result["complete"], true, "{result}");
}

#[test]
fn crawl_redirect_obeys_robots() {
    let (_, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"content"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":200,"headers":{},"body":"User-agent: *\nDisallow: /private"})
            } else {
                json!({"status":302,"headers":{"location":"/private"},"body":""})
            }
        },
    );
    assert!(
        !urls.iter().any(|url| url.ends_with("/private")),
        "{urls:?}"
    );
}

#[test]
fn depth_zero_reads_seed_but_no_children() {
    let (_, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"content","max_depth":0}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                html(
                    "<html><body><article><h1>Seed</h1><p>Read this useful source.</p><a href='/child'>Child</a></article></body></html>",
                )
            }
        },
    );
    assert!(urls.iter().any(|url| url.ends_with("/start")), "{urls:?}");
    assert!(!urls.iter().any(|url| url.ends_with("/child")), "{urls:?}");
}

#[test]
fn text_probe_is_not_reported_as_complete_source() {
    let (result, _) = drive(
        "web_fetch",
        json!({"url":"https://public.example/source","must_contain":"needle"}),
        |_| json!({"status":200,"headers":{"content-type":"text/plain"},"body":"Here is a needle within a much longer source document."}),
    );
    let row = &result["results"][0];
    assert_eq!(row["ok"], true, "{result}");
    assert_ne!(row["content_complete"], true, "{result}");
}

#[test]
fn search_total_failure_identifies_each_engine() {
    let (result, urls) = drive(
        "web_search",
        json!({"query":"rust asynchronous tasks"}),
        |_| json!({"error":"provider unavailable"}),
    );
    assert!(urls.len() >= 3);
    assert_eq!(result["ok"], false, "{result}");
    assert!(
        result["engine_health"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["engine"].is_string() && h["query"].is_string()),
        "{result}"
    );
}

#[test]
fn unavailable_http_is_explicit() {
    let result = run("web_fetch", json!({"url":"https://public.example/"}));
    assert!(result.get("http_calls").is_none(), "{result}");
    assert_eq!(result["ok"], false, "{result}");
}

#[test]
fn wrong_parameter_types_do_not_start_network_work() {
    for (tool, args) in [
        (
            "web_fetch",
            json!({"url":"https://public.example/","max_chars":"1000","http_calls":true}),
        ),
        (
            "web_fetch",
            json!({"url":"https://public.example/","links":"yes","http_calls":true}),
        ),
        ("web_search", json!({"query":"  ","http_calls":true})),
        (
            "web_crawl",
            json!({"url":"https://public.example/","respect_robots":"false","http_calls":true}),
        ),
    ] {
        let result = run(tool, args);
        assert!(result.get("http_calls").is_none(), "{result}");
        assert_eq!(result["ok"], false, "{result}");
    }
}

#[test]
fn failed_sitemap_is_attempted_once() {
    let (result, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"full"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":200,"headers":{},"body":"User-agent: *\nSitemap: https://public.example/broken.xml"})
            } else if url.ends_with("/broken.xml") {
                json!({"error":"connection reset"})
            } else if url.ends_with("/sitemap.xml") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                html(
                    "<html><body><article><p>A useful page with no outgoing links.</p></article></body></html>",
                )
            }
        },
    );
    assert_eq!(
        urls.iter().filter(|u| u.ends_with("/broken.xml")).count(),
        1,
        "{urls:?}; {result}"
    );
}

#[test]
fn sitemap_directive_cannot_leave_seed_origin() {
    let (_, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"full"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":200,"headers":{},"body":"User-agent: *\nSitemap: https://other.example/sitemap.xml"})
            } else if url.ends_with(".xml") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                html("<html><body><p>Original source content.</p></body></html>")
            }
        },
    );
    assert!(
        !urls.iter().any(|u| u.contains("other.example")),
        "{urls:?}"
    );
}

#[test]
fn sitemap_index_is_not_a_page_in_inventory() {
    let (result, _) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"map"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else if url.ends_with("/sitemap.xml") {
                json!({"status":200,"headers":{"content-type":"application/xml"},"body":"<sitemapindex><sitemap><loc>https://public.example/child.xml</loc></sitemap></sitemapindex>"})
            } else {
                json!({"status":200,"headers":{"content-type":"application/xml"},"body":"<urlset><url><loc>https://public.example/article</loc></url></urlset>"})
            }
        },
    );
    let inventory = result["inventory"].as_array().unwrap();
    assert!(
        inventory.contains(&json!("https://public.example/article")),
        "{result}"
    );
    assert!(
        !inventory.contains(&json!("https://public.example/child.xml")),
        "{result}"
    );
}

#[test]
fn nested_sitemap_index_does_not_spend_tight_page_inventory_budget() {
    let (result, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"map","map_limit":1}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else if url.ends_with("/sitemap.xml") {
                json!({"status":200,"headers":{"content-type":"application/xml"},"body":"<sitemapindex><sitemap><loc>https://public.example/nested.xml</loc></sitemap><sitemap><loc>https://public.example/nested.xml</loc></sitemap><sitemap><loc>https://other.example/offsite.xml</loc></sitemap></sitemapindex>"})
            } else if url.ends_with("/nested.xml") {
                json!({"status":200,"headers":{"content-type":"application/xml"},"body":"<urlset><url><loc>https://other.example/offsite</loc></url><url><loc>https://public.example/article</loc></url></urlset>"})
            } else {
                json!({"status":404,"headers":{},"body":""})
            }
        },
    );
    assert!(
        urls.iter().any(|url| url.ends_with("/nested.xml")),
        "{urls:?}"
    );
    assert_eq!(
        urls.iter()
            .filter(|url| url.ends_with("/nested.xml"))
            .count(),
        1,
        "{urls:?}"
    );
    assert!(
        !urls.iter().any(|url| url.contains("other.example")),
        "{urls:?}"
    );
    assert_eq!(
        result["inventory"],
        json!(["https://public.example/article"]),
        "{result}"
    );
}

#[test]
fn sitemap_index_persistence_is_bounded_and_deduplicated() {
    let xml = (0..300)
        .map(|i| format!("<sitemap><loc>https://public.example/child-{i}.xml</loc></sitemap>"))
        .collect::<String>();
    let (_, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"map","map_limit":1}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else if url.ends_with("/sitemap.xml") {
                json!({"status":200,"headers":{"content-type":"application/xml"},"body":format!("<sitemapindex>{xml}</sitemapindex>")})
            } else {
                json!({"status":404,"headers":{},"body":""})
            }
        },
    );
    let child_urls = urls
        .iter()
        .filter(|url| url.contains("/child-") && url.ends_with(".xml"))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        child_urls.len(),
        254,
        "{} child sitemap requests",
        child_urls.len()
    );
}

#[test]
fn overlong_sitemap_urls_are_reported_as_truncated() {
    let long_url = format!("https://public.example/{}", "a".repeat(2048));
    let xml = format!(
        "<urlset><url><loc>{long_url}</loc></url><url><loc>https://public.example/article</loc></url></urlset>"
    );
    let (result, _) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"map","map_limit":1}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else if url.ends_with("/sitemap.xml") {
                json!({"status":200,"headers":{"content-type":"application/xml"},"body":xml.clone()})
            } else {
                json!({"status":404,"headers":{},"body":""})
            }
        },
    );
    assert_eq!(
        result["inventory"],
        json!(["https://public.example/article"]),
        "{result}"
    );
    assert_eq!(result["inventory_truncated"], true, "{result}");
}

#[test]
fn positive_robots_crawl_delay_stops_before_follow_up_requests() {
    let (result, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"full"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":200,"headers":{},"body":"User-agent: *\nCrawl-delay: 5"})
            } else {
                html("<html><body><article><p>Should not be requested.</p></article></body></html>")
            }
        },
    );
    assert_eq!(urls, ["https://public.example/robots.txt"]);
    assert_eq!(result["complete"], false, "{result}");
    assert_eq!(result["stop_reason"], "robots_crawl_delay", "{result}");
    assert!(
        result["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "robots_crawl_delay_unsupported"),
        "{result}"
    );
}

#[test]
fn disabling_robots_bypasses_positive_crawl_delay_handling() {
    let (result, urls) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"full","respect_robots":false}),
        |url| {
            if url.ends_with("/sitemap.xml") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                html(
                    "<html><body><article><h1>Seed</h1><p>Page was fetched with robots disabled.</p></article></body></html>",
                )
            }
        },
    );
    assert!(
        urls.iter().any(|url| url.ends_with("/sitemap.xml")),
        "{urls:?}"
    );
    assert!(urls.iter().any(|url| url.ends_with("/start")), "{urls:?}");
    assert_ne!(result["stop_reason"], "robots_crawl_delay", "{result}");
}

#[test]
fn truncated_crawl_page_never_makes_a_complete_crawl() {
    let source = format!(
        "<html><body><article><p>{}</p></article></body></html>",
        "A long source passage about useful research. ".repeat(100)
    );
    let (result, _) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"content","per_page_max":200}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                html(&source)
            }
        },
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_ne!(result["complete"], true, "{result}");
}

#[test]
fn gzipped_sitemap_file_is_read_without_http_content_encoding() {
    let (result, _) = drive(
        "web_crawl",
        json!({"url":"https://public.example/start","mode":"map"}),
        |url| {
            if url.ends_with("/robots.txt") {
                json!({"status":404,"headers":{},"body":""})
            } else {
                json!({"status":200,"headers":{"content-type":"application/gzip"},"body_base64":"H4sIAAAAAAAC/7MpLcopTi2xswHSdjY5+cl2GSUlBcVW+voFpUk5mcl6qRWJuQU5qfrJ+bkFRanFxakpNvogZTb6YB36UP0ARyBXK0gAAAA="})
            }
        },
    );
    assert!(
        result["inventory"]
            .as_array()
            .unwrap()
            .contains(&json!("https://public.example/compressed")),
        "{result}"
    );
}

#[test]
fn useful_search_still_reports_failed_engines_as_degraded() {
    let fixture = (1..=3).map(|i| format!("<li class=\"b_algo\"><h2><a href=\"https://source{i}.example/rust\">Rust guide {i}</a></h2><div class=\"b_caption\"><p>Rust language reference</p></div></li>")).collect::<String>();
    let (result, _) = drive("web_search", json!({"query":"Rust"}), |url| {
        if url.contains("bing.com") {
            html(&fixture)
        } else {
            json!({"error":"provider unavailable"})
        }
    });
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["degraded"], true, "{result}");
}
