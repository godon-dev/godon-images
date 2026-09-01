//
// Copyright (c) 2019 Matthias Tafelmeier.
//
// AGPL-3.0 — see godon-images/LICENSE.
//
//! Persisted response-curve points.
//!
//! Write-through from the live CurveRegistry on every probe result,
//! replayed into the registry on startup so curves survive restarts.
//! Same archive DB the observation reader already uses.

use log::error;
use tokio_postgres::Error;

pub struct CurvePointRow {
    pub group_id: String,
    pub sender_id: String,
    /// Breeder whose readout this point measured. Rows persisted before
    /// the column existed replay as "unknown".
    pub receiver_id: String,
    pub probe_param: String,
    pub channel: String,
    pub probe_level: f64,
    pub shift: f64,
    pub bar: f64,
    pub convergence_threshold: f64,
    /// Standing dials of the group at measurement time — the regime
    /// this point was measured under. Rows persisted before the
    /// column existed replay as NULL (regime unrecorded).
    pub ambient: Option<String>,
}

pub async fn ensure_curve_table(client: &tokio_postgres::Client) -> Result<(), Error> {
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS curve_points (\
                 id BIGSERIAL PRIMARY KEY, \
                 group_id TEXT NOT NULL, \
                 sender_id TEXT NOT NULL, \
                 probe_param TEXT NOT NULL, \
                 probe_level DOUBLE PRECISION NOT NULL, \
                 shift DOUBLE PRECISION NOT NULL, \
                 bar DOUBLE PRECISION NOT NULL DEFAULT 0, \
             channel TEXT NOT NULL DEFAULT 'objective_0', \
                 convergence_threshold DOUBLE PRECISION NOT NULL, \
                 written_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
            &[],
        )
        .await?;
    // Tables created before the bar column existed (causal <= 0.11.1).
    client
        .execute(
            "ALTER TABLE curve_points \
             ADD COLUMN IF NOT EXISTS bar DOUBLE PRECISION NOT NULL DEFAULT 0",
            &[],
        )
        .await?;
    client
        .execute(
            "ALTER TABLE curve_points \
             ADD COLUMN IF NOT EXISTS channel TEXT NOT NULL DEFAULT 'objective_0'",
            &[],
        )
        .await?;
    // Tables created before the receiver column existed (causal <= 0.15.x,
    // 2-breeder era: one implicit receiver per window).
    client
        .execute(
            "ALTER TABLE curve_points \
             ADD COLUMN IF NOT EXISTS receiver_id TEXT NOT NULL DEFAULT 'unknown'",
            &[],
        )
        .await?;
    // Regime stamp: the group's standing dials at measurement time.
    client
        .execute(
            "ALTER TABLE curve_points \
             ADD COLUMN IF NOT EXISTS ambient JSONB",
            &[],
        )
        .await
        .map(|_| ())
}

pub async fn insert_curve_point(
    client: &tokio_postgres::Client,
    group_id: &str,
    sender_id: &str,
    receiver_id: &str,
    probe_param: &str,
    channel: &str,
    probe_level: f64,
    shift: f64,
    bar: f64,
    convergence_threshold: f64,
    ambient: Option<&str>,
) -> Result<(), Error> {
    client
        .execute(
            "INSERT INTO curve_points \
             (group_id, sender_id, receiver_id, probe_param, channel, probe_level, shift, bar, convergence_threshold, ambient) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &group_id,
                &sender_id,
                &receiver_id,
                &probe_param,
                &channel,
                &probe_level,
                &shift,
                &bar,
                &convergence_threshold,
                &ambient,
            ],
        )
        .await
        .map(|_| ())
}

pub async fn load_curve_points(
    client: &tokio_postgres::Client,
) -> Result<Vec<CurvePointRow>, Error> {
    let rows = client
        .query(
            "SELECT group_id, sender_id, receiver_id, probe_param, channel, probe_level, shift, bar, \
             convergence_threshold, CAST(ambient AS TEXT) FROM curve_points ORDER BY id",
            &[],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| CurvePointRow {
            group_id: row.get(0),
            sender_id: row.get(1),
            receiver_id: row.get(2),
            probe_param: row.get(3),
            channel: row.get(4),
            probe_level: row.get(5),
            shift: row.get(6),
            bar: row.get(7),
            convergence_threshold: row.get(8),
            ambient: row.get(9),
        })
        .collect())
}

/// Best-effort single-point persistence: log on failure, never propagate.
/// Measurement serving must not fail because the DB is down.
pub async fn persist_point(
    client: &tokio_postgres::Client,
    group_id: &str,
    sender_id: &str,
    receiver_id: &str,
    probe_param: &str,
    channel: &str,
    probe_level: f64,
    shift: f64,
    bar: f64,
    convergence_threshold: f64,
    ambient: Option<&str>,
) {
    if let Err(e) = insert_curve_point(
        client,
        group_id,
        sender_id,
        receiver_id,
        probe_param,
        channel,
        probe_level,
        shift,
        bar,
        convergence_threshold,
        ambient,
    )
    .await
    {
        // The table may not exist yet: startup ran while the DB was down
        // (stack reinstall ordering), or this deployment predates it.
        // Create it now and retry once — self-healing write path.
        if let Err(e2) = ensure_curve_table(client).await {
            error!(
                "curve point persistence failed and table setup failed (sender={} param={} level={}): insert: {} / setup: {}",
                sender_id, probe_param, probe_level, e, e2
            );
            return;
        }
        if let Err(e3) = insert_curve_point(
            client,
            group_id,
            sender_id,
            receiver_id,
            probe_param,
            channel,
            probe_level,
            shift,
            bar,
            convergence_threshold,
            ambient,
        )
        .await
        {
            error!(
                "curve point persistence failed after table setup (sender={} param={} level={}): {}",
                sender_id, probe_param, probe_level, e3
            );
        }
    }
}

/// Delete every persisted curve point owned by a sender (breeder purge).
/// Without this, startup replay resurrects purged breeders' curves as
/// ghosts in /curves and the graph artifact. Returns rows deleted.
pub async fn delete_curve_points(
    client: &tokio_postgres::Client,
    sender_id: &str,
) -> Result<u64, Error> {
    client
        .execute(
            "DELETE FROM curve_points WHERE sender_id = $1",
            &[&sender_id],
        )
        .await
}
