use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

#[cfg(test)]
const ORIGIN: &str = "http://127.0.0.1:18765";

fn unavailable(input: &Value, reason: &str) -> Value {
    if input.get("state").is_some() {
        return failure(
            input,
            format!("Browser availability changed after submission: {reason}"),
        );
    }
    let http = input["http_calls"] == true;
    json!({"response": {
        "status":if input["op"] == "doctor" {"limited"} else {"unavailable"},
        "mode":if http {"http_only"} else {"help_only"},
        "reason":reason,
        "capabilities":{"public_http":http,"browser":false},
        "phase":"availability","code_executed":false,
        "next":if http {
            "Continue public research with web_search, web_fetch and web_crawl. Browser rendering, logins, screenshots and actions are unavailable; do not retry browser calls until setup changes or substitute public HTTP for the user's signed-in browser. To enable browser support, use help/setup for runtime upgrade/reinstall or external-worker setup."
        } else {
            "Only offline help is available. Use a runtime supporting plugin HTTP rounds and install with --grant-authority. Browser interaction additionally needs managed pack services or an external worker."
        },
        "next_call":{"op":"help","topic":"setup"}
    }})
}

fn rendered_page(input: &Value, mut body: Value) -> Result<Value, String> {
    if input["op"] != "read" || body["response"]["status"] != "completed" {
        return Ok(body);
    }
    if let Err(error) = extract_page(input, &mut body) {
        let response = &mut body["response"];
        response["status"] = json!("error");
        response["error"] = json!(error);
        response["phase"] = json!("extraction");
        response["code_executed"] = json!(false);
        response["next"] = json!(
            "The DOM was captured; no model-written code was submitted. Retry read without selector/focus, or inspect the page with exec to choose a matching CSS selector. No reconnect is needed."
        );
        response["next_call"] =
            json!({"op":"read","session":input["session"].as_str().unwrap_or("default")});
    }
    Ok(body)
}

fn extract_page(input: &Value, body: &mut Value) -> Result<(), String> {
    let page = body["response"]
        .as_object_mut()
        .ok_or("Missing browser response")?
        .remove("page")
        .ok_or("Missing rendered page")?;
    let html = page["html"]
        .as_str()
        .filter(|s| s.len() <= 1_000_000)
        .ok_or("Missing or oversized rendered HTML")?;
    let url = page["url"].as_str().ok_or("Missing rendered page URL")?;
    let options = donsetch_core::extract::ExtractOptions {
        max_chars: Some(input["max_chars"].as_u64().unwrap_or(16000) as usize),
        offset: input["offset"].as_u64().unwrap_or(0) as usize,
        focus: input["focus"].as_str().map(str::to_owned),
        selector: input["selector"].as_str().map(str::to_owned),
        include_links: true,
        ..Default::default()
    };
    let result =
        donsetch_core::extract::extract(html.as_bytes(), "text/html; charset=utf-8", url, &options)
            .map_err(|e| format!("Rendered-page extraction failed: {e}"))?;
    body["response"]["page"] = json!({"url":url,"title":result.title,"content":result.markdown,
        "next_offset":result.next_offset,"total_chars":result.total_chars,"thin":result.thin,
        "partial":result.partial,"focus_omitted":result.focus_omitted,"provenance":"Live top-level HTML extracted by shared Donsetch Rust core; excludes iframe/shadow DOM and live form-control properties. Not a frozen snapshot."});
    Ok(())
}

fn failure(input: &Value, error: String) -> Value {
    let submitted = input.get("state").is_some();
    json!({"response": {
        "status": "error", "error": error,
        "phase": if submitted { "transport" } else { "validation" },
        "code_executed": if submitted { Value::Null } else { json!(false) },
        "next": if submitted {
            "The submitted execution outcome is unknown. Run doctor, then inspect browser state before repeating any write."
        } else {
            "Correct the input or connection setup. Call op=help with topic=setup or recovery, omitting connection. This call did not execute code."
        }
    }})
}

