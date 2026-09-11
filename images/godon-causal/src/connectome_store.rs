// ─── Connectome store ────────────────────────────────────────────────
//
// Durable per-group connectomes. The map an engine serves is an
// object: /build derives it, upload reinstates it, the archive DB
// keeps it alive across restarts. One connectome per inference group
// - the grain the substrate already speaks (observations, heartbeats,
// standing params are all group-scoped).

use crate::artifact::import_artifact;
use crate::graph::CausalGraph;
use tokio_postgres::Client;

pub const DEFAULT_GROUP: &str = "default";

pub async fn ensure_connectomes_table(client: &Client) -> Result<(), tokio_postgres::Error> {
    // YB does not reliably apply ALTER COLUMN TYPE (0.19.0 created the
    // column as JSONB and the ignored ALTER left it there - bit Sep 11,
    // persist failed with "error serializing parameter 1"). A non-TEXT
    // legacy table can hold no rows - persist never succeeded - so it is
    // dropped and recreated as TEXT.
    let ty: String = client
        .query_one(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_name = 'connectomes' AND column_name = 'artifact'",
            &[],
        )
        .await
        .map(|row| row.get(0))
        .unwrap_or_else(|_| "absent".to_string());

    if ty != "text" {
        client.execute("DROP TABLE IF EXISTS connectomes", &[]).await?;
    }

    client
        .execute(
            "CREATE TABLE IF NOT EXISTS connectomes (\
             group_id VARCHAR(64) PRIMARY KEY, \
             artifact TEXT NOT NULL, \
             updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
            &[],
        )
        .await
        .map(|_| ())
}

pub async fn upsert_connectome(
    client: &Client,
    group_id: &str,
    artifact_json: &str,
) -> Result<(), tokio_postgres::Error> {
    client
        .execute(
            "INSERT INTO connectomes (group_id, artifact, updated_at) \
             VALUES ($1, $2::text, NOW()) \
             ON CONFLICT (group_id) DO UPDATE \
             SET artifact = EXCLUDED.artifact, updated_at = NOW()",
            &[&group_id, &artifact_json],
        )
        .await
        .map(|_| ())
}

pub async fn load_connectomes(
    client: &Client,
) -> Result<Vec<(String, CausalGraph)>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT group_id, CAST(artifact AS TEXT) FROM connectomes ORDER BY group_id",
            &[],
        )
        .await?;

    let mut out = Vec::new();
    for row in rows {
        let group_id: String = row.get(0);
        let json_str: String = row.get(1);
        match import_artifact(&json_str) {
            Ok(graph) => out.push((group_id, graph)),
            Err(e) => log::error!("skipping corrupt connectome '{}': {}", group_id, e),
        }
    }
    Ok(out)
}

pub fn valid_group(group_id: &str) -> bool {
    !group_id.is_empty()
        && group_id.len() <= 64
        && group_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ids_are_constrained() {
        assert!(valid_group("default"));
        assert!(valid_group("inference-group_1"));
        assert!(!valid_group(""));
        assert!(!valid_group("../etc"));
        assert!(!valid_group(&"x".repeat(65)));
    }
}
