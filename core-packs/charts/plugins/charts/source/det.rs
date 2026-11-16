//! Natural logarithm, sine and cosine from additions, multiplications and
//! divisions only. The platform math libraries may differ in the last bit
//! between Linux and macOS; these do not, so a log axis or a pie slice puts a
//! coordinate at the same place everywhere.

use std::f64::consts::{FRAC_PI_2, LN_2, PI};

/// Natural logarithm of a positive finite `x`; NaN otherwise.
pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 || !x.is_finite() {
        return f64::NAN;
    }
    let mut bits = x.to_bits();
    let mut exp = 0_i64;
    if (bits >> 52) & 0x7ff == 0 {
        // Subnormal: scale into the normal range first.
        let scaled = x * 4_503_599_627_370_496.0;
        bits = scaled.to_bits();
        exp = -52;
    }
    exp += ((bits >> 52) & 0x7ff) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > std::f64::consts::SQRT_2 {
        m /= 2.0;
        exp += 1;
    }
    let z = (m - 1.0) / (m + 1.0);
    let z2 = z * z;
    let mut term = z;
    let mut sum = 0.0;
    let mut k = 1.0;
    for _ in 0..30 {
        sum += term / k;
        term *= z2;
        k += 2.0;
    }
    exp as f64 * LN_2 + 2.0 * sum
}

fn taylor_sin(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    for n in 1..14 {
        term *= -x2 / ((2 * n) as f64 * (2 * n + 1) as f64);
        sum += term;
    }
    sum
}

fn taylor_cos(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..14 {
        term *= -x2 / ((2 * n - 1) as f64 * (2 * n) as f64);
        sum += term;
    }
    sum
}

/// Sine and cosine of `x` radians.
pub fn sin_cos(x: f64) -> (f64, f64) {
    if !x.is_finite() {
        return (f64::NAN, f64::NAN);
    }
    let q = (x / FRAC_PI_2).round();
    let r = x - q * FRAC_PI_2;
    let (s, c) = (taylor_sin(r), taylor_cos(r));
    match (q as i64).rem_euclid(4) {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

/// Degrees to radians.
pub fn radians(degrees: f64) -> f64 {
    degrees * PI / 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ln_matches_the_library_within_a_few_ulps() {
        let mut x = 1e-300_f64;
        while x < 1e300 {
            let (a, b) = (ln(x), x.ln());
            assert!(
                (a - b).abs() <= 1e-13 * b.abs().max(1.0),
                "ln({x}) = {a}, expected {b}"
            );
            x *= 3.7;
        }
        for x in [
            0.5,
            1.0,
            2.0,
            10.0,
            0.1,
            1.5,
            std::f64::consts::SQRT_2,
            1.4142135623730954,
        ] {
            assert!((ln(x) - x.ln()).abs() < 1e-14, "{x}");
        }
        assert_eq!(ln(1.0), 0.0);
    }

    #[test]
    fn ln_handles_subnormals_and_refuses_non_positive_input() {
        assert!((ln(5e-324) - 5e-324_f64.ln()).abs() < 1e-10);
        assert!(ln(0.0).is_nan() && ln(-1.0).is_nan() && ln(f64::NAN).is_nan());
        assert!(ln(f64::INFINITY).is_nan());
    }

    #[test]
    fn ln_of_decades_differs_by_ln_ten() {
        for e in -10..10 {
            let d = ln(10f64.powi(e + 1)) - ln(10f64.powi(e));
            assert!((d - std::f64::consts::LN_10).abs() < 1e-13, "{e}");
        }
    }

    #[test]
    fn sin_cos_match_the_library_over_many_turns() {
        let mut x = -50.0;
        while x < 50.0 {
            let (s, c) = sin_cos(x);
            assert!((s - x.sin()).abs() < 1e-12, "sin({x})");
            assert!((c - x.cos()).abs() < 1e-12, "cos({x})");
            x += 0.137;
        }
    }

    #[test]
    fn quadrant_boundaries_are_exact_enough() {
        let (s, c) = sin_cos(0.0);
        assert_eq!((s, c), (0.0, 1.0));
        let (s, c) = sin_cos(FRAC_PI_2);
        assert!((s - 1.0).abs() < 1e-15 && c.abs() < 1e-15);
        let (s, c) = sin_cos(PI);
        assert!(s.abs() < 1e-15 && (c + 1.0).abs() < 1e-15);
        let (s, c) = sin_cos(3.0 * FRAC_PI_2);
        assert!((s + 1.0).abs() < 1e-15 && c.abs() < 1e-15);
    }

    #[test]
    fn non_finite_angles_give_nan() {
        assert!(sin_cos(f64::NAN).0.is_nan());
        assert!(sin_cos(f64::INFINITY).1.is_nan());
    }

    #[test]
    fn degrees_convert_to_radians() {
        assert!((radians(180.0) - PI).abs() < 1e-15);
        assert_eq!(radians(0.0), 0.0);
    }
}
