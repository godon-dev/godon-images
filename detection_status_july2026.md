# Interference Detection — Status and Analysis

**Date:** July 2, 2026
**Author:** Hermes Agent + Matthias (cherusk)

## What We're Building

A generic interference detection system for godon. Two coupled breeders
optimizing the same type of target. One sends a controlled perturbation
(impulse). The other holds still and measures the coupling echo. The
method is inspired by active sonar, seismic stacking, and radar detection
— proven across many domains.

The goal: detect UNKNOWN coupling between systems by active perturbation,
without prior knowledge of which parameters carry the coupling.

## What's Proven

### Direct Probe (SNR 4.21 at coupling 0.5)
Controlled experiment on the greenhouse bench: applied extreme params
to GH1, held GH2 at moderate params (heating=20, vent=0.5, light=300),
measured GH2's response. Clear signal:

- Baseline mean: 0.454 (std 0.015)
- Impulse mean: 0.389
- Shift: -0.065 (growth drops during sender impulse)
- SNR: 4.21 with 10 stacked samples

The coupling IS real and detectable at the temperature level.

### Detection at coupling 0.9 (live bench, June 20-21)
B2→B1 detected with SNR 3.84, then 5.10, then 16.76 across different
runs. The coordinator was still buggy (scattered trials, duplicate
workers) but the signal was strong enough to emerge.

### Detection at coupling 0.5 (live bench, July 1)
B2→B1 detected with SNR 16.76 on growth_rate (one run only). Shift
+0.168 (baseline 0.627 → signal 0.794). 22 signal vs 3 baseline samples.

## What's Working (Infrastructure)

### Coordinator (breeder 0.122.0)
The DetectionCoordinator state machine:
- WARMUP → SENDER_PUSH → SENDER_PAUSE → SENDER_DONE → RECEIVER_BASELINE → RECEIVER_HOLD → RECEIVER_POST → swap
- Force turn-taking: sender ALWAYS yields to receiver after round
- Push counter NOT reset on guardrail FAIL (AIMD handles separately)
- RECOVER phase removed (no free trials between detection rounds)
- Escape hatches on every state (MAX_PUSH_ATTEMPTS, MAX_HOLD_TRIALS, etc.)
- Warmup not interrupted by stale rounds

### Coordination (fencing token lease table)
- Replaced advisory locks (which broke when YugaByte closed idle connections)
- Each operation is a standard SQL UPDATE on a singleton row
- Fencing token prevents stale senders from corrupting state
- Lease expires after 90s if sender crashes (self-healing)
- Based on Martin Kleppmann's fencing token pattern
- No persistent connection needed

### Observer (0.60.0)
- hold_phase tags: baseline/signal/post on receiver trials
- Detection compares signal-hold vs baseline+post-hold values
- Falls back to timestamp windows if hold_phase not present
- MAD floor 0.01 to prevent SNR explosions

### Controller (0.49.0)
- Idempotent breeder create: returns existing breeder if name matches

### Bench Workflow
- Reuses existing breeders instead of delete+recreate
- 500 trial target, 180 minute timeout

### Stack
- YugaByte 2025.2.3 (PostgreSQL 15.12, advisory locks work but we use
  lease table now)
- 10 breeder worker pods, 1 logical worker per breeder
- Windmill orchestrates scripts, stores Python logs internally (NOT in Loki)

## What's NOT Working

### Core Problem: Receiver Fights the Coupling
The receiver holds with the OPTIMIZER'S BEST params — aggressive
settings that actively control the greenhouse. When coupling pushes heat
from the sender into the receiver, the receiver's own climate control
(heating, ventilation, shading) COMPENSATES for the perturbation.

Evidence from July 2 bench (coupling 0.7):
- B1 receiver during B2 push: growth locked at 0.82-0.83, zero response
- B2 receiver during B1 push: growth locked at 0.26-0.30, zero response
- Sender has clear swing (0.37→0.82) but receiver doesn't react

The direct probe worked because it used MODERATE receiver params
(heating=20, vent=0.5, light=300). The greenhouse wasn't fighting.
In the bench, the optimizer-best hold params actively resist coupling.

### Turn-Taking Bias (Fixed, but needs verification)
Before breeder 0.122.0: one breeder monopolized sender role.
Fix: SENDER_DONE always transitions to RECEIVER_BASELINE, never tries
to re-acquire the lease. Needs verification with clean 2-breeder bench.

