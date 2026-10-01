//! The steering planner: from a declared wish to a plan the tender can hold.
//!
//! Pure orchestration over measured curves - the math lives in steer.rs
//! (eval_level, eq. 3 propagation, bisection invert, two-stage compose).
//! This module resolves the wish's outcome against the curve registry,
//! enumerates candidate inputs, applies the refusal ladder, and picks a
//! deterministic best plan. No model, no extrapolation: curves exist only
//! where touched, and the plan is refused where the map cannot speak.
//!
//! Refusal ladder (the binding constraint, named):
//!   excluded_input         - every candidate dial is wish-excluded
//!   outside_measured_range - the target (or the declared span) leaves
//!                            what the curve actually measured
//!   magnitude_bound        - the maxChange window around neutral binds:
//!                            either it clips the reachable bracket empty,
//!                            or the target is only reachable outside it
//!   unmeasured_path        - a candidate exists but is not invertible as
//!                            measured (thin window, non-monotone or flat
//!                            branch; branch selection is a later era)
//!
//! Candidates come from the CURVE REGISTRY (measurement first: a curve is
//! direct evidence of influence - data over badge). The connectome is read
//! only to enrich refusal detail (detected edges that were never measured).
//! Choice policy, deterministic: fewest hops, then the tightest predicted
//! bar, then name order.
//!
//! Neutral = the midpoint of the declared param range (the worker's
//! _compute_neutral_params uses constraint midpoints); absent declared
//! ranges fall back to the measured span. Identity level algebra
//! (phi = identity) is the sealed v1 composition rule.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::graph::CausalGraph;
use crate::probe_curves::CurveEntry;
use crate::steer::{self, CurvePoint};

/// Bisection halvings (~full f64 precision, the iters the tests pin).
pub const BISECTION_ITERS: usize = 60;

// ─── Request shape (the /steer/plan contract) ───────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct SteerPlanRequest {
    /// Inference group whose connectome enriches refusals. Absent = the
    /// engine's default group.
    pub group_id: Option<String>,
    /// The wish's identity, minted by the controller at declaration.
    /// Present = the plan lands in causal's wish book (remembered, judged
    /// over GET). Absent = anonymous one-shot plan — exactly the
    /// pre-book behavior.
    pub wish_id: Option<String>,
    /// The owner's outer total, in repair rounds. None = standing, no
    /// cap — the wish dies only by derived evidence (wall, sign flip,
    /// refusal). The first round is free; the allowance counts repairs
    /// after it.
    #[serde(default)]
    pub budget: Option<u32>,
    /// The wish's outcome: one measured value, named from the registry.
    /// Empty on the stack shape - the claims carry the readings.
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub band: Band,
    pub limits: Option<Limits>,
    /// Bench-config param constraints, enriched by the controller.
    /// Absent entry for a param = the measured span stands in.
    pub param_ranges: Option<HashMap<String, [f64; 2]>>,
    /// The stack grammar: the aims, N >= 1. Empty = the legacy
    /// single-outcome shape above, which IS one claim.
    #[serde(default)]
    pub claims: Vec<ClaimSpec>,
    /// The price: protected readings the wish must never push out of
    /// band - claims that carry no dial. Empty = no price declared.
    #[serde(default)]
    pub terms: Vec<ClaimSpec>,
}

/// One line of the wish: an aim (claim) or a price (term). Same shape,
/// same law - the role (dial or never-actuate) is the compile's
/// decision, never the grammar's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimSpec {
    pub outcome: String,
    pub band: Band,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Band {
    /// Bounds are in outcome units — what the outcome shows (the
    /// thermometer reading), never a movement. The planner translates
    /// to movement units internally via the banked neutral anchor.
    pub lo: f64,
    pub hi: f64,
    /// Receipt/reporting only - the planner aims here when present,
    /// else at the band midpoint. Landing is judged against the band.
    pub target: Option<f64>,
}

