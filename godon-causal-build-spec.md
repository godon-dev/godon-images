# godon-causal — Build Specification (Revised)

Builds the causal connectome from interventional probe data produced by
the detection coordination protocol. Reads trial data directly from
YugabyteDB. Runs detection (CFAR, first implementation) and response
characterization in a single pass. Produces a transferable graph
artifact — the connectome snapshot.

This is a specification, not a design doc. Read it, reference the
existing code patterns, implement.

## What This Is

`godon-causal` is a Rust HTTP service (crate + Axum server) that:

1. Reads interventional probe trials from YugabyteDB (push/pause/hold
   data produced by the breeder detection coordinator)
2. Detects coupling edges between breeder pairs (CFAR block-step, first
   implementation — abstracted behind a trait)
3. Characterizes each detected edge: response magnitude, recovery,
   noise model, confidence
4. Assembles a directed coupling graph (the connectome)
5. Exports the graph as a transferable JSON artifact
6. Answers what-if / impact queries via composition along edges

It sits alongside `godon-observer` in the images directory. Both read
from the same YugabyteDB. Observer asks "are breeders interfering
right now?" Causal asks "what is the full causal structure, how strong
are the couplings, and what can we predict?"

Detection (does the edge exist?) and characterization (what are the
edge's properties?) are both internal. CFAR is the first detector
implementation, abstracted behind a trait so future detectors can be
added without touching the characterization or graph layers.

## Architectural Decision: Causal Owns Detection + Analysis

Causal is the canonical home for coupling detection AND analysis. It
reads trial data directly from YugabyteDB. It does NOT depend on the
observer at runtime.

Two cadences, one detection codebase:

1. REAL-TIME: `GET /detect/:sender_id/:receiver_id` — reads trials for
   one pair, runs CFAR, returns result immediately. Lightweight, on-demand.
   This is what the observer dashboard calls for "are they coupled now?"
   Does NOT touch the graph cache.

2. BATCH: `POST /build` — reads all breeders, all pairs, runs detection
   + characterization, assembles full graph, updates cache. Background
   task. This produces the connectome snapshot.

Same `EdgeDetector` trait, same `CfarDetector`, same trial reader.
Two entry points. No duplication.

The observer's existing CFAR detection code (`detect_watermark_coupling`
in optuna_reader.rs) is deprecated over time. The observer dashboard
migrates to calling causal's `/detect` endpoint. Observer keeps: metrics
scraping, trial browsing, dashboard rendering.

## Location

```
godon-images/images/godon-causal/
├── Cargo.toml
├── default.nix
├── src/
│   ├── lib.rs              (crate root — exports public types)
│   ├── main.rs             (Axum HTTP service)
│   ├── trial_reader.rs     (read interventional probe trials from DB)
│   ├── detector.rs         (EdgeDetector trait + CFAR implementation)
│   ├── characterizer.rs    (extract response functions from probe data)
│   ├── graph.rs            (CausalGraph data structure)
│   ├── composer.rs         (predict effects via edge composition)
│   ├── artifact.rs         (serde JSON serialize/deserialize the graph)
│   └── query.rs            (what-if, impact analysis, graph summary)
└── tests/
    └── integration.rs      (test against mock probe data)
```

## Reference Code (COPY PATTERNS, DON'T REINVENT)

Before implementing, read these existing files:

1. **`images/godon-observer/src/optuna_reader.rs`** (939 lines) — the
   primary reference. Copy:
   - DB connection pattern (`OptunaReader::from_env()`, `connect()`,
     `breeder_db_name()`)
   - All SQL queries (trials, params, values, user_attrs, study_directions)
   - The `TrialRecord` struct
   - The CFAR detection logic (`detect_watermark_coupling`) — this is
     the proven detection. Port it into `detector.rs` behind a trait.
   - Timestamp parsing (`parse_timestamp_secs`)
   - `median()`, `mad()` helper functions

2. **`images/godon-observer/src/main.rs`** — HTTP server pattern.
   Uses hyper 0.14. For godon-causal prefer Axum (cleaner routing) —
   reference `images/godon-api/src/main.rs` or
   `images/godon-mcp/src/main.rs` for Axum patterns.

3. **`images/godon-observer/default.nix`** — Nix build pattern. Copy
   and adjust the image name and Cargo.toml path.

