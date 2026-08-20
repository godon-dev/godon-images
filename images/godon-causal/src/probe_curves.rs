use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Result of one uncertainty-aware probe: surface movement (delta,
/// convergence signal), information score (z, study objective), and
/// the drift flag (re-measure disagreed beyond bars).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeOutcome {
    pub delta: f64,
    pub z: f64,
    pub drift: bool,
}

// ─── Response Curve (port of characterization.py) ───────────────────

#[derive(Clone)]
struct Point {
    level: f64,
    response: f64,
    /// Measurement uncertainty (±) of the response. Precision-weighted
    /// blending on re-measure; drives drift-vs-noise separation.
    bar: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CurveState {
    pub num_points: usize,
    pub last_delta: f64,
    pub converged: bool,
    /// (level, response, bar) — bar = measurement uncertainty.
    pub points: Vec<(f64, f64, f64)>,
}

/// Serializable registry entry: identity + full curve state.
/// Shape of GET /curves items and of graph-artifact curve entries.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CurveEntry {
    pub sender_id: String,
    pub param: String,
    pub state: CurveState,
}

pub struct ResponseCurve {
    convergence_threshold: f64,
    min_points: usize,
    points: Vec<Point>,
    prev_grid: Option<Vec<(f64, f64)>>,
    delta_history: Vec<f64>,
}

impl ResponseCurve {
    pub fn new(convergence_threshold: f64) -> Self {
        Self {
            convergence_threshold,
            min_points: 2,
            points: Vec::new(),
            prev_grid: None,
            delta_history: Vec::new(),
        }
    }

    pub fn add_point(&mut self, level: f64, response: f64) -> f64 {
        // Legacy entry: default bar (tests, replay of bar-less rows).
        self.probe(level, response, 0.02).delta
    }

    /// Add or re-measure a point, uncertainty-aware.
    ///
    /// Re-measured level, readings AGREE within their combined bars →
    /// inverse-variance blend: the point tightens, the surface barely
    /// moves → small delta, convergence proceeds. Readings DISAGREE
    /// beyond the bars → the point relocates, delta reflects the real
    /// movement, drift flag set (maintenance-loop signal).
    ///
    /// z = |response − prediction| / bar with the prediction taken
    /// from the curve BEFORE this update — the information score the
    /// characterization study consumes as its objective.
    pub fn probe(&mut self, level: f64, response: f64, bar: f64) -> ProbeOutcome {
        const BAR_FLOOR: f64 = 1e-6;
        let bar = bar.max(BAR_FLOOR);

        // Prediction before update (None on an empty curve).
        let predicted = if self.points.is_empty() {
            None
        } else {
            Some(Self::interp_at(level, &self.points))
        };

        let mut drift = false;
        match self
            .points
            .iter_mut()
            .find(|p| (p.level - level).abs() < 1e-9)
        {
            Some(p) => {
                let old_bar = p.bar.max(BAR_FLOOR);
                if (response - p.response).abs() <= old_bar + bar {
                    // Agreement: precision-weighted blend.
                    let w_old = 1.0 / (old_bar * old_bar);
                    let w_new = 1.0 / (bar * bar);
                    let w_sum = w_old + w_new;
                    let blended = (w_old * p.response + w_new * response) / w_sum;
                    let blended_bar = 1.0 / w_sum.sqrt();
                    log::info!(
                        "ResponseCurve: blend level {:.1}: {:.4} ±{:.4} + {:.4} ±{:.4} → {:.4} ±{:.4}",
                        p.level, p.response, old_bar, response, bar, blended, blended_bar
                    );
                    p.response = blended;
                    p.bar = blended_bar;
                } else {
                    log::warn!(
                        "ResponseCurve: DRIFT level {:.1}: {:.4} → {:.4} (beyond bars ±{:.4}/±{:.4})",
                        p.level, p.response, response, old_bar, bar
                    );
                    drift = true;
                    p.response = response;
                    p.bar = bar;
                }
            }
            None => {
                self.points.push(Point { level, response, bar });
            }
        }

        self.points
            .sort_by(|a, b| a.level.partial_cmp(&b.level).unwrap_or(std::cmp::Ordering::Equal));

        let delta = if self.points.len() < 2 {
            f64::INFINITY
        } else {
            let curr = self.eval_grid();
            let delta = match &self.prev_grid {
                Some(prev) => self.grid_distance(&curr, prev),
                None => f64::INFINITY,
            };
            self.prev_grid = Some(curr);
            delta
        };

        self.delta_history.push(delta);

        // First point on a curve: no prior — neutral score. Coverage
        // comes from the sampler's startup randomness.
        let z = match predicted {
            None => 1.0,
            Some(pr) => (response - pr).abs() / bar,
        };

        ProbeOutcome { delta, z, drift }
    }

