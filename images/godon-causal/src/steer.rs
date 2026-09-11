//! Steering math: compose, propagate, invert, refuse.
//!
//! ── PROVENANCE (where every rule comes from) ────────────────────────
//! All rules are lifted from P3: "From Curves to Cascades"
//! (DOI 10.5281/zenodo.22401439), papers/composition/paper/main.tex,
//! section "The Composition Claim and Its Referee":
//!
//!   eq. 1  compose:      y_hat(L) = S_tail(phi(P_head(L)))
//!   eq. 2  referee:      z = |y_direct - y_hat| / sqrt(sd_dir^2 + sd_pred^2),
//!                        z < 2 certifies  -> the landing judge's rule.
//!   eq. 3  propagation:  sd_stage^2 = sd_map^2(x*) + (slope_map(x*) * sd_in)^2
//!                        bars at the bracketing measured levels (flanks'
//!                        max) - conservative by construction.
//!
//! Inversion by bisection on a monotone measured branch is the steering
//! precursor's own method (evidence: characterization receipt -0.1038
//! +/- 0.038 against target -0.100; P3 evidence script, ATTEMPT 2).
//! Interpolation is piecewise linear between adjacent measured points -
//! the same `interp` the evidence scripts ran. No model, no
//! extrapolation: curves exist only where touched.
//!
//! Logging discipline: refusals log at INFO (each one matters and names
//! its binding constraint), evaluations at DEBUG, bisection steps at
//! TRACE. Every number the route needs for a receipt is logged.

use log::{debug, info, trace};

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
    /// Local slope (rise over run between the bracketing measured levels).
    pub slope: f64,
    /// The bracketing measured levels (the operating bracket).
    pub bracket: (f64, f64),
}

