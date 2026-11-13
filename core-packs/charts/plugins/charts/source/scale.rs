//! Axis geometry: nice tick generation, linear, logarithmic and band scales.

use crate::det::ln;
use crate::num::{floor_log10, pow10};

/// Ticks on round numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticks {
    /// Ascending, equally spaced values.
    pub values: Vec<f64>,
    /// Distance between neighbouring ticks.
    pub step: f64,
    /// Decimals needed to write every tick exactly.
    pub decimals: usize,
}

const NICE_EPS: f64 = 1e-9;

/// About `target` ticks on multiples of 1, 2 or 5 times a power of ten that
/// cover `[min, max]`: the first tick is at or below `min`, the last at or
/// above `max`. A degenerate or non-finite range is widened around its value.
pub fn nice_ticks(min: f64, max: f64, target: usize) -> Ticks {
    let (mut lo, mut hi) = (min, max);
    if !lo.is_finite() || !hi.is_finite() {
        (lo, hi) = (0.0, 1.0);
    }
    const LIMIT: f64 = 1e300;
    (lo, hi) = (lo.clamp(-LIMIT, LIMIT), hi.clamp(-LIMIT, LIMIT));
    if lo > hi {
        std::mem::swap(&mut lo, &mut hi);
    }
    if lo == hi {
        let pad = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
        lo -= pad;
        hi += pad;
    }
    // A span this small cannot be divided into ticks without leaving the
    // range of exact powers of ten, so it is widened to the smallest span that can.
    const MIN_SPAN: f64 = 1e-290;
    if hi - lo < MIN_SPAN {
        let mid = lo / 2.0 + hi / 2.0;
        (lo, hi) = (mid - MIN_SPAN / 2.0, mid + MIN_SPAN / 2.0);
    }
    let target = target.clamp(2, 50) as f64;
    let raw = (hi - lo) / target;
    let k = floor_log10(raw);
    let m = raw / pow10(k);
    let mut mult: i64 = if m <= std::f64::consts::SQRT_2 {
        1
    } else if m <= 3.162_277_660_168_379_5 {
        2
    } else if m <= 7.071_067_811_865_475_5 {
        5
    } else {
        10
    };
    // Ten times a power of ten is one times the next, so the decimals are minimal.
    let k = if mult == 10 {
        mult = 1;
        k + 1
    } else {
        k
    };
    let step = if k >= 0 {
        mult as f64 * pow10(k)
    } else {
        mult as f64 / pow10(-k)
    };
    let first = (lo / step + NICE_EPS).floor() as i64;
    let last = (hi / step - NICE_EPS).ceil() as i64;
    let at = |i: i64| {
        if k >= 0 {
            i as f64 * (mult as f64 * pow10(k))
        } else {
            (i * mult) as f64 / pow10(-k)
        }
    };
    let values = (first..=last).map(at).collect();
    Ticks {
        values,
        step,
        decimals: if k < 0 { (-k) as usize } else { 0 },
    }
}

/// Decade ticks for a logarithmic axis.
#[derive(Debug, Clone, PartialEq)]
pub struct LogTicks {
    /// Ascending tick values.
    pub values: Vec<f64>,
    /// Lower bound of the domain: a power of ten at or below the data.
    pub lo: f64,
    /// Upper bound of the domain: a power of ten at or above the data.
    pub hi: f64,
}

/// Ticks for a positive range `[min, max]`: powers of ten, thinned when there
/// are many decades, and 2 and 5 times a power of ten when there are few.
pub fn log_ticks(min: f64, max: f64) -> LogTicks {
    let lo_e = floor_log10(min);
    let mut hi_e = floor_log10(max);
    if pow10(hi_e) < max {
        hi_e += 1;
    }
    if hi_e == lo_e {
        hi_e += 1;
    }
    let n = hi_e - lo_e;
    let mut values = Vec::new();
    if n <= 2 {
        for e in lo_e..=hi_e {
            for m in [1.0, 2.0, 5.0] {
                let v = m * pow10(e);
                if v <= pow10(hi_e) {
                    values.push(v);
                }
            }
        }
    } else {
        let stride = (n + 6) / 7;
        let mut e = hi_e;
        let mut exps = Vec::new();
        while e >= lo_e {
            exps.push(e);
            e -= stride;
        }
        exps.reverse();
        values.extend(exps.into_iter().map(pow10));
    }
    LogTicks {
        values,
        lo: pow10(lo_e),
        hi: pow10(hi_e),
    }
}

/// A continuous scale from a data domain to a pixel range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    /// Domain start.
    pub d0: f64,
    /// Domain end.
    pub d1: f64,
    /// Pixel at the domain start.
    pub r0: f64,
    /// Pixel at the domain end.
    pub r1: f64,
    /// True for a logarithmic scale.
    pub log: bool,
}

impl Scale {
    /// A linear scale.
    pub fn linear(d0: f64, d1: f64, r0: f64, r1: f64) -> Self {
        Self {
            d0,
            d1,
            r0,
            r1,
            log: false,
        }
    }

