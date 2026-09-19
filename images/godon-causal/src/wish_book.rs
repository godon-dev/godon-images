//
// Copyright (c) 2019 Matthias Tafelmeier.
//
// This file is part of godon
//
// godon is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// godon is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this godon. If not, see <http://www.gnu.org/licenses/>.
//
//! The wish book: causal's durable record of every wish it was asked to plan.
//!
//! One row per wish in `wishes` (terms, current plan, status) and one event
//! stream per wish in `wish_events` (planned, refused, landed, missed,
//! undecidable, replanned, released). The book is written ONLY by causal, as
//! the side effect of its own work: terms enter at the plan ask, verdicts at
//! the judge, re-plans on fresh evidence. The controller keeps the declaration
//! (metadata); this book is the operative copy — the wish's state is its
//! newest event, derived on read.
//!
//! Same archive DB the curve points live in; same best-effort connect
//! pattern as curve_store (handlers connect per request).

use serde_json::Value;
use tokio_postgres::Error;

/// One wish's operative record, as read back from the book.
pub struct WishRow {
    pub wish_id: String,
    /// The plan request as sent: outcome, band, limits, group.
    pub terms: Value,
    /// The latest plan payload (moves, predicted ± bars) — or, for a
    /// refused wish, the refusal reason + detail. Null only before the
    /// first ask completed.
    pub plan: Option<Value>,
    /// Newest event: planned | refused | serving | missed | undecidable | released
    pub status: String,
}

pub async fn ensure_wish_tables(client: &tokio_postgres::Client) -> Result<(), Error> {
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS wishes (\
             wish_id TEXT PRIMARY KEY, \
             terms TEXT NOT NULL, \
             plan TEXT, \
             status TEXT NOT NULL, \
             created_tsz DOUBLE PRECISION NOT NULL, \
             updated_tsz DOUBLE PRECISION NOT NULL)",
            &[],
        )
        .await?;
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS wish_events (\
             wish_id TEXT NOT NULL, \
             tsz DOUBLE PRECISION NOT NULL, \
             event TEXT NOT NULL, \
             detail TEXT)",
            &[],
        )
        .await?;
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS outcome_anchors (\
             group_id TEXT NOT NULL, \
             receiver_id TEXT NOT NULL, \
             channel TEXT NOT NULL, \
             neutral DOUBLE PRECISION NOT NULL, \
             bar DOUBLE PRECISION NOT NULL, \
             updated_tsz DOUBLE PRECISION NOT NULL, \
             PRIMARY KEY (group_id, receiver_id, channel))",
            &[],
        )
        .await?;
    Ok(())
}

/// Bank the outcome channel's neutral reading: what the outcome shows
/// when the walked dial sits at rest. Every probe round's pause window
/// measures it; this upsert keeps the freshest. The wish band speaks in
/// these units (thermometer readings) — the wish door reads the anchor
/// to voice it in the curves' movement units. One anchor, one
/// translation, never a second unit in the wish itself.
pub async fn bank_outcome_anchor(
    client: &tokio_postgres::Client,
    group_id: &str,
    receiver_id: &str,
    channel: &str,
    neutral: f64,
    bar: f64,
) -> Result<(), Error> {
    client
        .execute(
            "INSERT INTO outcome_anchors (group_id, receiver_id, channel, neutral, bar, updated_tsz) \
             VALUES ($1, $2, $3, $4, $5, EXTRACT(EPOCH FROM now())) \
             ON CONFLICT (group_id, receiver_id, channel) DO UPDATE SET \
             neutral = EXCLUDED.neutral, bar = EXCLUDED.bar, \
             updated_tsz = EXTRACT(EPOCH FROM now())",
            &[&group_id, &receiver_id, &channel, &neutral, &bar],
        )
        .await
        .map(|_| ())
}

