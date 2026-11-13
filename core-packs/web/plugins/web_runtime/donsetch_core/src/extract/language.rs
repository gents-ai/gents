//! Language + script detection. Drives tokenization strategy
//! (CJK bigrams vs word-split), stopword filtering, and
//! stemming. No external model : pure Unicode-range analysis
//! + HTML hints. Good enough to route tokenization correctly;
//!
//! The focus filter degrades gracefully on wrong calls.
//!

use scraper::Html;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    Latin,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Devanagari, // Hindi, Nepali, Sanskrit
    Thai,
    Hangul, // Korean
    Han,    // CJK ideographs (Chinese, Japanese kanji)
    Kana,   // Japanese hiragana + katakana
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageInfo {
    /// BCP-47 tag from <html lang="..."> or meta, or best
    /// guess from script analysis. "en", "zh", "ja", "ko",
    /// "ar", "hi", "ne", "th", "ru", "de", "fr", "es", "pt",
    /// "unknown".
    pub code: String,
    /// Dominant script (drives tokenization).
    pub script: Script,
    /// All scripts present in the text (for mixed content).
    pub scripts: Vec<Script>,
}

/// Classify a single character's script.
pub fn char_script(c: char) -> Script {
    let u = c as u32;
    // CJK Unified Ideographs + Extensions A/B/C/D/E/F
    // + Compatibility Ideographs + Radicals + Strokes
    if (0x4E00..=0x9FFF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)      // Ext A
        || (0x20000..=0x2A6DF).contains(&u)    // Ext B
        || (0x2A700..=0x2B73F).contains(&u)   // Ext C
        || (0x2B740..=0x2B81F).contains(&u)    // Ext D
        || (0x2B820..=0x2CEAF).contains(&u)    // Ext E
        || (0x2CEB0..=0x2EBEF).contains(&u)    // Ext F
        || (0xF900..=0xFAFF).contains(&u)      // Compatibility Ideographs
        || (0x2F800..=0x2FA1F).contains(&u)   // Compatibility Supplement
        || (0x2E80..=0x2EFF).contains(&u)      // Radicals Supplement
        || (0x2F00..=0x2FDF).contains(&u)      // Kangxi Radicals
        || (0x31C0..=0x31EF).contains(&u)
    // CJK Strokes
    {
        return Script::Han;
    }
    // Hiragana
    if (0x3040..=0x309F).contains(&u) {
        return Script::Kana;
    }
    // Katakana
    if (0x30A0..=0x30FF).contains(&u) || (0xFF66..=0xFF9F).contains(&u) {
        return Script::Kana;
    }
    // Hangul syllables
    if (0xAC00..=0xD7AF).contains(&u)
        || (0x1100..=0x11FF).contains(&u)
        || (0x3130..=0x318F).contains(&u)
    {
        return Script::Hangul;
    }
    // Thai
    if (0x0E00..=0x0E7F).contains(&u) {
        return Script::Thai;
    }
    // Arabic
    if (0x0600..=0x06FF).contains(&u) || (0x0750..=0x077F).contains(&u) {
        return Script::Arabic;
    }
    // Hebrew
    if (0x0590..=0x05FF).contains(&u) {
        return Script::Hebrew;
    }
    // Devanagari (Hindi, Nepali, Sanskrit)
    if (0x0900..=0x097F).contains(&u) {
        return Script::Devanagari;
    }
    // Greek
    if (0x0370..=0x03FF).contains(&u) || (0x1F00..=0x1FFF).contains(&u) {
        return Script::Greek;
    }
    // Cyrillic
    if (0x0400..=0x04FF).contains(&u) || (0x0500..=0x052F).contains(&u) {
        return Script::Cyrillic;
    }
    // Latin (basic + extended)
    if c.is_ascii_alphanumeric() || (0x00C0..=0x024F).contains(&u) || (0x1E00..=0x1EFF).contains(&u)
    {
        return Script::Latin;
    }
    Script::Other
}

