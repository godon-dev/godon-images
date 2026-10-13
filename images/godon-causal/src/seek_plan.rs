//! The seek wish at causal's door — admission, the serving tender, and
//! the map-read (design §XIV; wire pinned in the chime 2026-10-12).
//!
//! Three questions, in order, all pure over the curve snapshot:
//! 1. VALIDATE — the body mirrors the two doors before it (controller
//!    and tender); causal re-checks because the wire is never trusted.
//! 2. WHO SERVES — the one tender whose params cover every region
//!    dial. No single tender, no seek (the region is unheld).
//! 3. CAN THE MAP READ IT — the sealed refusal rule: a FIND-BEST the
//!    map can already answer refuses WITH the answer inside the
//!    refusal. The honest conservative test: EXACTLY ONE region dial
//!    measurably reaches the criterion's (receiver, channel) with a
//!    settled curve — a single-dial read is a pure curve argmax.
//!    Two or more reaching dials is precisely the composition the
//!    neutral-baseline map cannot rank: admit the seek (that is what
//!    seeks are FOR). A reaching curve that is not settled is the
//!    "marginal freshness" boundary — also admitted.

use crate::probe_curves::CurveEntry;
use serde_json::{json, Value};

pub const COVER_NOW: &str = "cover_now";
pub const FIND_BEST: &str = "find_best";

#[derive(Debug, Clone, PartialEq)]
pub struct SeekBody {
    pub form: String,
    /// dial -> (lo, hi, is_int)
    pub region: Vec<(String, f64, f64, bool)>,
    /// (outcome, maximize?) — None for cover_now
    pub criterion: Option<(String, bool)>,
    pub budget: Option<i64>,
    pub candidates: i64,
}

fn direction_maximizes(word: &str) -> Option<bool> {
    match word {
        "max" | "maximize" => Some(true),
        "min" | "minimize" => Some(false),
        _ => None,
    }
}

/// Validate a walk body (the wire's `seek` object). Returns the parsed
/// body or a named refusal reason — the same names the two doors
/// before it use, so one vocabulary spans all three checks.
pub fn validate(body: &Value) -> Result<SeekBody, String> {
    let obj = body
        .as_object()
        .ok_or_else(|| "walk body must be an object".to_string())?;

    let form = obj
        .get("form")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            "form is required: find_best or cover_now (no omission)".to_string()
        })?
        .to_string();
    if form != COVER_NOW && form != FIND_BEST {
        return Err(format!("unknown_form: {form}"));
    }

    let region_obj = obj
        .get("region")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "region is required: at least one dial".to_string())?;
    let mut region = Vec::new();
    for (dial, spec) in region_obj {
        let arr = spec
            .as_array()
            .ok_or_else(|| format!("region.{dial} must be [lo, hi, is_int]"))?;
        if arr.len() < 2 {
            return Err(format!("region.{dial} must be [lo, hi, is_int]"));
        }
        let lo = arr[0]
            .as_f64()
            .ok_or_else(|| format!("region.{dial}.lo must be a number"))?;
        let hi = arr[1]
            .as_f64()
            .ok_or_else(|| format!("region.{dial}.hi must be a number"))?;
        if hi <= lo {
            return Err(format!("empty_range: region.{dial}"));
        }
        let is_int = arr.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
        region.push((dial.clone(), lo, hi, is_int));
    }
    if region.is_empty() {
        return Err("region is required: at least one dial".to_string());
    }

    let mut criterion = None;
    let mut budget = None;
    if form == FIND_BEST {
        let c = obj
            .get("criterion")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                "undeclared_criterion: find_best declares {outcome, direction}".to_string()
            })?;
        let outcome = c
            .get("outcome")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "undeclared_criterion: outcome is required".to_string())?;
        let dir = c
            .get("direction")
            .and_then(|v| v.as_str())
            .and_then(direction_maximizes)
            .ok_or_else(|| "unknown_direction".to_string())?;
        criterion = Some((outcome.to_string(), dir));
        let b = obj
            .get("budget")
            .and_then(|v| v.as_i64())
            .filter(|b| *b > 0)
            .ok_or_else(|| "undeclared_budget: a positive integer".to_string())?;
        budget = Some(b);
    } else if obj.contains_key("budget") {
        // The doors before this one refuse it; so does this one.
        return Err("cover_now walks on standing duty - no budget".to_string());
    }

    let candidates = obj.get("candidates").and_then(|v| v.as_i64()).unwrap_or(0);
    if candidates < 0 {
        return Err("negative_candidates".to_string());
    }
    if form == FIND_BEST {
        if let Some(b) = budget {
            if candidates > b {
                return Err(format!("candidates_exceed_budget: {candidates} > {b}"));
            }
        }
    }

    Ok(SeekBody {
        form,
        region,
        criterion,
        budget,
        candidates,
    })
}

