# Interference Detection — Complete State Handoff

**Date:** July 3, 2026
**Session:** Extremely long multi-day session covering coordinator refactor, lease table, edge detection restoration, and multiple bench runs.

## TL;DR

The detection infrastructure works. The signal is proven (SNR 4.21 direct probe, SNR 16.76 in one live bench). The edge detection was restored after being accidentally replaced with a broken median comparison. The current blocker is that growth_rate coupling signal is too weak at 0.7 coupling and energy/water give false positives from temporal drift. Next step: run at 0.9 coupling where detection was previously proven.

---

## What's Deployed Right Now

| Component | Version | Tag Format | Key Feature |
|-----------|---------|------------|-------------|
| Breeder | 0.123.0 | bare version number | Lease table, forced turn-taking, 15-trial blocks |
| Observer | 0.63.0 | `godon-observer-X.Y.Z` | Edge detection with timestamp alignment |
| Controller | 0.49.0 | bare version number | Idempotent breeder create |
| YugaByte | 2025.2.3 | container image 2025.2.3.2 | PostgreSQL 15.12, lease table support |
| Chart | 0.31.0 | `godon-X.Y.Z` | All above |

**All changes via PRs. All commits co-author cherusk. NEVER push directly to main on any repo, especially godon-charts.**

## Key Files

- `/tmp/godon-breeders-lease/engine/detection_coordinator.py` — coordinator state machine + lease table
- `/tmp/godon-breeders-lease/engine/breeder_worker.py` — worker loop, coordinator wiring
- `/tmp/godon-images/images/godon-observer/src/optuna_reader.rs` — detection algorithm (edge detection)
- `/tmp/godon-controller-fresh/controller/breeder_service.py` — idempotent create
- `/projects/godon/.github/workflows/bench-scenario-4.yml` — bench workflow (reuses breeders, no delete+recreate)
- `/projects/godon/examples/bench/scenario-4/breeders/breeder-1.yml` — bench config with block sizes
- `/projects/godon-charts/charts/godon/` — helm chart (values.yaml, Chart.yaml)
- `/projects/godon/docs/impulse-detection-fundamental-assessment.md` — THE spec document
- `/projects/godon/docs/detection-validation-report-june19.md` — June 19 validation results
- `/projects/godon/docs/detection-architecture-pickup-notes.md` — architecture notes
- `/projects/godon/docs/coupling-detection-statistical-methods.md` — why statistical methods fail
- `/projects/godon/docs/sprt-and-interference-detection-roadmap.md` — SPRT roadmap (not implemented)
- `/projects/godon/docs/detection-coordination.md` — coordination design (hold/impulse/optimize modes)

## Connection Details

- **SSH to runner:** `ssh -o StrictHostKeyChecking=no -i /tmp/ssh_key godon@140.211.166.29`
- **SSH key:** `sudo bash -c 'cat /projects/godon/openstack' > /tmp/ssh_key && chmod 600 /tmp/ssh_key`
- **kubectl:** `kubectl --kubeconfig=/tmp/kind_kubeconfig.yaml` (file is on the runner, not local)
- **GitHub App:** APP_ID=2594394, INSTALL_ID=102627012, PEM at `/data/.hermes/github-app.pem`
- **Token:** `/tmp/venv/bin/python /tmp/get_token.py` → `/tmp/gh_token.txt`
- **Push format:** `git push https://x-access-token:$TOKEN@github.com/godon-dev/<repo>.git`
- **YugaByte:** host `yb-tservers.godon.svc.cluster.local:5433`, user `yugabyte`, db `archive_db`, password `yugabyte`
- **YugaByte auth:** Use `.pgpass` file inside yb-tserver-0 pod, NOT PGPASSWORD env var
- **Observer API:** Inside pod on port 8089: `/api/breeders`, `/api/watermark-detection/<sender>/<receiver>`
- **Detection endpoint:** `/api/watermark-detection/<sender_id>/<receiver_id>`
- **Runner restart (NixOS):** `sudo systemctl restart github-runner-gh-runner.service`
- **Loki:** Pod `godon-observability-loki-0` in namespace `godon-observability`. Only has Windmill framework logs, NOT Python application logs. Python logs are in Windmill's `job_logs` table (column `logs`, not `value`).
- **Chart bumps:** MUST go through PRs. Never `sudo git push` to main directly.

