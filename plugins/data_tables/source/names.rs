//! Table and column names: turning file names into SQL names that need no
//! quoting, and keeping them unique.
use std::collections::HashSet;

const MAX_NAME_CHARS: usize = 63;

/// `raw` as a name of letters, digits and underscores that does not start with a digit.
/// Runs of other characters become one underscore; case is kept.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut gap = false;
    for c in raw.chars() {
        if c.is_alphanumeric() || c == '_' {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    if out.is_empty() {
        out.push_str("table");
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert_str(0, "t_");
    }
    out.chars().take(MAX_NAME_CHARS).collect()
}

/// `name`, or `name_2`, `name_3`... the first not in `taken`; the result is added to `taken`.
pub fn unique(name: &str, taken: &mut HashSet<String>) -> String {
    let mut candidate = name.to_string();
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{name}_{n}");
        n += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing() {
        for (raw, want) in [
            ("sales", "sales"),
            ("Sales 2024", "Sales_2024"),
            ("a--b..c", "a_b_c"),
            ("2024 data", "t_2024_data"),
            ("  --  ", "table"),
            ("", "table"),
            ("data (copy)", "data_copy"),
            ("naïve été", "naïve_été"),
            ("a/b.csv", "a_b_csv"),
        ] {
            assert_eq!(sanitize(raw), want, "{raw:?}");
        }
        assert_eq!(sanitize(&"x".repeat(100)).chars().count(), 63);
    }

    #[test]
    fn unique_names_count_up_and_are_remembered() {
        let mut taken = HashSet::new();
        assert_eq!(unique("t", &mut taken), "t");
        assert_eq!(unique("t", &mut taken), "t_2");
        assert_eq!(unique("t", &mut taken), "t_3");
        assert_eq!(unique("t_2", &mut taken), "t_2_2");
    }
}