    pub fn is_converged(&self) -> bool {
        if self.delta_history.is_empty() || self.points.len() < self.min_points {
            return false;
        }
        self.delta_history.last().copied().unwrap_or(f64::INFINITY) < self.convergence_threshold
    }

    pub fn last_delta(&self) -> f64 {
        self.delta_history.last().copied().unwrap_or(f64::INFINITY)
    }

    pub fn state(&self) -> CurveState {
        let ld = self.last_delta();
        CurveState {
            num_points: self.points.len(),
            // INFINITY (no prior yet, <3 points) serializes as null in
            // JSON — use the established f64::MAX/2 convention from the
            // /characterize handler. Consumers treat >1e10 as "no prior".
            last_delta: if ld.is_infinite() { f64::MAX / 2.0 } else { ld },
            converged: self.is_converged(),
            points: self
                .points
                .iter()
                .map(|p| (p.level, p.response, p.bar))
                .collect(),
        }
    }

    fn eval_grid(&self) -> Vec<(f64, f64)> {
        if self.points.len() < 2 {
            return self.points.iter().map(|p| (p.level, p.response)).collect();
        }
        let lo = self.points.first().unwrap().level;
        let hi = self.points.last().unwrap().level;
        if hi <= lo {
            return self.points.iter().map(|p| (p.level, p.response)).collect();
        }
        let n = 200;
        (0..=n)
            .map(|i| {
                let x = lo + (hi - lo) * i as f64 / n as f64;
                let y = Self::interp_at(x, &self.points);
                (x, y)
            })
            .collect()
    }

    fn interp_at(x: f64, pts: &[Point]) -> f64 {
        if pts.is_empty() {
            return 0.0;
        }
        if x <= pts[0].level {
            return pts[0].response;
        }
        if x >= pts.last().unwrap().level {
            return pts.last().unwrap().response;
        }
        for i in 0..pts.len() - 1 {
            let (x0, y0) = (pts[i].level, pts[i].response);
            let (x1, y1) = (pts[i + 1].level, pts[i + 1].response);
            if x0 <= x && x <= x1 {
                if (x1 - x0).abs() < 1e-12 {
                    return y0;
                }
                let t = (x - x0) / (x1 - x0);
                return y0 + t * (y1 - y0);
            }
        }
        pts.last().unwrap().response
    }

    fn grid_distance(&self, a: &[(f64, f64)], b: &[(f64, f64)]) -> f64 {
        if a.is_empty() || b.is_empty() {
            return f64::INFINITY;
        }
        let b_pts: Vec<Point> = b.iter().map(|(x, y)| Point { level: *x, response: *y }).collect();
        let total: f64 = a.iter()
            .map(|(x, ya)| {
                let yb = Self::interp_at(*x, &b_pts);
                (ya - yb).abs()
            })
            .sum();
        total / a.len() as f64
    }
}

// ─── Curve Registry (per edge: sender_id + param_name) ───────────────

pub struct CurveRegistry {
    curves: HashMap<(String, String), ResponseCurve>,
}

impl CurveRegistry {
    pub fn new() -> Self {
        Self { curves: HashMap::new() }
    }

    pub fn add_point(&mut self, sender_id: &str, param: &str,
                     level: f64, shift: f64,
                     threshold: f64) -> f64 {
        let key = (sender_id.to_string(), param.to_string());
        let curve = self.curves
            .entry(key)
            .or_insert_with(|| ResponseCurve::new(threshold));
        curve.add_point(level, shift)
    }

    /// Uncertainty-aware probe (the /characterize path).
    pub fn probe(&mut self, sender_id: &str, param: &str,
                 level: f64, shift: f64, bar: f64,
                 threshold: f64) -> ProbeOutcome {
        let key = (sender_id.to_string(), param.to_string());
        let curve = self.curves
            .entry(key)
            .or_insert_with(|| ResponseCurve::new(threshold));
        curve.probe(level, shift, bar)
    }