/// The freshest banked neutral for the outcome channel, with its bar.
/// None = no probe round has paused at neutral yet.
pub async fn read_outcome_anchor(
    client: &tokio_postgres::Client,
    group_id: &str,
    receiver_id: &str,
    channel: &str,
) -> Result<Option<(f64, f64)>, Error> {
    let row = client
        .query_opt(
            "SELECT neutral, bar FROM outcome_anchors \
             WHERE group_id = $1 AND receiver_id = $2 AND channel = $3",
            &[&group_id, &receiver_id, &channel],
        )
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

/// All banked anchors — the boot recovery read. One row per outcome
/// channel (the table's primary key), freshest write per channel.
pub async fn load_outcome_anchors(
    client: &tokio_postgres::Client,
) -> Result<Vec<(String, String, String, f64, f64)>, Error> {
    let rows = client
        .query(
            "SELECT group_id, receiver_id, channel, neutral, bar FROM outcome_anchors",
            &[],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
        .collect())
}

/// Upsert the wish's operative record. Called when a plan ask completes —
/// planned or refused. Never called by anyone but causal's own handlers.
pub async fn record_wish(
    client: &tokio_postgres::Client,
    wish_id: &str,
    terms: &Value,
    plan: Option<&Value>,
    status: &str,
) -> Result<(), Error> {
    client
        .execute(
            "INSERT INTO wishes (wish_id, terms, plan, status, created_tsz, updated_tsz) \
             VALUES ($1, $2, $3, $4, EXTRACT(EPOCH FROM now()), EXTRACT(EPOCH FROM now())) \
             ON CONFLICT (wish_id) DO UPDATE SET \
             plan = EXCLUDED.plan, status = EXCLUDED.status, \
             updated_tsz = EXTRACT(EPOCH FROM now())",
            &[
                &wish_id,
                &serde_json::to_string(terms).unwrap_or_default(),
                &plan.map(|p| serde_json::to_string(p).unwrap_or_default()),
                &status,
            ],
        )
        .await
        .map(|_| ())
}

pub async fn set_wish_status(
    client: &tokio_postgres::Client,
    wish_id: &str,
    status: &str,
) -> Result<(), Error> {
    client
        .execute(
            "UPDATE wishes SET status = $2, updated_tsz = EXTRACT(EPOCH FROM now()) \
             WHERE wish_id = $1",
            &[&wish_id, &status],
        )
        .await
        .map(|_| ())
}

pub async fn append_wish_event(
    client: &tokio_postgres::Client,
    wish_id: &str,
    event: &str,
    detail: Option<&Value>,
) -> Result<(), Error> {
    client
        .execute(
            "INSERT INTO wish_events (wish_id, tsz, event, detail) \
             VALUES ($1, EXTRACT(EPOCH FROM now()), $2, $3)",
            &[
                &wish_id,
                &event,
                &detail.map(|d| serde_json::to_string(d).unwrap_or_default()),
            ],
        )
        .await
        .map(|_| ())
}

pub async fn get_wish(
    client: &tokio_postgres::Client,
    wish_id: &str,
) -> Result<Option<WishRow>, Error> {
    let rows = client
        .query(
            "SELECT wish_id, terms, plan, status FROM wishes WHERE wish_id = $1",
            &[&wish_id],
        )
        .await?;
    Ok(rows.into_iter().next().map(|r| WishRow {
        wish_id: r.get(0),
        terms: serde_json::from_str(r.get::<_, String>(1).as_str()).unwrap_or(Value::Null),
        plan: r
            .get::<_, Option<String>>(2)
            .and_then(|p| serde_json::from_str(&p).ok()),
        status: r.get(3),
    }))
}

/// Timestamp of the most recent `event` for this wish, if any — the judge's
/// watermark (only trials written after it count toward the next verdict).
pub async fn last_event_tsz(
    client: &tokio_postgres::Client,
    wish_id: &str,
    event: &str,
) -> Result<Option<f64>, Error> {
    let rows = client
        .query(
            "SELECT tsz FROM wish_events WHERE wish_id = $1 AND event = $2 \
             ORDER BY tsz DESC LIMIT 1",
            &[&wish_id, &event],
        )
        .await?;
    Ok(rows.into_iter().next().map(|r| r.get(0)))
}

/// The band verdict — bar-decided, never count-decided. The reading's own
/// error bar decides: bar fully inside the half-band -> landed, bar clearly
/// outside -> missed, bar straddling the edge -> undecidable (measure more,
/// never punish the wish for our own blur).
pub fn band_verdict(
    median: f64,
    bar: f64,
    band_lo: f64,
    band_hi: f64,
    target: Option<f64>,
) -> &'static str {
    let aim = target.unwrap_or((band_lo + band_hi) / 2.0);
    let half = (band_hi - band_lo) / 2.0;
    let dev = (median - aim).abs();
    if dev + bar <= half {
        "landed"
    } else if dev - bar > half {
        "missed"
    } else {
        "undecidable"
    }
}