fn worker_response(result: &Value) -> Result<Value, String> {
    if let Some(error) = result.get("error") {
        let detail: String = error
            .as_str()
            .unwrap_or("unspecified host HTTP error")
            .chars()
            .take(512)
            .collect();
        return Err(format!(
            "Worker HTTP request failed: {detail}. The submitted execution outcome is unknown."
        ));
    }
    let status = result["status"]
        .as_u64()
        .ok_or("Missing worker HTTP status")?;
    let body: Value =
        serde_json::from_str(result["body"].as_str().ok_or("Missing worker response")?)
            .map_err(|_| format!("Invalid worker response (HTTP {status})"))?;
    if (status == 200 || status == 202 || (400..600).contains(&status))
        && (body["response"].is_object() || (status == 202 && body["status"] == "running"))
    {
        return Ok(body);
    }
    Err(format!(
        "Unexpected worker response (HTTP {status}); inspect state before repeating submitted actions."
    ))
}

fn run(input: &Value) -> Result<Value, String> {
    let op = input["op"].as_str().unwrap_or("exec");
    if op == "help" {
        let help: Value =
            serde_json::from_str(include_str!("browser_help.json")).map_err(|e| e.to_string())?;
        let topic = input["topic"].as_str().unwrap_or("overview");
        let response = help.get(topic).ok_or(
            "Unknown help topic. Use overview, interaction, tabs, files, recovery, connection or setup.",
        )?;
        return Ok(json!({"response":response}));
    }
    if input.get("url").is_some() {
        return Err("url is not a browser_exec argument. For a public page, first call op=exec with code=goto_url('https://...'); assert wait_for_element('body'), then op=read in the SAME session. For an existing user tab, connect and supply target_id instead of navigating.".into());
    }
    if ![
        "exec",
        "read",
        "doctor",
        "close",
        "discover",
        "connect",
        "status",
        "tabs",
        "disconnect",
    ]
    .contains(&op)
    {
        return Err(
            "Use exec, read, discover, connect, status, tabs, disconnect, doctor, close or help"
                .into(),
        );
    }
    for (key, min, max) in [("max_chars", 200, 100000), ("offset", 0, 10000000)] {
        if let Some(value) = input.get(key) {
            if !value.as_u64().is_some_and(|n| (min..=max).contains(&n)) {
                return Err(format!("{key} must be an integer between {min} and {max}"));
            }
        }
    }
    let session = input["session"].as_str().unwrap_or("default");
    if session.is_empty()
        || session.len() > 64
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("session must contain 1-64 letters, digits, underscores or hyphens".into());
    }
    let timeout = input
        .get("timeout_s")
        .map(|v| v.as_u64().ok_or("timeout_s must be an integer"))
        .transpose()?
        .unwrap_or(120);
    if !(1..=300).contains(&timeout) {
        return Err("timeout_s must be between 1 and 300".into());
    }
    if op == "exec" && input.get("state").is_none() {
        let code = input["code"].as_str().ok_or("code is required for exec")?;
        if code.trim().is_empty() || code.len() > 65536 {
            return Err("code must contain 1-65536 UTF-8 bytes".into());
        }
    }
    if input["http_calls"] != true {
        return Ok(unavailable(
            input,
            "Host plugin HTTP support is unavailable",
        ));
    }
    if input["service"]["status"] == "unavailable" {
        return Ok(unavailable(
            input,
            input["service"]["error"]
                .as_str()
                .unwrap_or("Native browser worker could not start"),
        ));
    }
    if !input["service"].is_object() && input["connection"].is_null() {
        return Ok(unavailable(
            input,
            "This runtime did not supply a managed browser service, and no external worker is configured",
        ));
    }
    let connection: Value = if input["service"].is_object() {
        input["service"].clone()
    } else {
        let path = input["connection"].as_str().ok_or("Native managed service is unavailable. Use a Gents runtime supporting pack services, or supply a connection file from worker setup.")?;
        let path = Path::new(path);
        let path = if path.is_dir() {
            path.join("connection.json")
        } else {
            path.to_path_buf()
        };
        serde_json::from_str(&fs::read_to_string(path).map_err(|_| "Cannot read connection.json. Run worker setup and bind its returned connection file.")?).map_err(|_| "Invalid connection.json; rerun setup")?
    };
    let origin = connection["endpoint"]
        .as_str()
        .ok_or("Missing worker endpoint")?;
    let port = origin
        .strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port > 0);
    if !port.is_some_and(|port| origin == format!("http://127.0.0.1:{port}")) {
        return Err("Worker endpoint must be http://127.0.0.1:<port>, without a path; its port must match the plugin network grant".into());
    }
    let token = connection["token"]
        .as_str()
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Invalid worker token; rerun setup")?;
    let request = |method: &str, route: String, body: Option<Value>, state: Value| {
        let mut req = json!({"id":"browser", "method":method, "url":format!("{origin}{route}"), "headers":{"Authorization":format!("Bearer {token}"), "Content-Type":"application/json"}});
        if let Some(body) = body {
            req["body"] = Value::String(body.to_string());
        }
        json!({"http_calls":{"requests":[req],"state":state}})
    };
    if let Some(state) = input.get("state") {
        let body = worker_response(&input["http_results"]["browser"])?;
        if body["status"] == "running" {
            let job = body["job"]
                .as_str()
                .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("Invalid worker job handle")?;
            return Ok(request("GET", format!("/jobs/{job}"), None, state.clone()));
        }
        let mut body = rendered_page(input, body)?;
        if op == "doctor" && body["response"]["status"] == "ready" {
            body["response"]["capabilities"] = json!({"public_http":true,"browser":true,
                "isolated_browser":body["response"]["isolated_launch"] == "available"});
        }
        return Ok(body);
    }
    Ok(request(
        "POST",
        "/call".into(),
        Some(
            json!({"op":op,"session":session,"code":input.get("code"),"timeout_s":timeout,
            "browser":input.get("browser").cloned().unwrap_or(json!("chrome")),"target_id":input.get("target_id")}),
        ),
        json!({"submitted":true}),
    ))
}

