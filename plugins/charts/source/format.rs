//! Number formats: a short pattern language for axis and label text, the
//! automatic axis format, and the compact form used in descriptions.
//!
//! A pattern is a spec, optionally inside literal text with the spec in
//! braces: `,.2f`, `$,.0f`, `{.1%}`, `${~s} USD`. The spec is `,` (thousands
//! separators), `.N` (decimals), `~` (drop trailing zeros) and a type:
//! `f` fixed, `d` whole numbers, `%` percent (the value times 100), `s` SI
//! prefix (k, M, G), `e` scientific. Everything is plain decimal arithmetic.

use crate::err::{fail, Res};
use crate::num::{floor_log10, pow10};

/// How the number itself is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Axis default: fixed decimals with thousands separators.
    Auto,
    /// Fixed decimals.
    Fixed,
    /// Whole numbers.
    Whole,
    /// Percent.
    Percent,
    /// SI prefix.
    Si,
    /// Scientific notation.
    Sci,
}

/// A parsed number format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumFormat {
    /// Literal text before the number.
    pub prefix: String,
    /// Literal text after the number.
    pub suffix: String,
    /// Thousands separators.
    pub group: bool,
    /// Decimals; `None` takes them from the axis step.
    pub digits: Option<usize>,
    /// Drop trailing zeros.
    pub trim: bool,
    /// Number style.
    pub kind: Kind,
}

impl NumFormat {
    /// The automatic format: plain with the decimals the axis needs.
    pub fn auto() -> Self {
        Self { prefix: String::new(), suffix: String::new(), group: true, digits: None, trim: false, kind: Kind::Auto }
    }
}

/// Parses a pattern; the error is the sentence to show.
pub fn parse(pattern: &str) -> Res<NumFormat> {
    let bad = || fail(format!("format {pattern:?} is not understood; examples are \",.2f\", \"$,.0f\", \".1%\", \"~s\" and \"{{,.0f}} units\""));
    if pattern.chars().count() > 64 {
        return bad();
    }
    let (prefix, spec, suffix) = match pattern.find('{') {
        Some(open) => {
            let Some(close) = pattern[open..].find('}') else { return bad() };
            (&pattern[..open], &pattern[open + 1..open + close], &pattern[open + close + 1..])
        }
        None => ("", pattern, ""),
    };
    if prefix.contains('}') || suffix.contains(['{', '}']) {
        return bad();
    }
    let mut f = NumFormat { prefix: prefix.into(), suffix: suffix.into(), group: false, digits: None, trim: false, kind: Kind::Auto };
    let mut rest = spec;
    if let Some(r) = rest.strip_prefix(',') {
        f.group = true;
        rest = r;
    }
    if let Some(r) = rest.strip_prefix('.') {
        let end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        let n: usize = match r[..end].parse() {
            Ok(n) if n <= 12 => n,
            _ => return bad(),
        };
        f.digits = Some(n);
        rest = &r[end..];
    }
    if let Some(r) = rest.strip_prefix('~') {
        f.trim = true;
        rest = r;
    }
    f.kind = match rest {
        "" if f.digits.is_some() => Kind::Fixed,
        "" => Kind::Auto,
        "f" => Kind::Fixed,
        "d" => Kind::Whole,
        "%" => Kind::Percent,
        "s" => Kind::Si,
        "e" => Kind::Sci,
        _ => return bad(),
    };
    if f.kind == Kind::Auto {
        f.group = true;
    }
    Ok(f)
}

fn group_thousands(int_part: &str) -> String {
    let n = int_part.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn trim_zeros(s: &mut String) {
    if s.contains('.') {
        let keep = s.trim_end_matches('0').trim_end_matches('.').len();
        s.truncate(keep);
    }
}

/// `v` with `decimals` decimals, optional separators and trimmed zeros.
fn fixed(v: f64, decimals: usize, group: bool, trim: bool) -> String {
    let mut body = format!("{:.*}", decimals, v.abs());
    if trim {
        trim_zeros(&mut body);
    }
    if group {
        let (int_part, frac) = match body.split_once('.') {
            Some((i, f)) => (i.to_owned(), Some(f.to_owned())),
            None => (body.clone(), None),
        };
        body = group_thousands(&int_part);
        if let Some(f) = frac {
            body.push('.');
            body.push_str(&f);
        }
    }
    if v < 0.0 && body.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        body.insert(0, '-');
    }
    body
}

const SI: [&str; 17] = ["y", "z", "a", "f", "p", "n", "\u{b5}", "m", "", "k", "M", "G", "T", "P", "E", "Z", "Y"];

