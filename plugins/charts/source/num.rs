//! Number helpers whose output is identical on every platform: fixed
//! decimals for coordinates and exact powers of ten that never go through a
//! platform `log10` or `pow`.

/// Appends `v` with at most two decimals and no trailing zeros; non-finite
/// values and negative zero print as `0`.
pub fn push_coord(out: &mut String, v: f64) {
    use std::fmt::Write as _;
    if !v.is_finite() {
        out.push('0');
        return;
    }
    let start = out.len();
    let _ = write!(out, "{v:.2}");
    let tail = &out[start..];
    let keep = if tail.contains('.') {
        tail.trim_end_matches('0').trim_end_matches('.').len()
    } else {
        tail.len()
    };
    out.truncate(start + keep);
    if &out[start..] == "-0" {
        out.truncate(start);
        out.push('0');
    }
}

/// [`push_coord`] into a new string.
pub fn coord(v: f64) -> String {
    let mut s = String::new();
    push_coord(&mut s, v);
    s
}

/// `10^k` by repeated multiplication, so it is the same on every platform.
pub fn pow10(k: i32) -> f64 {
    let mut r = 1.0_f64;
    for _ in 0..k.unsigned_abs() {
        r *= 10.0;
    }
    if k < 0 { 1.0 / r } else { r }
}

/// The exponent `e` with `10^e <= v < 10^(e+1)` for a positive finite `v`.
pub fn floor_log10(v: f64) -> i32 {
    debug_assert!(v > 0.0 && v.is_finite());
    let mut e = 0_i32;
    while v >= pow10(e + 1) && e < 308 {
        e += 1;
    }
    while v < pow10(e) && e > -323 {
        e -= 1;
    }
    e
}

/// Rounds half away from zero to `decimals` decimals without going through a
/// string; used for stable summary numbers.
pub fn round_to(v: f64, decimals: i32) -> f64 {
    let m = pow10(decimals);
    let r = (v * m).round() / m;
    if r == 0.0 { 0.0 } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_trim_zeros_and_round_to_two_decimals() {
        assert_eq!(coord(1.0), "1");
        assert_eq!(coord(1.5), "1.5");
        assert_eq!(coord(1.25), "1.25");
        assert_eq!(coord(1.005), "1");
        assert_eq!(coord(1.006), "1.01");
        assert_eq!(coord(-3.14159), "-3.14");
        assert_eq!(coord(100.0), "100");
        assert_eq!(coord(0.0), "0");
    }

    #[test]
    fn negative_zero_and_non_finite_print_as_zero() {
        assert_eq!(coord(-0.0), "0");
        assert_eq!(coord(-0.001), "0");
        assert_eq!(coord(f64::NAN), "0");
        assert_eq!(coord(f64::INFINITY), "0");
        assert_eq!(coord(f64::NEG_INFINITY), "0");
    }

    #[test]
    fn push_coord_appends_without_touching_the_prefix() {
        let mut s = String::from("a-");
        push_coord(&mut s, 10.0);
        push_coord(&mut s, 20.50);
        assert_eq!(s, "a-1020.5");
    }

    #[test]
    fn powers_of_ten_are_exact_for_small_exponents() {
        assert_eq!(pow10(0), 1.0);
        assert_eq!(pow10(3), 1000.0);
        assert_eq!(pow10(-2), 0.01);
        assert_eq!(pow10(15), 1e15);
    }

    #[test]
    fn floor_log10_is_exact_at_the_decade_boundaries() {
        assert_eq!(floor_log10(1.0), 0);
        assert_eq!(floor_log10(9.999), 0);
        assert_eq!(floor_log10(10.0), 1);
        assert_eq!(floor_log10(999.0), 2);
        assert_eq!(floor_log10(1000.0), 3);
        assert_eq!(floor_log10(0.1), -1);
        assert_eq!(floor_log10(0.0999), -2);
        assert_eq!(floor_log10(1e-5), -5);
    }

    #[test]
    fn round_to_never_returns_negative_zero() {
        assert_eq!(round_to(-0.0004, 3).to_bits(), 0.0_f64.to_bits());
        assert_eq!(round_to(2.3456, 2), 2.35);
    }
}