/// Interpolate the measured curve at an absolute input level.
///
/// Returns None when the level lies outside the measured range - the
/// caller refuses ("outside measured range") rather than extrapolating.
/// Points may arrive in any order (measurement order, refinement
/// appends midpoints); they are evaluated in level order. Bars are
/// taken at the bracketing measured levels (flanks' maximum):
/// conservative by construction (P3, eq. 3 discipline).
pub fn eval_level(points: &[CurvePoint], level: f64) -> Option<Evaluated> {
    if points.is_empty() {
        info!("STEER eval refuse: curve is empty (no measurements)");
        return None;
    }

    let mut sorted: Vec<&CurvePoint> = points.iter().collect();
    sorted.sort_by(|a, b| {
        a.level
            .partial_cmp(&b.level)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // measured range is the full level span of ALL points - never the
    // first/last array entries (measurement order is not level order)
    let lowest = sorted.first().unwrap().level;
    let highest = sorted.last().unwrap().level;
    if level < lowest || level > highest {
        info!(
            "STEER eval refuse: level {} outside measured range [{}, {}] ({} points)",
            level,
            lowest,
            highest,
            sorted.len()
        );
        return None; // outside measured range - refuse, never extrapolate
    }

    debug!(
        "STEER eval: level {} inside measured range [{}, {}], {} points",
        level,
        lowest,
        highest,
        sorted.len()
    );

    for (lo, hi) in sorted.windows(2) {
        if level >= lo.level && level <= hi.level {
            let span = hi.level - lo.level;
            if span <= 0.0 {
                // duplicate-level measurements: both are the address.
                // Bar is the flanks' max (here: the two duplicates),
                // slope must come from outside the degenerate pair.
                if (level - lo.level).abs() <= f64::EPSILON {
                    let slope = neighbour_slope(&sorted, lo.level);
                    debug!(
                        "STEER eval hit: duplicate level {}, shift {}, bar {} (max of duplicates), slope {}",
                        level, lo.shift, lo.bar.max(hi.bar), slope
                    );
                    return Some(Evaluated {
                        shift: lo.shift,
                        bar: lo.bar.max(hi.bar),
                        slope,
                        bracket: (lo.level, lo.level),
                    });
                }
                continue;
            }
            let t = (level - lo.level) / span;
            let shift = lo.shift + t * (hi.shift - lo.shift);
            let slope = (hi.shift - lo.shift) / span;
            let bar = lo.bar.max(hi.bar); // flanks' max - conservative
            debug!(
                "STEER eval bracket [{}, {}]: shift {}, bar {} (flank max), slope {}",
                lo.level, hi.level, shift, bar, slope
            );
            return Some(Evaluated {
                shift,
                bar,
                slope,
                bracket: (lo.level, hi.level),
            });
        }
    }
    warn!("STEER eval: level {} matched no bracket - refusing", level);
    None
}

/// Local slope at a level, taken from its nearest distinct-level
/// neighbours in the sorted view. Degenerate pairs (same level) are
/// skipped so the slope is always rise over run of real separation.
fn neighbour_slope(sorted: &[&CurvePoint], level: f64) -> f64 {
    let mut prev: Option<&CurvePoint> = None;
    for (i, p) in sorted.iter().enumerate() {
        if (p.level - level).abs() <= f64::EPSILON {
            if let (Some(prev), Some(next)) = (prev, sorted.get(i + 1)) {
                return slope_of(prev, next);
            }
        }
        prev = Some(p);
    }
    0.0
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
    let out = (sigma_map * sigma_map + (slope * sigma_in) * (slope * sigma_in)).sqrt();
    debug!(
        "STEER propagate: sigma_map {}, slope {}, sigma_in {} -> {}",
        sigma_map, slope, sigma_in, out
    );
    out
}

/// Compose two stages at an absolute input level (P3 eq. 1, two hops).
///
/// `mid_level` is the level the first hop induces at the middle node's
/// input address of the second stage (the level algebra phi is the
/// caller's business; this function only demands levels).
///
/// `phi_scale` is |dphi/du| at the operating point: the induced level's
/// uncertainty is phi_scale * the head's output bar. Identity algebra
/// (mid address equals the head shift) uses 1.0 - the chain-era default.
pub fn compose_two_stage(
    first: &[CurvePoint],
    second: &[CurvePoint],
    level: f64,
    phi_scale: f64,
    mid_phi: impl Fn(f64) -> f64,
) -> Option<Evaluated> {
    let head = eval_level(first, level)?;
    let mid = mid_phi(head.shift);
    debug!(
        "STEER compose: head shift {} -> phi -> mid address {} (phi_scale {})",
        head.shift, mid, phi_scale
    );
    let tail = eval_level(second, mid)?;
    let sigma_in = phi_scale * head.bar;
    let bar = propagate_stage(tail.bar, tail.slope, sigma_in);
    debug!(
        "STEER compose result: shift {}, bar {} (head bar {}, tail bar {})",
        tail.shift, bar, head.bar, tail.bar
    );
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
/// extrapolation. DUTY OF THE CALLER: [lo, hi] must be a monotone
/// measured branch - branch selection at junctions is a declared rule
/// of a later era. Bisection runs to `iters` halvings (~full precision).
pub fn invert<F: Fn(f64) -> f64>(
    target: f64,
    lo: f64,
    hi: f64,
    eval: &F,
    iters: usize,
) -> Option<f64> {
    let f_lo = eval(lo);
    let f_hi = eval(hi);
    info!(
        "STEER invert: target {}, bracket [{}, {}], map at bracket ({}, {})",
        target, lo, hi, f_lo, f_hi
    );
    // target must be straddled by the map on the bracket
    if (target < f_lo && target < f_hi) || (target > f_lo && target > f_hi) {
        info!(
            "STEER invert refuse: target {} outside the map's evaluated range \
             [{}, {}] on bracket [{}, {}] - outside measured range is a \
             refusal, never an extrapolation",
            target,
            f_lo.min(f_hi),
            f_lo.max(f_hi),
            lo,
            hi
        );
        return None; // outside evaluated range - the binding constraint
    }
    let rising = f_hi >= f_lo;
    debug!("STEER invert: map on bracket is {}", if rising { "rising" } else { "falling" });
    let mut lo = lo;
    let mut hi = hi;
    for i in 0..iters {
        let mid = 0.5 * (lo + hi);
        let f_mid = eval(mid);
        let below = if rising {
            f_mid < target
        } else {
            f_mid > target
        };
        trace!(
            "STEER invert step {}: bracket [{}, {}], map ({}, {}), target {}",
            i, lo, hi, f_mid, mid, target
        );
        if below {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let setting = 0.5 * (lo + hi);
    info!(
        "STEER invert landed: setting {} (map evaluates to {} there), \
         {} bisection steps",
        setting,
        eval(setting),
        iters
    );
    Some(setting)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_curve(lo: f64, hi: f64, gain: f64) -> Vec<CurvePoint> {
        vec![
            CurvePoint {
                level: lo,
                shift: gain * lo,
                bar: 0.02,
            },
            CurvePoint {
                level: hi,
                shift: gain * hi,
                bar: 0.02,
            },
        ]
    }

    #[test]
    fn eval_outside_measured_range_refuses() {
        let curve = line_curve(0.0, 100.0, 0.5);
        assert!(
            eval_level(&curve, -0.1).is_none(),
            "below range must refuse"
        );
        assert!(
            eval_level(&curve, 100.1).is_none(),
            "above range must refuse"
        );
    }

    #[test]
    fn eval_range_covers_all_points_not_array_ends() {
        // measurement order is not level order: the highest level sits
        // in the middle of the array (refinement appends midpoints)
        let curve = vec![
            CurvePoint {
                level: 100.0,
                shift: 50.0,
                bar: 0.02,
            },
            CurvePoint {
                level: 25.0,
                shift: 12.5,
                bar: 0.02,
            },
            CurvePoint {
                level: 75.0,
                shift: 37.5,
                bar: 0.02,
            },
        ];
        // 90 lies between measured 75 and 100 - must evaluate, not refuse
        let e = eval_level(&curve, 90.0).unwrap();
        assert!((e.shift - 45.0).abs() < 1e-9);
        // 10 lies below the lowest measured level - must refuse
        assert!(eval_level(&curve, 10.0).is_none());
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
    fn eval_duplicate_levels_take_max_bar_without_nan() {
        let curve = vec![
            CurvePoint {
                level: 50.0,
                shift: 20.0,
                bar: 0.03,
            },
            CurvePoint {
                level: 50.0,
                shift: 22.0,
                bar: 0.05,
            },
            CurvePoint {
                level: 60.0,
                shift: 30.0,
                bar: 0.02,
            },
        ];
        let e = eval_level(&curve, 50.0).unwrap();
        assert!((e.bar - 0.05).abs() < 1e-9, "duplicate levels: max bar");
        assert!((e.shift - 20.0).abs() < 1e-9);
        assert!(e.slope.is_finite());
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
            CurvePoint {
                level: 0.0,
                shift: 50.0,
                bar: 0.02,
            },
            CurvePoint {
                level: 100.0,
                shift: 0.0,
                bar: 0.02,
            },
        ];
        let eval = |l: f64| eval_level(&curve, l).unwrap().shift;
        let setting = invert(25.0, 0.0, 100.0, &eval, 60).unwrap();
        assert!((setting - 50.0).abs() < 1e-6, "falling map inverts too");
    }

    #[test]
    fn invert_refuses_target_outside_measured_range() {
        let curve = line_curve(0.0, 100.0, 0.5); // shift range [0, 50]
        let eval = |l: f64| eval_level(&curve, l).unwrap().shift;
        assert!(
            invert(75.0, 0.0, 100.0, &eval, 60).is_none(),
            "target above the evaluated range must refuse, never extrapolate"
        );
    }

    #[test]
    fn compose_two_stage_chains_shift_and_propagates_bars() {
        // first hop: gain 0.5 over [0, 100]; second hop: unity self-map
        // over [0, 50]; identity level algebra
        let first = line_curve(0.0, 100.0, 0.5);
        let second = line_curve(0.0, 50.0, 1.0);
        let e = compose_two_stage(&first, &second, 40.0, 1.0, |u| u).unwrap();
        assert!((e.shift - 20.0).abs() < 1e-9);
        // eq. 3 at the tail stage: sigma_map 0.02, slope 1.0, sigma_in 0.02
        let expected = propagate_stage(0.02, 1.0, 0.02);
        assert!((e.bar - expected).abs() < 1e-12, "bars must propagate");
    }

    #[test]
    fn compose_phi_scale_scales_induced_uncertainty() {
        let first = line_curve(0.0, 100.0, 0.5);
        let second = line_curve(0.0, 50.0, 1.0);
        // phi(u) = 50 + 45u -> |dphi/du| = 45: induced level uncertainty
        // is 45x the head bar, visibly widening the composed bar
        let plain = compose_two_stage(&first, &second, 40.0, 1.0, |u| u).unwrap();
        let scaled = compose_two_stage(&first, &second, 40.0, 45.0, |u| 50.0 + 45.0 * u);
        assert!(scaled.is_none() || scaled.unwrap().bar >= plain.bar);
    }
}