fn si(v: f64, digits: Option<usize>, trim: bool) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    let mut e3 = (floor_log10(a).div_euclid(3) * 3).clamp(-24, 24);
    let scale = |e3: i32| if e3 >= 0 { a / pow10(e3) } else { a * pow10(-e3) };
    let mut scaled = scale(e3);
    let decimals = |s: f64| digits.unwrap_or(if s >= 100.0 { 0 } else if s >= 10.0 { 1 } else { 2 });
    let mut text = format!("{:.*}", decimals(scaled), scaled);
    if text.parse::<f64>().is_ok_and(|r| r >= 1000.0) && e3 < 24 {
        e3 += 3;
        scaled = scale(e3);
        text = format!("{:.*}", decimals(scaled), scaled);
    }
    if trim || digits.is_none() {
        trim_zeros(&mut text);
    }
    let sign = if v < 0.0 { "-" } else { "" };
    format!("{sign}{text}{}", SI[(e3 / 3 + 8) as usize])
}

impl NumFormat {
    /// Writes `v`. `auto_decimals` is what an automatic or unspecified
    /// precision becomes.
    pub fn apply(&self, v: f64, auto_decimals: usize) -> String {
        if !v.is_finite() {
            return String::new();
        }
        let body = match self.kind {
            Kind::Auto | Kind::Fixed => fixed(v, self.digits.unwrap_or(auto_decimals), self.group, self.trim),
            Kind::Whole => fixed(v, 0, self.group, false),
            Kind::Percent => {
                let d = self.digits.unwrap_or_else(|| auto_decimals.saturating_sub(2));
                format!("{}%", fixed(v * 100.0, d, self.group, self.trim))
            }
            Kind::Si => si(v, self.digits, self.trim),
            Kind::Sci => {
                let d = self.digits.unwrap_or(2);
                let mut s = format!("{:.*e}", d, v);
                if self.trim {
                    if let Some((m, e)) = s.split_once('e') {
                        let mut m = m.to_owned();
                        trim_zeros(&mut m);
                        s = format!("{m}e{e}");
                    }
                }
                s
            }
        };
        format!("{}{}{}", self.prefix, body, self.suffix)
    }
}

/// The format an axis uses for `ticks`: the caller's, or an automatic one
/// that groups thousands, switches to SI prefixes from a million and to
/// scientific notation for very small steps.
pub fn axis_format(user: Option<&NumFormat>, ticks: &[f64], decimals: usize) -> NumFormat {
    if let Some(f) = user {
        return f.clone();
    }
    let max_abs = ticks.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let mut f = NumFormat::auto();
    if max_abs >= 1e6 {
        f.kind = Kind::Si;
    } else if decimals > 6 {
        f.kind = Kind::Sci;
        f.digits = Some(1);
        f.trim = true;
    }
    f
}

/// Labels for `ticks` in `format`, in order.
pub fn tick_labels(format: &NumFormat, ticks: &[f64], decimals: usize) -> Vec<String> {
    ticks.iter().map(|t| format.apply(*t, decimals)).collect()
}