/// What the tender should do, given the wish's status. The tender lives in
/// GET-land: it reads this instruction and acts — nothing more.
pub fn instruction_for(status: &str) -> &'static str {
    match status {
        "planned" | "serving" => "hold",
        "missed" => "remeasure",
        "undecidable" => "hold",
        "released" | "refused" => "release",
        _ => "hold",
    }
}

/// Few holding readings, bar-decided — a bar needs at least three.
pub const JUDGE_MIN_READINGS: usize = 3;
/// Fresh plan, no verdict yet: the judge looks at this much hold history
/// before concluding anything (bounded window instead of all-of-history).
pub const JUDGE_LOOKBACK_SECS: f64 = 1800.0;

/// The reading's own error bar: 2σ of the hold readings over √n — the 2σ
/// judge. None until JUDGE_MIN_READINGS readings exist (undecidable by
/// scarcity, never by guesswork).
pub fn reading_bar(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n < JUDGE_MIN_READINGS {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let var = values
        .iter()
        .map(|v| {
            let d = v - mean;
            d * d
        })
        .sum::<f64>()
        / (n as f64 - 1.0);
    Some(2.0 * var.sqrt() / (n as f64).sqrt())
}

/// How fresh the map is for one receiver: the newest curve-point timestamp,
/// if any point exists. The re-plan trigger compares it against the miss.
pub async fn latest_receiver_point_tsz(
    client: &tokio_postgres::Client,
    receiver_id: &str,
) -> Result<Option<f64>, Error> {
    let rows = client
        .query(
            "SELECT MAX(EXTRACT(EPOCH FROM written_at)) FROM curve_points \
             WHERE receiver_id = $1",
            &[&receiver_id],
        )
        .await?;
    Ok(rows.into_iter().next().and_then(|r| r.get(0)))
}

// ─── The re-walk: bisection between known-bad and known-good ────────
// A miss means the held setting no longer lands in band. The re-walk is
// a sequential search over the wish's dial range for the NEW doorstep
// (where the reading crosses into band), one level per boundary,
// steered here, executed by the tender's normal probing machinery.
// The stop rule is decidability — a re-plan whose predicted bars fit
// inside the band — never a trial count.

/// One measured point of the walk's dial: dial level, response, bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkPoint {
    pub level: f64,
    pub response: f64,
    pub bar: f64,
}

impl WalkPoint {
    pub fn new(level: f64, response: f64, bar: f64) -> Self {
        Self {
            level,
            response,
            bar,
        }
    }
}

/// Side of one fresh reading against the band, bar-decided — the same
/// arithmetic as the judge, applied per probe. "blurry" = the bar
/// straddles an edge: evidence, but not a verdict.
pub fn probe_side(response: f64, bar: f64, band_lo: f64, band_hi: f64) -> &'static str {
    if response + bar < band_lo {
        "below"
    } else if response - bar > band_hi {
        "above"
    } else if response - bar >= band_lo && response + bar <= band_hi {
        "in"
    } else {
        "blurry"
    }
}

/// The receipt: did the world actually move, beyond combined bars? The
/// prediction comes from the plan (predicted value ± bars), the reading
/// from the miss verdict (median ± reading bar). Both already stamped —
/// the miss itself plays the gavel; no probe is spent re-learning what
/// the book already holds. When the two do NOT separate, a probe at the
/// old level is the only thing that can sharpen it (caller's fallback).
pub fn receipt_separated(
    predicted: f64,
    predicted_bars: f64,
    miss_median: f64,
    miss_bar: f64,
) -> bool {
    (predicted - miss_median).abs() > predicted_bars + miss_bar
}