4. **`images/godon-observer/Cargo.toml`** — dependency patterns. Reuse
   tokio-postgres, serde, serde_json, log, env_logger.

5. **`build/build-container-nix.sh`** — the shared Nix build script.
   Called by default.nix. Don't modify it.

6. **`.github/workflows/godon-observer-ci.yml`** and
   **`godon-observer-release.yml`** — CI/release patterns. Copy and
   adjust image name and paths for godon-causal.

## Trial Data Model — What The Causal Image Reads

The causal image reads the SAME trial data as the observer. Every trial
record from YugabyteDB contains:

```rust
pub struct TrialRecord {
    pub number: i32,
    pub state: String,           // "COMPLETE" is what we use
    pub datetime_start: Option<String>,   // ISO timestamp — critical for cross-breeder alignment
    pub datetime_complete: Option<String>,
    pub params: HashMap<String, f64>,
    pub param_distributions: HashMap<String, serde_json::Value>,
    pub values: Vec<Option<f64>>,         // objective values
    pub user_attrs: HashMap<String, serde_json::Value>,
}
```

The detection coordinator writes these user_attrs per trial (from
`breeder_worker.py:994-1005`):

| Key | Values | Meaning |
|-----|--------|---------|
| `detection_mode` | `optimize` \| `hold` \| `impulse` | What the breeder did this trial |
| `coord_state` | `hold_calib` \| `impulse_calib` \| `push` \| `pause` \| `done` \| `cooldown` \| `hold` \| `optimize` | Coordinator state machine position |
| `impulse_phase` | `hold_calib` \| `impulse_calib` \| `push` \| `pause` | Phase tag for observer windowing (sender only) |
| `impulse_scale` | float (0.125–1.0) | Amplitude scale of the impulse probe |
| `lease_phase` | string | Sender's phase as observed by the receiver |
| `observations` | JSON string | Non-objective observation metrics (extra detection channels) |
| `guardrails` | JSON string | Per-guardrail readings |
| `watermark` | JSON string | Legacy spectral watermark (DISABLED — old trials only) |

The causal image filters trials by these attrs to identify:
- **Sender push trials**: `impulse_phase == "push"` — params at extremes
- **Sender pause trials**: `impulse_phase == "pause"` — params back to neutral
- **Sender hold_calib trials**: `impulse_phase == "hold_calib"` — neutral params, flatness search
- **Receiver hold trials**: `detection_mode == "hold"` AND `coord_state != "hold_calib"` — receiver holding neutral while sender probes

## DB Connection

Same env vars as observer:
- `GODON_ARCHIVE_DB_USER` (default: "yugabyte")
- `GODON_ARCHIVE_DB_PASSWORD` (default: "yugabyte")
- `GODON_ARCHIVE_DB_SERVICE_HOST` (default: "yb-tserver-0")
- `GODON_ARCHIVE_DB_SERVICE_PORT` (default: 5433)

Each breeder has its own DB: `breeder_{uuid_with_dashes_as_underscores}`.
The shared `yugabyte` DB lists all breeder study names.

SQL queries: identical to observer's optuna_reader.rs. Copy them.

## Dependencies (Cargo.toml)

```toml
[package]
name = "godon-causal"
version = "0.1.0"
edition = "2021"
authors = ["godon-dev Matthias Tafelmaier"]
description = "Godon Causal — coupling graph construction and prediction from probe data"
license = "AGPL-3.0"

[dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tokio-postgres = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
log = "0.4"
env_logger = "0.11"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
# whatever is needed for tests
```

No nalgebra (no OLS). No petgraph (the graph is simple enough for a
Vec-based representation — see graph.rs). Add petgraph later if path
enumeration for multi-hop composition needs it.

## Nix Build (default.nix)

Follow the EXACT pattern from `images/godon-observer/default.nix`.
Change image name to `godon-causal`, port 8091. Same structure:
`rustPlatform.buildRustPackage` + `dockerTools.buildLayeredImage`.

## Components — What Each File Does

### trial_reader.rs

Reads interventional probe trial data from YugabyteDB. Same DB access
patterns as observer's `optuna_reader.rs`. Copy the connection, query,
and TrialRecord building logic.