impl Default for Band {
    fn default() -> Self {
        Band {
            lo: 0.0,
            hi: 0.0,
            target: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub exclude: Vec<String>,
    /// No input may end further from neutral than this fraction of its
    /// own declared range. Unitless, in (0, 1).
    #[serde(rename = "maxChange")]
    pub max_change: Option<f64>,
}

// ─── Decision shape ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub sender: String,
    pub param: String,
    pub setting: f64,
    pub bars: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanDecision {
    Planned {
        moves: Vec<Move>,
        predicted_value: f64,
        predicted_bars: f64,
        /// (param, lo, hi) - the bracket the plan draws on: the map's
        /// honesty receipt, what later verdicts are judged against.
        range_used: Vec<(String, f64, f64)>,
        path: Vec<String>,
        /// per-claim predictions: the compile's receipt, per gavel
        /// unit - what the move-set predicts each claim's reading to be
        claim_predictions: Vec<ClaimPrediction>,
    },
    Refused {
        reason: String,
        detail: String,
    },
}

// ─── Outcome resolution ─────────────────────────────────────────────

/// Resolve the wish's outcome name to one (receiver, channel) curve pair.
/// Names come FROM the registry - no hand-maintained namespace. Accepted:
/// "receiver/channel", "receiver.channel", or a bare receiver when exactly
/// one of its channels carries curves. Anything else is a door refusal
/// naming what IS resolvable.
pub(crate) fn resolve_outcome(
    entries: &[CurveEntry],
    outcome: &str,
) -> Result<(String, String), String> {
    let mut pairs: Vec<(&str, &str)> = entries
        .iter()
        .map(|e| (e.receiver_id.as_str(), e.channel.as_str()))
        .collect();
    pairs.sort();
    pairs.dedup();
    let listing = || {
        pairs
            .iter()
            .map(|(r, c)| format!("{r}/{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    for sep in ['/', '.'] {
        if let Some(idx) = outcome.find(sep) {
            let (r, c) = (&outcome[..idx], &outcome[idx + 1..]);
            if pairs.iter().any(|(a, b)| *a == r && *b == c) {
                return Ok((r.to_string(), c.to_string()));
            }
            return Err(format!(
                "outcome '{}' names no measured (receiver, channel) pair; resolvable: {}",
                outcome,
                listing()
            ));
        }
    }

    let owned: Vec<(&str, &str)> = pairs
        .iter()
        .filter(|(r, _)| *r == outcome)
        .copied()
        .collect();
    match owned.len() {
        1 => Ok((owned[0].0.to_string(), owned[0].1.to_string())),
        0 => Err(format!(
            "outcome '{}' names no measured (receiver, channel) pair; resolvable: {}",
            outcome,
            listing()
        )),
        _ => Err(format!(
            "outcome '{}' is ambiguous across channels {}; name one: receiver/channel",
            outcome,
            owned
                .iter()
                .map(|(_, c)| c.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Fingerprint of every curve the wish's readings ride: claims and terms,
/// per reading, per (sender, param) curve into it — "points:last_shift".
/// The re-walk's fresh-evidence guard: the served plan stores the print;
/// a changed print means the remeasure walk actually moved the map and a
/// re-compile can learn. No change, no re-serve — the same plan served
/// twice teaches nothing.
pub fn curve_fingerprint(
    entries: &[CurveEntry],
    claims: &[ClaimSpec],
    terms: &[ClaimSpec],
) -> serde_json::Value {
    let mut named: Vec<&str> = claims
        .iter()
        .map(|c| c.outcome.as_str())
        .chain(terms.iter().map(|t| t.outcome.as_str()))
        .collect();
    named.sort();
    named.dedup();
    let mut fp = serde_json::Map::new();
    for name in named {
        let Ok((receiver, channel)) = resolve_outcome(entries, name) else {
            continue;
        };
        let mut curves: Vec<&CurveEntry> = entries
            .iter()
            .filter(|e| e.receiver_id == receiver && e.channel == channel)
            .collect();
        curves.sort_by(|a, b| {
            a.sender_id
                .cmp(&b.sender_id)
                .then_with(|| a.param.cmp(&b.param))
        });
        let mut per_curve = serde_json::Map::new();
        for e in curves {
            // The print is the FULL point set (level:shift pairs, sorted),
            // not a count-plus-last: hold-receipts bank mid-curve
            // corrections (the serve's true depth at its held setting)
            // without touching the highest level - a count+last print
            // would miss exactly the lessons the re-walk exists to learn.
            let pts = &e.state.points;
            let shape: String = pts
                .iter()
                .map(|p| format!("{:.2}:{:.6}", p.0, p.1))
                .collect::<Vec<_>>()
                .join(";");
            per_curve.insert(
                format!("{}/{}", e.sender_id, e.param),
                serde_json::json!(format!("{}|{}", pts.len(), shape)),
            );
        }
        fp.insert(name.to_string(), serde_json::Value::Object(per_curve));
    }
    serde_json::Value::Object(fp)
}

// ─── Refusal bookkeeping ────────────────────────────────────────────

/// How far a candidate got before its binding constraint stopped it.
/// Higher rank = further progress = the more honest refusal to report
/// when several candidates each hit their own constraint.
const RANK_EXCLUDED: u8 = 0;
const RANK_SPAN_EMPTY: u8 = 1;
const RANK_MAGNITUDE_EMPTY: u8 = 2;
const RANK_NOT_INVERTIBLE: u8 = 3;
const RANK_TARGET_UNREACHABLE: u8 = 4;

/// Holdability floor: a band must span at least this multiple of the
/// reading's median error bar. This is a NECESSARY condition, not a
/// containment guarantee: below 2x the bar no judging scheme keeps the
/// band (a raw sample needs ~3.9 sigma of width for 95%; a median over
/// ~6 samples needs ~2). Above the floor, keepability depends on how
/// the judge averages. The bar is also replicate-shrunk measurement
/// uncertainty - the reading's raw time-wander can exceed it, so this
/// gate under-catches rather than over-refuses.
const WANDER_GATE: f64 = 2.0;
/// The derived floor's geometry: a band has two sides, so its width
/// must cover the banked one-sided wander P95 twice. Not an assumed
/// multiplier - the percentile was chosen at banking time
/// (wish_book::WANDER_QUANTILE); this constant is pure geometry.
const DERIVED_WANDER_SIDES: f64 = 2.0;

struct CandidateRefusal {
    rank: u8,
    reason: &'static str,
    detail: String,
}

impl CandidateRefusal {
    fn new(rank: u8, reason: &'static str, detail: String) -> Self {
        Self {
            rank,
            reason,
            detail,
        }
    }
}

// ─── Bracket computation (the allowed operating window) ─────────────

struct Bracket {
    lo: f64,
    hi: f64,
    /// True when the maxChange window actually cut the bracket smaller
    /// than measured-inter-declared: a target unreachable on a clipped
    /// bracket is bound by the wish's own magnitude, not by measurement.
    window_clipped: bool,
}

fn compute_bracket(
    points: &[CurvePoint],
    param: &str,
    ranges: &HashMap<String, (f64, f64)>,
    max_change: Option<f64>,
) -> Result<Bracket, CandidateRefusal> {
    let measured_lo = points.iter().map(|p| p.level).fold(f64::INFINITY, f64::min);
    let measured_hi = points
        .iter()
        .map(|p| p.level)
        .fold(f64::NEG_INFINITY, f64::max);
    let declared = ranges.get(param).copied();
    let (declared_lo, declared_hi) = declared.unwrap_or((measured_lo, measured_hi));

    // measured-inter-declared: the range the curve can legally speak about
    let base_lo = measured_lo.max(declared_lo);
    let base_hi = measured_hi.min(declared_hi);
    if base_lo > base_hi {
        return Err(CandidateRefusal::new(
            RANK_SPAN_EMPTY,
            "outside_measured_range",
            format!(
                "param '{}': measured span [{}, {}] and declared range [{}, {}] do not overlap",
                param, measured_lo, measured_hi, declared_lo, declared_hi
            ),
        ));
    }

    // neutral = declared-range midpoint (the worker's _compute_neutral_params
    // uses constraint midpoints); maxChange bounds distance from it
    let neutral = 0.5 * (declared_lo + declared_hi);
    let mut bracket_lo = base_lo;
    let mut bracket_hi = base_hi;
    let mut window_clipped = false;
    if let Some(max_change) = max_change {
        let span = declared_hi - declared_lo;
        let window_lo = neutral - max_change * span;
        let window_hi = neutral + max_change * span;
        if window_lo > bracket_lo {
            bracket_lo = window_lo;
            window_clipped = true;
        }
        if window_hi < bracket_hi {
            bracket_hi = window_hi;
            window_clipped = true;
        }
        if bracket_lo > bracket_hi {
            return Err(CandidateRefusal::new(
                RANK_MAGNITUDE_EMPTY,
                "magnitude_bound",
                format!(
                    "param '{}': maxChange {} around neutral {} admits [{}, {}], \
                     measured-and-declared reaches [{}, {}] - the wish's own bound \
                     blocks every setting",
                    param, max_change, neutral, window_lo, window_hi, base_lo, base_hi
                ),
            ));
        }
    }
    Ok(Bracket {
        lo: bracket_lo,
        hi: bracket_hi,
        window_clipped,
    })
}

// ─── Branch checks (invert's duty of the caller) ────────────────────
//
// The measured points inside the bracket are first condensed into their
// step-path: adjacent levels whose shifts differ by no more than what
// noise could fake (2x the window's median error bar) pool together.
// What survives the pooling is the data's own story:
//   one pool            -> flat, no direction information: refuse
//   pools all one way   -> a monotone branch: invert it (full precision)
//   direction changes   -> a hill: the branches sitting at the target's
//                          shift are alternative answers; the best-
//                          measured one plans, the rest are named.

enum BranchVerdict {
    /// The step-path is one direction - proceed to invert on the bracket.
    Monotone,
    /// The path bends: the target's shift is reachable on the picked
    /// branch (best-measured); `alternatives` names how many other
    /// branches the path carries.
    HillPick {
        setting: f64,
        bar: f64,
        predicted_shift: f64,
        level_span: (f64, f64),
        alternatives: usize,
    },
}

fn branch_verdict(
    points: &[CurvePoint],
    bracket: &Bracket,
    shift_target: f64,
) -> Result<BranchVerdict, CandidateRefusal> {
    let mut in_bracket: Vec<CurvePoint> = points
        .iter()
        .filter(|p| p.level >= bracket.lo && p.level <= bracket.hi)
        .copied()
        .collect();
    in_bracket.sort_by(|a, b| {
        a.level
            .partial_cmp(&b.level)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if in_bracket.len() < 2 {
        return Err(CandidateRefusal::new(
            RANK_NOT_INVERTIBLE,
            "unmeasured_path",
            format!(
                "only {} measured point(s) inside the allowed window [{}, {}] - too thin to invert",
                in_bracket.len(),
                bracket.lo,
                bracket.hi
            ),
        ));
    }

    // robust per-window noise: the median error bar of the measured points
    let mut bars: Vec<f64> = in_bracket.iter().map(|p| p.bar).collect();
    bars.sort_by(f64::total_cmp);
    let sigma = bars[bars.len() / 2].max(1e-12);
    // a seam between adjacent pools is real only if the medians differ by
    // more than what noise could fake (2x the median bar - the same
    // measure-twice discipline as the relcache-retry cure)
    let split_gate = 2.0 * sigma;

    struct Seg {
        lo: f64,
        hi: f64,
        shift: f64,
        n: usize,
    }
    let mut segs: Vec<Seg> = in_bracket
        .iter()
        .map(|p| Seg {
            lo: p.level,
            hi: p.level,
            shift: p.shift,
            n: 1,
        })
        .collect();

    // agglomerative pooling: merge the weakest adjacent seam until every
    // remaining seam exceeds the gate. Direction changes above the gate
    // survive - a hill keeps its shape, noise loses its vote.
    loop {
        let mut weakest: Option<(usize, f64)> = None;
        for i in 0..segs.len().saturating_sub(1) {
            let gap = (segs[i + 1].shift - segs[i].shift).abs();
            if gap <= split_gate && weakest.map(|(_, g)| gap < g).unwrap_or(true) {
                weakest = Some((i, gap));
            }
        }
        let Some((i, _)) = weakest else {
            break;
        };
        let right = segs.remove(i + 1);
        let left = &mut segs[i];
        left.hi = right.hi;
        left.shift = (left.shift * left.n as f64 + right.shift * right.n as f64)
            / (left.n + right.n) as f64;
        left.n += right.n;
    }

    if segs.len() == 1 {
        return Err(CandidateRefusal::new(
            RANK_NOT_INVERTIBLE,
            "unmeasured_path",
            format!(
                "branch is flat on [{}, {}] - the measured shifts stay within {:.4} of each other, no direction information above the noise floor",
                segs[0].lo, segs[0].hi, split_gate
            ),
        ));
    }

    let rising = segs.windows(2).all(|w| w[1].shift >= w[0].shift);
    let falling = segs.windows(2).all(|w| w[1].shift <= w[0].shift);
    if rising || falling {
        return Ok(BranchVerdict::Monotone);
    }

    // genuine direction change: branches whose shift sits within the gate
    // of the target are alternative answers; the connecting stretch
    // between adjacent pools spans everything between their medians, so
    // any target between the outer medians is reachable by interpolation.
    let mut reachable: Vec<(f64, f64, usize)> = Vec::new(); // (level, predicted_shift, points)
    for (i, s) in segs.iter().enumerate() {
        if (s.shift - shift_target).abs() <= split_gate {
            reachable.push((0.5 * (s.lo + s.hi), s.shift, s.n));
        }
        if let Some(next) = segs.get(i + 1) {
            let span_lo = s.shift.min(next.shift);
            let span_hi = s.shift.max(next.shift);
            if shift_target >= span_lo
                && shift_target <= span_hi
                && (next.shift - s.shift).abs() > 1e-12
            {
                let level =
                    s.hi + (shift_target - s.shift) * (next.lo - s.hi) / (next.shift - s.shift);
                reachable.push((level, shift_target, s.n + next.n));
            }
        }
    }
    if reachable.is_empty() {
        let span: Vec<String> = segs
            .iter()
            .map(|s| format!("[{}, {}]~{:.4}", s.lo, s.hi, s.shift))
            .collect();
        return Err(CandidateRefusal::new(
            RANK_TARGET_UNREACHABLE,
            "outside_measured_range",
            format!(
                "hill on [{}, {}]: branches {} - none reaches shift {} within the noise gate {:.4}",
                bracket.lo,
                bracket.hi,
                span.join(" "),
                shift_target,
                split_gate
            ),
        ));
    }
    reachable.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.total_cmp(&b.0)));
    let (level, predicted_shift, n) = reachable[0];
    Ok(BranchVerdict::HillPick {
        setting: level,
        bar: sigma,
        predicted_shift,
        level_span: (bracket.lo, bracket.hi),
        alternatives: segs.len() - 1,
    })
}

// ─── The planner ────────────────────────────────────────────────────

/// The plan's per-claim receipt: what the move-set predicts for each
/// gavel unit. Landing is still judged against the band, per claim.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClaimPrediction {
    pub outcome: String,
    pub predicted: f64,
    pub bars: f64,
}

/// One claim's solo answer: the dial it would move, the setting it
/// wants, and the curve the answer rides - the joint compile needs all
/// three to reconcile claims that share a dial and terms that bound it.
struct ClaimPlan {
    setting: f64,
    dial: (String, String),
    points: Vec<CurvePoint>,
    bracket: (f64, f64),
    moves: Vec<Move>,
    predicted_value: f64,
    predicted_bars: f64,
    range_used: Vec<(String, f64, f64)>,
    path: Vec<String>,
}

/// Levels where the curve's shift crosses a band edge - the band's
/// preimage on this dial, clipped to the allowed bracket. The joint
/// candidates are these walls: the settings where a reading stands
/// exactly on its own edge.
fn band_crossings(
    points: &[CurvePoint],
    shift_lo: f64,
    shift_hi: f64,
    bracket: (f64, f64),
) -> Vec<f64> {
    let mut pts: Vec<CurvePoint> = points
        .iter()
        .filter(|p| p.level >= bracket.0 && p.level <= bracket.1)
        .copied()
        .collect();
    pts.sort_by(|a, b| a.level.total_cmp(&b.level));
    let mut out = Vec::new();
    for w in pts.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        for edge in [shift_lo, shift_hi] {
            if (a.shift - edge) * (b.shift - edge) <= 0.0
                && (b.shift - a.shift).abs() > 1e-12
            {
                let t = (edge - a.shift) / (b.shift - a.shift);
                out.push(a.level + t * (b.level - a.level));
            }
        }
    }
    out
}

/// One claim's solo answer - the shipped single-wish machinery, run
/// per claim. Refusals carry the claim's name: a multi-claim wish
/// refuses naming WHICH claim binds.
/// The wander floor, shared by claims and terms: None = the band is
/// holdable (or no measured points speak for it); Some(Refused) = no
/// setting can keep this band, the reason priced. Tier 1 prices the
/// floor from the curves' median error bar (necessary condition);
/// tier 2 from the banked empirical hold-wander P95 of this channel -
/// what the reading actually did at hold. The binding floor is the MAX:
/// two measured lower bounds, the stricter wins. The 2x on the derived
/// price is geometry (a band has two sides); the percentile was chosen
/// at banking time (wish_book::WANDER_QUANTILE).
fn wander_floor_refusal(
    kind: &str,
    name: &str,
    receiver: &str,
    channel: &str,
    band: &Band,
    entries: &[CurveEntry],
    wander: &HashMap<(String, String), f64>,
) -> Option<PlanDecision> {
    let mut bars: Vec<f64> = entries
        .iter()
        .filter(|e| e.receiver_id == receiver && e.channel == channel)
        .flat_map(|e| e.state.points.iter().map(|(_, _, bar)| *bar))
        .collect();
    if bars.is_empty() {
        return None;
    }
    bars.sort_by(f64::total_cmp);
    let sigma = bars[bars.len() / 2].max(1e-12);
    let width = band.hi - band.lo;
    let bar_floor = WANDER_GATE * sigma;
    let derived_floor = wander
        .get(&(receiver.to_string(), channel.to_string()))
        .map(|p95| DERIVED_WANDER_SIDES * p95);
    let (floor, priced_from) = match derived_floor {
        Some(d) if d > bar_floor => (d, format!("derived wander {:.4}", d)),
        _ => (bar_floor, format!("bar 2x median {:.4}", sigma)),
    };
    if width >= floor {
        return None;
    }
    Some(PlanDecision::Refused {
        reason: "wander_floor".to_string(),
        detail: format!(
            "{} '{}': band [{:.4}, {:.4}] spans {:.4} - under the wander floor {:.4} \
             ({}): no setting can hold a band narrower than the reading's own \
             wander; widen the band",
            kind, name, band.lo, band.hi, width, floor, priced_from
        ),
    })
}

fn solve_claim(
    entries: &[CurveEntry],
    graph: Option<&CausalGraph>,
    name: &str,
    receiver: &str,
    channel: &str,
    band: &Band,
    anchor: f64,
    exclude: &[&str],
    ranges: &HashMap<String, (f64, f64)>,
    max_change: Option<f64>,
    wander: &HashMap<(String, String), f64>,
) -> Result<ClaimPlan, PlanDecision> {
    let target = band.target.unwrap_or(0.5 * (band.lo + band.hi));
    let shift_target = target - anchor;
    let named = |detail: String| format!("claim '{}': {}", name, detail);

    // single-hop candidates: curves whose listener IS the outcome
    let mut candidates: Vec<(String, String, Vec<CurvePoint>)> = entries
        .iter()
        .filter(|e| e.receiver_id == receiver && e.channel == channel && e.sender_id != receiver)
        .map(|e| {
            (
                e.sender_id.clone(),
                e.param.clone(),
                e.state
                    .points
                    .iter()
                    .map(|(level, shift, bar)| CurvePoint {
                        level: *level,
                        shift: *shift,
                        bar: *bar,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    candidates.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

    // the wander floor, shared helper (claims and terms price the same law)
    if let Some(refused) = wander_floor_refusal(
        "claim", name, receiver, channel, band, entries, wander,
    ) {
        return Err(refused);
    }

    let mut refusals: Vec<CandidateRefusal> = Vec::new();
    // (hops, predicted bar, sender, param, answer) - sort key, in order
    let mut plans: Vec<(usize, f64, String, String, ClaimPlan)> = Vec::new();

    for (sender, param, points) in &candidates {
        if exclude.contains(&param.as_str()) {
            refusals.push(CandidateRefusal::new(
                RANK_EXCLUDED,
                "excluded_input",
                named(format!("param '{}' is wish-excluded", param)),
            ));
            continue;
        }
        let bracket = match compute_bracket(points, param, ranges, max_change) {
            Ok(b) => b,
            Err(r) => {
                refusals.push(CandidateRefusal::new(r.rank, r.reason, named(r.detail)));
                continue;
            }
        };
        match branch_verdict(points, &bracket, shift_target) {
            Err(r) => {
                refusals.push(CandidateRefusal::new(r.rank, r.reason, named(r.detail)));
                continue;
            }
            Ok(BranchVerdict::HillPick {
                setting,
                bar,
                predicted_shift,
                level_span,
                alternatives,
            }) => {
                log::info!(
                    "STEER plan: hill branch pick {} param '{}' -> setting {} on [{}, {}] predicts shift {} (bar {}) - {} other branch(es) named (claim '{}')",
                    sender, param, setting, level_span.0, level_span.1,
                    predicted_shift, bar, alternatives, name
                );
                plans.push((
                    1,
                    bar,
                    sender.clone(),
                    param.clone(),
                    ClaimPlan {
                        setting,
                        dial: (sender.clone(), param.clone()),
                        points: points.clone(),
                        bracket: (bracket.lo, bracket.hi),
                        moves: vec![Move {
                            sender: sender.clone(),
                            param: param.clone(),
                            setting,
                            bars: bar,
                        }],
                        predicted_value: anchor + predicted_shift,
                        predicted_bars: bar,
                        range_used: vec![(param.clone(), bracket.lo, bracket.hi)],
                        path: vec![sender.clone(), receiver.to_string()],
                    },
                ));
                continue;
            }
            Ok(BranchVerdict::Monotone) => {}
        }
        let eval = |l: f64| -> f64 {
            // bracket is inside the measured span - evaluation is total there
            steer::eval_level(points, l)
                .map(|e| e.shift)
                .unwrap_or(f64::NAN)
        };
        match steer::invert(shift_target, bracket.lo, bracket.hi, &eval, BISECTION_ITERS) {
            None => {
                let reason_detail = named(format!(
                    "band [{}, {}] with anchor {} wants a move to {}, outside the curve's evaluated range [{}, {}] on the allowed bracket [{}, {}]",
                    band.lo, band.hi, anchor, shift_target,
                    eval(bracket.lo), eval(bracket.hi), bracket.lo, bracket.hi
                ));
                // a window-clipped bracket that cannot reach the target is
                // bound by the wish's own magnitude, not by measurement
                let (rank, reason) = if bracket.window_clipped {
                    (RANK_MAGNITUDE_EMPTY, "magnitude_bound")
                } else {
                    (RANK_TARGET_UNREACHABLE, "outside_measured_range")
                };
                refusals.push(CandidateRefusal::new(rank, reason, reason_detail));
            }
            Some(setting) => {
                let evaluated = steer::eval_level(points, setting)
                    .expect("setting inside verified bracket must evaluate");
                log::info!(
                    "STEER plan: single-hop {} param '{}' -> setting {} predicts {} (move {} from anchor {}) +/- {} (claim '{}')",
                    sender, param, setting, anchor + evaluated.shift,
                    evaluated.shift, anchor, evaluated.bar, name
                );
                plans.push((
                    1,
                    evaluated.bar,
                    sender.clone(),
                    param.clone(),
                    ClaimPlan {
                        setting,
                        dial: (sender.clone(), param.clone()),
                        points: points.clone(),
                        bracket: (bracket.lo, bracket.hi),
                        moves: vec![Move {
                            sender: sender.clone(),
                            param: param.clone(),
                            setting,
                            bars: evaluated.bar,
                        }],
                        predicted_value: anchor + evaluated.shift,
                        predicted_bars: evaluated.bar,
                        range_used: vec![(param.clone(), bracket.lo, bracket.hi)],
                        path: vec![sender.clone(), receiver.to_string()],
                    },
                ));
            }
        }
    }

    plans.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    if let Some((_, _, _, _, answer)) = plans.into_iter().next() {
        return Ok(answer);
    }

    // nothing feasible: report the most-advanced candidate's constraint
    refusals.sort_by_key(|r| std::cmp::Reverse(r.rank));
    if let Some(worst) = refusals.first() {
        Err(PlanDecision::Refused {
            reason: worst.reason.to_string(),
            detail: format!(
                "{} candidate(s) evaluated; most advanced refusal - {}",
                candidates.len(),
                worst.detail
            ),
        })
    } else {
        Err(PlanDecision::Refused {
            reason: "unmeasured_path".to_string(),
            detail: named(no_candidates_detail(entries, graph, receiver, channel)),
        })
    }
}

/// A wish line, resolved: the reading behind the name, and the banked
/// neutral that translates the band into movement units.
#[derive(Clone)]
struct NamedReading {
    outcome: String,
    receiver: String,
    channel: String,
    anchor: f64,
    band: Band,
}

enum ResolvedNamed {
    Named(NamedReading),
    Unanchored(PlanDecision),
}

fn resolve_named(
    entries: &[CurveEntry],
    c: &ClaimSpec,
    kind: &str,
    anchor_of: &dyn Fn(&str, &str) -> Option<f64>,
) -> Result<ResolvedNamed, String> {
    let (receiver, channel) = resolve_outcome(entries, &c.outcome)
        .map_err(|e| format!("{} '{}': {}", kind, c.outcome, e))?;
    match anchor_of(&receiver, &channel).filter(|a| a.is_finite()) {
        Some(anchor) => Ok(ResolvedNamed::Named(NamedReading {
            outcome: c.outcome.clone(),
            receiver,
            channel,
            anchor,
            band: c.band.clone(),
        })),
        None => Ok(ResolvedNamed::Unanchored(PlanDecision::Refused {
            reason: if kind == "term" {
                "unanchored_term".to_string()
            } else {
                "unanchored_outcome".to_string()
            },
            detail: format!(
                "{} '{}' has no banked neutral reading yet - a probe round \
                 must pause at neutral before the wish can speak for it",
                kind, c.outcome
            ),
        })),
    }
}

/// Plan a wish against the measured map. Err = door refusal (malformed
/// request or unresolvable outcome - never planned). Ok(Refused) = the
/// binding constraint, named - claim or term. Ok(Planned) = the compile
/// speaks: one move-set keeping every claim in band and every term
/// honored, or the empty region named.
pub fn plan(
    entries: &[CurveEntry],
    graph: Option<&CausalGraph>,
    req: &SteerPlanRequest,
    anchor: Option<f64>,
    claim_anchors: &HashMap<(String, String), f64>,
    wander: &HashMap<(String, String), f64>,
) -> Result<PlanDecision, String> {
    // limits door: shape before meaning
    if let Some(max_change) = req.limits.as_ref().and_then(|l| l.max_change) {
        if !max_change.is_finite() || max_change <= 0.0 || max_change >= 1.0 {
            return Err(format!(
                "limits.maxChange must be in (0, 1), finite - got {}",
                max_change
            ));
        }
    }
    let mut ranges: HashMap<String, (f64, f64)> = HashMap::new();
    if let Some(map) = &req.param_ranges {
        for (param, r) in map {
            let (lo, hi) = (r[0], r[1]);
            if !lo.is_finite() || !hi.is_finite() || lo >= hi {
                return Err(format!(
                    "param_ranges[{}]: lo must be < hi, both finite (got [{}, {}])",
                    param, lo, hi
                ));
            }
            ranges.insert(param.clone(), (lo, hi));
        }
    }
    let max_change = req.limits.as_ref().and_then(|l| l.max_change);
    let exclude: Vec<&str> = req
        .limits
        .as_ref()
        .map(|l| l.exclude.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    // The wish speaks claims: the legacy single-outcome shape IS
    // claims of length one - one code path, one gavel.
    let claims: Vec<ClaimSpec> = if req.claims.is_empty() {
        // legacy wire: the band door still applies to the sugar fields
        if !req.band.lo.is_finite() || !req.band.hi.is_finite() || req.band.lo >= req.band.hi {
            return Err(format!(
                "band: lo must be < hi, both finite (got [{}, {}])",
                req.band.lo, req.band.hi
            ));
        }
        vec![ClaimSpec {
            outcome: req.outcome.clone(),
            band: req.band.clone(),
        }]
    } else {
        req.claims.clone()
    };
    let terms: Vec<ClaimSpec> = req.terms.clone();

    // door: every band, aim or price, speaks finite lo < hi
    for (i, c) in claims.iter().enumerate() {
        let b = &c.band;
        if !b.lo.is_finite() || !b.hi.is_finite() || b.lo >= b.hi {
            return Err(format!(
                "claims[{}].band: lo must be < hi, both finite (got [{}, {}])",
                i, b.lo, b.hi
            ));
        }
    }
    for (i, t) in terms.iter().enumerate() {
        let b = &t.band;
        if !b.lo.is_finite() || !b.hi.is_finite() || b.lo >= b.hi {
            return Err(format!(
                "terms[{}].band: lo must be < hi, both finite (got [{}, {}])",
                i, b.lo, b.hi
            ));
        }
    }
    // one claim per reading: two aims on one outcome would hide the
    // verdict inside the wish - the gavel judges per claim, named
    for i in 0..claims.len() {
        for j in (i + 1)..claims.len() {
            if claims[i].outcome == claims[j].outcome {
                return Err(format!(
                    "claims[{}] and claims[{}] name the same reading '{}' - one claim per outcome per wish",
                    i, j, claims[i].outcome
                ));
            }
        }
    }

    let anchor_of = |r: &str, ch: &str| -> Option<f64> {
        claim_anchors
            .get(&(r.to_string(), ch.to_string()))
            .copied()
            .or(anchor)
    };

    let mut resolved_claims: Vec<NamedReading> = Vec::new();
    for c in &claims {
        match resolve_named(entries, c, "claim", &anchor_of)? {
            ResolvedNamed::Named(n) => resolved_claims.push(n),
            ResolvedNamed::Unanchored(dec) => return Ok(dec),
        }
    }
    let mut resolved_terms: Vec<NamedReading> = Vec::new();
    for t in &terms {
        match resolve_named(entries, t, "term", &anchor_of)? {
            ResolvedNamed::Named(n) => resolved_terms.push(n),
            ResolvedNamed::Unanchored(dec) => return Ok(dec),
        }
    }

    // per-claim solo answers: the shipped single-wish machinery, run
    // per claim - a refusal here names its claim
    let mut solved: Vec<(NamedReading, ClaimPlan)> = Vec::new();
    for c in &resolved_claims {
        match solve_claim(
            entries,
            graph,
            &c.outcome,
            &c.receiver,
            &c.channel,
            &c.band,
            c.anchor,
            &exclude,
            &ranges,
            max_change,
            wander,
        ) {
            Ok(p) => solved.push((c.clone(), p)),
            Err(dec) => return Ok(dec),
        }
    }

    // terms carry bands too - the same wander floor prices the price:
    // a protected reading nobody can keep inside its band is a term no
    // compile can honor, refused before the joint compile speaks.
    for t in &resolved_terms {
        if let Some(refused) = wander_floor_refusal(
            "term",
            &t.outcome,
            &t.receiver,
            &t.channel,
            &t.band,
            entries,
            wander,
        ) {
            return Ok(refused);
        }
    }

    // ─── the joint compile ──────────────────────────────────────────
    // One dial, many bands: the final setting on a dial must keep
    // EVERY reading that dial measurably reaches inside its own band -
    // the claims riding the dial first, then the price and the
    // neighbouring claims as the corridor. Bands are the weights; the
    // engine never invents them. The empty region refuses, named.
    let mut settings: Vec<f64> = solved.iter().map(|(_, p)| p.setting).collect();

    // (a) claims sharing a dial: reconcile to one setting
    {
        let mut by_dial: std::collections::BTreeMap<(String, String), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, (_, p)) in solved.iter().enumerate() {
            by_dial.entry(p.dial.clone()).or_default().push(i);
        }
        for (dial, idxs) in &by_dial {
            if idxs.len() < 2 {
                continue;
            }
            let mut candidates: Vec<f64> = idxs.iter().map(|&i| settings[i]).collect();
            for &i in idxs {
                let (c, p) = &solved[i];
                candidates.extend(band_crossings(
                    &p.points,
                    c.band.lo - c.anchor,
                    c.band.hi - c.anchor,
                    p.bracket,
                ));
            }
            candidates.sort_by(f64::total_cmp);
            candidates.dedup();
            let in_all_bands = |s: f64| -> bool {
                idxs.iter().all(|&i| {
                    let (c, p) = &solved[i];
                    match steer::eval_level(&p.points, s) {
                        Some(e) => {
                            let v = c.anchor + e.shift;
                            v >= c.band.lo && v <= c.band.hi
                        }
                        None => false,
                    }
                })
            };
            let feasible: Vec<f64> =
                candidates.into_iter().filter(|&s| in_all_bands(s)).collect();
            if feasible.is_empty() {
                let wanted: Vec<String> = idxs
                    .iter()
                    .map(|&i| {
                        let (c, _) = &solved[i];
                        format!(
                            "'{}' wants setting {} for band [{}, {}]",
                            c.outcome, settings[i], c.band.lo, c.band.hi
                        )
                    })
                    .collect();
                return Ok(PlanDecision::Refused {
                    reason: "joint_compile_empty".to_string(),
                    detail: format!(
                        "claims {} share dial '{}:{}' but no setting on it keeps every band",
                        wanted.join(", "),
                        dial.0,
                        dial.1
                    ),
                });
            }
            let anchor_setting = settings[idxs[0]];
            let chosen = feasible
                .into_iter()
                .min_by(|x, y| {
                    (x - anchor_setting)
                        .abs()
                        .total_cmp(&(y - anchor_setting).abs())
                })
                .expect("feasible is non-empty");
            for &i in idxs {
                settings[i] = chosen;
            }
        }
    }

    // (b) the corridor: each dial's final setting must also keep every
    // OTHER reading it measurably reaches inside its own band - the
    // wish's terms (the price) and the neighbouring claims (the room
    // is one). A dial with no measured reach on a reading cannot touch
    // it; coupling the map learns later surfaces at the notice rung.
    for i in 0..solved.len() {
        let dial = solved[i].1.dial.clone();
        struct Prot {
            kind: &'static str,
            name: String,
            anchor: f64,
            band: Band,
            points: Vec<CurvePoint>,
        }
        let mut prots: Vec<Prot> = Vec::new();
        for (j, (oc, op)) in solved.iter().enumerate() {
            if j == i || op.dial == dial {
                continue; // own aim, or reconciled on this same dial
            }
            if let Some(e) = entries.iter().find(|e| {
                e.receiver_id == oc.receiver
                    && e.channel == oc.channel
                    && e.sender_id == dial.0
                    && e.param == dial.1
            }) {
                prots.push(Prot {
                    kind: "claim",
                    name: oc.outcome.clone(),
                    anchor: oc.anchor,
                    band: oc.band.clone(),
                    points: e
                        .state
                        .points
                        .iter()
                        .map(|(l, s, b)| CurvePoint {
                            level: *l,
                            shift: *s,
                            bar: *b,
                        })
                        .collect(),
                });
            }
        }
        for t in &resolved_terms {
            if let Some(e) = entries.iter().find(|e| {
                e.receiver_id == t.receiver
                    && e.channel == t.channel
                    && e.sender_id == dial.0
                    && e.param == dial.1
            }) {
                prots.push(Prot {
                    kind: "term",
                    name: t.outcome.clone(),
                    anchor: t.anchor,
                    band: t.band.clone(),
                    points: e
                        .state
                        .points
                        .iter()
                        .map(|(l, s, b)| CurvePoint {
                            level: *l,
                            shift: *s,
                            bar: *b,
                        })
                        .collect(),
                });
            }
        }
        if prots.is_empty() {
            continue;
        }
        let (c, p) = &solved[i];
        let mut candidates = vec![settings[i]];
        candidates.extend(band_crossings(
            &p.points,
            c.band.lo - c.anchor,
            c.band.hi - c.anchor,
            p.bracket,
        ));
        for prot in &prots {
            candidates.extend(band_crossings(
                &prot.points,
                prot.band.lo - prot.anchor,
                prot.band.hi - prot.anchor,
                p.bracket,
            ));
        }
        candidates.sort_by(f64::total_cmp);
        candidates.dedup();
        let holds = |s: f64| -> Option<bool> {
            let aim = steer::eval_level(&p.points, s)?;
            let v = c.anchor + aim.shift;
            if v < c.band.lo || v > c.band.hi {
                return Some(false);
            }
            for prot in &prots {
                let pv = steer::eval_level(&prot.points, s)?;
                let v = prot.anchor + pv.shift;
                if v < prot.band.lo || v > prot.band.hi {
                    return Some(false);
                }
            }
            Some(true)
        };
        let picked = candidates.into_iter().find(|&s| holds(s) == Some(true));
        match picked {
            Some(s) => settings[i] = s,
            None => {
                let prot = &prots[0];
                return Ok(PlanDecision::Refused {
                    reason: "joint_compile_empty".to_string(),
                    detail: format!(
                        "{} '{}' refuses the aim '{}' on dial '{}:{}': no setting in the allowed bracket keeps both in band - the price is the aim's corridor",
                        prot.kind, prot.name, c.outcome, dial.0, dial.1
                    ),
                });
            }
        }
    }

    // ─── the move-set: one move per dial, the claims' receipt ───────
    let mut moves: Vec<Move> = Vec::new();
    let mut range_used: Vec<(String, f64, f64)> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut claim_predictions: Vec<ClaimPrediction> = Vec::new();
    for (i, (c, p)) in solved.iter().enumerate() {
        if !moves
            .iter()
            .any(|m| m.sender == p.dial.0 && m.param == p.dial.1)
        {
            moves.push(Move {
                sender: p.dial.0.clone(),
                param: p.dial.1.clone(),
                setting: settings[i],
                bars: p.predicted_bars,
            });
        }
        for r in &p.range_used {
            if !range_used.iter().any(|(rp, _, _)| rp == &r.0) {
                range_used.push(r.clone());
            }
        }
        for step in &p.path {
            if !path.contains(step) {
                path.push(step.clone());
            }
        }
        let shift = steer::eval_level(&p.points, settings[i])
            .map(|e| e.shift)
            .unwrap_or(f64::NAN);
        claim_predictions.push(ClaimPrediction {
            outcome: c.outcome.clone(),
            predicted: c.anchor + shift,
            bars: p.predicted_bars,
        });
    }
    Ok(PlanDecision::Planned {
        predicted_value: claim_predictions[0].predicted,
        predicted_bars: claim_predictions[0].bars,
        claim_predictions,
        moves,
        range_used,
        path,
    })
}

/// Refusal detail when the outcome resolves but not a single curve
/// candidate existed toward it: name the measured pairs and any
/// detected-but-never-measured wiring into the outcome.
fn no_candidates_detail(
    entries: &[CurveEntry],
    graph: Option<&CausalGraph>,
    receiver: &str,
    channel: &str,
) -> String {
    let measured: String = entries
        .iter()
        .map(|e| format!("{}/{}", e.receiver_id, e.channel))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let unmeasured: Vec<String> = match graph {
        Some(g) => g
            .edges_into(receiver)
            .iter()
            .filter(|e| {
                e.detected
                    && !entries
                        .iter()
                        .any(|c| c.sender_id == e.sender_id && c.receiver_id == receiver)
            })
            .map(|e| format!("{} -> {} ({})", e.sender_id, receiver, e.channel))
            .collect(),
        None => Vec::new(),
    };
    if unmeasured.is_empty() {
        format!(
            "no curve listens on {}/{}; measured pairs: {}",
            receiver, channel, measured
        )
    } else {
        format!(
            "wiring detects {} but no curve ever measured it; measured pairs: {}",
            unmeasured.join(", "),
            measured
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe_curves::{CurveState, GapInfo};

    fn entry(
        sender: &str,
        receiver: &str,
        param: &str,
        channel: &str,
        pts: &[(f64, f64, f64)],
    ) -> CurveEntry {
        CurveEntry {
            sender_id: sender.to_string(),
            receiver_id: receiver.to_string(),
            param: param.to_string(),
            channel: channel.to_string(),
            state: CurveState {
                num_points: pts.len(),
                last_delta: 0.0,
                converged: true,
                points: pts.to_vec(),
                gaps: Vec::<GapInfo>::new(),
            },
        }
    }

    fn req(
        outcome: &str,
        target: f64,
        limits: Option<Limits>,
        ranges: Option<HashMap<String, [f64; 2]>>,
    ) -> SteerPlanRequest {
        SteerPlanRequest {
            group_id: None,
            wish_id: None,
            budget: None,
            outcome: outcome.to_string(),
            band: Band {
                lo: target - 5.0,
                hi: target + 5.0,
                target: Some(target),
            },
            limits,
            param_ranges: ranges,
            claims: Vec::new(),
            terms: Vec::new(),
        }
    }

    const GAIN_UP: &[(f64, f64, f64)] = &[(0.0, 0.0, 0.02), (100.0, 50.0, 0.02)];

    #[test]
    fn single_hop_lands_on_target() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 25.0, None, None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned {
                moves,
                predicted_value,
                predicted_bars,
                range_used,
                path,
                ..
            } => {
                assert_eq!(moves.len(), 1);
                assert_eq!(moves[0].sender, "a");
                assert_eq!(moves[0].param, "p");
                assert!((moves[0].setting - 50.0).abs() < 1e-6);
                assert!((moves[0].bars - 0.02).abs() < 1e-9);
                assert!((predicted_value - 25.0).abs() < 1e-6);
                assert!((predicted_bars - 0.02).abs() < 1e-9);
                assert_eq!(range_used, vec![("p".to_string(), 0.0, 100.0)]);
                assert_eq!(path, vec!["a".to_string(), "R".to_string()]);
            }
            other => panic!("expected a plan, got {other:?}"),
        }
    }

    #[test]
    fn target_outside_measured_range_refuses() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 75.0, None, None); // shift range is [0, 50]
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, .. } => assert_eq!(reason, "outside_measured_range"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn anchor_translates_absolute_band_to_move() {
        // The customer's wish: the reading sits at 90 at rest and should
        // stand at 115 (band 110-120). The curve only moves the reading
        // by up to +50 from that rest: the planner must hear "115" as
        // "move +25" and aim the dial where the move is +25 (level 50).
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 115.0, None, None);
        match plan(&entries, None, &r, Some(90.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned {
                moves,
                predicted_value,
                ..
            } => {
                assert!(
                    (moves[0].setting - 50.0).abs() < 1e-6,
                    "setting {:?} should be 50",
                    moves[0].setting
                );
                assert!(
                    (predicted_value - 115.0).abs() < 1e-6,
                    "prediction {predicted_value} must be in thermometer units"
                );
            }
            other => panic!("expected a plan, got {other:?}"),
        }
    }

    #[test]
    fn sane_absolute_band_far_from_anchor_refuses_by_move_span() {
        // Band 160-170 with anchor 90 asks a move of +75; the curve
        // spans moves [0, 50] — refused on the move span, not the
        // thermometer numbers.
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 165.0, None, None);
        match plan(&entries, None, &r, Some(90.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, .. } => assert_eq!(reason, "outside_measured_range"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn unanchored_outcome_refuses_named() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 25.0, None, None);
        match plan(&entries, None, &r, None, &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "unanchored_outcome");
                assert!(detail.contains("neutral"), "detail should teach: {detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn window_clipped_unreachable_target_is_magnitude_bound() {
        let pts: &[(f64, f64, f64)] = &[
            (0.0, 0.0, 0.02),
            (30.0, 15.0, 0.02),
            (70.0, 35.0, 0.02),
            (100.0, 50.0, 0.02),
        ];
        let entries = vec![entry("a", "R", "p", "objective_0", pts)];
        let mut ranges = HashMap::new();
        ranges.insert("p".to_string(), [0.0, 100.0]);
        // window [30, 70] -> map reaches [15, 35]; target 10 needs setting 20
        let limits = Limits {
            exclude: vec![],
            max_change: Some(0.2),
        };
        let r = req("R", 10.0, Some(limits), Some(ranges));
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, .. } => assert_eq!(reason, "magnitude_bound"),
            other => panic!("expected magnitude_bound, got {other:?}"),
        }
    }

    #[test]
    fn excluded_param_refuses_by_name() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let limits = Limits {
            exclude: vec!["p".to_string()],
            max_change: None,
        };
        let r = req("R", 25.0, Some(limits), None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "excluded_input");
                assert!(detail.contains('p'), "detail must name the excluded param");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn hill_branch_plans_the_span_side() {
        // The hill: shifts rise, fall, rise. The target's shift (20)
        // sits on the falling stretch (40, 30) -> (60, 10), reachable at
        // level 50 by interpolation - the planner picks the branch
        // instead of refusing. Branch selection is this era. Three
        // crossings exist (26.67, 50, 70); the tie-break plans the
        // lowest setting.
        let pts: &[(f64, f64, f64)] = &[
            (0.0, 0.0, 0.02),
            (40.0, 30.0, 0.02),
            (60.0, 10.0, 0.02),
            (100.0, 50.0, 0.02),
        ];
        let entries = vec![entry("a", "R", "p", "objective_0", pts)];
        let r = req("R", 20.0, None, None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned {
                moves, predicted_value, ..
            } => {
                assert_eq!(moves[0].param, "p");
                assert!((moves[0].setting - 80.0 / 3.0).abs() < 1e-6);
                assert!((predicted_value - 20.0).abs() < 1e-6);
            }
            other => panic!("expected a hill plan, got {other:?}"),
        }
    }

    #[test]
    fn noisy_monotone_still_inverts() {
        // A true slope under noise: the small dip (26 -> 24) pools away
        // (2x bar gate), the path stays one direction, the wish plans
        // instead of refusing on raw-point zigzag.
        let pts: &[(f64, f64, f64)] = &[
            (0.0, 0.0, 0.02),
            (40.0, 26.0, 0.02),
            (60.0, 24.0, 0.02),
            (100.0, 50.0, 0.02),
        ];
        let entries = vec![entry("a", "R", "p", "objective_0", pts)];
        let r = req("R", 20.0, None, None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned { moves, .. } => {
                assert_eq!(moves[0].param, "p");
                assert!((moves[0].setting - 30.0).abs() < 1.0);
            }
            other => panic!("expected a plan, got {other:?}"),
        }
    }

    #[test]
    fn bare_tail_curve_plans_direct_not_chained() {
        // The dominance finding, pinned: with a tail curve M -> R present,
        // the plan is DIRECT via M's own dial (fewest hops; the chain would
        // move the very same dial) - never a chain through a's dial.
        let entries = vec![
            entry("a", "M", "h", "objective_0", GAIN_UP),
            entry(
                "M",
                "R",
                "t",
                "objective_0",
                &[(0.0, 0.0, 0.02), (50.0, 50.0, 0.02)],
            ),
        ];
        let r = req("R", 20.0, None, None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned { moves, path, .. } => {
                assert_eq!(moves[0].sender, "M", "direct dial on the middle node");
                assert_eq!(moves[0].param, "t");
                assert!((moves[0].setting - 20.0).abs() < 1e-6);
                assert_eq!(
                    path,
                    vec!["M", "R"],
                    "no chain: dominated by the direct plan"
                );
            }
            other => panic!("expected a direct plan, got {other:?}"),
        }
    }

    #[test]
    fn tightest_bar_wins_among_peers() {
        let tight: &[(f64, f64, f64)] = &[(0.0, 0.0, 0.01), (100.0, 100.0, 0.01)];
        let loose: &[(f64, f64, f64)] = &[(0.0, 0.0, 0.05), (100.0, 100.0, 0.05)];
        let entries = vec![
            entry("zz_late", "R", "p2", "objective_0", tight),
            entry("aa_early", "R", "p1", "objective_0", loose),
        ];
        let r = req("R", 50.0, None, None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned {
                moves,
                predicted_bars,
                ..
            } => {
                assert_eq!(moves[0].sender, "zz_late", "bar tightness beats name order");
                assert!((predicted_bars - 0.01).abs() < 1e-9);
            }
            other => panic!("expected a plan, got {other:?}"),
        }
    }

    #[test]
    fn outcome_resolution_forms() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        // bare receiver, dotted pair, slashed pair - all resolve the same
        for outcome in ["R", "R.objective_0", "R/objective_0"] {
            let r = req(outcome, 25.0, None, None);
            assert!(
                plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).is_ok(),
                "'{outcome}' must resolve"
            );
        }
    }

    #[test]
    fn ambiguous_and_unknown_outcomes_are_door_refusals() {
        let entries = vec![
            entry("a", "R", "p", "objective_0", GAIN_UP),
            entry("b", "R", "q", "objective_1", GAIN_UP),
        ];
        let r = req("R", 25.0, None, None);
        let err = plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(err.contains("ambiguous"), "two channels: {err}");
        assert!(err.contains("objective_0") && err.contains("objective_1"));

        let r = req("nowhere", 25.0, None, None);
        let err = plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(
            err.contains("resolvable"),
            "unknown outcome lists what exists: {err}"
        );
    }

    #[test]
    fn malformed_requests_are_door_refusals() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        // inverted band
        let bad_band = SteerPlanRequest {
            group_id: None,
            wish_id: None,
            budget: None,
            outcome: "R".into(),
            band: Band {
                lo: 30.0,
                hi: 20.0,
                target: None,
            },
            limits: None,
            param_ranges: None,
            claims: Vec::new(),
            terms: Vec::new(),
        };
        assert!(plan(&entries, None, &bad_band, Some(0.0), &HashMap::new(), &HashMap::new()).is_err());
        // maxChange outside (0, 1)
        let r = req(
            "R",
            25.0,
            Some(Limits {
                exclude: vec![],
                max_change: Some(1.5),
            }),
            None,
        );
        assert!(plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).is_err());
        // inverted declared range
        let mut ranges = HashMap::new();
        ranges.insert("p".to_string(), [100.0, 0.0]);
        let r = req("R", 25.0, None, Some(ranges));
        assert!(plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).is_err());
    }

    // ─── the joint compile: claims + terms per wish ─────────────────

    fn anchors(pairs: &[(&str, f64)]) -> HashMap<(String, String), f64> {
        pairs
            .iter()
            .map(|(o, v)| ((o.to_string(), "objective_0".to_string()), *v))
            .collect()
    }

    fn req_claims(
        claims: Vec<ClaimSpec>,
        terms: Vec<ClaimSpec>,
        ranges: Option<HashMap<String, [f64; 2]>>,
    ) -> SteerPlanRequest {
        SteerPlanRequest {
            group_id: None,
            wish_id: None,
            budget: None,
            outcome: String::new(),
            band: Band::default(),
            limits: None,
            param_ranges: ranges,
            claims,
            terms,
        }
    }

    fn claim(outcome: &str, lo: f64, hi: f64, target: f64) -> ClaimSpec {
        ClaimSpec {
            outcome: outcome.to_string(),
            band: Band {
                lo,
                hi,
                target: Some(target),
            },
        }
    }

    #[test]
    fn band_below_wander_floor_refuses() {
        // the 2+2 lesson as a gate: bars of 0.30 make the wander floor
        // 0.60; a band spanning 0.40 is unholdable by ANY setting - the
        // door names it instead of letting the wish miss for noise
        let entries = vec![entry(
            "a",
            "R",
            "p",
            "objective_0",
            &[(0.0, 0.0, 0.30), (100.0, 50.0, 0.30)],
        )];
        let r = req_claims(vec![claim("R", -0.2, 0.2, 0.0)], vec![], None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "wander_floor");
                assert!(
                    detail.contains("wander floor") && detail.contains("0.6"),
                    "the refusal prices the floor: {detail}"
                );
            }
            other => panic!("expected a wander_floor refusal, got {:?}", other),
        }
    }

    #[test]
    fn band_above_wander_floor_still_plans() {
        // same wobble, honest width: the gate must not over-refuse
        let entries = vec![entry(
            "a",
            "R",
            "p",
            "objective_0",
            &[(0.0, 0.0, 0.30), (100.0, 50.0, 0.30)],
        )];
        let r = req_claims(vec![claim("R", -2.5, 2.5, 0.0)], vec![], None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Planned { .. } => {}
            other => panic!("expected a plan, got {:?}", other),
        }
    }

    #[test]
    fn derived_wander_floor_refuses() {
        // tier 2: the bank says this channel's reading wandered P95=0.30
        // at hold - the derived floor is 0.60 (two sides), the bars say
        // only 0.04, and the band spans 0.50: the DERIVED floor binds
        let entries = vec![entry(
            "a",
            "R",
            "p",
            "objective_0",
            &[(0.0, 0.0, 0.02), (100.0, 50.0, 0.02)],
        )];
        let mut wander = HashMap::new();
        wander.insert(("R".to_string(), "objective_0".to_string()), 0.30);
        let r = req_claims(vec![claim("R", -0.25, 0.25, 0.0)], vec![], None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &wander).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "wander_floor");
                assert!(
                    detail.contains("derived wander 0.6000"),
                    "the refusal prices the derived floor: {detail}"
                );
            }
            other => panic!("expected a derived wander_floor refusal, got {:?}", other),
        }
    }

