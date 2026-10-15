//! The names a template is filled with, all derived from the pack name.
//!
//! The CLI delegates name validation and spelling to this owner.

/// `review_toolkit` -> `review-toolkit` (document ids) and
/// `ReviewToolkit` (collection names), both derived from one name.
#[derive(Clone, Debug)]
pub struct Names {
    pub name: String,
    pub namespace: String,
    /// `review_toolkit` -> `review-toolkit`, for document ids.
    pub kebab: String,
    /// `review_toolkit` -> `ReviewToolkit`, for collection names.
    pub title: String,
}

impl Names {
    pub fn new(name: &str, namespace: &str) -> anyhow::Result<Self> {
        let snake = to_snake_case(name);
        anyhow::ensure!(
            snake == name && !name.is_empty(),
            "pack names are snake_case; use {snake:?}"
        );
        Ok(Self {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            kebab: name.replace('_', "-"),
            title: name
                .split('_')
                .map(|part| {
                    let mut chars = part.chars();
                    chars
                        .next()
                        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                        .unwrap_or_default()
                })
                .collect(),
        })
    }
}

/// `ReviewToolkit` -> `review_toolkit`, the inverse spelling [`Names::new`]
/// validates against.
pub fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_every_spelling_from_one_snake_case_name() {
        let names = Names::new("review_toolkit", "acme").unwrap();
        assert_eq!(names.name, "review_toolkit");
        assert_eq!(names.namespace, "acme");
        assert_eq!(names.kebab, "review-toolkit");
        assert_eq!(names.title, "ReviewToolkit");
    }

    #[test]
    fn a_non_snake_case_name_is_refused_with_the_spelling_to_use() {
        let error = Names::new("ReviewToolkit", "acme").unwrap_err();
        assert!(
            format!("{error:#}").contains("\"review_toolkit\""),
            "{error:#}"
        );
        assert!(Names::new("", "acme").is_err());
    }

    /// Mirrors the CLI scaffold tests' own expectation for the same input,
    /// so the two spellings cannot drift apart.
    #[test]
    fn to_snake_case_matches_the_cli_rules() {
        assert_eq!(to_snake_case("ReviewToolkit"), "review_toolkit");
        assert_eq!(to_snake_case("review-toolkit"), "review_toolkit");
        assert_eq!(to_snake_case("already_snake"), "already_snake");
        assert_eq!(to_snake_case("with space"), "with_space");
        assert_eq!(to_snake_case("trailing__"), "trailing");
    }
}
