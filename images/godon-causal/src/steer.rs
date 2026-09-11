//! Steering math: compose, propagate, invert, refuse.
//!
//! Rules lifted verbatim from P3 (godon papers/composition/paper/main.tex,
//! eqs. 1-3). Level is level - addresses are total input levels, never
//! knob positions. Bars propagate in quadrature per stage: local bar at
//! the operating point plus the local slope acting on the input's bar,
//! taken at the bracketing measured levels (flanks' max, conservative).

/// One measured point of a response curve: input level, response, bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    pub level: f64,
    pub shift: f64,
    pub bar: f64,
}

/// Evaluated response at an absolute input level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Evaluated {
    pub shift: f64,
    /// Conservative bar: max of the bracketing measured points' bars.
    pub bar: f64,
    /// Local slope (rise over run between the bracketing measured points).
    pub slope: f64,
    /// The bracketing measured levels (the operating bracket).
    pub bracket: (f64, f64),
}

/// Interpolate the measured curve at an absolute level.
///
/// Returns None when the level lies outside the measured range - the
/// caller refuses ("outside measured range") rather than extrapolating.
/// Bars are taken at the bracketing measured levels (flanks' maximum):
/// conservative by construction (P3, eq. 3 discipline).
pub fn eval_level(points: &[CurvePoint], level: f64) -> Option<Evaluated> {
    if points.is_empty() {
        return None;
    }
    let first = &points[0];
    let last = &points[points.len() - 1];
    if level < first.level.min(last.level) || level > first.level.max(last.level) {
        return None; // outside measured range - refuse, never extrapolate
    }

    for (i, b) in points.iter().enumerate() {
        if (b.level - level).abs() < f64::EPSILON {
            // exact measured hit - slope from neighbours if present
            let slope = match i {
                0 if points.len() > 1 => slope_of(first, &points[1]),
                i if i + 1 < points.len() => slope_of(b, &points[i + 1]),
                i if i > 0 => slope_of(&points[i - 1], b),
                _ => 0.0,
            };
            return Some(Evaluated {
                shift: b.shift,
                bar: b.bar,
                slope,
                bracket: (b.level, b.level),
            });
        }
    }

    // find the bracketing pair (points are ordered by measurement, not
    // necessarily by level - sort a view by level first)
    let mut sorted: Vec<&CurvePoint> = points.iter().collect();
    sorted.sort_by(|a, b| a.level.partial_cmp(&b.level).unwrap_or(std::cmp::Ordering::Equal));
    for (lo, hi) in sorted.windows(2) {
        if level >= lo.level && level <= hi.level {
            let span = hi.level - lo.level;
            if span <= 0.0 {
                continue;
            }
            let t = (level - lo.level) / span;
            let shift = lo.shift + t * (hi.shift - lo.shift);
            let slope = (hi.shift - lo.shift) / span;
            let bar = lo.bar.max(hi.bar); // flanks' max - conservative
            return Some(Evaluated {
                shift,
                bar,
                slope,
                bracket: (lo.level, hi.level),
            });
        }
    }
    None
}

fn slope_of(a: &CurvePoint, b: &CurvePoint) -> f64 {
    let span = b.level - a.level;
    if span.abs() <= 0.0 {
        return 0.0;
    }
    (b.shift - a.shift) / span
}

/// P3 eq. 3: one stage's output uncertainty - the stage's local bar at the
/// operating point plus its local slope acting on the input's bar,
/// combined in quadrature.
pub fn propagate_stage(sigma_map: f64, slope: f64, sigma_in: f64) -> f64 {
    (sigma_map * sigma_map + (slope * sigma_in) * (slope * sigma_in)).sqrt()
}

/// Compose two stages at an absolute input level (P3 eq. 1, two hops).
///
/// `mid_level` is the level the first hop induces at the middle node's
/// input address of the second stage (the level algebra phi is the
/// caller's business; this function only demands levels).
pub fn compose_two_stage(
    first: &[CurvePoint],
    second: &[CurvePoint],
    level: f64,
    mid_phi: impl Fn(f64) -> f64,
) -> Option<Evaluated> {
    let head = eval_level(first, level)?;
    let tail = eval_level(second, mid_phi(head.shift))?;
    let bar = propagate_stage(head.bar, tail.slope, head.bar);
    Some(Evaluated {
        shift: tail.shift,
        bar,
        slope: head.slope * tail.slope,
        bracket: (level, level),
    })
}

