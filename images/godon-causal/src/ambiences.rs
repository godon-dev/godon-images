//! Ambience entries: the map's local ground — own-effects measured where the
//! tender lives (design §XVIII, rung 5).
//!
//! The SAME walk, re-anchored (a small priced window around the dial's
//! held value) and re-addressed (the resting spot joins the entry's
//! identity). A ambience row is the self-curve's own-effect point, but
//! keyed by the baseline it was measured at — today's ambient stamp
//! records where a curve was measured and nothing reads it; ambiences
//! sign their notes.
//!
//! KEYING (sealed): exact-match on the canonical standing map, the
//! walking dial pinned to its declared anchor — the key is stable
//! across a window's levels (every level of one walk banks under one
//! key) and moves when any dial's resting value moves. Nearest-
//! within-bars matching is a NAMED later motion if churn proves real.
//!
//! The two scopes NEVER compose: chain entries (cross-effects at the
//! walked ambient) and ambience entries (own-effects at one named resting
//! spot) are separate read surfaces. Coupling is the trigger for
//! local measurement; the joint surface is never built by gluing.

use std::collections::HashMap;

use crate::probe_curves::GapInfo;
use crate::steer::CurvePoint;

/// A ambience's window seed: one eighth of the dial's banked range —
/// small enough to be local, wide enough to interpolate inside.
/// The window is priced, never fixed: it always grows to contain
/// what the walk measured (the fence never fences out a reading).
pub const AMBIENCE_WINDOW_FRACTION: f64 = 0.125;

/// A patch older than this names window-aging debt (vigilance
/// cadence). First-flight tunable by evidence, not a knob.
pub const AMBIENCE_AGING_SECS: f64 = 1800.0;

/// One measured own-effect point at a named resting spot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbiencePoint {
    pub level: f64,
    pub shift: f64,
    pub bar: f64,
    /// The re-measure generation: a fresh walk of the same window
    /// banks at the epoch the duty page named. The consult reads the
    /// highest epoch per level (ties: newest written).
    pub epoch: u64,
    /// Unix seconds — aging's clock.
    pub written_at: f64,
}

/// The patch: every point banked at one (tender, param, channel,
/// baseline). Per-level, the consult reads max epoch.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub anchor: f64,
    pub points: Vec<AmbiencePoint>,
}

impl Patch {
    /// The freshest point per level (max epoch, ties newest).
    pub fn freshest(&self) -> Vec<AmbiencePoint> {
        let mut best: HashMap<u64, AmbiencePoint> = HashMap::new();
        for p in &self.points {
            let key = p.level.to_bits();
            match best.get(&key) {
                Some(b)
                    if b.epoch > p.epoch
                        || (b.epoch == p.epoch && b.written_at >= p.written_at) => {}
                _ => {
                    best.insert(key, *p);
                }
            }
        }
        let mut out: Vec<AmbiencePoint> = best.into_values().collect();
        out.sort_by(|a, b| a.level.total_cmp(&b.level));
        out
    }

    pub fn newest_written_at(&self) -> Option<f64> {
        self.points
            .iter()
            .map(|p| p.written_at)
            .fold(f64::NEG_INFINITY, f64::max)
            .into()
    }

    pub fn max_epoch(&self) -> u64 {
        self.points.iter().map(|p| p.epoch).max().unwrap_or(0)
    }
}

/// In-memory registry, curves idiom: write-through on bank, restored
/// at boot from ambience_points. Keyed (group, tender, param, channel)
/// -> baseline key -> patch.
/// the registry body — (group, tender, param, channel) → baseline → patch
type AmbienceInner = HashMap<(String, String, String, String), HashMap<String, Patch>>;

#[derive(Default)]
pub struct AmbienceRegistry {
    inner: std::sync::RwLock<AmbienceInner>,
}