    /// A logarithmic scale over a positive domain.
    pub fn log(d0: f64, d1: f64, r0: f64, r1: f64) -> Self {
        Self {
            d0,
            d1,
            r0,
            r1,
            log: true,
        }
    }

    /// Pixel position of `v`; not clamped.
    pub fn map(&self, v: f64) -> f64 {
        let t = if self.log {
            let (a, b) = (ln(self.d0), ln(self.d1));
            if b == a { 0.5 } else { (ln(v) - a) / (b - a) }
        } else if self.d1 == self.d0 {
            0.5
        } else {
            (v - self.d0) / (self.d1 - self.d0)
        };
        self.r0 + t * (self.r1 - self.r0)
    }

    /// True when `v` lies inside the domain, ends included.
    pub fn contains(&self, v: f64) -> bool {
        let (a, b) = if self.d0 <= self.d1 {
            (self.d0, self.d1)
        } else {
            (self.d1, self.d0)
        };
        v >= a && v <= b
    }
}

/// Evenly spaced slots for categories.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    /// Number of categories.
    pub n: usize,
    /// Pixel at the first slot's start.
    pub r0: f64,
    /// Pixel at the last slot's end.
    pub r1: f64,
}

impl Band {
    /// Width of one slot in pixels (signed when the range runs backwards).
    pub fn step(&self) -> f64 {
        (self.r1 - self.r0) / self.n.max(1) as f64
    }

    /// Centre of slot `i`.
    pub fn center(&self, i: usize) -> f64 {
        self.r0 + (i as f64 + 0.5) * self.step()
    }

