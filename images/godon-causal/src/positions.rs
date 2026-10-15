//! Positions (design §XIX): the derived picture of what every live
//! wish holds on the dials, and the coexistence consult.
//!
//! One mechanism serves both: the OTHER live wishes' claims ride the
//! compile as terms — a plan is admitted only when it keeps every
//! incumbent's band (joint reachability through the measured map,
//! answered with the compile's own numbers and bars). A refusal that
//! names an outcome owned by a standing position is a `position_conflict`: the mover's ask
//! refuses with a complete repair instruction, the positions as they stand
//! keep serving the incumbent, and coexistence is found by wishers
//! re-declaring outside — never by an engine-chosen settlement.
//! Unmeasured ground is honest silence here: the compile can only
//! refuse on measured evidence, so an unmeasured coupling does not
//! block the move — the per-serve promise check and vigilance catch
//! actual breakage with numbers, and the ground gets measured next.
//!
//! The view (`GET /positions`) derives fresh on every read from what
//! already exists — the book's plan journals — and is never stored:
//! three row kinds and no more (held, measuring, orphaned).

use serde_json::{json, Value};
use tokio_postgres::Client;

use crate::planner::{Band, ClaimSpec};

/// One claim of one OTHER live wish — a term the standing positions add to every
/// compile. Derived fresh from the book; never stored.
#[derive(Debug, Clone)]
pub struct PositionClaim {
    pub wish_id: String,
    pub outcome: String,
    pub lo: f64,
    pub hi: f64,
    /// The dial the incumbent's plan holds (sender, param) and its
    /// level — the repair instruction's "held where".
    pub sender: String,
    pub param: String,
    pub level: f64,
    /// The plan's last update (book time) — the "since when".
    pub since: f64,
}

impl PositionClaim {
    /// The claim as a compile term: same shape, same law — the positions's
    /// bands are protected readings the mover must never push out.
    pub fn as_term(&self) -> ClaimSpec {
        ClaimSpec {
            outcome: self.outcome.clone(),
            band: Band {
                lo: self.lo,
                hi: self.hi,
                target: None,
            },
        }
    }
}

/// The statuses the book treats as living. Terminal wishes (refused,
/// released) hold nothing; `declared` rows carry no plan yet.
const LIVE_STATUSES: &str = "('planned', 'serving', 'missed', 'remeasure', 'undecidable', 'declared')";

fn plan_dial(plan: &Value) -> Option<(String, String, f64)> {
    let mv = plan.get("moves")?.as_array()?.first()?;
    let sender = mv.get("sender")?.as_str()?.to_string();
    let param = mv.get("param")?.as_str()?.to_string();
    let level = mv.get("setting")?.as_f64()?;
    Some((sender, param, level))
}

/// The other live wishes' claims — the positions's terms. Seeks walk their
/// own region on their own dial (trial-level movement: detect and
/// surface, §XIX's spillover split), so only hold wishes contribute
/// terms. Best-effort callers may treat an error as an empty set.
pub async fn positions_terms(client: &Client, wish_id: &str) -> Result<Vec<PositionClaim>, tokio_postgres::Error> {
    let rows = client
        .query(
            &format!(
                "SELECT wish_id, terms, plan, updated_tsz FROM wishes \
                 WHERE status IN {LIVE_STATUSES} AND wish_id != $1 AND plan IS NOT NULL"
            ),
            &[&wish_id],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let other: String = r.get(0);
        let terms_s: String = r.get(1);
        let plan_s: Option<String> = r.get(2);
        let since: f64 = r.get(3);
        let Ok(terms) = serde_json::from_str::<Value>(&terms_s) else {
            continue;
        };
        // seeks hold no band (their candidates are partial service by
        // nature); only hold wishes speak claims
        if terms.get("type").and_then(|t| t.as_str()) == Some("seek") {
            continue;
        }
        let Some(plan_s) = plan_s else { continue };
        let Ok(plan) = serde_json::from_str::<Value>(&plan_s) else {
            continue;
        };
        let Some((sender, param, level)) = plan_dial(&plan) else {
            continue;
        };
        let Some(claims) = terms.get("claims").and_then(|c| c.as_array()) else {
            continue;
        };
        for c in claims {
            let (Some(outcome), Some(band)) = (
                c.get("outcome").and_then(|o| o.as_str()),
                c.get("band"),
            ) else {
                continue;
            };
            let (Some(lo), Some(hi)) = (
                band.get("lo").and_then(|v| v.as_f64()),
                band.get("hi").and_then(|v| v.as_f64()),
            ) else {
                continue;
            };
            out.push(PositionClaim {
                wish_id: other.clone(),
                outcome: outcome.to_string(),
                lo,
                hi,
                sender: sender.clone(),
                param: param.clone(),
                level,
                since,
            });
        }
    }
    Ok(out)
}

