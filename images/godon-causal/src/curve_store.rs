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
    pub probe_param: String,
    pub probe_level: f64,
    pub shift: f64,
    pub convergence_threshold: f64,
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
                 convergence_threshold DOUBLE PRECISION NOT NULL, \
                 written_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
            &[],
        )
        .await
        .map(|_| ())
}

pub async fn insert_curve_point(
    client: &tokio_postgres::Client,
    group_id: &str,
    sender_id: &str,
    probe_param: &str,
    probe_level: f64,
    shift: f64,
    convergence_threshold: f64,
) -> Result<(), Error> {
    client
        .execute(
            "INSERT INTO curve_points \
             (group_id, sender_id, probe_param, probe_level, shift, convergence_threshold) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &group_id,
                &sender_id,
                &probe_param,
                &probe_level,
                &shift,
                &convergence_threshold,
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
            "SELECT group_id, sender_id, probe_param, probe_level, shift, \
             convergence_threshold FROM curve_points ORDER BY id",
            &[],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| CurvePointRow {
            group_id: row.get(0),
            sender_id: row.get(1),
            probe_param: row.get(2),
            probe_level: row.get(3),
            shift: row.get(4),
            convergence_threshold: row.get(5),
        })
        .collect())
}

/// Best-effort single-point persistence: log on failure, never propagate.
/// Measurement serving must not fail because the DB is down.
pub async fn persist_point(
    client: &tokio_postgres::Client,
    group_id: &str,
    sender_id: &str,
    probe_param: &str,
    probe_level: f64,
    shift: f64,
    convergence_threshold: f64,
) {
    if let Err(e) = insert_curve_point(
        client,
        group_id,
        sender_id,
        probe_param,
        probe_level,
        shift,
        convergence_threshold,
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
            probe_param,
            probe_level,
            shift,
            convergence_threshold,
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