    #[test]
    fn derived_floor_silent_when_wider() {
        // the banked wander is small: the bar floor (tier 1) stays
        // binding and the detail names it - no false derived claims
        let entries = vec![entry(
            "a",
            "R",
            "p",
            "objective_0",
            &[(0.0, 0.0, 0.30), (100.0, 50.0, 0.30)],
        )];
        let mut wander = HashMap::new();
        wander.insert(("R".to_string(), "objective_0".to_string()), 0.05);
        let r = req_claims(vec![claim("R", -0.2, 0.2, 0.0)], vec![], None);
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &wander).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "wander_floor");
                assert!(
                    detail.contains("bar 2x median"),
                    "the binding floor is the bar's: {detail}"
                );
            }
            other => panic!("expected a bar-floor refusal, got {:?}", other),
        }
    }

    #[test]
    fn term_band_below_wander_floor_refuses() {
        // terms carry bands too: a protected reading nobody can keep
        // inside its band is a term no compile can honor
        let entries = vec![entry(
            "a",
            "R",
            "p",
            "objective_0",
            &[(0.0, 0.0, 0.30), (100.0, 50.0, 0.30)],
        )];
        let r = req_claims(
            vec![claim("R", -2.5, 2.5, 0.0)],
            vec![claim("R", -0.2, 0.2, 0.0)],
            None,
        );
        match plan(&entries, None, &r, Some(0.0), &HashMap::new(), &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "wander_floor");
                assert!(
                    detail.contains("term 'R'"),
                    "the refusal names the term: {detail}"
                );
            }
            other => panic!("expected a term wander_floor refusal, got {:?}", other),
        }
    }

    #[test]
    fn two_claims_one_dial_reconcile_to_one_setting() {
        // one dial, two readings: R1 slope 0.5, R2 slope 0.25
        let entries = vec![
            entry("a", "R1", "p", "objective_0", GAIN_UP),
            entry("a", "R2", "p", "objective_0", &[(0.0, 0.0, 0.02), (100.0, 25.0, 0.02)]),
        ];
        let r = req_claims(
            vec![
                claim("R1", 20.0, 30.0, 25.0),
                claim("R2", 7.5, 12.5, 10.0),
            ],
            vec![],
            None,
        );
        let map = anchors(&[("R1", 0.0), ("R2", 0.0)]);
        match plan(&entries, None, &r, None, &map, &HashMap::new()).unwrap() {
            PlanDecision::Planned {
                moves,
                claim_predictions,
                ..
            } => {
                // one dial -> exactly one move; both predictions in band
                assert_eq!(moves.len(), 1, "one dial carries one move");
                let s = moves[0].setting;
                assert!(
                    s >= 40.0 && s <= 50.0,
                    "setting inside the joint span [40, 50], got {}",
                    s
                );
                for cp in &claim_predictions {
                    let band = if cp.outcome == "R1" {
                        (20.0, 30.0)
                    } else {
                        (7.5, 12.5)
                    };
                    assert!(
                        cp.predicted >= band.0 && cp.predicted <= band.1,
                        "claim '{}' predicted {} outside {:?}",
                        cp.outcome,
                        cp.predicted,
                        band
                    );
                }
            }
            other => panic!("expected planned, got {:?}", other),
        }
    }

    #[test]
    fn disjoint_claims_on_one_dial_refuse_naming_both() {
        let entries = vec![
            entry("a", "R1", "p", "objective_0", GAIN_UP),
            entry("a", "R2", "p", "objective_0", &[(0.0, 0.0, 0.02), (100.0, 25.0, 0.02)]),
        ];
        let r = req_claims(
            vec![
                claim("R1", 24.0, 26.0, 25.0),  // levels [48, 52]
                claim("R2", 4.5, 5.5, 5.0),     // levels [18, 22] - disjoint
            ],
            vec![],
            None,
        );
        let map = anchors(&[("R1", 0.0), ("R2", 0.0)]);
        match plan(&entries, None, &r, None, &map, &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "joint_compile_empty");
                assert!(detail.contains("R1") && detail.contains("R2"),
                    "the refusal names both claims: {detail}");
            }
            other => panic!("expected refusal, got {:?}", other),
        }
    }

    #[test]
    fn term_corridor_pulls_the_setting() {
        // R1 slope 0.5 wants level 50 (shift 25); the term on the same
        // dial reads R2 at slope 0.25 - level 50 would push R2 to 12.5,
        // outside its [0, 10] band. The corridor pulls to level 40,
        // where R1 sits at its own band edge (20) and R2 at exactly 10.
        let entries = vec![
            entry("a", "R1", "p", "objective_0", GAIN_UP),
            entry("a", "R2", "p", "objective_0", &[(0.0, 0.0, 0.02), (100.0, 25.0, 0.02)]),
        ];
        let r = req_claims(
            vec![claim("R1", 20.0, 30.0, 25.0)],
            vec![claim("R2", 0.0, 10.0, 5.0)],
            None,
        );
        let map = anchors(&[("R1", 0.0), ("R2", 0.0)]);
        match plan(&entries, None, &r, None, &map, &HashMap::new()).unwrap() {
            PlanDecision::Planned { moves, .. } => {
                assert!(
                    (moves[0].setting - 40.0).abs() < 1e-6,
                    "the price is the aim's corridor: setting 40, got {}",
                    moves[0].setting
                );
            }
            other => panic!("expected planned, got {:?}", other),
        }
    }

    #[test]
    fn term_impossible_refuses_naming_the_price() {
        let entries = vec![
            entry("a", "R1", "p", "objective_0", GAIN_UP),
            entry("a", "R2", "p", "objective_0", &[(0.0, 0.0, 0.02), (100.0, 25.0, 0.02)]),
        ];
        let r = req_claims(
            vec![claim("R1", 20.0, 30.0, 25.0)],
            vec![claim("R2", 0.0, 8.0, 4.0)], // R1's own band floor (20) already puts R2 at 10 > 8
            None,
        );
        let map = anchors(&[("R1", 0.0), ("R2", 0.0)]);
        match plan(&entries, None, &r, None, &map, &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "joint_compile_empty");
                assert!(detail.contains("term") && detail.contains("R2") && detail.contains("R1"),
                    "the refusal names the price and the aim: {detail}");
            }
            other => panic!("expected refusal, got {:?}", other),
        }
    }

    #[test]
    fn unanchored_claim_refuses_named() {
        let entries = vec![
            entry("a", "R1", "p", "objective_0", GAIN_UP),
            entry("a", "R2", "p", "objective_0", GAIN_UP),
        ];
        let r = req_claims(
            vec![claim("R1", 20.0, 30.0, 25.0), claim("R2", 4.0, 6.0, 5.0)],
            vec![],
            None,
        );
        // only R1 banked a neutral: R2 is refused, named
        let map = anchors(&[("R1", 0.0)]);
        match plan(&entries, None, &r, None, &map, &HashMap::new()).unwrap() {
            PlanDecision::Refused { reason, detail } => {
                assert_eq!(reason, "unanchored_outcome");
                assert!(detail.contains("R2"), "the refusal names the claim: {detail}");
            }
            other => panic!("expected refusal, got {:?}", other),
        }
    }

    #[test]
    fn curve_fingerprint_tracks_new_points_only() {
        // The re-walk's fresh-evidence guard: the print moves exactly when
        // a curve the wish rides gains a point or its last shift moves.
        let entries = vec![
            entry("a", "R1", "p", "objective_0", &[(0.0, 0.0, 0.02), (100.0, 25.0, 0.02)]),
            entry("a", "R2", "p", "objective_0", &[(50.0, -0.007, 0.02)]),
        ];
        let r = req_claims(
            vec![claim("R1", 20.0, 30.0, 25.0), claim("R2", 4.0, 6.0, 5.0)],
            vec![],
            None,
        );
        let before = curve_fingerprint(&entries, &r.claims, &r.terms);

        // Unrelated curve moves: the print stands.
        let mut unrelated = entries.clone();
        unrelated.push(entry("b", "R9", "q", "objective_0", &[(0.0, 9.0, 0.02)]));
        assert_eq!(
            before,
            curve_fingerprint(&unrelated, &r.claims, &r.terms),
            "unrelated curves must not move the print"
        );

        // The wish's own curve gains a point: the print moves.
        let mut grown = entries.clone();
        grown[1].state.points.push((25.0, -0.1, 0.015));
        let after = curve_fingerprint(&grown, &r.claims, &r.terms);
        assert_ne!(before, after, "a new point on a ridden curve moves the print");

        // Same point count, different last shift: the print moves too.
        let mut moved = entries.clone();
        moved[1].state.points[0].1 = -0.05;
        assert_ne!(
            before,
            curve_fingerprint(&moved, &r.claims, &r.terms),
            "a moved last shift changes the print even at equal point count"
        );

        // Same point count, a MID-curve shift change (the hold-receipt
        // blend case): the print moves - this is the correction the
        // count+last print would have missed.
        let mut mid = entries.clone();
        mid[0].state.points[0].1 = -0.26;
        assert_ne!(
            before,
            curve_fingerprint(&mid, &r.claims, &r.terms),
            "a mid-curve correction must move the print"
        );

        // Re-running on unchanged entries is stable (deterministic print).
        assert_eq!(after, curve_fingerprint(&grown, &r.claims, &r.terms));
    }
}