    pub fn is_converged(&self, sender_id: &str, param: &str) -> bool {
        self.curves
            .get(&(sender_id.to_string(), param.to_string()))
            .map(|c| c.is_converged())
            .unwrap_or(false)
    }

    pub fn get_state(&self, sender_id: &str, param: &str) -> Option<&CurveState> {
        // Can't return reference to temporary — caller should use is_converged + last_delta
        None
    }

    pub fn get_curve(&self, sender_id: &str, param: &str) -> Option<&ResponseCurve> {
        self.curves.get(&(sender_id.to_string(), param.to_string()))
    }

    pub fn all_curves(&self) -> Vec<(String, String, CurveState)> {
        self.curves
            .iter()
            .map(|((sender, param), curve)| (sender.clone(), param.clone(), curve.state()))
            .collect()
    }

    /// Serializable snapshot of every curve in the registry,
    /// ordered by (sender_id, param) for stable output.
    pub fn snapshot(&self) -> Vec<CurveEntry> {
        let mut entries: Vec<CurveEntry> = self
            .curves
            .iter()
            .map(|((sender, param), curve)| CurveEntry {
                sender_id: sender.clone(),
                param: param.clone(),
                state: curve.state(),
            })
            .collect();
        entries.sort_by(|a, b| (&a.sender_id, &a.param).cmp(&(&b.sender_id, &b.param)));
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── CurveRegistry snapshot ──────────────────────────────────

    #[test]
    fn test_snapshot_empty_registry() {
        let reg = CurveRegistry::new();
        assert!(reg.snapshot().is_empty());
    }

    #[test]
    fn test_snapshot_entries_sorted_with_points() {
        let mut reg = CurveRegistry::new();
        reg.add_point("b1", "param_1", 20.0, 0.1, 0.02);
        reg.add_point("b1", "param_1", 40.0, 0.25, 0.02);
        reg.add_point("b1", "param_0", 10.0, 0.01, 0.02);
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2, "one entry per (sender, param)");
        assert_eq!(snap[0].param, "param_0", "sorted by param");
        assert_eq!(snap[1].param, "param_1");
        assert_eq!(snap[1].sender_id, "b1");
        assert_eq!(snap[1].state.num_points, 2);
        assert_eq!(snap[1].state.points[0], (20.0, 0.1, 0.02), "points sorted by level");
        assert!(!snap[1].state.converged);
    }