/// Local steepness of the old curve at the held level: the pair of old
/// points bracketing it (or the two nearest when the level sits outside
/// the measured span). None when the old map cannot carry a slope —
/// fewer than two points, or a flat pair.
pub fn local_slope(points: &[WalkPoint], at: f64) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| {
        a.level
            .partial_cmp(&b.level)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for w in pts.windows(2) {
        if w[0].level <= at + 1e-9 && w[1].level >= at - 1e-9 {
            let dl = w[1].level - w[0].level;
            if dl.abs() > 1e-12 {
                let s = (w[1].response - w[0].response) / dl;
                // a flat local pair carries no direction — no prior
                return if s.abs() > 1e-12 { Some(s) } else { None };
            }
        }
    }
    let n = pts.len();
    let dl = pts[n - 1].level - pts[n - 2].level;
    if dl.abs() > 1e-12 {
        let s = (pts[n - 1].response - pts[n - 2].response) / dl;
        if s.abs() > 1e-12 {
            Some(s)
        } else {
            None
        }
    } else {
        None
    }
}

/// The next level to probe. A PURE function of the walk's evidence —
/// the same level is served on every GET until its point lands, so the
/// hint carries no state of its own; a failed probe retries, and the
/// payback ceiling bounds the retries. Order: the slope jump first (the
/// old curve demoted to a prior gets exactly one step), then midpoints
/// of the open frontier toward the range boundary, then bisection of
/// the closed bracket. The bracket's inner gap contains the crossing,
/// so narrowing it IS the doorstep-first ordering — disagreement far
/// from the doorstep is trivia for this wish.
///
/// Units: every position here (band bounds, miss reading, aim) is a
/// MOVEMENT from the neutral anchor, not a thermometer reading — the
/// caller translates the wish's absolute band once at its own door.
/// Deltas are space-invariant and pass through untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkHint {
    pub level: f64,
    pub phase: &'static str,
}