/// Whether a script needs character-level tokenization
/// (no spaces between words). CJK, Hangul, Thai.
pub fn needs_char_tokenize(s: Script) -> bool {
    matches!(
        s,
        Script::Han | Script::Kana | Script::Hangul | Script::Thai
    )
}

/// Detect language from raw text (no HTML). Used by crawl
/// scoring on focus queries + anchor text. Samples up to 5000
/// chars and classifies by script census.
pub fn detect_from_text(text: &str) -> LanguageInfo {
    let mut counts: std::collections::HashMap<Script, usize> = std::collections::HashMap::new();
    let mut sampled = 0usize;
    for c in text.chars() {
        let s = char_script(c);
        if s != Script::Other && (s != Script::Latin || c.is_alphabetic()) {
            *counts.entry(s).or_insert(0) += 1;
        }
        sampled += 1;
        if sampled > 5000 {
            break;
        }
    }
    // Count threshold: long samples need c>5 to matter, but a
    // 4-char CJK query (机器学习) must still detect : 4 < 6
    // would wrongly fall through to Latin.
    let min_count = if sampled < 30 { 1 } else { 5 };
    let mut scripts: Vec<Script> = counts
        .iter()
        .filter(|&(_, &c)| c > min_count)
        .map(|(&s, _)| s)
        .collect();
    scripts.sort_by(|a, b| {
        counts
            .get(b)
            .copied()
            .unwrap_or(0)
            .cmp(&counts.get(a).copied().unwrap_or(0))
    });
    if scripts.is_empty() {
        return LanguageInfo {
            code: "en".to_string(),
            script: Script::Latin,
            scripts: vec![Script::Latin],
        };
    }
    let dominant = *scripts.first().unwrap();
    let code = script_to_lang(dominant, &scripts);
    LanguageInfo {
        code,
        script: dominant,
        scripts,
    }
}

/// Detect language from an HTML document. Checks
/// `<html lang>`, `<meta http-equiv=content-language>`,
/// then falls back to script analysis of text content.
pub fn detect(doc: &Html) -> LanguageInfo {
    // 1. HTML lang attribute : most reliable.
    if let Some(lang) = html_lang(doc) {
        let script = lang_to_script(&lang);
        return LanguageInfo {
            code: lang,
            script,
            scripts: vec![script],
        };
    }
    // 2. Meta content-language.
    if let Some(lang) = meta_content_language(doc) {
        let script = lang_to_script(&lang);
        return LanguageInfo {
            code: lang,
            script,
            scripts: vec![script],
        };
    }
    // 3. Script analysis of text content.
    script_analysis(doc)
}

fn html_lang(doc: &Html) -> Option<String> {
    let sel = scraper::Selector::parse("html[lang]").ok()?;
    let lang = doc.select(&sel).next()?.value().attr("lang")?;
    let code = normalize_lang(lang);
    if code.is_empty() || code == "und" {
        return None;
    }
    Some(code)
}

fn meta_content_language(doc: &Html) -> Option<String> {
    for selector in &[
        "meta[http-equiv='content-language']",
        "meta[http-equiv='Content-Language']",
        "meta[name='language']",
    ] {
        if let Ok(sel) = scraper::Selector::parse(selector)
            && let Some(el) = doc.select(&sel).next()
            && let Some(content) = el.value().attr("content")
        {
            let code = normalize_lang(content);
            if !code.is_empty() && code != "und" {
                return Some(code);
            }
        }
    }
    None
}

fn script_analysis(doc: &Html) -> LanguageInfo {
    let body_sel = scraper::Selector::parse("body").ok();
    let root: scraper::ElementRef<'_> = if let Some(sel) = &body_sel {
        doc.select(sel).next().unwrap_or_else(|| {
            doc.select(&scraper::Selector::parse("html").unwrap())
                .next()
                .unwrap()
        })
    } else {
        doc.select(&scraper::Selector::parse("html").unwrap())
            .next()
            .unwrap()
    };

    // Sample body text, then classify with the same census the
    // text path uses.
    let mut sampled = String::new();
    for t in root.text() {
        sampled.push_str(t);
        if sampled.len() > 20_000 {
            break;
        }
    }
    detect_from_text(&sampled)
}

