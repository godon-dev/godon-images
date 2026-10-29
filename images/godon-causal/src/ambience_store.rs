//! ambience_points: the local ground's persistence, curve_store idiom.
//! Write-through on bank (best-effort — measurement serving never
//! fails on the DB), restored at boot. The separation from
//! curve_points is the read surface's, not the storage's twin:
//! chain entries and ambience entries are different objects.

use tokio_postgres::Error;

#[derive(Debug, Clone)]
pub struct AmbienceRow {
    pub group_id: String,
    pub tender_id: String,
    pub ambience_param: String,
    pub channel: String,
    pub baseline: String,
    pub anchor: f64,
    pub level: f64,
    pub shift: f64,
    pub bar: f64,
    pub epoch: u64,
    pub written_at: f64,
}

pub async fn ensure_ambience_table(client: &tokio_postgres::Client) -> Result<(), Error> {
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS ambience_points (\
             id BIGSERIAL PRIMARY KEY, \
             group_id TEXT NOT NULL, \
             tender_id TEXT NOT NULL, \
             ambience_param TEXT NOT NULL, \
             channel TEXT NOT NULL DEFAULT 'objective_0', \
             baseline TEXT NOT NULL, \
             anchor DOUBLE PRECISION NOT NULL, \
             probe_level DOUBLE PRECISION NOT NULL, \
             shift DOUBLE PRECISION NOT NULL, \
             bar DOUBLE PRECISION NOT NULL DEFAULT 0, \
             epoch BIGINT NOT NULL DEFAULT 0, \
             written_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
            &[],
        )
        .await?;
    client
        .execute(
            "ALTER TABLE ambience_points ADD COLUMN IF NOT EXISTS adopted_from TEXT",
            &[],
        )
        .await?;
    client
        .execute(
            "CREATE INDEX IF NOT EXISTS ambience_points_key ON ambience_points \
             (group_id, tender_id, ambience_param, channel, baseline)",
            &[],
        )
        .await?;
    Ok(())
}

pub async fn insert_ambience_point(
    client: &tokio_postgres::Client,
    row: &AmbienceRow,
) -> Result<(), Error> {
    client
        .execute(
            "INSERT INTO ambience_points \
             (group_id, tender_id, ambience_param, channel, baseline, anchor, \
              probe_level, shift, bar, epoch) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &row.group_id,
                &row.tender_id,
                &row.ambience_param,
                &row.channel,
                &row.baseline,
                &row.anchor,
                &row.level,
                &row.shift,
                &row.bar,
                &(row.epoch as i64),
            ],
        )
        .await
        .map(|_| ())
}

pub async fn load_ambience_points(
    client: &tokio_postgres::Client,
) -> Result<Vec<AmbienceRow>, Error> {
    let rows = client
        .query(
            "SELECT group_id, tender_id, ambience_param, channel, baseline, anchor, \
             probe_level, shift, bar, epoch, \
             EXTRACT(EPOCH FROM written_at) FROM ambience_points ORDER BY id",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| AmbienceRow {
            group_id: row.get(0),
            tender_id: row.get(1),
            ambience_param: row.get(2),
            channel: row.get(3),
            baseline: row.get(4),
            anchor: row.get(5),
            level: row.get(6),
            shift: row.get(7),
            bar: row.get(8),
            epoch: row.get::<_, i64>(9) as u64,
            written_at: row.get::<_, f64>(10),
        })
        .collect())
}

/// Successor adoption, persistence side (rung 8, designs/2026-10-11
/// XX + amendment): orphaned ambience rows on one dial — rows whose
/// tender no longer appears in the room — re-parent to the tender
/// that banks there, ONLY on a bank that declared the takeover. The
/// row keeps its provenance: adopted_from carries the prior tender,
/// attribution time stays its own, and the prior tender's ledger is
/// never read. Returns rows re-parented.
pub async fn reparent_orphaned(
    client: &tokio_postgres::Client,
    group_id: &str,
    successor: &str,
    ambience_param: &str,
    live: &[String],
) -> Result<u64, Error> {
    let live_vec: Vec<String> = live.to_vec();
    let n = client
        .execute(
            "UPDATE ambience_points SET tender_id = $1, adopted_from = tender_id \
             WHERE group_id = $2 AND ambience_param = $3 \
             AND tender_id <> $1 \
             AND NOT (tender_id = ANY($4))",
            &[
                &successor.to_string(),
                &group_id.to_string(),
                &ambience_param.to_string(),
                &live_vec,
            ],
        )
        .await?;
    Ok(n)
}
