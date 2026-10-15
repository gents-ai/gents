use base64::Engine as _;
use tiny_http::{Header, Response, StatusCode as TinyStatusCode};

// Shared self-contained OAuth callback UI.

/// The app's typefaces, inlined: the page's CSP admits no other origin, and
/// the callback must look the same offline and with any system fonts.
const FONTS: [(&str, &[u8]); 2] = [
    ("Inter Variable", include_bytes!("../fonts/inter.woff2")),
    ("Lora Variable", include_bytes!("../fonts/lora.woff2")),
];

/// What the browser tab says once a provider sends the person back. The tab
/// is a dead end, so it only says where to go next; the reason a sign-in
/// failed, which can carry provider text, goes to the app instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackPage {
    SignedIn,
    Cancelled,
    /// The provider ended the sign-in without a grant.
    NotCompleted,
    /// The provider granted it, but the code could not be exchanged.
    ExchangeFailed,
    /// The state did not match this sign-in: an old tab or a second visit.
    StaleLink,
    /// The redirect arrived without the parts a sign-in needs.
    Incomplete,
    /// Any other path, such as a browser's favicon request.
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

/// The page's HTML, or `None` for a path that is not a sign-in at all.
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
    fn callback_pages_distinguish_success_cancellation_and_errors() {
        let text = |page| render(page).expect("a page");
        assert!(text(CallbackPage::SignedIn).contains("You’re signed in."));
        assert!(text(CallbackPage::Cancelled).contains("No rush."));
        for page in [
            CallbackPage::NotCompleted,
            CallbackPage::ExchangeFailed,
            CallbackPage::StaleLink,
            CallbackPage::Incomplete,
        ] {
            assert!(page.status() >= 400);
            assert!(text(page).contains("Let’s try that again."));
        }
        for page in PAGES {
            assert!(!text(page).contains("{{"));
        }
        assert_eq!(render(CallbackPage::NotFound), None);
    }

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

    #[test]
    fn an_unknown_path_is_a_plain_not_found() {
        let response = response(CallbackPage::NotFound);
        assert_eq!(response.status_code().0, 404);
        assert!(response
            .headers()
            .iter()
            .any(|header| header.field.equiv("Content-Type")
                && header.value.as_str().starts_with("text/plain")));
    }
}