impl AmbienceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        group: &str,
        tender: &str,
        param: &str,
        channel: &str,
        baseline: &str,
        anchor: f64,
        point: AmbiencePoint,
    ) {
        let mut guard = self.inner.write().expect("ambience registry lock");
        guard
            .entry((
                group.to_string(),
                tender.to_string(),
                param.to_string(),
                channel.to_string(),
            ))
            .or_default()
            .entry(baseline.to_string())
            .or_insert_with(|| Patch {
                anchor,
                points: Vec::new(),
            })
            .points
            .push(point);
    }

    /// Every patch for one (group, tender, param, channel), any baseline.
    pub fn patches(
        &self,
        group: &str,
        tender: &str,
        param: &str,
        channel: &str,
    ) -> Vec<(String, Patch)> {
        let guard = self.inner.read().expect("ambience registry lock");
        guard
            .get(&(
                group.to_string(),
                tender.to_string(),
                param.to_string(),
                channel.to_string(),
            ))
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// Successor adoption, memory side (rung 8, designs/2026-10-11 XX):
    /// orphaned patches on one dial — patches whose tender no longer
    /// appears in the room — move to the successor. The DB flip
    /// (ambience_store::reparent_orphaned) and this must move together
    /// or the consult reads stale ownership until reboot. A live
    /// tender's ground never moves; the successor's own baselines
    /// merge, never overwrite.
    pub fn reparent(&self, group: &str, successor: &str, param: &str, live: &[String]) -> usize {
        let mut guard = self.inner.write().expect("ambience registry lock");
        let keys: Vec<(String, String, String, String)> = guard.keys().cloned().collect();
        let mut moved = 0;
        for k in keys {
            let (g, t, p, c) = k.clone();
            if g != group || p != param || t == successor || live.contains(&t) {
                continue;
            }
            let Some(baselines) = guard.remove(&k) else {
                continue;
            };
            let dest = guard.entry((g, successor.to_string(), p, c)).or_default();
            for (b, patch) in baselines {
                moved += patch.points.len();
                match dest.get_mut(&b) {
                    Some(existing) => existing.points.extend(patch.points),
                    None => {
                        dest.insert(b, patch);
                    }
                }
            }
        }
        moved
    }

    /// Every param carrying any patch for (group, tender, channel).
    pub fn params_with_patches(&self, group: &str, tender: &str, channel: &str) -> Vec<String> {
        let guard = self.inner.read().expect("ambience registry lock");
        let mut out: Vec<String> = guard
            .keys()
            .filter(|(g, t, _, ch)| g == group && t == tender && ch == channel)
            .map(|(_, _, p, _)| p.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

// ─── The canonical baseline key ─────────────────────────────────────

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn canonical_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Number(n) => serde_json::json!(round6(n.as_f64().unwrap_or(0.0))),
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(canonical_value).collect())
        }
        serde_json::Value::Object(o) => {
            // serde_json::Map is key-sorted without preserve_order —
            // the canonical form IS the sorted form.
            let m: serde_json::Map<String, serde_json::Value> = o
                .iter()
                .map(|(k, v)| (k.clone(), canonical_value(v)))
                .collect();
            serde_json::Value::Object(m)
        }
        other => other.clone(),
    }
}

/// The resting-spot key: the group's standing map with the walking
/// dial pinned to its declared anchor. Stable across a window's
/// levels (the pushed value never enters the key); moves when any
/// dial's resting value moves — that movement IS baseline drift.
pub fn canonical_baseline(
    standing: &serde_json::Value,
    tender: &str,
    param: &str,
    anchor: f64,
) -> String {
    let mut map = standing.as_object().cloned().unwrap_or_default();
    let entry = map
        .entry(tender.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = entry.as_object_mut() {
        obj.insert(param.to_string(), serde_json::json!(round6(anchor)));
    }
    canonical_value(&serde_json::Value::Object(map)).to_string()
}

// ─── The duty page (what standing duty consults) ────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct GapOut {
    pub from_level: f64,
    pub to_level: f64,
    #[serde(rename = "jump")]
    pub _jump: f64,
    pub bars_sum: f64,
    pub width: f64,
    pub unresolved: bool,
    pub ignorance: f64,
}