/// Map a BCP-47 tag to its dominant script.
fn lang_to_script(lang: &str) -> Script {
    let primary = lang.split(['-', '_']).next().unwrap_or("").to_lowercase();
    match primary.as_str() {
        "zh" | "zh-cn" | "zh-tw" | "zh-hk" | "zh-sg" | "zh-mo" => Script::Han,
        "ja" => Script::Han, // Japanese mixes Han + Kana; Han is dominant usually
        "ko" => Script::Hangul,
        "th" => Script::Thai,
        "ar" => Script::Arabic,
        "he" | "iw" => Script::Hebrew,
        "hi" | "ne" | "sa" | "mr" | "bn" | "gu" | "ta" | "te" | "pa" => Script::Devanagari,
        "ru" | "uk" | "be" | "bg" | "sr" | "mk" | "kk" | "ky" | "mn" => Script::Cyrillic,
        "el" => Script::Greek,
        _ => Script::Latin,
    }
}

/// Map a dominant script to a best-guess language code.
/// When Han + Kana co-occur → Japanese. Han alone → Chinese.
fn script_to_lang(dominant: Script, all: &[Script]) -> String {
    match dominant {
        Script::Han => {
            if all.contains(&Script::Kana) {
                "ja".to_string()
            } else {
                "zh".to_string()
            }
        }
        Script::Kana => "ja".to_string(),
        Script::Hangul => "ko".to_string(),
        Script::Thai => "th".to_string(),
        Script::Arabic => "ar".to_string(),
        Script::Hebrew => "he".to_string(),
        Script::Devanagari => "hi".to_string(), // could be ne, but hi is more common
        Script::Cyrillic => "ru".to_string(),
        Script::Greek => "el".to_string(),
        Script::Latin => "en".to_string(), // default Latin = English
        Script::Other => "unknown".to_string(),
    }
}

/// Normalize a raw lang attribute to a short BCP-47 code.
/// "en-US" → "en", "zh-Hant" → "zh", "ja-JP" → "ja".
fn normalize_lang(raw: &str) -> String {
    let raw = raw.trim().to_lowercase();
    if raw.is_empty() {
        return String::new();
    }
    // Take the primary subtag.
    let primary = raw.split(['-', '_']).next().unwrap_or("");
    if primary.is_empty() {
        return String::new();
    }
    // Validate: should be 2-3 alpha chars.
    if (primary.len() == 2 || primary.len() == 3)
        && primary.chars().all(|c| c.is_ascii_alphabetic())
    {
        return primary.to_string();
    }
    String::new()
}

