//! Positive content tests: "the extraction produced text" is not
//! the same claim as "the page had content".
//!
//! Issue #282: the only guard on the success path was
//! non-emptiness, so a client-rendered shell's navigation/login
//! chrome and an unsolved challenge interstitial both shipped as
//! ContentOk. The tests here answer "is this text actually the
//! page's content" for the shapes that slipped through; the
//! challenge-side twin lives in `detect::walls::challenge_text`.

/// Extracted text that is only page chrome: navigation, footer and
/// authentication prompts, no prose.
///
/// Calibrated against live shapes: reddit's shell renders nav + a
/// post title + sidebar + "Continue with Email" (262 chars, served
/// as ContentOk); the old.reddit login wall is "Log in or sign"
/// plus footer links (293 chars, served as ContentOk after a
/// bypass). Both are small, prompt-laden, and prose-poor. A short
/// real page (example.com, a profile card) carries no auth prompts;
/// a long page fails the size bound; a real article whose header
/// carries a login link fails the share test.
pub fn chrome_only(markdown: &str) -> bool {
    let chars = markdown.chars().count();
    if chars == 0 || chars > 700 {
        return false;
    }
    let lower = markdown.to_lowercase();
    const PROMPTS: &[&str] = &[
        "log in",
        "sign in",
        "sign up",
        "continue with email",
        "continue with phone",
        "continue with google",
        "continue with apple",
        "join the most real place",
        "we use cookies",
        "cookie policy",
    ];
    if !PROMPTS.iter().any(|m| lower.contains(m)) {
        return false;
    }
    prose_share(markdown) < 0.5
}

/// The share of characters on prose-like lines: at least 60 chars
/// and 8 words. Chrome is labels and links; content is sentences.
fn prose_share(markdown: &str) -> f64 {
    let total = markdown.chars().count();
    if total == 0 {
        return 0.0;
    }
    let prose: usize = markdown
        .lines()
        .filter(|l| l.chars().count() >= 60 && l.split_whitespace().count() >= 8)
        .map(|l| l.chars().count())
        .sum();
    prose as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    // The reported shape (#282 case A): nav + post title + sidebar +
    // auth prompts, 262 chars, served as ContentOk by the ghost.
    #[test]
    fn the_reddit_shell_shape_is_chrome() {
        let md = "# TIL that Pierce Brosnan was fired\n\
                  http://127.0.0.1:8901/fixture-shell.html\n\n\
                  Reddit r/todayilearned Log In\n\n\
                  # TIL that Pierce Brosnan was fired from the James Bond series during a 2 minute phone call with the producers.\n\n\
                  Continue with Phone Number Continue with Email\n\n\
                  Join the most real place on the internet";
        assert!(chrome_only(md));
    }

    // The old.reddit login wall served after a bypass (293 chars).
    #[test]
    fn a_login_wall_is_chrome() {
        assert!(chrome_only(
            "# Welcome to Reddit\n\n> Log in or sign\n\nUser Agreement Privacy Policy Content Policy Help\n\nWelcome to Reddit, the front page of the internet."
        ));
    }

    // Short pages with real content must keep passing.
    #[test]
    fn short_real_pages_are_not_chrome() {
        // example.com: tiny, prose, no prompts.
        assert!(!chrome_only(
            "# Example Domain\n\nThis domain is for use in illustrative examples in documents. You may use this domain in literature without prior coordination or asking for permission."
        ));
        // A profile card: no prompts, short labels.
        assert!(!chrome_only("cristiano\n679M followers\n12 posts"));
        // A real article whose header carries a login link: the
        // prose dominates.
        let article = format!(
            "Log in\n\n# How DNS works\n\n{}",
            "This paragraph is a real sentence of article content that runs well past sixty characters. "
                .repeat(4)
        );
        assert!(!chrome_only(&article));
        // A long page near an auth prompt is never flagged: the
        // size bound protects it.
        let long = format!("Sign up\n{}", "word ".repeat(300));
        assert!(!chrome_only(&long));
    }
}
