use crate::detector::DetectionResult;
use crate::graph::{CharacterizedEdge, ResponseFunction};
use crate::trial_reader::ProbeTrials;

pub fn characterize(detection: &DetectionResult, sender: &ProbeTrials) -> CharacterizedEdge {
    let rising_edge = detection.rising_edge;
    let falling_edge = detection.falling_edge;

    let impulse_scale = sender
        .push_trials
        .first()
        .map(|t| t.impulse_scale)
        .unwrap_or(1.0);

    let sensitivity = if impulse_scale > 0.0 {
        rising_edge / impulse_scale
    } else {
        rising_edge
    };

    let recovery_fraction = if rising_edge.abs() > 1e-12 {
        (falling_edge / rising_edge).abs()
    } else {
        0.0
    };

    let response = ResponseFunction::StepResponse {
        sensitivity,
        baseline: detection.baseline_median,
        recovery_fraction,
    };

    CharacterizedEdge {
        sender_id: detection.sender_id.clone(),
        receiver_id: detection.receiver_id.clone(),
        channel: detection.channel.clone(),
        detected: detection.detected,
        confidence: detection.confidence,
        method: detection.method.clone(),

        response,
        noise_floor: detection.baseline_mad,
        impulse_scale,

        rising_edge,
        falling_edge,
        baseline_median: detection.baseline_median,
        push_median: detection.push_median,
        pause_median: detection.pause_median,

        n_push_samples: detection.n_push_samples,
        n_pause_samples: detection.n_pause_samples,
        n_baseline_samples: detection.n_baseline_samples,

        characterized_at: chrono::Utc::now().to_rfc3339(),
        rounds_total: detection.rounds_total,
    }
}

/// Build a complete graph from all detection results for all pairs.
pub fn build_edges(
    sender_id: &str,
    receiver_id: &str,
    sender: &ProbeTrials,
    detections: &[DetectionResult],
) -> Vec<CharacterizedEdge> {
    detections
        .iter()
        .map(|d| characterize(d, sender))
        .collect()
}