pub fn next_hint(
    held: f64,
    miss_median: f64,
    journal: &[WalkPoint],
    band_lo: f64,
    band_hi: f64,
    range_lo: f64,
    range_hi: f64,
    slope: Option<f64>,
    aim: f64,
) -> Option<WalkHint> {
    // The dial boundary the reading must travel toward: below the band
    // the reading must rise, above it must fall. The slope says which
    // boundary that is; without a slope the first fresh probe's own
    // movement is the evidence; with neither, blind.
    let explore_boundary = |seed_below: bool| -> Option<f64> {
        let want_reading_up = seed_below;
        if let Some(s) = slope {
            if (s > 0.0) == want_reading_up {
                Some(range_hi)
            } else {
                Some(range_lo)
            }
        } else if let Some(first) = journal.first() {
            let dl = first.level - held;
            let dr = first.response - miss_median;
            if dl.abs() > 1e-9 && dr.abs() > 1e-12 {
                let rises_with_dial_up = (dr > 0.0) == (dl > 0.0);
                if rises_with_dial_up == want_reading_up {
                    Some(range_hi)
                } else {
                    Some(range_lo)
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    if journal.is_empty() {
        // First probe of the walk: the prior's one step — to move the
        // reading to aim at steepness s, move the dial by (aim −
        // reading)/s, clamped into the plan's legal range.
        if let Some(s) = slope {
            if s.abs() > 1e-12 {
                let level = (held + (aim - miss_median) / s).clamp(range_lo, range_hi);
                if (level - held).abs() > 1e-9 {
                    return Some(WalkHint {
                        level,
                        phase: "slope",
                    });
                }
            }
        }
        // No usable prior (or it points nowhere new): blind start at
        // the middle of the legal range; the first result brackets.
        return Some(WalkHint {
            level: 0.5 * (range_lo + range_hi),
            phase: "blind",
        });
    }

    let seed_below = miss_median < 0.5 * (band_lo + band_hi);
    let mut highest_below: Option<f64> = None;
    let mut lowest_above: Option<f64> = None;
    for p in journal {
        match probe_side(p.response, p.bar, band_lo, band_hi) {
            "below" => highest_below = Some(highest_below.map_or(p.level, |l: f64| l.max(p.level))),
            "above" => lowest_above = Some(lowest_above.map_or(p.level, |l: f64| l.min(p.level))),
            _ => {}
        }
    }
    if let (Some(lo), Some(hi)) = (highest_below, lowest_above) {
        // Bracketed: bisect the gap that contains the doorstep.
        if hi - lo > 1e-9 {
            return Some(WalkHint {
                level: 0.5 * (lo + hi),
                phase: "bisect",
            });
        }
        return None;
    }
    // Bracket still open: explore from the frontier toward the boundary.
    let Some(boundary) = explore_boundary(seed_below) else {
        return Some(WalkHint {
            level: 0.5 * (range_lo + range_hi),
            phase: "blind",
        });
    };
    let frontier = journal.iter().map(|p| p.level).fold(held, |acc, l| {
        if (l - boundary).abs() < (acc - boundary).abs() {
            l
        } else {
            acc
        }
    });
    if (frontier - boundary).abs() <= 1e-9 {
        // The frontier IS the boundary and still no crossing — the
        // re-plan's refusal is the authority (no keepable point).
        return None;
    }
    Some(WalkHint {
        level: 0.5 * (frontier + boundary),
        phase: "explore",
    })
}

/// Two consecutive probes moved the reading OPPOSITE to the stored
/// curve's direction, beyond the fresh bar — the old curve's direction
/// was trusted and reality falsified it. A finding, not a failure.
pub fn sign_flip(held: f64, miss_median: f64, journal: &[WalkPoint], slope: f64) -> bool {
    let mut wrong: u32 = 0;
    let mut prev = (held, miss_median);
    for p in journal {
        let dl = p.level - prev.0;
        let dr = p.response - prev.1;
        let implied = slope * dl;
        if dl.abs() > 1e-9
            && dr.abs() > p.bar
            && implied.abs() > 1e-12
            && dr.signum() != implied.signum()
        {
            wrong += 1;
        } else {
            wrong = 0;
        }
        if wrong >= 2 {
            return true;
        }
        prev = (p.level, p.response);
    }
    false
}

/// The owner's outer total, in repair rounds. The first round is free
/// (the Sep-14 pricing: price = rounds/month, first round free); the
/// allowance counts repairs after it. None = standing, no cap — the
/// wish dies only by derived evidence (wall, sign flip, refusal).
pub fn budget_exhausted(budget: Option<u32>, walks_completed: usize) -> bool {
    match budget {
        None => false,
        Some(b) => walks_completed >= 1 + b as usize,
    }
}

// ─── Book reads the walk needs ──────────────────────────────────────

/// One book event as written, detail parsed when present.
pub struct WalkEventRow {
    pub tsz: f64,
    pub event: String,
    pub detail: Option<Value>,
}

pub async fn list_wish_events(
    client: &tokio_postgres::Client,
    wish_id: &str,
    since: f64,
) -> Result<Vec<WalkEventRow>, Error> {
    let rows = client
        .query(
            "SELECT EXTRACT(EPOCH FROM tsz), event, detail FROM wish_events \
             WHERE wish_id = $1 AND tsz >= TO_TIMESTAMP($2) ORDER BY tsz",
            &[&wish_id, &since],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| WalkEventRow {
            tsz: r.get(0),
            event: r.get(1),
            detail: r
                .get::<_, Option<String>>(2)
                .and_then(|d| serde_json::from_str(&d).ok()),
        })
        .collect())
}

pub async fn count_wish_events(
    client: &tokio_postgres::Client,
    wish_id: &str,
    event: &str,
) -> Result<usize, Error> {
    let rows = client
        .query(
            "SELECT COUNT(*) FROM wish_events WHERE wish_id = $1 AND event = $2",
            &[&wish_id, &event],
        )
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .map(|r| r.get::<_, i64>(0) as usize)
        .unwrap_or(0))
}

/// Latest tsz of any of `events` for the wish, optionally only before
/// `before` — the dry-spell arithmetic's source.
pub async fn latest_event_tsz(
    client: &tokio_postgres::Client,
    wish_id: &str,
    events: &[&str],
    before: Option<f64>,
) -> Result<Option<f64>, Error> {
    let rows = client
        .query(
            "SELECT MAX(EXTRACT(EPOCH FROM tsz)) FROM wish_events \
             WHERE wish_id = $1 AND event = ANY($2) \
             AND ($3::double precision IS NULL OR tsz < TO_TIMESTAMP($3))",
            &[&wish_id, &events, &before],
        )
        .await?;
    Ok(rows.into_iter().next().and_then(|r| r.get(0)))
}

/// The detail payload of the latest `event`, parsed — the miss verdict's
/// own numbers (median, bar) for the receipt and the seed.
pub async fn latest_event_detail(
    client: &tokio_postgres::Client,
    wish_id: &str,
    event: &str,
) -> Result<Option<Value>, Error> {
    let rows = client
        .query(
            "SELECT detail FROM wish_events WHERE wish_id = $1 AND event = $2 \
             ORDER BY tsz DESC LIMIT 1",
            &[&wish_id, &event],
        )
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.get::<_, Option<String>>(0))
        .and_then(|d| serde_json::from_str(&d).ok()))
}