**Additional capability beyond observer:** the causal image needs to
enumerate ALL breeder pairs, not just one sender/receiver. The trial
reader must:

1. List all breeder databases (query `yugabyte` DB for study names,
   derive breeder IDs)
2. For a given breeder pair (sender, receiver), load both breeders'
   complete trials

```rust
pub struct TrialReader {
    config: DbConfig,
}

impl TrialReader {
    pub fn from_env() -> Self;

    /// List all breeder IDs that have databases.
    pub async fn list_breeders(&self) -> Result<Vec<String>;

    /// Load all COMPLETE trials for a breeder's default study.
    pub async fn read_trials(&self, breeder_id: &str)
        -> Result<Vec<TrialRecord>>;

    /// Load trials and classify them by detection role.
    /// Returns sender phases (push/pause/hold_calib) and receiver hold trials.
    pub async fn read_probe_trials(&self, breeder_id: &str)
        -> Result<ProbeTrials>;
}

/// Classified trial data for a single breeder.
pub struct ProbeTrials {
    pub breeder_id: String,
    /// Sender push trials — params at extremes, timestamped.
    /// Each entry is (timestamp_secs, trial_number, params, values, impulse_scale).
    pub push_trials: Vec<ProbeTrial>,
    /// Sender pause trials — params back to neutral, timestamped.
    pub pause_trials: Vec<ProbeTrial>,
    /// Sender hold_calib trials — neutral params, flatness reference.
    pub hold_calib_trials: Vec<ProbeTrial>,
    /// Receiver hold trials — holding neutral while sender probed.
    /// Each entry carries values + observed phase from sender's lease.
    pub receiver_hold_trials: Vec<ReceiverTrial>,
    /// All complete trials (for reference / noise estimation).
    pub all_complete: Vec<TrialRecord>,
}

pub struct ProbeTrial {
    pub timestamp: f64,        // epoch seconds
    pub trial_number: i32,
    pub params: HashMap<String, f64>,
    pub values: Vec<f64>,      // objective values
    pub observations: Vec<f64>,// non-objective channels (from observations attr)
    pub impulse_scale: f64,
}

pub struct ReceiverTrial {
    pub timestamp: f64,
    pub trial_number: i32,
    pub values: Vec<f64>,
    pub observations: Vec<f64>,
    pub phase: String,         // sender's lease_phase as observed
}
```

