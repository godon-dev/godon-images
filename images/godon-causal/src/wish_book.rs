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
    Ok(())
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
pub fn band_verdict(median: f64, bar: f64, band_lo: f64, band_hi: f64, target: Option<f64>) -> &'static str {
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

/// The precise remeasure: which levels to probe so the wish's curve
/// section regenerates — the held setting, the target, and the midpoint
/// between them, clamped into the plan's legal range (span ∩ declared ∩
/// maxChange window). Sorted, deduped; the tender cycles through them.
pub fn remeasure_levels(setting: f64, target: f64, lo: f64, hi: f64) -> Vec<f64> {
    let clamp = |x: f64| x.clamp(lo, hi);
    let mut v = vec![clamp(setting), clamp((setting + target) / 2.0), clamp(target)];
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_dead_center_is_landed() {
        // the walkthrough numbers: target −0.10, band ±0.04, reading −0.1038 ± 0.0128
        assert_eq!(band_verdict(-0.1038, 0.0128, -0.14, -0.06, Some(-0.10)), "landed");
    }

    #[test]
    fn verdict_clearly_outside_is_missed() {
        // reading −0.19 ± 0.02 vs band [−0.14, −0.06]: dev 0.09, dev − bar 0.07 > 0.04
        assert_eq!(band_verdict(-0.19, 0.02, -0.14, -0.06, Some(-0.10)), "missed");
    }

    #[test]
    fn verdict_bar_straddling_edge_is_undecidable() {
        // reading −0.135 ± 0.030: dev 0.035, bar 0.030 — dev + bar 0.065 > 0.04,
        // dev − bar 0.005 ≤ 0.04: the bar straddles the band edge
        assert_eq!(band_verdict(-0.135, 0.030, -0.14, -0.06, Some(-0.10)), "undecidable");
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

    #[test]
    fn remeasure_levels_bracket_setting_and_target() {
        let v = remeasure_levels(0.62, 0.70, 0.50, 0.80);
        assert_eq!(v.len(), 3);
        assert!((v[0] - 0.62).abs() < 1e-9);
        assert!((v[1] - 0.66).abs() < 1e-9, "midpoint was {}", v[1]);
        assert!((v[2] - 0.70).abs() < 1e-9);
    }

    #[test]
    fn remeasure_levels_clamp_into_the_legal_range() {
        // target beyond the plan's range: clamped, never illegal
        let v = remeasure_levels(0.62, 1.50, 0.50, 0.80);
        assert_eq!(v, vec![0.62, 0.80]);
    }

    #[test]
    fn remeasure_levels_dedup_when_setting_meets_target() {
        assert_eq!(remeasure_levels(0.70, 0.70, 0.50, 0.80), vec![0.70]);
    }
}
