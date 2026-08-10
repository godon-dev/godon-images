use std::collections::HashMap;
use serde::Serialize;

// ─── Response Curve (port of characterization.py) ───────────────────

#[derive(Clone)]
struct Point {
    level: f64,
    response: f64,
}

#[derive(Serialize, Clone)]
pub struct CurveState {
    pub num_points: usize,
    pub last_delta: f64,
    pub converged: bool,
    pub points: Vec<(f64, f64)>,
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
        // Replace existing point at same level (drift detection)
        if let Some(p) = self.points.iter_mut().find(|p| (p.level - level).abs() < 1e-9) {
            log::info!(
                "ResponseCurve: re-measured level {:.1}: {:.4} → {:.4}",
                level, p.response, response
            );
            p.response = response;
        } else {
            self.points.push(Point { level, response });
        }

        self.points.sort_by(|a, b| a.level.partial_cmp(&b.level).unwrap_or(std::cmp::Ordering::Equal));

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
        delta
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
        CurveState {
            num_points: self.points.len(),
            last_delta: self.last_delta(),
            converged: self.is_converged(),
            points: self.points.iter().map(|p| (p.level, p.response)).collect(),
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
}
