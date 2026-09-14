use tiny_http::{Header, Response, StatusCode as TinyStatusCode};

// Shared self-contained OAuth callback UI.

pub fn render(status: u16, message: &str) -> String {
    let (title, note) = if status >= 400 {
        (
            "Let’s try that again.",
            "Return to Gents to retry sign-in. You can close this tab.",
        )
    } else if message.contains("cancelled") {
        (
            "No rush.",
            "Sign-in was cancelled. Return to Gents whenever you’re ready.",
        )
    } else {
        (
            "You’re signed in.",
            "Return to Gents to choose your model and finish setup. You can close this tab.",
        )
    };
    let message = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    include_str!("callback_page.html")
        .replace("{{title}}", title)
        .replace("{{note}}", note)
        .replace(
            "{{details}}",
            &if status >= 400 {
                format!("<p class=\"detail\">{message}</p>")
            } else {
                String::new()
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_pages_distinguish_success_cancellation_and_errors() {
        assert!(render(200, "ChatGPT sign-in complete.").contains("You’re signed in."));
        assert!(render(200, "ChatGPT login cancelled.").contains("No rush."));
        let error = render(400, "State mismatch <script>alert('secret')</script>");
        assert!(error.contains("Let’s try that again."));
        assert!(!error.contains("<script>"));
        assert!(error.contains("&lt;script&gt;"));
        assert!(!error.contains("{{"));
        for provider in ["ChatGPT", "Claude", "Grok"] {
            let page = render(200, &format!("{provider} sign-in complete."));
            assert!(page.contains("Gents account connection"));
            assert!(!page.contains("ChatGPT connection"));
        }
    }

    #[test]
    fn callback_response_is_html_without_cache_or_external_resources() {
        let response = response(400, "Retry sign-in".into());
        assert_eq!(response.status_code().0, 400);
        let header = |name| {
            response
                .headers()
                .iter()
                .find(|header| header.field.equiv(name))
                .map(|header| header.value.as_str())
        };
        assert_eq!(header("Content-Type"), Some("text/html; charset=utf-8"));
        assert_eq!(header("Cache-Control"), Some("no-store"));
        assert_eq!(header("Referrer-Policy"), Some("no-referrer"));
        assert!(header("Content-Security-Policy")
            .unwrap()
            .contains("default-src 'none'"));
    }
}

pub fn response(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response =
        Response::from_string(render(status, &body)).with_status_code(TinyStatusCode(status));
    for (name, value) in [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Cache-Control", "no-store"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Content-Type-Options", "nosniff"),
        ("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
    ] {
        if let Ok(header) = Header::from_bytes(name, value) {
            response.add_header(header);
        }
    }
    response
}