/// Does a refusal's detail speak an outcome owned by a standing
/// position? The compile
/// names its binding constraint ("term 'X': ..." / "claim 'X': ...");
/// when the named outcome belongs to another wish, the refusal is the
/// position's, and the mover deserves the full repair instruction.
pub fn owns_outcome<'a>(positions: &'a [PositionClaim], detail: &str) -> Option<&'a PositionClaim> {
    positions.iter()
        .find(|rc| detail.contains(&format!("'{}'", rc.outcome)))
        .or_else(|| positions.iter().find(|rc| detail.contains(&rc.outcome)))
}

/// The complete repair instruction for a position conflict: enough numbers
/// to design the coexisting wish in one round (which outcome, held
/// where by which wish since when, the compile's own numbers).
pub fn repair_instruction(rc: &PositionClaim, detail: &str) -> (String, Value) {
    let text = format!(
        "position conflict: your plan would break a held band. outcome '{}' is \
         held by wish {} (band [{:.4}, {:.4}]) on dial {}/{} at level {:.4} \
         since t={:.0}; the compile says: {}. Coexistence is found by \
         re-declaring outside: retarget, widen a band, or release theirs — \
         the engine never chooses between wishes.",
        rc.outcome, rc.wish_id, rc.lo, rc.hi, rc.sender, rc.param, rc.level,
        rc.since, detail
    );
    let payload = json!({
        "other_wish": rc.wish_id,
        "outcome": rc.outcome,
        "their_band": {"lo": rc.lo, "hi": rc.hi},
        "held": {"sender": rc.sender, "param": rc.param, "level": rc.level},
        "since": rc.since,
        "compile": detail,
    });
    (text, payload)
}