/// Whether the language code indicates a CJK language
/// (needs character-level tokenization).
#[allow(dead_code)]
pub fn is_cjk(lang: &str) -> bool {
    matches!(lang, "zh" | "ja" | "ko")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Html {
        Html::parse_document(html)
    }

    #[test]
    fn html_lang_attr() {
        let doc = parse(r#"<html lang="ja"><body>こんにちは</body></html>"#);
        let info = detect(&doc);
        assert_eq!(info.code, "ja");
        assert_eq!(info.script, Script::Han);
    }

    #[test]
    fn html_lang_with_region() {
        let doc = parse(r#"<html lang="zh-Hant"><body>你好</body></html>"#);
        let info = detect(&doc);
        assert_eq!(info.code, "zh");
    }

    #[test]
    fn meta_content_language() {
        let doc = parse(
            r#"<html><head><meta http-equiv="content-language" content="de"></head><body>Hallo</body></html>"#,
        );
        let info = detect(&doc);
        assert_eq!(info.code, "de");
    }

    #[test]
    fn script_analysis_chinese() {
        let doc = parse("<html><body>机器学习是人工智能的一个分支</body></html>");
        let info = detect(&doc);
        assert_eq!(info.script, Script::Han);
        assert_eq!(info.code, "zh");
    }

    #[test]
    fn script_analysis_japanese() {
        let doc = parse("<html><body>これは日本語のテキストです。機械学習の分野</body></html>");
        let info = detect(&doc);
        assert_eq!(info.code, "ja");
        // Should detect both Han and Kana.
        assert!(info.scripts.contains(&Script::Kana));
    }

    #[test]
    fn script_analysis_korean() {
        let doc = parse("<html><body>안녕하세요. 이것은 한국어 텍스트입니다.</body></html>");
        let info = detect(&doc);
        assert_eq!(info.code, "ko");
        assert_eq!(info.script, Script::Hangul);
    }

    #[test]
    fn script_analysis_arabic() {
        let doc = parse("<html><body>هذا نص باللغة العربية</body></html>");
        let info = detect(&doc);
        assert_eq!(info.code, "ar");
        assert_eq!(info.script, Script::Arabic);
    }

    #[test]
    fn script_analysis_devanagari() {
        let doc = parse("<html><body>यह हिंदी का पाठ है</body></html>");
        let info = detect(&doc);
        assert_eq!(info.script, Script::Devanagari);
    }

    #[test]
    fn script_analysis_thai() {
        let doc = parse("<html><body>นี่คือข้อความภาษาไทย</body></html>");
        let info = detect(&doc);
        assert_eq!(info.script, Script::Thai);
    }

    #[test]
    fn script_analysis_cyrillic() {
        let doc = parse("<html><body>Это текст на русском языке</body></html>");
        let info = detect(&doc);
        assert_eq!(info.script, Script::Cyrillic);
        assert_eq!(info.code, "ru");
    }

    #[test]
    fn default_latin_is_english() {
        let doc = parse("<html><body>This is English text content.</body></html>");
        let info = detect(&doc);
        assert_eq!(info.code, "en");
        assert_eq!(info.script, Script::Latin);
    }

    #[test]
    fn empty_page() {
        let doc = parse("<html><body></body></html>");
        let info = detect(&doc);
        // No lang attr, no text → defaults to English.
        assert_eq!(info.code, "en");
    }

    #[test]
    fn needs_char_tokenize_cjk() {
        assert!(needs_char_tokenize(Script::Han));
        assert!(needs_char_tokenize(Script::Kana));
        assert!(needs_char_tokenize(Script::Hangul));
        assert!(needs_char_tokenize(Script::Thai));
        assert!(!needs_char_tokenize(Script::Latin));
        assert!(!needs_char_tokenize(Script::Cyrillic));
    }

    #[test]
    fn is_cjk_check() {
        assert!(is_cjk("zh"));
        assert!(is_cjk("ja"));
        assert!(is_cjk("ko"));
        assert!(!is_cjk("en"));
        assert!(!is_cjk("th"));
    }

    #[test]
    fn char_script_boundaries() {
        assert_eq!(char_script('a'), Script::Latin);
        assert_eq!(char_script('A'), Script::Latin);
        assert_eq!(char_script('ä'), Script::Latin);
        assert_eq!(char_script('中'), Script::Han);
        assert_eq!(char_script('あ'), Script::Kana);
        assert_eq!(char_script('ア'), Script::Kana);
        assert_eq!(char_script('한'), Script::Hangul);
        assert_eq!(char_script('ก'), Script::Thai);
        assert_eq!(char_script('ا'), Script::Arabic);
        assert_eq!(char_script('א'), Script::Hebrew);
        assert_eq!(char_script('अ'), Script::Devanagari);
        assert_eq!(char_script('α'), Script::Greek);
        assert_eq!(char_script('я'), Script::Cyrillic);
        assert_eq!(char_script(' '), Script::Other);
        assert_eq!(char_script('1'), Script::Latin);
    }
}
