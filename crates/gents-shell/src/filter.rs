#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Exact,
    Match,
}

pub fn subsequence(query: &str, text: &str) -> bool {
    let mut text = text.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| text.any(|c| c == wanted))
}

/// Aliases match by prefix: they are hidden, so a subsequence hit would select a row for no
/// visible reason, and `!` alone would reach `q!` and quit.
pub fn rank(query: &str, label: &str, aliases: &[&str]) -> Option<Rank> {
    let lowered = query.to_lowercase();
    let mut aliases = aliases.iter().map(|alias| alias.to_lowercase());
    if label.to_lowercase() == lowered || aliases.clone().any(|alias| alias == lowered) {
        Some(Rank::Exact)
    } else if subsequence(query, label) || aliases.any(|alias| alias.starts_with(&lowered)) {
        Some(Rank::Match)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_in_order_ignoring_case() {
        assert!(subsequence("rev", "Reviewer"));
        assert!(subsequence("REV", "Reviewer"));
        assert!(subsequence("", "Reviewer"));
        assert!(subsequence("rw", "Reviewer"));
        assert!(!subsequence("zz", "Reviewer"));
        assert!(!subsequence("wr", "Rw"));
        assert!(subsequence("日", "日本語"));
        assert!(!subsequence("日本語", "日本"));
        assert!(subsequence("rev", "flaky recovery test"));
    }

    #[test]
    fn rank_prefers_an_exact_label_or_alias() {
        assert_eq!(rank("q", "Quit", &["q"]), Some(Rank::Exact));
        assert_eq!(rank("QUIT", "Quit", &[]), Some(Rank::Exact));
        assert_eq!(rank("tabn", "New tab", &["tabnew"]), Some(Rank::Match));
        assert_eq!(rank("x", "New tab", &["tabnew"]), None);
        assert_eq!(rank("", "New tab", &[]), Some(Rank::Match));
        assert!(Rank::Exact < Rank::Match);
    }

    #[test]
    fn aliases_match_by_prefix_and_labels_by_subsequence() {
        assert_eq!(rank("TABN", "New tab", &["tabnew"]), Some(Rank::Match));
        assert_eq!(rank("!", "Quit", &["q!"]), None);
        assert_eq!(rank("a", "Quit", &["qa"]), None);
        assert_eq!(rank("q!", "Quit", &["q!"]), Some(Rank::Exact));
        assert_eq!(rank("qi", "Quit", &["q!"]), Some(Rank::Match));
    }
}
