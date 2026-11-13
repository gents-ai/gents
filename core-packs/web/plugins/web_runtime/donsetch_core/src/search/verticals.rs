//! Portable date normalization shared by upstream ranking signals.

/// Convert an RFC-822-style RSS date to an ISO calendar date.
pub fn rss_date_to_iso(date: &str) -> Option<String> {
    let parts: Vec<&str> = date.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| parts[2].starts_with(m))?;
    Some(format!("{}-{:02}-{}", parts[3], month + 1, parts[1]))
}