### Duplicate Workers (Recurring)
Multiple code paths create duplicate breeder instances:
- Controller retries create_breeder on API timeout
- Bench workflow retries on CLI error
- Controller idempotent check + workflow reuse fix deployed
- Still recurs on stale stack state (needs clean restack between runs)
- DB-level unique constraint on name is NOT the right fix (names aren't unique by design)

### Duplicate Trial Numbers (Optuna + YugaByte)
Optuna's RDBStorage creates duplicate trial numbers on YugaByte 2025.2.3.
Not caused by duplicate workers — happens with single worker too.
6-13 duplicate trial numbers per breeder consistently.
This is an Optuna/YugaByte compatibility issue, not a coordinator bug.

### Dashboard Color
Observer binary still has red (#da3633) for push trials. Color fix to
orange (#f0883e) was made to dashboard.html but never rebuilt into the
observer image.

### Breeder Application Logs Not in Loki
Windmill captures Python process stdout into internal job_logs table,
not the container stdout. Alloy only picks up Windmill framework logs.
Controller logs DO reach Loki (short scripts complete and flush).

## Coupling Level Sweet Spot

| Coupling | FAIL Rate | Signal Strength | Detection | Notes |
|----------|-----------|-----------------|-----------|-------|
| 0.1 | Low | Too weak | No | SNR ~0.5 |
| 0.5 | Low (~10%) | Weak | One run yes | SNR 0.2-16.8 (variable) |
| 0.7 | Low (~12%) | Medium | No | SNR 0.8-1.2, receiver absorbs |
| 0.9 | High (~90%) | Strong | Yes (when data survives) | Receiver destroyed |

0.9 has the signal but kills the receiver. 0.5/0.7 survive but signal is weak.
0.7 is the sweet spot for survivability — needs the receiver to stop fighting.

## Three Levers to Fix Detection

### Lever 1: Neutral Hold Params (Biggest Impact)
Instead of optimizer-best params, receiver holds with NEUTRAL/minimal
intervention params. The greenhouse becomes a passive sensor:

Current (broken):
- Hold params = best trial: heating=31, vent=1.0, light=230 (aggressive control)
- Receiver's climate system compensates for coupling → no signal

Proposed:
- Hold params = neutral: heating=15, vent=0.3, light=100 (passive observation)
- Receiver lets coupling pass through → signal visible

The challenge: we can't hardcode neutral params (domain-specific). Need to
derive them — either midpoint of constraints, or params that minimize the
control effort (closest to "doing nothing").

### Lever 2: Multi-Signal Detection
The coupling hits temperature, CO2, humidity BEFORE affecting growth_rate.
We measure guardrails (max_temp, max_humidity, max_co2) every trial but
only use them for safety. The observer should also check these for coupling
shifts:

- growth_rate: smoothed biological response, coupling barely visible
- max_temp: direct physical quantity, coupling visible immediately
- max_co2: direct physical quantity, coupling visible immediately
- max_humidity: direct physical quantity, coupling visible immediately

### Lever 3: CFAR Adaptive Threshold
Current: fixed 2.5 MAD threshold. SNR 1.17 not detected despite signal
being present. The noise floor varies across objectives and coupling levels.

CFAR: compute local noise floor from surrounding baseline trials. Set
threshold at noise_floor * (1 + alpha) where alpha is the false alarm
probability. Adapts to actual noise conditions.

## Roadmap (Priority Order)

1. **Neutral hold params** — fix receiver's fight against coupling
2. **Verify clean turn-taking** — run 2-breeder bench, confirm alternation
3. **Multi-signal detection** — check guardrail values, not just objectives
4. **CFAR threshold** — replace fixed 2.5 MAD
5. **EMD preprocessing** — separate nonstationary drift from coupling signal
6. **Alternating push direction** — upper bounds one round, lower bounds next
7. **Fix duplicate trial numbers** — Optuna/YugaByte 2025.2.3 compatibility

## Deployed Versions (as of July 2)

| Component | Version | Key Feature |
|-----------|---------|-------------|
| Breeder | 0.122.0 | Forced turn-taking, lease table, no RECOVER |
| Controller | 0.49.0 | Idempotent create |
| Observer | 0.60.0 | hold_phase detection |
| YugaByte | 2025.2.3 | PostgreSQL 15.12, lease table |
| Chart | 0.26.0 | All above |

## Key Files

- `/tmp/godon-breeders-lease/engine/detection_coordinator.py` — state machine + lease table
- `/tmp/godon-breeders-lease/engine/breeder_worker.py` — worker loop, coordinator wiring
- `/tmp/godon-images/images/godon-observer/src/optuna_reader.rs` — detection algorithm
- `/tmp/godon-controller-fresh/controller/breeder_service.py` — idempotent create
- `/projects/godon/.github/workflows/bench-scenario-4.yml` — bench workflow
- `/projects/godon-charts/charts/godon/` — helm chart