## How to Trigger a Bench

```bash
# Refresh token
/tmp/venv/bin/python /tmp/get_token.py
TOK=$(cat /tmp/gh_token.txt)

# Restack (clears YugaByte, deploys fresh)
curl -s -X POST -H "Authorization: token $TOK" \
  "https://api.github.com/repos/godon-dev/godon-charts/actions/workflows/charts--reinstall-stack.yaml/dispatches" \
  -d '{"ref":"main"}'

# Wait for restack to complete, then bench
curl -s -X POST -H "Authorization: token $TOK" \
  "https://api.github.com/repos/godon-dev/godon/actions/workflows/bench-scenario-4.yml/dispatches" \
  -d '{"ref":"main","inputs":{"min_trials":"500","max_wait_minutes":"180","coupling_factor":"0.7"}}'
```

## How to Check Results Mid-Run

```bash
# SSH to runner, get breeder IDs
ssh godon@140.211.166.29 'OBS=$(kubectl --kubeconfig=/tmp/kind_kubeconfig.yaml get pods -n godon | grep observer | awk "{print \$1}") && kubectl --kubeconfig=/tmp/kind_kubeconfig.yaml exec -n godon $OBS -- wget -qO- http://localhost:8089/api/breeders'

# Detection endpoint (replace IDs)
ssh godon@140.211.166.29 'OBS=$(kubectl ... ) && kubectl exec -n godon $OBS -- wget -qO- "http://localhost:8089/api/watermark-detection/<B1_ID>/<B2_ID>"'
```

## Checking CI Before Merging

```bash
# ALWAYS check CI status before merging a PR
for i in $(seq 1 40); do
  STATUS=$(curl -s -H "Authorization: token $TOK" \
    "https://api.github.com/repos/godon-dev/<repo>/actions/runs?per_page=1" | \
    python3 -c "import sys,json;run=json.load(sys.stdin)['workflow_runs'][0];print(run['status'],run.get('conclusion',''))")
  echo "$STATUS"
  echo "$STATUS" | grep -q completed && break
  sleep 15
done
# Only merge if conclusion is 'success'
```

## Downloading CI Failure Logs

```bash
RUN_ID=$(curl -s -H "Authorization: token $TOK" \
  "https://api.github.com/repos/godon-dev/<repo>/actions/runs?per_page=1" | \
  python3 -c "import sys,json;print(json.load(sys.stdin)['workflow_runs'][0]['id'])")

curl -s -L -H "Authorization: token $TOK" \
  "https://api.github.com/repos/godon-dev/<repo>/actions/runs/$RUN_ID/logs" -o /tmp/ci_logs.zip

# Check which step failed
curl -s -H "Authorization: token $TOK" \
  "https://api.github.com/repos/godon-dev/<repo>/actions/runs/$RUN_ID/jobs?per_page=1" | \
  python3 -c "
import sys,json
job = json.load(sys.stdin)['jobs'][0]
for s in job.get('steps', []):
    c = s.get('conclusion','')
    if c == 'failure': print('FAILED:', s['name'])
"
```

---

## The Detection Architecture

### Coordinator State Machine (breeder 0.123.0)

States:
```
WARMUP → SENDER_PUSH → SENDER_PAUSE → SENDER_DONE → RECEIVER_BASELINE → RECEIVER_HOLD → RECEIVER_POST → (swap)
```

- **WARMUP:** 15 COMPLETE trials of free optimization. Not interrupted by stale rounds.
- **SENDER_PUSH:** 15 trials of extreme params (upper bounds on top-3 params by range)
- **SENDER_PAUSE:** 15 trials of baseline params (best warmup trial)
- **SENDER_DONE:** Release lease, ALWAYS become RECEIVER_BASELINE (forced turn-taking)
- **RECEIVER_BASELINE:** 5 trials of hold before sender starts (clean baseline)
- **RECEIVER_HOLD:** Hold during sender's push+pause (signal window)
- **RECEIVER_POST:** 5 trials of hold after sender finishes (post baseline)

No RECOVER phase — transitions are direct between roles.

### Coordination: Fencing Token Lease Table

Replaces advisory locks (which broke when YugaByte closed idle connections).