/// GET /positions — the derived positions view: three row kinds and no more,
/// computed fresh from the book's plan journals on every read, never
/// stored. `live_senders` (the graph's current senders) marks rows
/// whose serving tender is gone: orphaned settings are history —
/// hints speak, promises never, until re-measured.
pub async fn positions_view(
    client: &Client,
    live_senders: &[String],
) -> Result<Vec<Value>, tokio_postgres::Error> {
    let rows = client
        .query(
            &format!(
                "SELECT wish_id, terms, plan, status, updated_tsz FROM wishes \
                 WHERE status IN {LIVE_STATUSES} AND plan IS NOT NULL"
            ),
            &[],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let wish_id: String = r.get(0);
        let terms_s: String = r.get(1);
        let plan_s: Option<String> = r.get(2);
        let status: String = r.get(3);
        let since: f64 = r.get(4);
        let Ok(terms) = serde_json::from_str::<Value>(&terms_s) else {
            continue;
        };
        let Some(plan_s) = plan_s else { continue };
        let Ok(plan) = serde_json::from_str::<Value>(&plan_s) else {
            continue;
        };
        let Some((sender, param, level)) = plan_dial(&plan) else {
            continue;
        };
        let is_seek =
            terms.get("type").and_then(|t| t.as_str()) == Some("seek");
        let live = live_senders.iter().any(|s| *s == sender);
        let kind = if !live {
            "orphaned"
        } else if is_seek {
            "measuring"
        } else {
            "held"
        };
        let mut row = json!({
            "kind": kind,
            "wish": wish_id,
            "status": status,
            "setting": {"sender": sender, "param": param},
            "since": since,
        });
        if is_seek {
            // the never-two-movers window: the region the seek walks
            let window = terms
                .get("seek")
                .and_then(|s| s.get("region"))
                .cloned()
                .unwrap_or(Value::Null);
            row["window"] = window;
        } else {
            row["level"] = json!(level);
            row["bands"] = json!(terms
                .get("claims")
                .and_then(|c| c.as_array())
                .map(|claims| {
                    claims
                        .iter()
                        .map(|c| {
                            json!({
                                "outcome": c.get("outcome"),
                                "band": c.get("band"),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default());
        }
        out.push(row);
    }
    Ok(out)
}

/// A refusal that stands (§XIX): appended to the wish's own record
/// once per distinct refusal — the standing record while a wish sits
/// refused at the boundary, never a fresh event per poll. Deduped
/// against the refusal already standing since the last serve.
pub async fn standing_refusal(
    client: &Client,
    wish_id: &str,
    reason: &str,
    detail: &str,
) {
    let Ok(events) = crate::wish_book::list_wish_events(client, wish_id, 0.0).await else {
        return;
    };
    for e in events.iter().rev() {
        match e.event.as_str() {
            "planned" | "replanned" | "serving" => break, // a serve followed: fresh standing
            "refused" => {
                // the same refusal already stands — the record is the truth
                if let Some(d) = e.detail.as_ref() {
                    if d.get("reason").and_then(|r| r.as_str()) == Some(reason)
                        && d.get("detail").and_then(|x| x.as_str()) == Some(detail)
                    {
                        return;
                    }
                }
                break; // a different refusal stands; this one supersedes it
            }
            _ => continue,
        }
    }
    let _ = crate::wish_book::append_wish_event(
        client,
        wish_id,
        "refused",
        Some(&json!({"reason": reason, "detail": detail})),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(wish: &str, outcome: &str, lo: f64, hi: f64) -> PositionClaim {
        PositionClaim {
            wish_id: wish.to_string(),
            outcome: outcome.to_string(),
            lo,
            hi,
            sender: "a".to_string(),
            param: "p".to_string(),
            level: 40.0,
            since: 100.0,
        }
    }

    #[test]
    fn owns_outcome_matches_the_named_outcome() {
        let positions = vec![
            claim("w1", "chainend.shift", -0.1, 0.1),
            claim("w2", "room/objective_1", 0.0, 1.0),
        ];
        let hit = owns_outcome(
            &positions,
            "term 'room/objective_1': band [0.0000, 1.0000] ...",
        )
        .expect("the standing position owns the named outcome");
        assert_eq!(hit.wish_id, "w2");
        assert!(owns_outcome(&positions, "claim 'other'").is_none());
    }

    #[test]
    fn repair_instruction_carries_the_full_numbers() {
        let rc = claim("w9", "chainend.shift", -0.14, -0.06);
        let (text, payload) = repair_instruction(&rc, "the binding detail");
        assert!(text.contains("w9"));
        assert!(text.contains("chainend.shift"));
        assert!(text.contains("re-declaring outside"));
        assert_eq!(payload["other_wish"], "w9");
        assert_eq!(payload["held"]["level"], 40.0);
        assert_eq!(payload["their_band"]["lo"], -0.14);
    }

    #[test]
    fn as_term_is_a_plain_claim_spec() {
        let t = claim("w1", "o", 0.0, 1.0).as_term();
        assert_eq!(t.outcome, "o");
        assert_eq!(t.band.lo, 0.0);
        assert_eq!(t.band.hi, 1.0);
        assert!(t.band.target.is_none());
    }
}
