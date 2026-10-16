use base64::Engine as _;
use tiny_http::{Header, Response, StatusCode as TinyStatusCode};

const FONTS: [(&str, &[u8]); 2] = [
    ("Inter Variable", include_bytes!("../fonts/inter.woff2")),
    ("Lora Variable", include_bytes!("../fonts/lora.woff2")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackPage {
    SignedIn,
    Cancelled,
    NotCompleted,
    ExchangeFailed,
    StaleLink,
    Incomplete,
    NotFound,
}

impl CallbackPage {
    pub fn status(self) -> u16 {
        match self {
            Self::SignedIn | Self::Cancelled => 200,
            Self::NotCompleted | Self::StaleLink | Self::Incomplete => 400,
            Self::NotFound => 404,
            Self::ExchangeFailed => 502,
        }
    }

    fn copy(self) -> Option<(&'static str, &'static str)> {
        const RETRY: &str = "Let’s try that again.";
        Some(match self {
            Self::SignedIn => (
                "You’re signed in.",
                "Return to Gents to choose your model and finish setup.<br>You can close this tab.",
            ),
            Self::Cancelled => (
                "No rush.",
                "Sign-in was cancelled.<br>Return to Gents whenever you’re ready.",
            ),
            Self::NotCompleted => (
                RETRY,
                "Sign-in wasn’t completed.<br>Return to Gents for the details.",
            ),
            Self::ExchangeFailed => (
                RETRY,
                "Sign-in couldn’t be finished.<br>Return to Gents for the details.",
            ),
            Self::StaleLink => (
                RETRY,
                "This sign-in link expired or was already used.<br>Return to Gents and sign in again.",
            ),
            Self::Incomplete => (
                RETRY,
                "The sign-in response was incomplete.<br>Return to Gents and sign in again.",
            ),
            Self::NotFound => return None,
        })
    }
}

fn font_faces() -> String {
    FONTS
        .iter()
        .map(|(family, woff2)| {
            format!(
                "@font-face {{ font-family: \"{family}\"; font-style: normal; \
                 font-weight: 100 900; font-display: swap; \
                 src: url(data:font/woff2;base64,{}) format(\"woff2\"); }}",
                base64::engine::general_purpose::STANDARD.encode(woff2)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render(page: CallbackPage) -> Option<String> {
    let (title, note) = page.copy()?;
    Some(
        include_str!("callback_page.html")
            .replace("{{fonts}}", &font_faces())
            .replace("{{title}}", title)
            .replace("{{note}}", note),
    )
}

pub fn response(page: CallbackPage) -> Response<std::io::Cursor<Vec<u8>>> {
    let (body, content_type) = match render(page) {
        Some(html) => (html, "text/html; charset=utf-8"),
        None => ("Not found.".to_string(), "text/plain; charset=utf-8"),
    };
    let mut response = Response::from_string(body).with_status_code(TinyStatusCode(page.status()));
    for (name, value) in [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Content-Type-Options", "nosniff"),
        ("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
    ] {
        if let Ok(header) = Header::from_bytes(name, value) {
            response.add_header(header);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGES: [CallbackPage; 6] = [
        CallbackPage::SignedIn,
        CallbackPage::Cancelled,
        CallbackPage::NotCompleted,
        CallbackPage::ExchangeFailed,
        CallbackPage::StaleLink,
        CallbackPage::Incomplete,
    ];

    #[test]
    fn callback_response_is_html_without_cache_or_external_resources() {
        let response = response(CallbackPage::StaleLink);
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
        let csp = header("Content-Security-Policy").unwrap();
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("font-src data:"));
        for page in PAGES {
            let html = render(page).expect("a page");
            assert!(!html.contains("https://") && !html.contains("http://"));
            for (family, _) in FONTS {
                assert!(html.contains(&format!("font-family: \"{family}\"")));
            }
        }
    }
}