```sql
CREATE TABLE sender_lease (
    id INT PRIMARY KEY DEFAULT 1,
    holder VARCHAR(255),
    token INT DEFAULT 0,
    expires_at TIMESTAMPTZ,
    CHECK (id = 1)
);
```

- **Acquire:** `UPDATE sender_lease SET holder=$id, token=token+1, expires_at=NOW()+INTERVAL '90 seconds' WHERE id=1 AND (holder IS NULL OR expires_at < NOW())`
- **Heartbeat:** `UPDATE sender_lease SET expires_at=NOW()+INTERVAL '90 seconds' WHERE id=1 AND holder=$id AND token=$my_token` (every trial while sender)
- **Release:** `UPDATE sender_lease SET holder=NULL WHERE id=1 AND holder=$id AND token=$my_token`
- **Check active sender:** `SELECT count(*) FROM sender_lease WHERE id=1 AND holder IS NOT NULL AND expires_at > NOW()`

Each operation is a standard SQL UPDATE — no persistent connection needed. Fencing token prevents stale senders. Lease expires after 90s if sender crashes.

**IMPORTANT:** The INTERVAL in SQL uses string concatenation, NOT `%d` format. psycopg2 interprets `%d` as a parameter placeholder and silently fails. Use `"INTERVAL '" + str(seconds) + " seconds'"`.

### Edge Detection (observer 0.63.0)

Three windows based on sender timestamps:
- **baseline:** receiver trials before push_start (with -30s buffer)
- **push:** receiver trials during push block (push_start to push_end + 20s lag)
- **pause:** receiver trials during pause block (pause_start to pause_end + 20s lag)

Detection:
```rust
rising_edge = push_median - baseline_median  // step when push starts
falling_edge = push_median - pause_median    // recovery when push ends
rising_snr = |rising_edge| / MAD(baseline)
falling_snr = |falling_edge| / MAD(baseline)
rising_detected = rising_snr >= 1.5
falling_detected = falling_snr >= 0.5
detected = rising_detected AND falling_detected
```

**When hold_phase tags exist** (they do in current breeder): receiver signal trials are split into push/pause by their TIMESTAMP relative to sender's push/pause windows, NOT by trial number (sender and receiver have independent trial sequences).

MAD floor: 0.01 (prevents billion-SNR with tiny baseline variance).

### Block Design Rationale

From `impulse-detection-fundamental-assessment.md`:

The greenhouse thermal mass is a low-pass filter. Single-trial impulses (ping/listen) get smeared to DC. Need sustained block excitation (AB design):

```
Block A (baseline):    15+ trials at baseline params
Block B (intervention): 15+ trials at extreme params
Block A' (recovery):    15+ trials at baseline params (ABA reversal)
```

With per-sample SNR = 0.67:
- N=5  → SNR = 1.5 (below threshold)
- N=15 → SNR = 2.6 (borderline)
- N=20 → SNR = 3.0 (detectable)

---

## What's Proven

### Direct Probe (SNR 4.21)
Controlled experiment: applied extreme params to GH1, held GH2 at moderate params (heating=20, vent=0.5, light=300), measured GH2's response.
- Baseline mean: 0.454 (std 0.015)
- Impulse mean: 0.389
- Shift: -0.065
- SNR: 4.21 with 10 stacked samples

### June 19 Validation (coupling 0.9)
Edge detection worked at coupling 0.9:
- Growth_rate rising edge detected
- Uncoupled (0.0) correctly showed no edge on growth_rate
- Energy/water gave FALSE POSITIVES from temporal drift (only growth_rate is reliable)
- Validation report at `/projects/godon/docs/detection-validation-report-june19.md`

### July 1 Bench (coupling 0.5)
B2→B1 detected with SNR 16.76 on growth_rate (one run). Shift +0.168.

### July 3 Bench (coupling 0.7, latest)
Edge detection working with proper timestamp alignment:
- B2→B1: detected=true on ENERGY (SNR 5.05) — FALSE POSITIVE from drift
- Growth_rate: rising_edge -0.030 (SNR 1.14) — too weak at 0.7
- B1→B2: no impulse trials (B1 never sent — turn-taking bias persists)
- Both breeders sent AND received (turn-taking partially working)

---

## Known Issues

### 1. Turn-Taking Bias
B2 sends more rounds than B1. SENDER_DONE always becomes RECEIVER_BASELINE, but B2 seems to acquire the lease first and more often. Need to investigate why B1 doesn't acquire when B2 releases.