/// Invert a monotone composed map by bisection (P3 steering-precursor
/// method). Returns the input setting whose evaluation hits `target`.
///
/// Refuses (None) when the target lies outside the map's evaluated range
/// on [lo, hi] - outside measured range is a refusal, never an
/// extrapolation. Bisection runs to `iters` halvings or ~full precision.
pub fn invert<F: Fn(f64) -> f64>(target: f64, lo: f64, hi: f64, eval: &F, iters: usize) -> Option<f64> {
    let f_lo = eval(lo);
    let f_hi = eval(hi);
    // target must be straddled by the map on the bracket
    if (target < f_lo && target < f_hi) || (target > f_lo && target > f_hi) {
        return None; // outside measured range - the binding constraint
    }
    let rising = f_hi >= f_lo;
    let mut lo = lo;
    let mut hi = hi;
    for _ in 0..iters {
        let mid = 0.5 * (lo + hi);
        let f_mid = eval(mid);
        let below = if rising { f_mid < target } else { f_mid > target };
        if below {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(0.5 * (lo + hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_curve(lo: f64, hi: f64, gain: f64) -> Vec<CurvePoint> {
        vec![
            CurvePoint { level: lo, shift: gain * lo, bar: 0.02 },
            CurvePoint { level: hi, shift: gain * hi, bar: 0.02 },
        ]
    }

    #[test]
    fn eval_outside_measured_range_refuses() {
        let curve = line_curve(0.0, 100.0, 0.5);
        assert!(eval_level(&curve, -0.1).is_none(), "below range must refuse");
        assert!(eval_level(&curve, 100.1).is_none(), "above range must refuse");
    }

    #[test]
    fn eval_interpolates_with_flank_max_bar() {
        let mut curve = line_curve(0.0, 100.0, 0.5);
        curve[1].bar = 0.05; // asymmetric flanks: max must win
        let e = eval_level(&curve, 40.0).unwrap();
        assert!((e.shift - 20.0).abs() < 1e-9);
        assert!((e.bar - 0.05).abs() < 1e-9, "flanks' max is the bar");
        assert!((e.slope - 0.5).abs() < 1e-9);
    }

    #[test]
    fn propagate_matches_p3_equation_three() {
        // sigma_stage = sqrt(sigma_map^2 + (slope * sigma_in)^2)
        let s = propagate_stage(0.02, 0.5, 0.1);
        assert!((s - (0.02_f64.powi(2) + 0.05_f64.powi(2)).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn invert_lands_on_target_for_rising_map() {
        let curve = line_curve(0.0, 100.0, 0.5);
        let eval = |l: f64| eval_level(&curve, l).unwrap().shift;
        let setting = invert(25.0, 0.0, 100.0, &eval, 60).unwrap();
        assert!((setting - 50.0).abs() < 1e-6);
    }

    #[test]
    fn invert_handles_falling_map() {
        let curve = vec![
            CurvePoint { level: 0.0, shift: 50.0, bar: 0.02 },
            CurvePoint { level: 100.0, shift: 0.0, bar: 0.02 },
        ];
        let eval = |l: f64| eval_level(&curve, l).unwrap().shift;
        let setting = invert(25.0, 0.0, 100.0, &eval, 60).unwrap();
        assert!((setting - 50.0).abs() < 1e-6, "falling map inverts too");
    }

    #[test]
    fn invert_refuses_target_outside_measured_range() {
        let curve = line_curve(0.0, 100.0, 0.5); // shift range [0, 50]
        let eval = |l: f64| eval_level(&curve, l).unwrap().shift;
        assert!(invert(75.0, 0.0, 100.0, &eval, 60).is_none(),
            "target above the evaluated range must refuse, never extrapolate");
    }

    #[test]
    fn compose_two_stage_chains_shift_and_propagates_bars() {
        // first hop: gain 0.5 over [0, 100]; second hop: unity self-map
        let first = line_curve(0.0, 100.0, 0.5);
        let second = line_curve(0.0, 50.0, 1.0);
        let e = compose_two_stage(&first, &second, 40.0, |u| u).unwrap();
        assert!((e.shift - 20.0).abs() < 1e-9);
        assert!(e.bar > 0.0, "bars must propagate, not vanish");
    }
}