/// A short, human form of `v` for descriptions and tooltips: four significant
/// digits, thousands separators, no trailing zeros.
pub fn compact(v: f64) -> String {
    if !v.is_finite() {
        return "n/a".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-4..1e15).contains(&a) {
        let mut s = format!("{v:.3e}");
        if let Some((m, e)) = s.clone().split_once('e') {
            let mut m = m.to_owned();
            trim_zeros(&mut m);
            s = format!("{m}e{e}");
        }
        return s;
    }
    let decimals = (3 - floor_log10(a)).clamp(0, 8) as usize;
    fixed(v, decimals, true, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(pattern: &str, v: f64) -> String {
        parse(pattern).unwrap().apply(v, 2)
    }

    #[test]
    fn fixed_decimals_with_and_without_separators() {
        assert_eq!(f(",.2f", 1234567.891), "1,234,567.89");
        assert_eq!(f(".1f", 3.14159), "3.1");
        assert_eq!(f(",.0f", 999.5), "1,000");
        assert_eq!(f(".3f", 2.0), "2.000");
        assert_eq!(f(".3~f", 2.5), "2.5");
        assert_eq!(f(",d", 12345.6), "12,346");
        assert_eq!(f("d", -0.4), "0");
    }

    #[test]
    fn negative_numbers_keep_their_sign_unless_they_round_to_zero() {
        assert_eq!(f(",.1f", -1234.56), "-1,234.6");
        assert_eq!(f(".1f", -0.04), "0.0");
        assert_eq!(f(".0f", -0.0), "0");
    }

    #[test]
    fn percent_multiplies_by_one_hundred() {
        assert_eq!(f(".1%", 0.256), "25.6%");
        assert_eq!(f(".0%", 1.0), "100%");
        assert_eq!(f("%", 0.5), "50%");
        assert_eq!(f(".2%", 0.00123), "0.12%");
    }

    #[test]
    fn si_prefixes_scale_and_trim() {
        assert_eq!(f("s", 1500.0), "1.5k");
        assert_eq!(f("s", 2_000_000.0), "2M");
        assert_eq!(f("s", 999_999.0), "1M");
        assert_eq!(f("s", 123_456.0), "123k");
        assert_eq!(f("s", 0.0015), "1.5m");
        assert_eq!(f("s", 0.0), "0");
        assert_eq!(f("s", -45_000.0), "-45k");
        assert_eq!(f("s", 12.0), "12");
        assert_eq!(f("s", 3.2e9), "3.2G");
        assert_eq!(f(".3s", 1500.0), "1.500k");
        assert_eq!(f("~s", 1500.0), "1.5k");
    }

    #[test]
    fn scientific_notation() {
        assert_eq!(f("e", 12345.0), "1.23e4");
        assert_eq!(f(".1e", 0.00042), "4.2e-4");
        assert_eq!(f(".3~e", 1000.0), "1e3");
    }

    #[test]
    fn literal_prefix_and_suffix_surround_the_number() {
        assert_eq!(f("{,.0f} units", 1234.0), "1,234 units");
        assert_eq!(f("${,.2f}", 5.0), "$5.00");
        assert_eq!(f("EUR {,.0f}", 99.0), "EUR 99");
        assert_eq!(f("${~s} USD", 2500.0), "$2.5k USD");
        assert_eq!(f("{}", 1234.5), "1,234.50");
    }

    #[test]
    fn the_empty_pattern_is_the_automatic_one() {
        let a = parse("").unwrap();
        assert_eq!(a.kind, Kind::Auto);
        assert_eq!(a.apply(1234.5, 1), "1,234.5");
        assert_eq!(a.apply(7.0, 0), "7");
    }

    #[test]
    fn bad_patterns_are_refused_with_one_sentence() {
        for p in [",.2x", "{,.2f", ".13f", ".f1", "f}", "{.2f}{x}", "abc", "..2f", &"x".repeat(70)] {
            let e = parse(p).unwrap_err().0;
            assert!(e.contains("is not understood"), "{p}: {e}");
        }
    }

    #[test]
    fn non_finite_values_format_to_nothing() {
        assert_eq!(f(",.2f", f64::NAN), "");
        assert_eq!(f("s", f64::INFINITY), "");
    }

    #[test]
    fn the_automatic_axis_format_follows_the_magnitude() {
        let small = axis_format(None, &[0.0, 20.0, 40.0], 0);
        assert_eq!(tick_labels(&small, &[0.0, 20.0, 40.0], 0), ["0", "20", "40"]);
        let thousands = axis_format(None, &[0.0, 5000.0, 10000.0], 0);
        assert_eq!(tick_labels(&thousands, &[0.0, 5000.0, 10000.0], 0), ["0", "5,000", "10,000"]);
        let millions = axis_format(None, &[0.0, 1.5e6, 3e6], 0);
        assert_eq!(tick_labels(&millions, &[0.0, 1.5e6, 3e6], 0), ["0", "1.5M", "3M"]);
        let fine = axis_format(None, &[0.0, 0.25, 0.5], 2);
        assert_eq!(tick_labels(&fine, &[0.0, 0.25, 0.5], 2), ["0.00", "0.25", "0.50"]);
        let tiny = axis_format(None, &[0.0, 2e-8], 8);
        assert_eq!(tick_labels(&tiny, &[0.0, 2e-8], 8), ["0e0", "2e-8"]);
    }

    #[test]
    fn a_user_format_overrides_the_automatic_one() {
        let user = parse("{.0%}").unwrap();
        let fmt = axis_format(Some(&user), &[0.0, 0.5, 1.0], 1);
        assert_eq!(tick_labels(&fmt, &[0.0, 0.5, 1.0], 1), ["0%", "50%", "100%"]);
    }

    #[test]
    fn compact_keeps_four_significant_digits() {
        assert_eq!(compact(0.0), "0");
        assert_eq!(compact(98.5), "98.5");
        assert_eq!(compact(1234567.89), "1,234,568");
        assert_eq!(compact(0.123456), "0.1235");
        assert_eq!(compact(-12.3456), "-12.35");
        assert_eq!(compact(1000.0), "1,000");
        assert_eq!(compact(2.5e-7), "2.5e-7");
        assert_eq!(compact(1.5e20), "1.5e20");
        assert_eq!(compact(f64::NAN), "n/a");
    }
}