### 2. Duplicate Workers (Recurring)
Multiple code paths create duplicate breeder instances:
- Controller retries create_breeder on API timeout
- Bench workflow retries on CLI error
- Controller idempotent check + workflow reuse fix deployed but still recurs
- The fix was to NOT delete+recreate in the bench workflow — just reuse existing breeders by name
- Still happens on stale stack state — always restack between runs

### 3. Duplicate Trial Numbers (Optuna + YugaByte)
Optuna's RDBStorage creates duplicate trial numbers on YugaByte 2025.2.3. Not caused by duplicate workers — happens with single worker too. 6-13 duplicate trial numbers per breeder consistently.

### 4. Energy/Water False Positives
Energy and water objectives increase monotonically over time (temporal drift). The edge detector sees this as a "step." Only growth_rate is coupling-sensitive. The June 19 report documents this. Fix: filter to growth_rate only, or add drift regressors (GLM approach from fundamental assessment).

### 5. Receiver Fights Coupling
The receiver holds with optimizer-best params (heating=31, vent=1.0, light=230). These aggressively control the greenhouse climate. When coupling pushes heat from sender, the receiver's own climate system compensates. The direct probe used moderate params (heating=20) and the signal came through.

### 6. Dashboard Push Color Still Red
Observer binary has `#da3633` (red) for push trials. Fix to orange `#f0883e` was made to dashboard.html but never rebuilt into the observer image.

### 7. Python Logs Not in Loki
Windmill captures Python stdout into internal `job_logs` table. Controller logs DO reach Loki (short scripts flush). Breeder logs do NOT. Query `job_logs` table in Windmill DB:
```sql
SELECT l.logs FROM job_logs l JOIN v2_job j ON l.job_id = j.id 
WHERE j.runnable_path = 'f/breeder/engine/breeder_worker' AND l.logs LIKE '%lease%'
```

---

## What Needs to Happen Next

### Option A: Run at 0.9 Coupling (Fastest Path to Detection)
The coordinator now handles FAILs (push counter not reset, escape hatches, AIMD). June 19 proved detection works at 0.9. The FAIL rate will be high but enough trials should survive for detection.

```bash
curl -s -X POST -H "Authorization: token $TOK" \
  "https://api.github.com/repos/godon-dev/godon/actions/workflows/bench-scenario-4.yml/dispatches" \
  -d '{"ref":"main","inputs":{"min_trials":"500","max_wait_minutes":"180","coupling_factor":"0.9"}}'
```

### Option B: Neutral Hold Params (Biggest Impact on Signal Quality)
Change the receiver's hold params from optimizer-best to neutral/minimal intervention. The greenhouse becomes a passive sensor instead of an active controller. This requires a code change in the coordinator — `_baseline_params` should be neutral defaults, not the best trial's params.

### Option C: Filter to Growth Rate Only
In the observer detection, only check objective_index=0 (growth_rate). Ignore energy and water. This prevents false positives from temporal drift. One-line change in the detection loop.

### Option D: Fix Turn-Taking
Investigate why B2 acquires the lease more than B1. The SENDER_DONE→RECEIVER_BASELINE transition should give B1 a chance. But B1 might be stuck in RECEIVER_HOLD when B2 releases — the `_has_active_sender()` check might not see the release fast enough.

---

## Lessons Learned This Session

1. **Never replace working code without understanding why it works.** The edge detection from June 19 was replaced with a median comparison that was strictly worse.

2. **Check CI before merging.** Multiple times I merged PRs with failing CI, then discovered the failure from the image build step.

3. **Don't match by trial number across independent sequences.** Sender and receiver have independent trial numbers. Use timestamps.

4. **Don't declare success on partial data.** Check the full trial sequence, verify sample counts in all windows, confirm the detection format is correct before reporting.

5. **Read the existing docs.** The fundamental assessment document diagnosed every problem we hit. It should have been the first thing read each session.

6. **All repos need PRs.** Including godon-charts. Never `sudo git push` to main.

7. **psycopg2 `%d` is a parameter placeholder.** Use string concatenation for SQL INTERVAL clauses.

8. **Restack between bench runs.** Stale breeders and detection_rounds persist across runs.
