//! The precision-contract guard (ruling 2026-10-31, engine-status):
//! per-tender bar divergence makes findings incoherent — the book
//! merges mixed-bar curves, detection calibrates against one contract,
//! cells stop comparing. The CREATE door refuses mixed EFFECTIVE bars
//! within the live group; the UPDATE door stays the runtime lever by
//! design (respawn picks up new det in seconds).

/// The estate default bar, mirrored from charts (values.yaml sets the
/// same value here and on the robot worker group — one declared term,
/// two readers). Env values parse honestly: missing / unparsable /
/// non-finite / non-positive fall back to 0.02, never a nonsense bar.
fn estate_default_bar() -> f64 {
    std::env::var("GODON_CONVERGENCE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(0.02)
}

/// The bar a tender actually runs: its det's declared value, else the
/// estate default — the EXACT precedence the engine's coordinator
/// constructor runs (det > env > 0.02). Declared values pass through
/// unfiltered, as the engine takes them.
pub(crate) fn effective_bar(declared: Option<f64>) -> f64 {
    declared.unwrap_or_else(estate_default_bar)
}

/// One live peer's declared bar (None = det silent, rides the default)
/// and its interference-detection group — the unit of comparability.
/// The precision contract binds bars WITHIN a group; different groups
/// (disjoint studies) may legitimately carry different bars and never
/// compare (holder correction 2026-11-13: the group is where the knob
/// lives — the guard must not assume one-room-equals-all-tenders).
pub(crate) struct PeerBar {
    pub name: String,
    pub group: Option<String>,
    pub declared: Option<f64>,
}

/// A refused create: the first disagreeing peer, both effective bars.
pub(crate) struct BarConflict {
    pub peer_name: String,
    pub peer_bar: f64,
    pub new_bar: f64,
}

/// Pure decision: the new tender's EFFECTIVE bar against every live
/// peer's — but only peers in the SAME interference group. First
/// disagreement wins; None = uniform (or no peers in this group),
/// allow. Comparison is exact — declared bars are decimal literals,
/// and the default composes from a single deterministic parse.
pub(crate) fn detect_mixed_bar(
    new_declared: Option<f64>,
    new_group: Option<&str>,
    peers: &[PeerBar],
) -> Option<BarConflict> {
    let new_bar = effective_bar(new_declared);
    peers
        .iter()
        .filter(|p| match (new_group, p.group.as_deref()) {
            (Some(g), Some(pg)) => g == pg,
            _ => false,
        })
        .find_map(|peer| {
            let peer_bar = effective_bar(peer.declared);
            (peer_bar != new_bar).then(|| BarConflict {
                peer_name: peer.name.clone(),
                peer_bar,
                new_bar,
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str, declared: Option<f64>) -> PeerBar {
        PeerBar {
            name: name.to_string(),
            group: Some("study-a".to_string()),
            declared,
        }
    }

    // Every test wants the estate env silent; remove_var is idempotent
    // across parallel tests that share the process env.
    fn clean_env() {
        std::env::remove_var("GODON_CONVERGENCE_THRESHOLD");
    }

    #[test]
    fn uniform_declared_bars_allow() {
        clean_env();
        let peers = vec![peer("node-1", Some(0.05)), peer("node-2", Some(0.05))];
        assert!(detect_mixed_bar(Some(0.05), Some("study-a"), &peers).is_none());
    }

    #[test]
    fn mixed_declared_bars_refuse_and_name_the_peer() {
        clean_env();
        let peers = vec![peer("node-1", Some(0.02)), peer("node-2", Some(0.05))];
        let c = detect_mixed_bar(Some(0.05), Some("study-a"), &peers).unwrap();
        assert_eq!(c.peer_name, "node-1");
        assert_eq!(c.peer_bar, 0.02);
        assert_eq!(c.new_bar, 0.05);
    }

    #[test]
    fn different_groups_never_compare() {
        // The holder's architecture: the knob lives at the group level —
        // disjoint studies may carry different bars and never compare.
        clean_env();
        let mut other = peer("solo-1", Some(0.05));
        other.group = Some("study-b".to_string());
        let peers = vec![other];
        assert!(detect_mixed_bar(Some(0.02), Some("study-a"), &peers).is_none());
    }

    #[test]
    fn tender_without_group_is_not_guarded() {
        // No declared group = nothing to compare against; comparability
        // lives in the group.
        clean_env();
        let mut ungrouped = peer("loose", Some(0.05));
        ungrouped.group = None;
        let peers = vec![ungrouped];
        assert!(detect_mixed_bar(Some(0.02), None, &peers).is_none());
        assert!(detect_mixed_bar(Some(0.02), Some("study-a"), &peers).is_none());
    }

    #[test]
    fn undeclared_rides_the_estate_default_uniformly() {
        clean_env();
        let peers = vec![peer("node-1", None), peer("node-2", None)];
        assert!(detect_mixed_bar(None, Some("study-a"), &peers).is_none());
    }

    #[test]
    fn undeclared_peer_matches_declared_default_bar() {
        clean_env();
        // Peer det silent → effective 0.02; new declares exactly that.
        let peers = vec![peer("node-1", None)];
        assert!(detect_mixed_bar(Some(0.02), Some("study-a"), &peers).is_none());
    }

    #[test]
    fn undeclared_peer_vs_new_bar_refuses() {
        clean_env();
        // The incoherence class the guard exists for: a silent-det
        // peer (effective 0.02) and a new 0.05 tender would mix bars
        // without a single explicit conflict to point at.
        let peers = vec![peer("node-1", None)];
        let c = detect_mixed_bar(Some(0.05), Some("study-a"), &peers).unwrap();
        assert_eq!(c.peer_bar, 0.02);
        assert_eq!(c.new_bar, 0.05);
    }

    #[test]
    fn empty_group_allows_anything() {
        clean_env();
        assert!(detect_mixed_bar(Some(0.05), Some("study-a"), &[]).is_none());
    }
}