    /// Start edge of slot `i`.
    pub fn start(&self, i: usize) -> f64 {
        self.r0 + i as f64 * self.step()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_nice(step: f64) -> bool {
        let k = floor_log10(step);
        let m = step / pow10(k);
        [1.0, 2.0, 5.0, 10.0].iter().any(|n| (m - n).abs() < 1e-9)
    }

    #[test]
    fn ticks_for_a_zero_to_hundred_range_are_the_obvious_ones() {
        let t = nice_ticks(0.0, 100.0, 5);
        assert_eq!(t.values, [0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        assert_eq!((t.step, t.decimals), (20.0, 0));
    }

    #[test]
    fn ticks_widen_to_cover_the_data() {
        let t = nice_ticks(3.2, 97.1, 5);
        assert_eq!(t.values.first(), Some(&0.0));
        assert_eq!(t.values.last(), Some(&100.0));
        let t = nice_ticks(-12.0, 43.0, 6);
        assert_eq!(t.values, [-20.0, -10.0, 0.0, 10.0, 20.0, 30.0, 40.0, 50.0]);
    }

    #[test]
    fn fractional_ranges_use_exact_decimal_values() {
        let t = nice_ticks(0.0, 1.0, 5);
        assert_eq!(t.values, [0.0, 0.2, 0.4, 0.6, 0.8, 1.0]);
        assert_eq!(t.decimals, 1);
        let t = nice_ticks(0.0, 0.03, 6);
        assert_eq!(t.values, [0.0, 0.005, 0.01, 0.015, 0.02, 0.025, 0.03]);
        assert_eq!(t.decimals, 3);
    }

    #[test]
    fn a_step_of_one_needs_no_decimals_even_when_it_came_from_a_tenth_scale() {
        let t = nice_ticks(0.0, 3.18, 4);
        assert_eq!(t.values, [0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!((t.step, t.decimals), (1.0, 0));
        let t = nice_ticks(0.0, 0.322, 4);
        assert_eq!(t.values, [0.0, 0.1, 0.2, 0.3, 0.4]);
        assert_eq!(t.decimals, 1);
        let t = nice_ticks(0.0, 31.8, 4);
        assert_eq!((t.step, t.decimals), (10.0, 0));
    }

    #[test]
    fn a_range_on_exact_multiples_adds_no_extra_tick() {
        let t = nice_ticks(10.0, 50.0, 4);
        assert_eq!(t.values, [10.0, 20.0, 30.0, 40.0, 50.0]);
    }

    #[test]
    fn large_and_tiny_magnitudes_stay_nice() {
        let t = nice_ticks(0.0, 4.2e9, 5);
        assert_eq!(t.step, 1e9);
        assert_eq!(t.values.last(), Some(&5e9));
        let t = nice_ticks(1e-7, 9e-7, 4);
        assert!(is_nice(t.step));
        assert!(t.values[0] <= 1e-7 && *t.values.last().unwrap() >= 9e-7);
    }

    #[test]
    fn degenerate_and_reversed_ranges_are_widened_around_the_value() {
        let t = nice_ticks(5.0, 5.0, 5);
        assert!(t.values[0] < 5.0 && *t.values.last().unwrap() > 5.0);
        let t = nice_ticks(0.0, 0.0, 5);
        assert!(t.values[0] < 0.0 && *t.values.last().unwrap() > 0.0);
        assert_eq!(nice_ticks(100.0, 0.0, 5), nice_ticks(0.0, 100.0, 5));
    }

    #[test]
    fn non_finite_input_falls_back_to_unit_ticks() {
        let t = nice_ticks(f64::NAN, 5.0, 5);
        assert_eq!(t.values.first(), Some(&0.0));
        assert_eq!(t.values.last(), Some(&1.0));
        assert!(nice_ticks(f64::NEG_INFINITY, f64::INFINITY, 5).values.len() >= 2);
    }

    #[test]
    fn astronomically_large_ranges_still_give_finite_ticks() {
        for (lo, hi) in [
            (-1e308, 1e308),
            (0.0, f64::MAX),
            (-f64::MAX, f64::MAX),
            (1e-320, 5e-320),
            (1e299, 1e300),
        ] {
            let t = nice_ticks(lo, hi, 5);
            assert!(
                t.values.len() >= 2 && t.values.len() < 100,
                "{lo} {hi}: {}",
                t.values.len()
            );
            assert!(
                t.values.iter().all(|v| v.is_finite()),
                "{lo} {hi}: {:?}",
                t.values
            );
            assert!(t.values.windows(2).all(|w| w[0] < w[1]), "{lo} {hi}");
        }
    }

    #[test]
    fn tick_count_tracks_the_target() {
        for target in 2..=12 {
            let t = nice_ticks(0.0, 137.0, target);
            assert!(
                t.values.len() >= 2 && t.values.len() <= 2 * target + 2,
                "{target}: {}",
                t.values.len()
            );
        }
    }

    #[test]
    fn log_ticks_for_many_decades_are_powers_of_ten() {
        let t = log_ticks(3.0, 4.0e5);
        assert_eq!((t.lo, t.hi), (1.0, 1e6));
        assert_eq!(t.values, [1.0, 10.0, 100.0, 1e3, 1e4, 1e5, 1e6]);
    }

    #[test]
    fn log_ticks_for_one_decade_add_two_and_five() {
        let t = log_ticks(1.5, 8.0);
        assert_eq!((t.lo, t.hi), (1.0, 10.0));
        assert_eq!(t.values, [1.0, 2.0, 5.0, 10.0]);
    }

    #[test]
    fn log_ticks_thin_out_over_a_huge_span() {
        let t = log_ticks(1e-20, 1e20);
        assert!(t.values.len() <= 8, "{:?}", t.values);
        assert_eq!(t.values.last(), Some(&1e20));
        assert!(t.values.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn log_ticks_for_a_single_power_still_span_a_decade() {
        let t = log_ticks(100.0, 100.0);
        assert_eq!((t.lo, t.hi), (100.0, 1000.0));
        assert!(t.values.contains(&100.0) && t.values.contains(&1000.0));
    }

    #[test]
    fn a_log_range_that_starts_below_one_covers_the_data() {
        let t = log_ticks(0.004, 0.5);
        assert_eq!(t.lo, 0.001);
        assert_eq!(t.hi, 1.0);
    }

    #[test]
    fn linear_scale_maps_ends_middle_and_reverse() {
        let s = Scale::linear(0.0, 10.0, 100.0, 300.0);
        assert_eq!(s.map(0.0), 100.0);
        assert_eq!(s.map(10.0), 300.0);
        assert_eq!(s.map(5.0), 200.0);
        let up = Scale::linear(0.0, 10.0, 300.0, 100.0);
        assert_eq!(up.map(2.5), 250.0);
        assert!(s.contains(0.0) && s.contains(10.0) && !s.contains(10.1) && !s.contains(-0.1));
    }

    #[test]
    fn a_degenerate_domain_maps_to_the_middle() {
        let s = Scale::linear(3.0, 3.0, 0.0, 100.0);
        assert_eq!(s.map(3.0), 50.0);
        assert_eq!(Scale::log(5.0, 5.0, 0.0, 100.0).map(5.0), 50.0);
    }

    #[test]
    fn log_scale_places_decades_evenly() {
        let s = Scale::log(1.0, 1000.0, 0.0, 300.0);
        assert!((s.map(1.0)).abs() < 1e-9);
        assert!((s.map(10.0) - 100.0).abs() < 1e-9);
        assert!((s.map(100.0) - 200.0).abs() < 1e-9);
        assert!((s.map(1000.0) - 300.0).abs() < 1e-9);
        assert!((s.map(31.622776601683793) - 150.0).abs() < 1e-9);
    }

    #[test]
    fn band_slots_tile_the_range() {
        let b = Band {
            n: 4,
            r0: 100.0,
            r1: 500.0,
        };
        assert_eq!(b.step(), 100.0);
        assert_eq!(b.start(0), 100.0);
        assert_eq!(b.center(0), 150.0);
        assert_eq!(b.center(3), 450.0);
        assert_eq!(b.start(4), 500.0);
        let down = Band {
            n: 2,
            r0: 200.0,
            r1: 0.0,
        };
        assert_eq!(down.center(0), 150.0);
        assert_eq!(
            Band {
                n: 0,
                r0: 0.0,
                r1: 10.0
            }
            .step(),
            10.0
        );
    }
}