    #[test]
    fn test_state_maps_infinite_delta_to_finite() {
        // Curves with fewer than 3 points have INFINITY last_delta.
        // JSON serializes that as null — state() must emit f64::MAX/2
        // (the established convention) so GET /curves and the artifact
        // stay parseable.
        let mut reg = CurveRegistry::new();
        reg.add_point("b1", "param_1", 20.0, 0.1, 0.02);
        let entry = reg.snapshot().pop().unwrap();
        assert!(entry.state.last_delta.is_finite());
        assert!(entry.state.last_delta > 1e10, "sentinel value, got {}", entry.state.last_delta);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("null"), "null breaks consumers: {}", json);
    }

    #[test]
    fn test_curve_entry_serde_roundtrip() {
        let mut reg = CurveRegistry::new();
        reg.add_point("b1", "param_1", 20.0, 0.1, 0.02);
        reg.add_point("b1", "param_1", 40.0, 0.25, 0.02);
        let entry = reg.snapshot().pop().unwrap();
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"sender_id\""));
        assert!(json.contains("\"num_points\""), "CurveState flattened");
        let back: CurveEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sender_id, "b1");
        assert_eq!(back.state.num_points, 2);
        assert_eq!(back.state.points, entry.state.points);
    }

    #[test]
    fn test_snapshot_replay_matches_live_registry() {
        // The persistence contract: replaying the same points through
        // add_point reconstructs the same snapshot (restart recovery).
        let mut live = CurveRegistry::new();
        live.add_point("b1", "param_1", 20.0, 0.1, 0.02);
        live.add_point("b1", "param_1", 40.0, 0.25, 0.02);
        live.add_point("b1", "param_1", 20.0, 0.12, 0.02); // re-measure
        let mut replayed = CurveRegistry::new();
        replayed.add_point("b1", "param_1", 20.0, 0.1, 0.02);
        replayed.add_point("b1", "param_1", 40.0, 0.25, 0.02);
        replayed.add_point("b1", "param_1", 20.0, 0.12, 0.02);
        assert_eq!(live.snapshot(), replayed.snapshot());
    }

    // ─── Uncertainty-aware probe: blend / drift / z ───────────────

    #[test]
    fn test_probe_blend_within_bars() {
        // Re-measure agreeing within combined bars: point tightens,
        // no drift flag, surface movement stays small.
        let mut c = ResponseCurve::new(0.02);
        c.add_point(0.0, 0.0);
        c.add_point(100.0, 1.0);
        let out = c.probe(100.0, 0.98, 0.02); // |Δ|=0.02 <= 0.04
        assert!(!out.drift, "agreement within bars is not drift");
        let pt = c.state().points.iter().find(|p| p.0 == 100.0).unwrap();
        assert!(pt.1 > 0.98 && pt.1 < 1.0, "blended between readings, got {}", pt.1);
        assert!(pt.2 < 0.02, "bar tightens with replication, got ±{}", pt.2);
        assert!(out.delta < 0.05, "blend barely moves the surface, delta={}", out.delta);
    }

    #[test]
    fn test_probe_drift_beyond_bars() {
        // Re-measure disagreeing beyond bars: point relocates, drift
        // flag set, delta reflects the real movement.
        let mut c = ResponseCurve::new(0.02);
        c.add_point(0.0, 0.0);
        c.add_point(100.0, 1.0);
        let out = c.probe(100.0, 0.4, 0.02); // |Δ|=0.6 >> 0.04
        assert!(out.drift, "disagreement beyond bars is drift");
        let pt = c.state().points.iter().find(|p| p.0 == 100.0).unwrap();
        assert!((pt.1 - 0.4).abs() < 1e-9, "relocated to new reading");
        assert!(out.delta > 0.1, "surface genuinely moved, delta={}", out.delta);
    }

    #[test]
    fn test_probe_z_scores() {
        // First point: neutral. Prediction-matching re-probe: small z.
        // Prediction-contradicting probe: large z.
        let mut c = ResponseCurve::new(0.02);
        let first = c.probe(0.0, 0.0, 0.01);
        assert!((first.z - 1.0).abs() < 1e-9, "first point neutral, z={}", first.z);
        c.probe(100.0, 1.0, 0.01);
        // Line 0→1: prediction at 50 is 0.5.
        let boring = c.probe(50.0, 0.505, 0.01); // surprise 0.005, bar 0.01
        assert!(boring.z < 1.0, "reading within its own bar → z<1, got {}", boring.z);
        let surprising = c.probe(60.0, 0.9, 0.01); // prediction 0.6, surprise 0.3
        assert!(surprising.z > 10.0, "surprise 30× the bar → z>10, got {}", surprising.z);
    }

    #[test]
    fn test_probe_bar_floor_prevents_nan() {
        let mut c = ResponseCurve::new(0.02);
        c.probe(0.0, 0.1, 0.0); // degenerate bar (e.g. single sample)
        let out = c.probe(10.0, 0.1, 0.0);
        assert!(out.z.is_finite(), "z must be finite even with zero bar");
    }

    #[test]
    fn test_noisy_dead_param_stays_boring() {
        // The run-1/run-2 failure mode: a dead param whose re-measures
        // wiggle at the noise scale must NOT look informative. With
        // honest bars, every re-probe scores z≈1 and blends — the
        // sampler loses interest, convergence proceeds.
        let mut c = ResponseCurve::new(0.02);
        c.probe(25.0, 0.02, 0.02);
        c.probe(50.0, 0.03, 0.02);
        c.probe(75.0, 0.04, 0.02);
        let zs: Vec<f64> = vec![
            c.probe(25.0, 0.06, 0.03).z, // wiggle within bars
            c.probe(50.0, 0.0, 0.03).z,  // wiggle other direction
            c.probe(75.0, 0.06, 0.03).z,
        ];
        assert!(zs.iter().all(|z| *z < 2.0), "noise wiggles score low z, got {:?}", zs);
        assert!(!c.state().points.iter().any(|p| p.2 > 0.05), "bars stay tight");
    }

    // ─── ResponseCurve ────────────────────────────────────────────

    #[test]
    fn test_first_point_returns_infinity() {
        // The first point has no prior to compare against → INFINITY.
        // This is what caused the JSON null bug. Document it.
        let mut curve = ResponseCurve::new(0.02);
        let delta = curve.add_point(50.0, 0.1);
        assert!(delta.is_infinite(), "first point should return INFINITY");
    }

    #[test]
    fn test_second_point_returns_infinity_too() {
        // First two points return INFINITY because prev_grid is only
        // set when points.len() >= 2 (the else branch). The first
        // point: len < 2 → INFINITY. The second point: prev_grid is
        // None → INFINITY. Finite delta starts on the third point.
        let mut curve = ResponseCurve::new(0.02);
        curve.add_point(0.0, 0.0);
        let delta = curve.add_point(100.0, 1.0);
        assert!(delta.is_infinite(), "second point delta should also be INFINITY (no prev_grid yet)");
    }

    #[test]
    fn test_third_point_returns_finite_delta() {
        let mut curve = ResponseCurve::new(0.02);
        curve.add_point(0.0, 0.0);
        curve.add_point(100.0, 1.0);
        let delta = curve.add_point(50.0, 0.5);
        assert!(delta.is_finite(), "third point delta should be finite");
        assert!(delta >= 0.0, "delta should be non-negative");
    }

    #[test]
    fn test_third_point_smaller_delta_than_second() {
        // As the curve stabilizes, delta should decrease.
        let mut curve = ResponseCurve::new(0.02);
        curve.add_point(0.0, 0.0);
        let d2 = curve.add_point(100.0, 1.0);
        let d3 = curve.add_point(50.0, 0.5);
        // Adding a midpoint between two known points should move the
        // surface less than adding a point that extended the range.
        assert!(d3 <= d2, "delta should decrease as curve stabilizes: d2={} d3={}", d2, d3);
    }

    #[test]
    fn test_convergence_after_stable_points() {
        let mut curve = ResponseCurve::new(0.001);
        // Build a linear curve
        for i in 0..10 {
            let level = i as f64 * 10.0;
            curve.add_point(level, level * 0.01);
        }
        // Adding points that barely change the surface
        let delta = curve.add_point(5.0, 0.05);
        assert!(delta < 0.001, "should be converged, delta={}", delta);
        assert!(curve.is_converged());
    }

    #[test]
    fn test_remeasure_updates_point() {
        let mut curve = ResponseCurve::new(0.02);
        curve.add_point(50.0, 0.5);
        curve.add_point(100.0, 1.0);
        // Re-measure at same level with different response → drift signal
        let delta = curve.add_point(50.0, 0.8);
        assert!(delta.is_finite());
        assert!(delta > 0.0, "drift should produce positive delta");
    }

    #[test]
    fn test_not_converged_with_few_points() {
        let mut curve = ResponseCurve::new(0.02);
        curve.add_point(0.0, 0.0);
        curve.add_point(100.0, 1.0);
        assert!(!curve.is_converged(), "need more than min_points for convergence");
    }

    // ─── CurveRegistry ────────────────────────────────────────────

    #[test]
    fn test_registry_isolates_curves_per_edge() {
        let mut registry = CurveRegistry::new();
        let d1 = registry.add_point("sender_a", "param_0", 50.0, 0.1, 0.02);
        let d2 = registry.add_point("sender_b", "param_0", 50.0, 0.2, 0.02);
        // Different sender → different curve, both are first points
        assert!(d1.is_infinite());
        assert!(d2.is_infinite());
        assert!(!registry.is_converged("sender_a", "param_0"));
    }

    #[test]
    fn test_registry_adds_to_same_curve_for_same_edge() {
        let mut registry = CurveRegistry::new();
        registry.add_point("sender_a", "param_0", 0.0, 0.0, 0.02);
        registry.add_point("sender_a", "param_0", 100.0, 1.0, 0.02);
        // Third point — now prev_grid exists, delta should be finite
        let d3 = registry.add_point("sender_a", "param_0", 50.0, 0.5, 0.02);
        assert!(d3.is_finite(), "third point on same curve should produce finite delta");
    }
}