/// The one tender whose params cover every region dial — the seek's
/// serving sender. Its driver walks its own dials; a region spanning
/// two tenders is unheld (v1: one seek, one tender).
pub fn serving_sender(entries: &[CurveEntry], body: &SeekBody) -> Option<String> {
    let mut sender: Option<String> = None;
    for (dial, _, _, _) in &body.region {
        let mut holders: Vec<&str> = entries
            .iter()
            .filter(|e| &e.param == dial)
            .map(|e| e.sender_id.as_str())
            .collect();
        holders.sort();
        holders.dedup();
        if holders.len() != 1 {
            return None; // zero holders (unknown dial) or split ownership
        }
        let holder = holders[0].to_string();
        match &sender {
            None => sender = Some(holder),
            Some(s) if *s == holder => {}
            _ => return None, // region spans tenders
        }
    }
    sender
}

/// Can the map read the FIND-BEST answer already? The honest
/// conservative rule (§XIV): exactly ONE region dial measurably
/// reaches the criterion's (receiver, channel) with a settled curve —
/// the answer is that curve's argmax, handed back inside the refusal.
/// Anything else (several reaching dials, an unsettled curve, no
/// reach at all) admits the seek.
pub fn read_off_map(
    entries: &[CurveEntry],
    body: &SeekBody,
    receiver: &str,
    channel: &str,
) -> Option<Value> {
    let (outcome, maximize) = body.criterion.as_ref()?;
    let mut reaching: Vec<&CurveEntry> = entries
        .iter()
        .filter(|e| {
            e.receiver_id == receiver
                && e.channel == channel
                && body.region.iter().any(|(d, _, _, _)| *d == e.param)
        })
        .collect();
    if reaching.len() != 1 {
        return None; // unmeasured, or composition the map cannot rank
    }
    let curve = reaching.remove(0);
    if !curve.state.converged || curve.state.points.is_empty() {
        return None; // marginal freshness admits the seek
    }
    let mut best = &curve.state.points[0];
    for p in curve.state.points.iter().skip(1) {
        let better = if *maximize { p.1 > best.1 } else { p.1 < best.1 };
        if better {
            best = p;
        }
    }
    let mut params = serde_json::Map::new();
    params.insert(curve.param.clone(), json!(best.0));
    Some(json!({
        "reading": best.1,
        "bars": best.2,
        "params": Value::Object(params),
        "outcome": outcome,
        "basis": "single-dial read off a settled curve",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe_curves::CurveState;

    fn entry(
        sender: &str,
        receiver: &str,
        param: &str,
        channel: &str,
        points: Vec<(f64, f64, f64)>,
        converged: bool,
    ) -> CurveEntry {
        let n = points.len();
        CurveEntry {
            sender_id: sender.into(),
            receiver_id: receiver.into(),
            param: param.into(),
            channel: channel.into(),
            state: CurveState {
                num_points: n,
                last_delta: 0.0,
                converged,
                points,
                gaps: vec![],
            },
        }
    }

    fn body_json(
        region: Value,
        criterion: Option<Value>,
        budget: Option<i64>,
        candidates: i64,
    ) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("form".into(), json!(FIND_BEST));
        o.insert("region".into(), region);
        o.insert(
            "criterion".into(),
            criterion.unwrap_or(json!({"outcome": "r/obj", "direction": "maximize"})),
        );
        if let Some(b) = budget {
            o.insert("budget".into(), json!(b));
        }
        o.insert("candidates".into(), json!(candidates));
        Value::Object(o)
    }

    #[test]
    fn door_mirrors_the_two_doors_before_it() {
        assert!(validate(&body_json(
            json!({"a": [0.0, 10.0, false]}),
            None,
            Some(50),
            60
        ))
        .is_err());
        let err = validate(&body_json(
            json!({"a": [10.0, 0.0, false]}),
            None,
            Some(50),
            5,
        ))
        .unwrap_err();
        assert!(err.contains("empty_range"));
        let err = validate(&body_json(json!({}), None, Some(50), 0)).unwrap_err();
        assert!(err.contains("region"));
        let err = validate(&serde_json::json!({
            "form": "cover_now", "region": {"a": [0.0, 1.0, false]},
            "budget": 10,
        }))
        .unwrap_err();
        // The controller's door refuses a budget on cover_now; this
        // door mirrors.
        assert!(err.contains("no budget"));
    }

    #[test]
    fn cover_now_admits_with_empty_search_fields() {
        let v = serde_json::json!({
            "form": "cover_now", "region": {"a": [0.0, 1.0, false]},
        });
        let b = validate(&v).unwrap();
        assert_eq!(b.form, COVER_NOW);
        assert!(b.criterion.is_none() && b.budget.is_none());
    }

    #[test]
    fn direction_words_normalize() {
        for (word, max) in [
            ("maximize", true),
            ("min", false),
            ("max", true),
            ("minimize", false),
        ] {
            let v = body_json(
                json!({"a": [0.0, 1.0, false]}),
                Some(json!({"outcome": "r/obj", "direction": word})),
                Some(10),
                0,
            );
            assert_eq!(validate(&v).unwrap().criterion.unwrap().1, max, "{word}");
        }
    }

    #[test]
    fn single_holder_serves_split_region_refuses() {
        let b = validate(&body_json(
            json!({"a": [0.0, 1.0, false], "b": [0.0, 1.0, false]}),
            None,
            Some(10),
            0,
        ))
        .unwrap();
        let entries = vec![
            entry("s1", "r", "a", "objective_0", vec![], true),
            entry("s1", "r", "b", "objective_0", vec![], true),
        ];
        assert_eq!(serving_sender(&entries, &b).as_deref(), Some("s1"));
        let split = vec![
            entry("s1", "r", "a", "objective_0", vec![], true),
            entry("s2", "r", "b", "objective_0", vec![], true),
        ];
        assert_eq!(serving_sender(&split, &b), None);
        let hole = vec![entry("s1", "r", "a", "objective_0", vec![], true)];
        assert_eq!(serving_sender(&hole, &b), None);
    }

    #[test]
    fn one_settled_reaching_dial_reads_the_map() {
        let b = validate(&body_json(
            json!({"a": [0.0, 100.0, false]}),
            None,
            Some(10),
            0,
        ))
        .unwrap();
        let entries = vec![entry(
            "s1",
            "r",
            "a",
            "obj",
            vec![(0.0, 0.1, 0.01), (50.0, 0.9, 0.01), (100.0, 0.3, 0.01)],
            true,
        )];
        let read = read_off_map(&entries, &b, "r", "obj").unwrap();
        assert_eq!(read["reading"], serde_json::json!(0.9));
        assert_eq!(read["params"]["a"], serde_json::json!(50.0));
    }

    #[test]
    fn minimize_reads_the_lowest() {
        let b = validate(&body_json(
            json!({"a": [0.0, 100.0, false]}),
            Some(json!({"outcome": "r/obj", "direction": "minimize"})),
            Some(10),
            0,
        ))
        .unwrap();
        let entries = vec![entry(
            "s1",
            "r",
            "a",
            "obj",
            vec![(0.0, 0.5, 0.01), (50.0, 0.9, 0.01), (100.0, 0.2, 0.01)],
            true,
        )];
        let read = read_off_map(&entries, &b, "r", "obj").unwrap();
        assert_eq!(read["reading"], serde_json::json!(0.2));
        assert_eq!(read["params"]["a"], serde_json::json!(100.0));
    }

    #[test]
    fn two_reaching_dials_admit_the_seek() {
        // The composition the neutral map cannot rank — the seek's
        // whole reason to exist.
        let b = validate(&body_json(
            json!({"a": [0.0, 1.0, false], "b": [0.0, 1.0, false]}),
            None,
            Some(10),
            0,
        ))
        .unwrap();
        let entries = vec![
            entry("s1", "r", "a", "obj", vec![(0.5, 0.4, 0.01)], true),
            entry("s1", "r", "b", "obj", vec![(0.5, 0.6, 0.01)], true),
        ];
        assert_eq!(read_off_map(&entries, &b, "r", "obj"), None);
    }

    #[test]
    fn unsettled_curve_admits_the_seek() {
        let b = validate(&body_json(
            json!({"a": [0.0, 100.0, false]}),
            None,
            Some(10),
            0,
        ))
        .unwrap();
        let entries = vec![entry(
            "s1",
            "r",
            "a",
            "obj",
            vec![(0.0, 0.1, 0.01), (100.0, 0.3, 0.01)],
            false, // still walking — marginal freshness admits
        )];
        assert_eq!(read_off_map(&entries, &b, "r", "obj"), None);
    }

    #[test]
    fn cover_now_never_reads_the_map() {
        let v = serde_json::json!({
            "form": "cover_now", "region": {"a": [0.0, 1.0, false]},
        });
        let b = validate(&v).unwrap();
        assert_eq!(
            read_off_map(
                &[entry("s1", "r", "a", "obj", vec![(0.5, 1.0, 0.01)], true)],
                &b,
                "r",
                "obj"
            ),
            None
        );
    }
}