**Classification logic** (matching observer's optuna_reader.rs):
- Push: `impulse_phase == "push"`
- Pause: `impulse_phase == "pause"`
- Hold_calib: `impulse_phase == "hold_calib"`
- Receiver hold: `detection_mode == "hold"` AND `coord_state != "hold_calib"`
- Timestamp from `datetime_start` via `parse_timestamp_secs()`

### detector.rs

Detection: does a coupling edge exist between sender and receiver on
a given channel? Abstracted behind a trait. CFAR block-step is the
first implementation, ported from observer's proven logic.

```rust
/// A detected coupling edge (or absence thereof).
#[derive(Debug, Clone, Serialize)]
pub struct DetectionResult {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,
    pub detected: bool,
    pub confidence: f64,       // fraction of rounds that detected
    pub rounds_detected: usize,
    pub rounds_total: usize,
    pub rising_edge: f64,      // push_median - baseline_median
    pub falling_edge: f64,     // push_median - pause_median
    pub baseline_median: f64,
    pub push_median: f64,
    pub pause_median: f64,
    pub baseline_mad: f64,     // noise floor at receiver
    pub n_push_samples: usize,
    pub n_pause_samples: usize,
    pub n_baseline_samples: usize,
    pub method: String,        // "cfar_block_step" etc.
    pub parameters: serde_json::Value,  // detector-specific params used
}

/// Identifies which channel (objective or observation) an edge operates on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelId {
    Objective(usize),          // index into trial.values
    Observation(String),       // name from observations attr
}

/// Trait for edge detection algorithms. CFAR is the first impl.
pub trait EdgeDetector: Send + Sync {
    fn detect(
        &self,
        sender: &ProbeTrials,
        receiver: &ProbeTrials,
    ) -> Vec<DetectionResult>;
}
```

**CFAR implementation** (`CfarDetector`), ported from observer's
`detect_watermark_coupling`:

1. Identify sender rounds: group push trials by temporal gaps
   (gap > 2× median inter-push interval = new round). Each round is a
   contiguous push block followed by a pause block.

2. Per round, per channel (objective + observation channels):
   - Reference window: receiver hold_calib trials before this round's
     push_start (pure noise baseline — receiver at neutral, sender
     also at neutral during hold_calib)
   - Push window: receiver hold trials timestamped within
     [push_start - 10s, push_end + propagation_lag]. propagation_lag
     default 20s (thermal mass delay).
   - Pause window: receiver hold trials timestamped within
     [pause_start - 10s, pause_end + propagation_lag + 30s]
   - Compute median and MAD (Median Absolute Deviation × 1.4826)
   - Dynamic CFAR threshold:
     `k = N_ref × (Pfa^(-1/N_ref) - 1)`, `threshold = k × MAD`
   - Pfa configurable, default 0.05 (95% confidence)
   - Rising edge: `push_median - baseline_median`
   - Falling edge: `push_median - pause_median`
   - Detected: BOTH edges exceed threshold (reversibility check)

3. Stack across rounds: majority of rounds must agree (rounds_detected
   >= rounds_total / 2 + 1)

4. Average edges across detected rounds for the stacked result.

```rust
pub struct CfarDetector {
    pub propagation_lag: f64,      // default 20.0 seconds
    pub min_ref_cells: usize,      // default 3
    pub min_test_cells: usize,     // default 3
    pub detection_confidence: f64, // default 0.95 (Pfa = 0.05)
}

impl EdgeDetector for CfarDetector {
    fn detect(&self, sender: &ProbeTrials, receiver: &ProbeTrials)
        -> Vec<DetectionResult>
    {
        // 1. Build rounds from sender push/pause trials
        // 2. For each channel, for each round:
        //    - Extract baseline / push / pause windows from receiver
        //    - Compute medians, MAD, CFAR threshold
        //    - Check rising + falling edges
        // 3. Stack across rounds
        // 4. Return per-channel DetectionResult
    }
}
```

The helper functions `median()`, `mad()`, `parse_timestamp_secs()` are
copied from observer's optuna_reader.rs.

### characterizer.rs

Takes a DetectionResult (edge exists) and the raw ProbeTrials (the
interventional data) and extracts a response function. This is the
core value-add of the causal image — turning "edge detected" into
"here's HOW the receiver responds."

```rust
/// A characterized coupling edge — the full response description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterizedEdge {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,

    // From detection
    pub detected: bool,
    pub confidence: f64,
    pub method: String,

    // Response characteristics (measured from intervention)
    pub response: ResponseFunction,
    pub noise_floor: f64,       // receiver MAD during baseline (exogenous noise)
    pub impulse_scale: f64,     // sender's probe amplitude

    // Raw measurements (for transparency and future fitting)
    pub rising_edge: f64,       // objective shift during push
    pub falling_edge: f64,      // objective recovery during pause
    pub baseline_median: f64,
    pub push_median: f64,
    pub pause_median: f64,

    // Sample counts
    pub n_push_samples: usize,
    pub n_pause_samples: usize,
    pub n_baseline_samples: usize,

    // Metadata
    pub characterized_at: String,  // ISO timestamp
    pub rounds_total: usize,
}
```

**ResponseFunction** — what the receiver does when the sender probes.
For v1: step response (the measured shift per unit of impulse).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseFunction {
    /// Measured step response from push/pause intervention.
    /// sensitivity = rising_edge / impulse_scale
    /// "Sender probe at scale S shifts receiver by sensitivity × S."
    StepResponse {
        sensitivity: f64,       // shift per unit impulse scale
        baseline: f64,          // receiver's baseline value
        recovery_fraction: f64, // falling_edge / rising_edge (1.0 = full recovery)
    },
    // Future variants:
    // RampResponse { curve: Vec<(f64, f64)>, settling_time: f64 }
    // PolynomialResponse { coeffs: Vec<f64> }
}

impl ResponseFunction {
    /// Predict the receiver's response to a given impulse scale.
    pub fn predict_shift(&self, impulse_scale: f64) -> f64 {
        match self {
            ResponseFunction::StepResponse { sensitivity, .. } => {
                sensitivity * impulse_scale
            }
        }
    }
}
```

**Characterization logic:**

```rust
pub fn characterize(
    detection: &DetectionResult,
    sender: &ProbeTrials,
) -> CharacterizedEdge
{
    let rising_edge = detection.rising_edge;
    let falling_edge = detection.falling_edge;
    let impulse_scale = sender.push_trials.first()
        .map(|t| t.impulse_scale).unwrap_or(1.0);

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

    // Assemble CharacterizedEdge from detection + response
}
```

**Why this is NOT OLS regression:**

The sensitivity is a DIRECTLY MEASURED quantity from controlled
intervention: "sender pushed params to upper_bound × scale, receiver
shifted by rising_edge." No statistical confounding. The sender was at
extremes, the receiver was holding still. The shift IS the coupling
response. This is Pearl Level 2 (intervention), not Level 1
(association).

### graph.rs

The causal graph — the connectome snapshot. Nodes are breeders,
directed edges carry characterized coupling.

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalNode {
    pub id: String,           // breeder UUID
    pub label: String,        // human-readable name
    pub objectives: Vec<String>,   // objective names/channels
    pub observations: Vec<String>, // observation channel names
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalGraph {
    pub nodes: Vec<CausalNode>,
    pub edges: Vec<CharacterizedEdge>,
    pub built_at: String,         // ISO timestamp
    pub detector: String,         // "cfar_block_step"
    pub detector_params: serde_json::Value,
    pub breeders_scanned: usize,
    pub pairs_evaluated: usize,   // directed pairs checked
    pub edges_detected: usize,
}

impl CausalGraph {
    /// All edges from a given node (what does this breeder affect?)
    pub fn edges_from(&self, node_id: &str) -> Vec<&CharacterizedEdge>;

    /// All edges into a given node (what affects this breeder?)
    pub fn edges_into(&self, node_id: &str) -> Vec<&CharacterizedEdge>;

    /// All edges between two nodes (both directions)
    pub fn edges_between(&self, a: &str, b: &str) -> Vec<&CharacterizedEdge>;

    /// Channels where a sender affects a receiver
    pub fn channels(&self, sender: &str, receiver: &str) -> Vec<&ChannelId>;

    /// Is the graph empty (no edges)?
    pub fn is_empty(&self) -> bool;

    /// Summary statistics
    pub fn summary(&self) -> GraphSummary;
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSummary {
    pub node_count: usize,
    pub edge_count: usize,
    pub detected_edge_count: usize,
    pub avg_confidence: f64,
    pub strongest_edge: Option<EdgeSummary>,
    pub channels_per_pair: HashMap<(String, String), usize>,
}
```

**Graph assembly:**

```rust
pub fn build_graph(
    breeders: &[String],
    reader: &TrialReader,
    detector: &dyn EdgeDetector,
) -> Result<CausalGraph>
{
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    // For each directed pair (sender, receiver):
    for sender_id in breeders {
        for receiver_id in breeders {
            if sender_id == receiver_id { continue; }

            let sender_trials = reader.read_probe_trials(sender_id).await?;
            let receiver_trials = reader.read_probe_trials(receiver_id).await?;

            // Skip if sender never probed
            if sender_trials.push_trials.is_empty() { continue; }

            // Detect edges
            let detections = detector.detect(&sender_trials, &receiver_trials);

            // Characterize detected edges
            for d in &detections {
                if d.detected {
                    let edge = characterizer::characterize(d, &sender_trials);
                    edges.push(edge);
                }
            }
        }
    }

    // Build nodes from breeder metadata
    // ...
}
```

### composer.rs

Predicts the effect of a perturbation by composing along graph edges.
For v1: direct edges only (sender → receiver per channel).

```rust
#[derive(Debug, Clone, Serialize)]
pub struct Prediction {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,
    pub impulse_scale: f64,       // hypothetical probe scale
    pub predicted_shift: f64,     // expected change in receiver's channel
    pub confidence: f64,          // edge detection confidence
    pub noise_floor: f64,         // receiver's noise level
    pub snr_estimate: f64,        // predicted_shift / noise_floor
    pub path: Vec<String>,        // [sender, receiver] for v1
}

impl CausalGraph {
    /// Predict what happens if sender probes at a given scale.
    /// v1: direct edges only.
    /// Future: multi-hop path composition (sender → X → receiver).
    pub fn predict(
        &self,
        sender_id: &str,
        impulse_scale: f64,
    ) -> Vec<Prediction>
    {
        self.edges_from(sender_id).iter()
            .filter(|e| e.detected)
            .map(|edge| {
                let shift = edge.response.predict_shift(impulse_scale);
                Prediction {
                    sender_id: sender_id.to_string(),
                    receiver_id: edge.receiver_id.clone(),
                    channel: edge.channel.clone(),
                    impulse_scale,
                    predicted_shift: shift,
                    confidence: edge.confidence,
                    noise_floor: edge.noise_floor,
                    snr_estimate: shift.abs() / edge.noise_floor.max(1e-12),
                    path: vec![sender_id.to_string(), edge.receiver_id.clone()],
                }
            })
            .collect()
    }
}
```

**v1 limit: direct edges only.** The sender probed, the receiver
responded. No intermediate hops. Multi-hop composition (the keystone —
Step 3 in the arc) is v2. It requires path enumeration through the
graph and function composition along each path.

### artifact.rs

Serialize/deserialize the CausalGraph. The artifact is the connectome
snapshot — the transferable product.

```rust
pub fn export_artifact(graph: &CausalGraph) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(graph)
}

pub fn import_artifact(json: &str) -> Result<CausalGraph, serde_json::Error> {
    serde_json::from_str(json)
}
```

**Artifact JSON format:**

```json
{
  "nodes": [
    {
      "id": "a1b2c3d4-...",
      "label": "greenhouse-1",
      "objectives": ["growth_rate", "crop_quality", "water_efficiency"],
      "observations": ["max_temp", "max_humidity"]
    }
  ],
  "edges": [
    {
      "sender_id": "a1b2c3d4-...",
      "receiver_id": "e5f6g7h8-...",
      "channel": {"Objective": 0},
      "detected": true,
      "confidence": 0.75,
      "method": "cfar_block_step",
      "response": {
        "StepResponse": {
          "sensitivity": 0.42,
          "baseline": 0.51,
          "recovery_fraction": 0.88
        }
      },
      "noise_floor": 0.034,
      "impulse_scale": 1.0,
      "rising_edge": 0.42,
      "falling_edge": 0.37,
      "baseline_median": 0.51,
      "push_median": 0.93,
      "pause_median": 0.56,
      "n_push_samples": 15,
      "n_pause_samples": 15,
      "n_baseline_samples": 5,
      "characterized_at": "2026-07-24T14:30:00Z",
      "rounds_total": 4
    }
  ],
  "built_at": "2026-07-24T14:30:00Z",
  "detector": "cfar_block_step",
  "detector_params": {"detection_confidence": 0.95, "propagation_lag": 20.0},
  "breeders_scanned": 2,
  "pairs_evaluated": 2,
  "edges_detected": 1
}
```

The artifact is self-describing. A consumer that has never seen godon
can load this JSON and understand: "node A coupling to node B on
growth_rate at sensitivity 0.42, confidence 75%, noise floor 0.034."

### query.rs

Query helpers for the HTTP API and CLI.

```rust
pub struct QueryEngine<'a> {
    graph: &'a CausalGraph,
}

impl<'a> QueryEngine<'a> {
    /// "If breeder A probes at scale S, what happens to everyone?"
    pub fn what_if(&self, sender_id: &str, impulse_scale: f64)
        -> Vec<Prediction>;

    /// "What affects breeder B, and through which channels?"
    pub fn causes_of(&self, receiver_id: &str)
        -> Vec<&CharacterizedEdge>;

    /// "What does breeder A affect?"
    pub fn impact_of(&self, sender_id: &str)
        -> Vec<&CharacterizedEdge>;

    /// Graph summary for human consumption
    pub fn summary(&self) -> GraphSummary;
}
```

### main.rs

Axum HTTP service. No CLI mode. The service is always running and
serves the cached graph from memory. The graph is built on POST /build
(externally triggered) and runs as a background tokio task.

```rust
use axum::{routing::{get, post}, Router, extract::State, Json};
use std::sync::Arc;
use tokio::sync::RwLock;

struct CausalState {
    reader: TrialReader,
    detector: Box<dyn EdgeDetector>,
    graph: RwLock<Option<CausalGraph>>,
    build_status: RwLock<BuildStatus>,
}

enum BuildStatus {
    Idle,
    Building { started_at: String },
    LastBuild { at: String, edges: usize, duration_secs: f64 },
}

// POST /build — externally triggered, runs in background
// Returns immediately with {"status": "building"}
// When done, updates the cached graph.

// GET /graph — returns cached graph (or 503 if never built)
// GET /artifact — returns downloadable JSON
// POST /predict — reads cached graph, computes prediction
// GET /impact/:id — edges from this breeder
// GET /causes/:id — edges into this breeder
// GET /summary — summary stats
```

**Service lifecycle:**
- Starts with empty graph (graph = None, status = Idle)
- POST /build spawns background task: list breeders → for each directed
  pair, load probe trials → detect edges → characterize → assemble graph
  → update cache. Returns immediately with build started confirmation.
- GET /graph returns cached graph. If no build has completed yet,
  returns 503 with {"error": "no graph built yet", "hint": "POST /build"}.
- GET /artifact returns the same graph as downloadable JSON — this is
  the connectome snapshot, transferable to other systems.
- Service restart loses the cache — next POST /build rebuilds from DB.

**No clap dependency.** Config via env vars only (same as observer):
- HOST (default 0.0.0.0)
- PORT (default 8091)
- GODON_ARCHIVE_DB_* (same as observer)
- GODON_DETECTION_CONFIDENCE (default 0.95)

## HTTP API

Using Axum. All responses are JSON.

### Real-Time Detection

```
GET  /detect/:sender_id/:receiver_id
  → reads trials for this pair, runs CFAR, returns result immediately
  → does NOT touch the graph cache
  → {"detected": true|false, "method": "cfar_block_step",
     "sender_id": "...", "receiver_id": "...",
     "push_trials": N, "pause_trials": N,
     "receiver_hold_trials": N,
     "per_objective": [{...DetectionResult...}]}
```

This endpoint is what the observer dashboard calls for real-time coupling
views. Same detector, same trial reader as the batch build.

### Health & Build Status

```
GET  /health
  → {"status": "ok", "graph_built": true|false, "db_reachable": true|false}

GET  /build/status
  → {"status": "idle"|"building", "last_build": {...}}
```

### Batch Graph Building

```
POST /build
  body: {"detection_confidence": 0.95}  (optional)
  → {"status": "building", "detection_confidence": 0.95}
  → or {"status": "already_building"} if a build is in progress
  Background task: reads all breeders, detects, characterizes,
  assembles graph.
```

### Cached Graph Endpoints

```
GET  /graph
  → full CausalGraph JSON (from cache)
  → 503 if no graph built yet

GET  /artifact
  → full artifact JSON (downloadable, transferable connectome snapshot)

POST /predict
  body: {"sender_id": "abc-123", "impulse_scale": 1.0}
  → {"predictions": [{"receiver_id": "def-456", ...}]}

GET  /impact/:breeder_id
  → all edges from this breeder (everything it affects)

GET  /causes/:breeder_id
  → all edges into this breeder (everything that affects it)

GET  /summary
  → {"node_count": 2, "edge_count": 3, "strongest_edge": {...}, ...}
```

**In-memory state:** `Arc<CausalState>` shared across handlers. The
graph lives in `RwLock<Option<CausalGraph>>`. No persistence — the DB
is the source of truth, the in-memory graph is a cache. POST /build
invalidates and rebuilds.

**Build trigger:** externally triggered by whatever orchestrates the
detection rounds — controller, observer when it sees rounds complete,
or a simple Windmill script that calls POST /build after detection
finishes. The causal service does not self-trigger.

## CI Workflows

### .github/workflows/godon-causal-ci.yml

Copy `godon-observer-ci.yml` and change:
- paths filter: `images/godon-causal/**`
- working directory: `images/godon-causal`
- build command: `cargo build`, `cargo test`, `cargo clippy`
- container port: 8091
- health check: curl `/health`

### .github/workflows/godon-causal-release.yml

Copy `godon-observer-release.yml` and change:
- trigger tag: `godon-causal-*`
- image name: `godon-causal`
- paths: `images/godon-causal/**`

Reference the shared `release-image.yml` workflow (same as other images).

## Implementation Order

1. **Cargo.toml + default.nix** — scaffold, get it building (empty main.rs)
2. **trial_reader.rs** — copy DB patterns from observer, read trials
   into `ProbeTrials`. Test: connect to a breeder DB, classify trials
   by detection_mode/impulse_phase, print counts.
3. **detector.rs** — port CFAR block-step from observer's
   `detect_watermark_coupling`. Behind `EdgeDetector` trait. Test: feed
   mock probe data with known coupling, verify detection. Test: feed
   uncoupled data, verify no detection.
4. **characterizer.rs** — extract StepResponse from DetectionResult +
   probe trials. Test: verify sensitivity = rising_edge / impulse_scale.
5. **graph.rs** — CausalGraph struct with serde. Test: build a graph
   from characterized edges, serialize to JSON, deserialize back.
6. **artifact.rs** — trivial (serde derive). Test: round-trip serialize.
7. **composer.rs + query.rs** — prediction via direct edges, what-if /
   impact / causes / summary queries. Test: verify predict_shift uses
   sensitivity correctly.
8. **main.rs** — Axum HTTP server: POST /build (background task),
   GET /graph, GET /artifact, POST /predict, GET /impact/:id,
   GET /causes/:id, GET /summary, GET /build/status. Test: start
   service, hit /health, POST /build against a test DB, GET /graph.
9. **CI workflows** — copy and adjust from observer.
10. **default.nix** — Nix container build (copy from observer).

## What NOT to Build (v1)

- Multi-hop composition (sender -> intermediate -> receiver) — direct
  edges only for v1. Add path enumeration + function composition in v2.
  This is the keystone (Step 3 in the arc) and deserves its own focused
  iteration.
- Nonlinear response functions (ramp, polynomial, spline) — StepResponse
  only for v1. The push/pause data gives a measured step, which is
  sufficient for v1 prediction.
- POST /artifact (artifact import) — export only for v1
- Dashboard HTML — the observer dashboard fetches from causal's API.
  Causal serves JSON only.
- MCP integration — the MCP server can proxy to causal's HTTP API.
- OLS regression / statistical detection — explicitly excluded. All
  edges come from interventional probe data via the detection
  coordinator protocol.
- CLI mode — service only. The artifact is exported via GET /artifact.
- Self-triggered builds — externally triggered via POST /build.
- Validator (held-out round validation) — deferred to after v1 works.
  Important but not blocking the first artifact.

## Validation Strategy

The validator answers: "do the characterized edges predict untested
interventions?"

Method:
1. Load probe trials for a sender/receiver pair
2. Split rounds into train (first 70%) and test (last 30%)
3. Run detection + characterization on train rounds only
4. For each test round, predict the receiver's shift using the train
   response function
5. Compare predicted shift to actual measured shift
6. Report per-channel: mean absolute error, mean percentage error

This validates the response function's predictive power on held-out
interventional data.

## Notes

- The observer's `optuna_reader.rs` is the primary reference for ALL DB
  access and CFAR detection. Port faithfully — the logic is proven.

- The detection coordinator protocol (push/pause/hold_calib) is the
  probing contract. The causal image reads the trial metadata it
  produces (impulse_phase, detection_mode, coord_state) but does not
  care about the coordination mechanism itself — only that the
  interventional trials exist.

- CFAR is abstracted behind `EdgeDetector`. Future detectors (learned,
  distribution-free, adaptive CFAR) implement the same trait. The
  characterization and graph layers are detector-agnostic.

- The `CharacterizedEdge` carries both the detection result AND the raw
  measurements (rising_edge, push_median, etc.). This makes the artifact
  transparent — a consumer can see exactly what was measured, not just
  the fitted function.

- No nalgebra. No petgraph (yet). The v1 graph is simple enough for
  Vec-based representation. Add dependencies only when needed.

- Connection to YugabyteDB: same SSL/TLS settings as observer (NoTls,
  same as `optuna_reader.rs`).

- The artifact is THE PRODUCT. It's what transfers between deployments.
  Keep it clean, self-describing, and forward-compatible (use serde
  with `#[serde(default)]` on optional fields).

- Use `env_logger` for logging (same as observer). Log at info level:
  which breeders scanned, how many pairs, how many edges detected,
  build time.
