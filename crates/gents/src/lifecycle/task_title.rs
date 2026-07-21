use chrono::{DateTime, Utc};

const TASK_TITLE_LABEL_MAX_LEN: usize = 56;

pub fn task_run_conversation_title(task_label: &str) -> String {
    task_run_conversation_title_at(task_label, Utc::now())
}

pub(crate) fn task_run_conversation_title_at(task_label: &str, timestamp: DateTime<Utc>) -> String {
    let slug = slugify_task_label(task_label);
    let timestamp = timestamp
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
        .to_lowercase();
    format!("{slug}-{timestamp}")
}

fn slugify_task_label(task_label: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for ch in task_label.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_was_dash = false;
            continue;
        }

        if !slug.is_empty() && !previous_was_dash {
            slug.push('-');
            previous_was_dash = true;
        }
    }

    let mut slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "task".to_string();
    }

    if slug.len() > TASK_TITLE_LABEL_MAX_LEN {
        slug.truncate(TASK_TITLE_LABEL_MAX_LEN);
        slug = slug.trim_matches('-').to_string();
    }

    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::task_run_conversation_title_at;

    #[test]
    fn task_run_conversation_title_slugs_label_and_appends_timestamp() {
        let timestamp = Utc.with_ymd_and_hms(2026, 4, 30, 18, 4, 5).unwrap();

        assert_eq!(
            task_run_conversation_title_at("Mini 1 Host Health 6h", timestamp),
            "mini-1-host-health-6h-20260430t180405z"
        );
    }

    #[test]
    fn task_run_conversation_title_falls_back_for_empty_labels() {
        let timestamp = Utc.with_ymd_and_hms(2026, 4, 30, 18, 4, 5).unwrap();

        assert_eq!(
            task_run_conversation_title_at(" :  ", timestamp),
            "task-20260430t180405z"
        );
    }
}