/// Every raw curve point of the walk's dial tuple, newest last:
/// (written_tsz, level, shift, bar). Raw rows, one per probe write —
/// the registry holds the blends; this is the log with timestamps.
/// The dial's measured history, keyed by the globally-unique uuids
/// (sender, receiver) — never by group. The wish doors speak uuids;
/// groups are probe isolation, not identity (found live, Sep 19).
pub async fn read_dial_points(
    client: &tokio_postgres::Client,
    sender_id: &str,
    receiver_id: &str,
    param: &str,
    channel: &str,
) -> Result<Vec<(f64, f64, f64, f64)>, Error> {
    let rows = client
        .query(
            "SELECT EXTRACT(EPOCH FROM written_at), probe_level, shift, bar \
             FROM curve_points \
             WHERE sender_id = $1 AND receiver_id = $2 \
             AND probe_param = $3 AND channel = $4 ORDER BY written_at",
            &[&sender_id, &receiver_id, &param, &channel],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_dead_center_is_landed() {
        // the walkthrough numbers: target −0.10, band ±0.04, reading −0.1038 ± 0.0128
        assert_eq!(
            band_verdict(-0.1038, 0.0128, -0.14, -0.06, Some(-0.10)),
            "landed"
        );
    }

    #[test]
    fn verdict_clearly_outside_is_missed() {
        // reading −0.19 ± 0.02 vs band [−0.14, −0.06]: dev 0.09, dev − bar 0.07 > 0.04
        assert_eq!(
            band_verdict(-0.19, 0.02, -0.14, -0.06, Some(-0.10)),
            "missed"
        );
    }

    #[test]
    fn verdict_bar_straddling_edge_is_undecidable() {
        // reading −0.135 ± 0.030: dev 0.035, bar 0.030 — dev + bar 0.065 > 0.04,
        // dev − bar 0.005 ≤ 0.04: the bar straddles the band edge
        assert_eq!(
            band_verdict(-0.135, 0.030, -0.14, -0.06, Some(-0.10)),
            "undecidable"
        );
    }

    #[test]
    fn verdict_target_absent_aims_at_midpoint() {
        // band [−0.14, −0.06] midpoint −0.10; dead-center reading lands
        assert_eq!(band_verdict(-0.10, 0.005, -0.14, -0.06, None), "landed");
    }

    #[test]
    fn instructions_map_status_to_tender_duty() {
        assert_eq!(instruction_for("planned"), "hold");
        assert_eq!(instruction_for("serving"), "hold");
        assert_eq!(instruction_for("missed"), "remeasure");
        assert_eq!(instruction_for("undecidable"), "hold");
        assert_eq!(instruction_for("released"), "release");
        assert_eq!(instruction_for("refused"), "release");
    }

    #[test]
    fn reading_bar_needs_three_readings() {
        assert_eq!(reading_bar(&[]), None);
        assert_eq!(reading_bar(&[-0.10]), None);
        assert_eq!(reading_bar(&[-0.10, -0.11]), None);
    }

    #[test]
    fn reading_bar_of_identical_readings_is_zero() {
        let bar = reading_bar(&[-0.1038, -0.1038, -0.1038, -0.1038]).unwrap();
        assert!(bar.abs() < 1e-12);
    }

    #[test]
    fn reading_bar_scales_as_two_sigma_over_root_n() {
        // 4 readings, sample σ = 0.02 → bar = 2 * 0.02 / 2 = 0.02
        let v = [-0.10, -0.10, -0.10, -0.14];
        let bar = reading_bar(&v).unwrap();
        assert!((bar - 0.02).abs() < 1e-9, "bar was {bar}");
    }

    // ─── the re-walk: the walkthrough numbers ─────────────────────────
    // Wish W: chainend.shift at −0.10, band [−0.14, −0.06]. Held 0.62,
    // steepness ~0.9, miss −0.17 ± 0.02. The book seeds: out at 0.62.

    const BAND_LO: f64 = -0.14;
    const BAND_HI: f64 = -0.06;
    const HELD: f64 = 0.62;
    const MISS_MEDIAN: f64 = -0.17;
    const MISS_BAR: f64 = 0.02;
    const RANGE: (f64, f64) = (0.50, 0.80);
    const AIM: f64 = -0.10;

    #[test]
    fn walkthrough_receipt_separates_free() {
        // plan predicted −0.10 ± 0.02 vs miss −0.17 ± 0.02: the miss
        // itself is the gavel — no probe spent on the receipt.
        assert!(receipt_separated(-0.10, 0.02, MISS_MEDIAN, MISS_BAR));
        // a blurry pair cannot separate: retest territory
        assert!(!receipt_separated(-0.135, 0.03, MISS_MEDIAN, 0.04));
    }

    #[test]
    fn walkthrough_first_probe_is_the_slope_jump() {
        // steepness 0.9: to lift +0.07, open ~0.08 → 0.70
        let slope = local_slope(
            &[
                WalkPoint::new(0.62, -0.10, 0.01),
                WalkPoint::new(0.70, -0.028, 0.01),
            ],
            HELD,
        );
        assert!((slope.unwrap() - 0.9).abs() < 1e-9);
        let h = next_hint(
            HELD,
            MISS_MEDIAN,
            &[],
            BAND_LO,
            BAND_HI,
            RANGE.0,
            RANGE.1,
            slope,
            AIM,
        )
        .unwrap();
        assert_eq!(h.phase, "slope");
        assert!((h.level - 0.6978).abs() < 1e-3, "level was {}", h.level);
    }

    #[test]
    fn walkthrough_first_probe_blind_without_a_prior() {
        let h = next_hint(
            HELD,
            MISS_MEDIAN,
            &[],
            BAND_LO,
            BAND_HI,
            RANGE.0,
            RANGE.1,
            None,
            AIM,
        )
        .unwrap();
        assert_eq!(h.phase, "blind");
        assert!((h.level - 0.65).abs() < 1e-9);
    }

    #[test]
    fn walkthrough_trial_one_still_out_explores_toward_the_boundary() {
        // 0.70 → −0.155 ± 0.01: still out low. Frontier 0.70, ceiling
        // 0.80 → midpoint. Doorstep-first: the gap toward the boundary
        // is where the crossing lives.
        let journal = [WalkPoint::new(0.70, -0.155, 0.01)];
        assert_eq!(probe_side(-0.155, 0.01, BAND_LO, BAND_HI), "below");
        let h = next_hint(
            HELD,
            MISS_MEDIAN,
            &journal,
            BAND_LO,
            BAND_HI,
            RANGE.0,
            RANGE.1,
            Some(0.9),
            AIM,
        )
        .unwrap();
        assert_eq!(h.phase, "explore");
        assert!((h.level - 0.75).abs() < 1e-9, "level was {}", h.level);
    }

    #[test]
    fn walkthrough_bracketed_bisects_the_doorstep_gap() {
        // 0.70 out low, 0.75 out high (−0.03 ± 0.01): the answer lives
        // in [0.70, 0.75] — the third probe is its midpoint.
        let journal = [
            WalkPoint::new(0.70, -0.155, 0.01),
            WalkPoint::new(0.75, -0.03, 0.01),
        ];
        assert_eq!(probe_side(-0.03, 0.01, BAND_LO, BAND_HI), "above");
        let h = next_hint(
            HELD,
            MISS_MEDIAN,
            &journal,
            BAND_LO,
            BAND_HI,
            RANGE.0,
            RANGE.1,
            Some(0.9),
            AIM,
        )
        .unwrap();
        assert_eq!(h.phase, "bisect");
        assert!((h.level - 0.725).abs() < 1e-9, "level was {}", h.level);
    }

    #[test]
    fn probe_side_straddling_edge_is_blurry_evidence() {
        // −0.135 ± 0.03: the bar dips below lo, median inside — evidence
        // of the doorstep, not a verdict.
        assert_eq!(probe_side(-0.135, 0.03, BAND_LO, BAND_HI), "blurry");
        // −0.09 ± 0.01: fully inside — the decidable landing
        assert_eq!(probe_side(-0.09, 0.01, BAND_LO, BAND_HI), "in");
    }

    #[test]
    fn sign_flip_needs_two_consecutive_wrong_way_probes() {
        // slope says reading rises with the dial; reality falls instead
        let one = [WalkPoint::new(0.65, -0.20, 0.01)];
        assert!(!sign_flip(HELD, MISS_MEDIAN, &one, 0.9));
        let two = [
            WalkPoint::new(0.65, -0.20, 0.01),
            WalkPoint::new(0.68, -0.23, 0.01),
        ];
        assert!(sign_flip(HELD, MISS_MEDIAN, &two, 0.9));
        // one wrong then one right: noise, not a flip
        let mixed = [
            WalkPoint::new(0.65, -0.20, 0.01),
            WalkPoint::new(0.68, -0.17, 0.01),
        ];
        assert!(!sign_flip(HELD, MISS_MEDIAN, &mixed, 0.9));
    }

    #[test]
    fn budget_first_round_free_then_the_allowance_counts() {
        assert!(!budget_exhausted(None, 1000), "standing never exhausts");
        assert!(!budget_exhausted(Some(0), 0), "the first round is free");
        assert!(
            budget_exhausted(Some(0), 1),
            "budget 0 buys exactly the free round"
        );
        assert!(!budget_exhausted(Some(2), 2));
        assert!(budget_exhausted(Some(2), 3));
    }

    #[test]
    fn slope_is_none_on_a_flat_or_thin_old_map() {
        assert_eq!(local_slope(&[], HELD), None);
        assert_eq!(
            local_slope(&[WalkPoint::new(0.62, -0.10, 0.01)], HELD),
            None
        );
        let flat = [
            WalkPoint::new(0.62, -0.10, 0.01),
            WalkPoint::new(0.70, -0.10, 0.01),
        ];
        assert_eq!(local_slope(&flat, HELD), None);
    }

    #[test]
    fn hint_at_the_boundary_with_no_crossing_is_none() {
        // frontier reached the ceiling still below: the re-plan's
        // refusal is the authority, not another probe
        let journal = [WalkPoint::new(RANGE.1, -0.155, 0.01)];
        assert_eq!(
            next_hint(
                HELD,
                MISS_MEDIAN,
                &journal,
                BAND_LO,
                BAND_HI,
                RANGE.0,
                RANGE.1,
                Some(0.9),
                AIM
            ),
            None
        );
    }
}