impl From<GapInfo> for GapOut {
    fn from(g: GapInfo) -> Self {
        GapOut {
            from_level: g.from_level,
            to_level: g.to_level,
            _jump: g.jump,
            bars_sum: g.bars_sum,
            width: g.width,
            unresolved: g.unresolved,
            ignorance: g.ignorance,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DebtEntry {
    pub param: String,
    pub anchor: f64,
    pub baseline: String,
    pub window: [f64; 2],
    pub epoch: u64,
    pub reason: &'static str,
    pub levels: Vec<f64>,
    pub gaps: Vec<GapOut>,
}

/// Gaps over a patch's freshest points, the walk's own pricing
/// (jump x width vs the endpoints' combined blur), scaled by the
/// window's span — the same referee as the coverage walk's view.
pub fn patch_gaps(points: &[AmbiencePoint]) -> Vec<GapInfo> {
    let mut pts: Vec<&AmbiencePoint> = points.iter().collect();
    pts.sort_by(|a, b| a.level.total_cmp(&b.level));
    let raw =
        pts.last().map(|p| p.level).unwrap_or(0.0) - pts.first().map(|p| p.level).unwrap_or(0.0);
    let range = if raw.abs() > 1e-9 { raw.abs() } else { 1.0 };
    let mut out = Vec::new();
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let jump = (b.shift - a.shift).abs();
        let bars_sum = a.bar + b.bar;
        let width = b.level - a.level;
        out.push(GapInfo {
            from_level: a.level,
            to_level: b.level,
            jump,
            bars_sum,
            width,
            unresolved: jump > bars_sum,
            ignorance: jump * width / range,
        });
    }
    out
}

/// The window for a patch: the seed fence grown to contain every
/// measured level. Priced, never fixed — the walk's own readings
/// set its extent; deliberately stepping beyond the measured hull
/// is a named later motion (the seed's edges have no readings to
/// price from yet).
pub fn patch_window(anchor: f64, param_range: Option<f64>, freshest: &[AmbiencePoint]) -> [f64; 2] {
    let half = AMBIENCE_WINDOW_FRACTION * param_range.filter(|r| *r > 1e-9).unwrap_or(1.0);
    let mut lo = anchor - half;
    let mut hi = anchor + half;
    for p in freshest {
        lo = lo.min(p.level);
        hi = hi.max(p.level);
    }
    [lo, hi]
}

// ─── The planner's consult (what the ladder reads) ──────────────────

/// The consult for one own-reading claim, keyed (receiver, channel).
#[derive(Debug, Clone)]
pub enum AmbienceConsult {
    /// Patches exist but none keyed at the current resting spot —
    /// the local ground drifted. Re-measure first.
    Drift {
        current: String,
        banked: Vec<String>,
    },
    /// The patch at the current resting spot: local evidence for the
    /// own-effect dial, freshest points, in movement units from the
    /// banked neutral (the same units every curve speaks).
    Patch {
        param: String,
        baseline: String,
        anchor: f64,
        points: Vec<CurvePoint>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(level: f64, shift: f64, bar: f64, epoch: u64, at: f64) -> AmbiencePoint {
        AmbiencePoint {
            level,
            shift,
            bar,
            epoch,
            written_at: at,
        }
    }

    #[test]
    fn canonical_keys_sort_and_round() {
        let standing = serde_json::json!({
            "t-b": {"dial_z": 1.23456789, "dial_a": [10.0, 20.0]},
            "t-a": {"x": 0.1000000001},
        });
        let k1 = canonical_baseline(&standing, "t-a", "x", 50.0);
        let k2 = canonical_baseline(&standing, "t-a", "x", 50.0);
        assert_eq!(k1, k2);
        // sorted keys, rounded values
        assert!(k1.starts_with("{\"t-a\":{\"x\":50.0},\"t-b\":"));
        assert!(k1.contains("\"dial_z\":1.234568"));
        // the walked dial is pinned, not the pushed level
        let k3 = canonical_baseline(&standing, "t-a", "x", 51.0);
        assert_ne!(k1, k3);
    }

    #[test]
    fn freshest_reads_max_epoch_per_level() {
        let p = Patch {
            anchor: 50.0,
            points: vec![
                pt(50.0, 0.1, 0.01, 0, 100.0),
                pt(50.0, 0.2, 0.01, 1, 200.0), // re-measured at epoch 1
                pt(55.0, 1.0, 0.01, 1, 200.0),
            ],
        };
        let f = p.freshest();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].shift, 0.2); // epoch 1 wins at level 50
        assert_eq!(f[1].level, 55.0);
    }

    #[test]
    fn window_seed_grows_to_hull() {
        let f = vec![pt(48.0, 0.1, 0.01, 0, 1.0), pt(52.0, 0.3, 0.01, 0, 1.0)];
        let [lo, hi] = patch_window(50.0, Some(100.0), &f);
        // seed half = 12.5 -> [37.5, 62.5]; hull inside
        assert!((lo - 37.5).abs() < 1e-9 && (hi - 62.5).abs() < 1e-9);
        let f = vec![pt(20.0, 0.1, 0.01, 0, 1.0), pt(80.0, 0.3, 0.01, 0, 1.0)];
        let [lo, hi] = patch_window(50.0, Some(100.0), &f);
        assert!((lo - 20.0).abs() < 1e-9 && (hi - 80.0).abs() < 1e-9); // grew to hull
    }

    #[test]
    fn registry_records_and_reads_baselines_apart() {
        let r = AmbienceRegistry::new();
        r.record(
            "g",
            "t",
            "p",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(50.0, 0.0, 0.01, 0, 1.0),
        );
        r.record(
            "g",
            "t",
            "p",
            "objective_0",
            "{\"k\":2}",
            51.0,
            pt(51.0, 0.5, 0.01, 0, 2.0),
        );
        let patches = r.patches("g", "t", "p", "objective_0");
        assert_eq!(patches.len(), 2);
        assert_eq!(
            r.params_with_patches("g", "t", "objective_0"),
            vec!["p".to_string()]
        );
    }

    #[test]
    fn reparent_moves_only_orphaned_ground_on_the_dial() {
        let r = AmbienceRegistry::new();
        r.record(
            "g",
            "t-dead",
            "p",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(50.0, 0.2, 0.01, 0, 1.0),
        );
        r.record(
            "g",
            "t-dead",
            "q",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(50.0, 0.3, 0.01, 0, 1.0),
        );
        r.record(
            "g",
            "t-live",
            "p",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(50.0, 0.4, 0.01, 0, 1.0),
        );

        let moved = r.reparent(
            "g",
            "t-new",
            "p",
            &["t-live".to_string(), "t-new".to_string()],
        );
        assert_eq!(moved, 1);
        // the dead ground on the worked dial now serves the successor
        assert_eq!(r.patches("g", "t-new", "p", "objective_0").len(), 1);
        // the dead tender's other dial stays orphaned until worked
        assert_eq!(r.patches("g", "t-dead", "q", "objective_0").len(), 1);
        assert_eq!(r.patches("g", "t-dead", "p", "objective_0").len(), 0);
        // a live tender's ground never moves
        assert_eq!(r.patches("g", "t-live", "p", "objective_0").len(), 1);
    }

    #[test]
    fn reparent_merges_into_the_successors_own_baseline() {
        let r = AmbienceRegistry::new();
        r.record(
            "g",
            "t-new",
            "p",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(50.0, 0.1, 0.01, 0, 1.0),
        );
        r.record(
            "g",
            "t-dead",
            "p",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(60.0, 0.2, 0.01, 0, 2.0),
        );
        let moved = r.reparent("g", "t-new", "p", &[]);
        assert_eq!(moved, 1);
        let patches = r.patches("g", "t-new", "p", "objective_0");
        assert_eq!(patches.len(), 1);
        let (_, patch) = patches.into_iter().next().unwrap();
        assert_eq!(
            patch.points.len(),
            2,
            "inherited points merge into the successor's own baseline"
        );
    }

    #[test]
    fn reparent_with_no_orphans_is_a_noop() {
        let r = AmbienceRegistry::new();
        r.record(
            "g",
            "t-live",
            "p",
            "objective_0",
            "{\"k\":1}",
            50.0,
            pt(50.0, 0.1, 0.01, 0, 1.0),
        );
        assert_eq!(r.reparent("g", "t-new", "p", &["t-live".to_string()]), 0);
        assert!(r.patches("g", "t-new", "p", "objective_0").is_empty());
    }
}