pub fn main() {
    let mut input = String::new();
    let output = io::stdin()
        .take(8 * 1024 * 1024)
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str(&input).map_err(|e| e.to_string()))
        .map(|v| run(&v).unwrap_or_else(|error| failure(&v, error)))
        .unwrap_or_else(|error| failure(&Value::Null, error));
    println!("{output}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_endpoints_are_canonical_loopback_origins() {
        for endpoint in ["http://127.0.0.1:21001", ORIGIN] {
            let result = run(&json!({"op":"doctor", "http_calls":true,"service":{"endpoint":endpoint,"token":"a".repeat(64)}})).unwrap();
            assert_eq!(
                result["http_calls"]["requests"][0]["url"],
                format!("{endpoint}/call")
            );
        }
        for endpoint in [
            "http://example.com:18765",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:18765/path",
            "http://127.0.0.1:18765@evil",
            "http://127.0.0.1:018765",
            "https://127.0.0.1:18765",
        ] {
            assert!(run(&json!({"op":"doctor","http_calls":true,"service":{"endpoint":endpoint,"token":"a".repeat(64)}})).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn absent_native_support_keeps_public_http_available() {
        for service in [
            Value::Null,
            json!({"status":"unavailable","error":"uv not installed"}),
        ] {
            for op in ["doctor", "discover", "exec"] {
                let result =
                    run(&json!({"op":op,"code":"print(1)","http_calls":true,"service":service}))
                        .unwrap();
                let response = &result["response"];
                assert_eq!(response["mode"], "http_only");
                assert_eq!(response["capabilities"]["public_http"], true);
                assert_eq!(response["capabilities"]["browser"], false);
                assert_eq!(response["code_executed"], false);
                assert!(result.get("http_calls").is_none());
                assert!(response["next"].as_str().unwrap().contains("web_fetch"));
            }
        }
        let help = run(&json!({"op":"help"})).unwrap();
        assert!(help["response"]["helpers"].is_string());
        assert_eq!(
            run(&json!({"op":"doctor"})).unwrap()["response"]["mode"],
            "help_only"
        );
    }

    #[test]
    fn unavailable_after_submission_preserves_execution_uncertainty() {
        let result = unavailable(
            &json!({"state":{"submitted":true},"http_calls":true}),
            "worker lost",
        );
        assert!(result["response"]["code_executed"].is_null());
        assert_eq!(result["response"]["phase"], "transport");
    }

    #[test]
    fn external_worker_still_works_without_native_service_support() {
        let dir = std::env::temp_dir().join(format!("web-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connection.json");
        std::fs::write(
            &path,
            json!({"endpoint":ORIGIN,"token":"a".repeat(64)}).to_string(),
        )
        .unwrap();
        let result = run(&json!({"op":"doctor","http_calls":true,"connection":path})).unwrap();
        assert_eq!(
            result["http_calls"]["requests"][0]["url"],
            format!("{ORIGIN}/call")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn extraction_errors_keep_artifacts_and_offer_read_recovery() {
        for selector in ["[", "#missing"] {
            let result = rendered_page(&json!({"op":"read","session":"research","selector":selector}),
                json!({"response":{"status":"completed","artifacts":{"stdout":"/tmp/read"},"page":{"url":"https://example.test","html":"<main>Evidence</main>"}}})).unwrap();
            let response = &result["response"];
            assert_eq!(response["phase"], "extraction");
            assert_eq!(response["code_executed"], false);
            assert_eq!(response["artifacts"]["stdout"], "/tmp/read");
            assert_eq!(
                response["next_call"],
                json!({"op":"read","session":"research"})
            );
            assert!(response.get("page").is_none());
        }
    }

    #[test]
    fn rendered_read_uses_shared_extractor_without_returning_raw_html() {
        let html = "<title>Live article</title><main><h1>Browser evidence</h1><p>Existing signed-in page with useful rendered content and a <a href='/source'>primary source</a>.</p></main><script>SECRET_SCRIPT</script>";
        let result = rendered_page(&json!({"op":"read","max_chars":1000}),
            json!({"response":{"status":"completed","page":{"url":"https://example.com/article","html":html}}})).unwrap();
        let page = &result["response"]["page"];
        assert!(
            page["content"]
                .as_str()
                .unwrap()
                .contains("Existing signed-in")
        );
        assert!(
            page["content"]
                .as_str()
                .unwrap()
                .contains("https://example.com/source")
        );
        assert!(page.get("html").is_none());
        assert!(!result.to_string().contains("SECRET_SCRIPT"));
        assert_eq!(page["url"], "https://example.com/article");
        let failure = json!({"response":{"status":"failed","next":"inspect"}});
        assert_eq!(
            rendered_page(&json!({"op":"read"}), failure.clone()).unwrap(),
            failure
        );
    }

    #[test]
    fn worker_errors_preserve_model_recovery_details() {
        for (status, phase, executed) in [
            (200, "startup", json!(false)),
            (400, "admission", json!(false)),
            (403, "authorization", Value::Null),
            (404, "polling", Value::Null),
        ] {
            let body = json!({"response":{"status":"error", "error":"probe", "phase":phase,
                "code_executed":executed, "next":"inspect state", "artifacts":{"stderr":"/tmp/probe"}}});
            let result = json!({"status":status,"body":body.to_string()});
            assert_eq!(worker_response(&result).unwrap(), body);
        }
    }

    #[test]
    fn transport_failures_never_claim_code_did_not_run() {
        for result in [
            json!({"error":"timeout"}),
            json!({"status":502,"body":"invalid"}),
        ] {
            let error = worker_response(&result).unwrap_err();
            let output = failure(&json!({"state":{"submitted":true}}), error);
            assert!(output["response"]["code_executed"].is_null());
            assert_eq!(output["response"]["phase"], "transport");
            assert!(output.get("http_calls").is_none());
        }
        assert_eq!(
            failure(&json!({}), "bad input".into())["response"]["code_executed"],
            false
        );
    }
    #[test]
    fn validation_needs_no_worker() {
        assert!(run(&json!({"op":"help"})).unwrap()["response"]["helpers"].is_string());
        for input in [
            json!({"code":"x","session":"../other"}),
            json!({"code":"x","timeout_s":301}),
            json!({"code":""}),
            json!({"op":"shell"}),
        ] {
            assert!(run(&input).is_err());
        }
    }
}
