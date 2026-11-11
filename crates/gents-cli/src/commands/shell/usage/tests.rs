use chrono::{Duration, TimeZone};
use gents::inference::usage_observation::account_usage::UsageSource;
use gents::inference::usage_observation::{UsageView, WindowView};
use gents_shell::sample::Tone::{Accent, Label, Plain, Warn};

use super::*;

fn window(label: &str, used_pct: f64, resets_at: Option<DateTime<Utc>>) -> WindowView {
    WindowView {
        label: label.to_string(),
        window_minutes: None,
        used_pct,
        resets_at,
        resets_in_secs: None,
        source: UsageSource::Endpoint,
        observed_at: Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap(),
        age_secs: 0,
        last_known: false,
    }
}

fn account(
    label: &str,
    provider: &str,
    outcome: UsageRead,
    windows: Option<Vec<WindowView>>,
) -> AccountUsageRead {
    AccountUsageRead {
        label: label.to_string(),
        provider: provider.to_string(),
        account_ref: None,
        backend_id: None,
        outcome,
        usage: windows.map(|windows| UsageView {
            windows,
            plan: None,
            note: None,
            read_at: None,
            read_error: None,
        }),
    }
}

fn shown(rows: Vec<MenuRow>) -> Vec<(String, Tone, Option<MenuAct>)> {
    rows.into_iter()
        .map(|row| (row.text, row.tone, row.enter))
        .collect()
}

fn text(text: &str, tone: Tone) -> (String, Tone, Option<MenuAct>) {
    (text.to_string(), tone, None)
}

fn refresh() -> (String, Tone, Option<MenuAct>) {
    (" Refresh".to_string(), Accent, Some(MenuAct::RefreshUsage))
}

#[test]
fn each_account_shows_its_windows_as_the_desktop_does() {
    let now = Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap();
    let soon = now + Duration::minutes(130) + Duration::seconds(30);
    let later = now + Duration::days(3) + Duration::hours(4);
    let mut kept = window("daily", 50.0, None);
    kept.last_known = true;
    let reads = [
        account(
            "Max",
            "claude-subscription",
            UsageRead::Read,
            Some(vec![
                window("5h", 42.4, Some(soon)),
                window("7d", 80.0, Some(later)),
            ]),
        ),
        account(
            "Grok",
            "xai-oauth",
            UsageRead::Unavailable("no billing endpoint".into()),
            None,
        ),
        account("Codex", "chatgpt-codex", UsageRead::Disabled, None),
        account(
            "OpenRouter",
            "OpenRouter",
            UsageRead::Read,
            Some(vec![window("tokens/min", 10.0, None), kept]),
        ),
    ];

    let rows = shown(rows(&reads, now));

    assert_eq!(
        rows,
        [
            refresh(),
            text(" Max   Anthropic / Claude", Plain),
            text("   5h          ████░░░░░░   42%  resets in 2h10m", Plain),
            text("   7d          ████████░░   80%  resets in 3d4h", Warn),
            text(" Grok   Grok / xAI", Plain),
            text("   Usage unknown", Label),
            text("   Not read: no billing endpoint", Label),
            text(" Codex   ChatGPT / Codex", Plain),
            text("   Disabled", Label),
            text(" OpenRouter", Plain),
            text("   Token limits", Label),
            text("   tokens/min  █░░░░░░░░░   10%", Plain),
            text("   Spending limits", Label),
            text("   daily       █████░░░░░   50%  last known", Plain),
        ]
    );
}

#[test]
fn no_reads_say_so_under_refresh() {
    let now = Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap();

    let rows = shown(rows(&[], now));

    let none = text(" No account limits have been reported.", Label);
    assert_eq!(rows, [refresh(), none]);
}
