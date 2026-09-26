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
    pub outcome: String,
    pub band: Band,
    pub limits: Option<Limits>,
    /// Bench-config param constraints, enriched by the controller.
    /// Absent entry for a param = the measured span stands in.
    pub param_ranges: Option<HashMap<String, [f64; 2]>>,
}

#[derive(Debug, Serialize, Deserialize)]
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

// ─── Refusal bookkeeping ────────────────────────────────────────────

/// How far a candidate got before its binding constraint stopped it.
/// Higher rank = further progress = the more honest refusal to report
/// when several candidates each hit their own constraint.
const RANK_EXCLUDED: u8 = 0;
const RANK_SPAN_EMPTY: u8 = 1;
const RANK_MAGNITUDE_EMPTY: u8 = 2;
const RANK_NOT_INVERTIBLE: u8 = 3;
const RANK_TARGET_UNREACHABLE: u8 = 4;

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

/// Plan a wish against the measured map. Err = door refusal (malformed
/// request or unresolvable outcome - never planned). Ok(Refused) = the
/// binding constraint, named. Ok(Planned) = the map speaks.
pub fn plan(
    entries: &[CurveEntry],
    graph: Option<&CausalGraph>,
    req: &SteerPlanRequest,
    anchor: Option<f64>,
) -> Result<PlanDecision, String> {
    // door checks: malformed is refused before anything is planned
    if !req.band.lo.is_finite() || !req.band.hi.is_finite() || req.band.lo >= req.band.hi {
        return Err(format!(
            "band: lo must be < hi, both finite (got [{}, {}])",
            req.band.lo, req.band.hi
        ));
    }
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

    let (receiver, channel) = resolve_outcome(entries, &req.outcome)?;
    let target = req.band.target.unwrap_or(0.5 * (req.band.lo + req.band.hi));

    // The wish speaks in what the outcome shows (thermometer units);
    // the curves speak in movements from the banked neutral. The anchor
    // is the one translation point — and without it the wish cannot be
    // voiced in curve units at all: refuse, named, never guessed.
    let Some(anchor) = anchor.filter(|a| a.is_finite()) else {
        return Ok(PlanDecision::Refused {
            reason: "unanchored_outcome".to_string(),
            detail: format!(
                "outcome '{}' has no banked neutral reading yet - a probe round \
                 must pause at neutral before a wish on it can be planned",
                req.outcome
            ),
        });
    };
    let shift_target = target - anchor;

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

    let mut refusals: Vec<CandidateRefusal> = Vec::new();
    // (hops, predicted bar, sender, param, decision) - sort key, in order
    let mut plans: Vec<(usize, f64, String, String, PlanDecision)> = Vec::new();

    for (sender, param, points) in &candidates {
        if exclude.contains(&param.as_str()) {
            refusals.push(CandidateRefusal::new(
                RANK_EXCLUDED,
                "excluded_input",
                format!("param '{}' is wish-excluded", param),
            ));
            continue;
        }
        let bracket = match compute_bracket(points, param, &ranges, max_change) {
            Ok(b) => b,
            Err(r) => {
                refusals.push(r);
                continue;
            }
        };
        match branch_verdict(points, &bracket, shift_target) {
            Err(r) => {
                refusals.push(r);
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
                    "STEER plan: hill branch pick {} param '{}' -> setting {} on [{}, {}] predicts shift {} (bar {}) - {} other branch(es) named",
                    sender,
                    param,
                    setting,
                    level_span.0,
                    level_span.1,
                    predicted_shift,
                    bar,
                    alternatives
                );
                plans.push((
                    1,
                    bar,
                    sender.clone(),
                    param.clone(),
                    PlanDecision::Planned {
                        moves: vec![Move {
                            sender: sender.clone(),
                            param: param.clone(),
                            setting,
                            bars: bar,
                        }],
                        predicted_value: anchor + predicted_shift,
                        predicted_bars: bar,
                        range_used: vec![(param.clone(), bracket.lo, bracket.hi)],
                        path: vec![sender.clone(), receiver.clone()],
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
                let reason_detail = format!(
                    "param '{}': band [{}, {}] with anchor {} wants a move to {}, outside the curve's evaluated range [{}, {}] on the allowed bracket [{}, {}]",
                    param,
                    req.band.lo,
                    req.band.hi,
                    anchor,
                    shift_target,
                    eval(bracket.lo),
                    eval(bracket.hi),
                    bracket.lo,
                    bracket.hi
                );
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
                    "STEER plan: single-hop {} param '{}' -> setting {} predicts {} (move {} from anchor {}) +/- {}",
                    sender,
                    param,
                    setting,
                    anchor + evaluated.shift,
                    evaluated.shift,
                    anchor,
                    evaluated.bar
                );
                plans.push((
                    1,
                    evaluated.bar,
                    sender.clone(),
                    param.clone(),
                    PlanDecision::Planned {
                        moves: vec![Move {
                            sender: sender.clone(),
                            param: param.clone(),
                            setting,
                            bars: evaluated.bar,
                        }],
                        predicted_value: anchor + evaluated.shift,
                        predicted_bars: evaluated.bar,
                        range_used: vec![(param.clone(), bracket.lo, bracket.hi)],
                        path: vec![sender.clone(), receiver.clone()],
                    },
                ));
            }
        }
    }

    // Chains (a -> mid -> outcome) are deliberately NOT enumerated here.
    // Under v1 semantics a chain is always dominated by its own tail's
    // direct plan - same dial moved, same displacement, strictly more bar -
    // and the one legitimate trigger (a middle dial held by a live wish,
    // routing the move around it) is controller knowledge the engine does
    // not hold. Chains enter when the controller's enrichment can name
    // held dials; the compose math stays in steer.rs, tested.

    plans.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    if let Some((_, _, _, _, decision)) = plans.into_iter().next() {
        return Ok(decision);
    }

    // nothing feasible: report the most-advanced candidate's constraint
    refusals.sort_by_key(|r| std::cmp::Reverse(r.rank));
    if let Some(worst) = refusals.first() {
        Ok(PlanDecision::Refused {
            reason: worst.reason.to_string(),
            detail: format!(
                "{} candidate(s) evaluated; most advanced refusal - {}",
                candidates.len(),
                worst.detail
            ),
        })
    } else {
        Ok(PlanDecision::Refused {
            reason: "unmeasured_path".to_string(),
            detail: no_candidates_detail(entries, graph, &receiver, &channel),
        })
    }
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
        }
    }

    const GAIN_UP: &[(f64, f64, f64)] = &[(0.0, 0.0, 0.02), (100.0, 50.0, 0.02)];

    #[test]
    fn single_hop_lands_on_target() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 25.0, None, None);
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
            PlanDecision::Planned {
                moves,
                predicted_value,
                predicted_bars,
                range_used,
                path,
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(90.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(90.0)).unwrap() {
            PlanDecision::Refused { reason, .. } => assert_eq!(reason, "outside_measured_range"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn unanchored_outcome_refuses_named() {
        let entries = vec![entry("a", "R", "p", "objective_0", GAIN_UP)];
        let r = req("R", 25.0, None, None);
        match plan(&entries, None, &r, None).unwrap() {
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
        match plan(&entries, None, &r, Some(0.0)).unwrap() {
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
                plan(&entries, None, &r, Some(0.0)).is_ok(),
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
        let err = plan(&entries, None, &r, Some(0.0)).unwrap_err();
        assert!(err.contains("ambiguous"), "two channels: {err}");
        assert!(err.contains("objective_0") && err.contains("objective_1"));

        let r = req("nowhere", 25.0, None, None);
        let err = plan(&entries, None, &r, Some(0.0)).unwrap_err();
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
        };
        assert!(plan(&entries, None, &bad_band, Some(0.0)).is_err());
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
        assert!(plan(&entries, None, &r, Some(0.0)).is_err());
        // inverted declared range
        let mut ranges = HashMap::new();
        ranges.insert("p".to_string(), [100.0, 0.0]);
        let r = req("R", 25.0, None, Some(ranges));
        assert!(plan(&entries, None, &r, Some(0.0)).is_err());
    }
}
