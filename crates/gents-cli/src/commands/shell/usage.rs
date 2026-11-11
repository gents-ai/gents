//! The desktop's Usage limits (`ProviderUsage.tsx:329-423`), as the `/usage` menu.

use chrono::{DateTime, Utc};
use gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
use gents_inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
use gents_inference::clients::xai_grok::XAI_OAUTH_PROVIDER;
use gents_inference::usage_observation::{AccountUsageRead, UsageRead, UsageTrigger, WindowView};
use gents_shell::sample::Tone;
use gents_shell::source::{MenuAct, MenuRow};

use super::menus::{blocking, row};
use super::work::Work;
use crate::commands::accounts::short_duration;

/// A meter's cells.
const BAR: usize = 10;

/// The node's accounts' limits, read through the runtime as the desktop reads them
/// (`accounts/usage.rs:105-147`); `refresh` asks each provider again, as its button does.
pub(super) async fn read(
    work: &Result<Work, String>,
    refresh: bool,
) -> Result<Vec<MenuRow>, String> {
    let work = work.as_ref().map_err(String::clone)?;
    let trigger = match refresh {
        true => UsageTrigger::Refresh,
        false => UsageTrigger::Open,
    };
    let home = work.home.clone();
    let reads = async {
        let identity = blocking(move || {
            crate::commands::p2p::enrollment_admin::resolve_home_identity(Some(&home))
        })
        .await?;
        crate::commands::accounts::request_usage_reads(
            identity.as_ref(),
            &work.endpoint,
            trigger,
            None,
        )
        .await
    };
    let reads = reads.await.map_err(|error| format!("{error:#}"))?;
    Ok(rows(&reads.reads, Utc::now()))
}

/// Refresh, then each account: its name and provider, then its windows as rows whose bars line
/// up across the menu, grouped as `UsageRows` groups them (`ProviderUsage.tsx:247-296`).
pub(super) fn rows(reads: &[AccountUsageRead], now: DateTime<Utc>) -> Vec<MenuRow> {
    let shown = reads
        .iter()
        .filter(|read| read.outcome != UsageRead::Disabled);
    let labels = shown.flat_map(|read| read.usage.iter().flat_map(|usage| &usage.windows));
    let width = labels.map(|w| w.label.chars().count()).max();
    let width = width.unwrap_or_default();
    let refresh = MenuRow {
        enter: Some(MenuAct::RefreshUsage),
        ..row(" Refresh".to_string(), Tone::Accent)
    };
    let mut rows = vec![refresh];
    if reads.is_empty() {
        let none = " No account limits have been reported.";
        rows.push(row(none.to_string(), Tone::Label));
    }
    for read in reads {
        let title = provider_title(&read.provider);
        let header = match title == read.label {
            true => format!(" {}", read.label),
            false => format!(" {}   {title}", read.label),
        };
        rows.push(row(header, Tone::Plain));
        if read.outcome == UsageRead::Disabled {
            rows.push(row("   Disabled".to_string(), Tone::Label));
            continue;
        }
        let windows = read.usage.iter().flat_map(|usage| &usage.windows);
        let mut groups: Vec<(&str, Vec<&WindowView>)> = Vec::new();
        for window in windows {
            let group = window_group(&window.label);
            match groups.iter_mut().find(|(label, _)| *label == group) {
                Some((_, members)) => members.push(window),
                None => groups.push((group, vec![window])),
            }
        }
        for (group, windows) in &groups {
            if groups.len() > 1 {
                rows.push(row(format!("   {group}"), Tone::Label));
            }
            rows.extend(windows.iter().map(|window| window_row(window, width, now)));
        }
        if groups.is_empty() {
            let note = read.usage.as_ref().and_then(|usage| usage.note.as_deref());
            let note = note.unwrap_or("Usage unknown");
            rows.push(row(format!("   {note}"), Tone::Label));
        }
        let why = match &read.outcome {
            UsageRead::Unavailable(reason) => Some(format!("Not read: {reason}")),
            _ => read
                .usage
                .as_ref()
                .and_then(|usage| usage.read_error.clone()),
        };
        rows.extend(why.map(|why| row(format!("   {why}"), Tone::Label)));
    }
    rows
}

/// `WindowRow` (`ProviderUsage.tsx:136-155`): its name, how full it is and when it empties,
/// marked from 75% as `usageLevel` warns.
fn window_row(window: &WindowView, width: usize, now: DateTime<Utc>) -> MenuRow {
    let pct = window.used_pct.round().max(0.0) as usize;
    let filled = (pct.min(100) * BAR + 50) / 100;
    let bar = "█".repeat(filled) + &"░".repeat(BAR - filled);
    let when = match window.resets_at {
        Some(at) => format!("resets in {}", short_duration((at - now).num_seconds())),
        None if window.last_known => "last known".to_string(),
        None => String::new(),
    };
    let label = &window.label;
    let text = format!("   {label:<width$}  {bar}  {pct:>3}%  {when}");
    let tone = match pct >= 75 {
        true => Tone::Warn,
        false => Tone::Plain,
    };
    row(text.trim_end().to_string(), tone)
}

/// `usageWindowGroup` (`ProviderUsage.tsx:239-245`).
fn window_group(label: &str) -> &'static str {
    match label {
        label if label.contains("tokens") => "Token limits",
        "requests" => "Request limits",
        "daily" | "weekly" | "monthly" | "credits" => "Spending limits",
        _ => "Quota windows",
    }
}

/// `providerTitle` (`ProviderUsage.tsx:218-222`): a subscription's title, else its kind's label
/// (`inferenceKinds.ts:15-22`), else the provider as named.
fn provider_title(provider: &str) -> &str {
    match provider {
        CHATGPT_CODEX_PROVIDER => "ChatGPT / Codex",
        XAI_OAUTH_PROVIDER => "Grok / xAI",
        CLAUDE_OAUTH_PROVIDER => "Anthropic / Claude",
        "OpenAiCompatible" => "OpenAI compatible",
        "OpenRouter" => "OpenRouter",
        "ChatGptCodex" => "ChatGPT / Codex (subscription)",
        "XaiGrokOAuth" => "Grok (subscription)",
        "ClaudeCliSubscription" => "Anthropic / Claude (subscription)",
        "AnthropicApiKey" => "Anthropic API key",
        other => other,
    }
}

#[cfg(test)]
mod tests;
