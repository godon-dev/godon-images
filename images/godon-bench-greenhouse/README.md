# godon-bench-greenhouse

A multi-zone greenhouse simulation for verifying the godon optimization engine.

## What It Does

Simulates a greenhouse with 2-6 climate zones. Each zone tracks temperature, humidity, CO2 level, and plant growth rate. The optimizer (godon breeder) sends parameter sets via HTTP, the simulator runs the physics forward, and returns measurable outcomes (growth rate, energy consumed, water used).

The optimizer's job: find heating, ventilation, shading, CO2 injection, lighting, and irrigation settings that maximize plant growth while minimizing resource usage -- without violating safety guardrails (temperature extremes, disease-risk humidity, CO2 limits).

## Why This Exists

The godon engine needs a **target to optimize** that is:

- **Intuitive**: anyone can reason about "warmer = more growth, but too hot = bad"
- **Non-trivial**: parameters interact (ventilation cools but loses CO2, shading reduces light and heat)
- **Multi-objective**: growth vs energy vs water -- genuine tradeoffs
- **Dynamic**: weather drifts over time, forcing continuous adaptation
- **Guardrail-friendly**: temperature, humidity, and CO2 extremes trigger rollback
- **Composable**: more zones = more breeders that must cooperate through shared walls

This is not a production greenhouse control system. It is a verification bench.

## Parameters

The optimizer sends these via `POST /apply`:

| Parameter | Type | Range | Per-Zone | Effect |
|-----------|------|-------|----------|--------|
| `heating_setpoints` | float[] | 5-40 °C | Yes | Target temperature per zone. Heating system drives toward it. |
| `vent_openings` | float[] | 0.0-1.0 | Yes | Ventilation opening. Cools the zone but loses CO2 and humidity. |
| `shading` | float | 0.0-1.0 | No | Shade cloth position. Reduces solar heat gain AND light for photosynthesis. |
| `co2_injection` | float | 0-20 | No | CO2 enrichment rate. Boosts photosynthesis but lost through ventilation. |
| `light_intensity` | float | 0-1000 | No | Supplemental grow light intensity. Adds to solar radiation. |
| `irrigation` | float | 0-3.0 | No | Watering rate. Drives transpiration and growth. Too much = overwatering penalty. |
| `sim_steps` | int | 1-1000 | No | Simulation timesteps to run. Each = 0.1 simulated hours. Default 60. |

For `heating_setpoints` and `vent_openings`, pass either one value per zone or a single value (broadcast to all zones).

## Metrics (Objectives and Guardrails)

The simulator returns these metrics:

### Objectives (optimize these)

| Metric | Direction | Description |
|--------|-----------|-------------|
| `growth_rate` | **Maximize** | Average plant growth rate across all zones. Product of temperature, light, CO2, water, and humidity factors. |
| `trial_energy_kwh` | **Minimize** | Cumulative energy from heating, ventilation fans, and grow lights. |
| `trial_water_liters` | **Minimize** | Cumulative water consumed through irrigation. |

### Guardrails (safety limits)

| Metric | Hard Limit | Why |
|--------|-----------|-----|
| `max_temp` | 40 °C | Above this, plants die (growth rate = 0) |
| `min_temp` | 5 °C | Below this, plants die (growth rate = 0) |
| `max_humidity` | 0.9 | Above 90%, disease risk (growth penalty) |
| `max_co2` | 1500 ppm | Wasteful, diminishing returns above 1200 ppm |

### Zone-level metrics

Per-zone gauges are exposed for detailed monitoring:
- `greenhouse_zone_{N}_temp_celsius`
- `greenhouse_zone_{N}_humidity_ratio`
- `greenhouse_zone_{N}_co2_ppm`
- `greenhouse_zone_{N}_growth_rate`

## Physics Model

Each simulation timestep (dt = 0.1 hours) updates each zone:

### Temperature

```
temp += dt * (solar_gain + wall_transfer - heat_loss_outside - vent_cooling + heating_power - cooling_power)
```

- **Solar gain**: `solar_radiation * (1 - shading) * 0.02` -- reduced by shading
- **Wall transfer**: Newton's law of cooling through shared walls between adjacent zones
- **Heat loss outside**: `(zone_temp - outside_temp) * 0.1` -- insulation factor
- **Ventilation cooling**: `vent * (zone_temp - outside_temp) * 0.3` -- exchanges inside air with outside
- **Heating power**: proportional controller toward setpoint
- **Cooling power**: activates when zone exceeds setpoint by 2°C deadband

### Humidity

```
humidity += dt * (transpiration - vent_drying)
```

- **Transpiration**: plants release water vapor proportional to irrigation and temperature
- **Vent drying**: ventilation removes humidity by exchanging with drier outside air

### CO2

```
co2 += dt * (injection - vent_loss - plant_uptake)
```

- **Injection**: directly from the `co2_injection` parameter
- **Vent loss**: proportional to vent opening and CO2 concentration difference with ambient (420 ppm)
- **Plant uptake**: proportional to current growth rate

### Growth Rate

```
growth = temp_factor * light_factor * co2_factor * water_factor * humidity_factor
```

Multiplicative model where each factor is in [0, 1]:

| Factor | 0 (dead) | Ramping up | Optimal | Ramping down |
|--------|----------|------------|---------|--------------|
| Temperature | <5°C or >40°C | 5-15°C | 15-30°C | 30-40°C |
| Light | 0 W/m² | 50-200 W/m² | 200-600 W/m² | >600 W/m² (photoinhibition) |
| CO2 | <200 ppm | 200-800 ppm | 800-1200 ppm | >1200 ppm (diminishing) |
| Water | 0 | 0.1-1.0 | 1.0-2.0 | >2.0 (overwatering) |
| Humidity | <0.2 (drought) | -- | 0.2-0.9 | >0.9 (disease) |

The multiplicative model means **any single bad factor kills growth**. This creates the non-linearity that stresses the optimizer.

### Weather Drift

Outside temperature and solar radiation drift over time. The behavior depends on the weather mode (see below).

### Weather Modes

Set via `GREENHOUSE_WEATHER` environment variable. Seed via `GREENHOUSE_SEED` (default: 42).

| Mode | Env Value | Behavior | Use Case |
|------|-----------|----------|----------|
| **Smooth** | `smooth` (default) | Deterministic Lissajous oscillations. Same tick = same weather. | Verify basic convergence |
| **Noisy** | `noisy` | Smooth drift + gaussian noise on weather (±1.5°C, ±30 W/m²) and sensor readings (±0.3°C, ±15ppm CO2). | Test robustness to measurement uncertainty |
| **Adversarial** | `adversarial` | Smooth drift + noise + random shocks. ~2% chance per tick of a ±10-15°C temperature swing or ±200 W/m² radiation change. Shocks can push zones into guardrail territory even with reasonable parameters. | Stress-test guardrails, rollback, and re-convergence |

The seeded RNG (ChaCha8) ensures reproducibility: same seed + same steps = identical results. Reset preserves the seed.

#### Smooth mode weather

```
outside_temp = 10 + 8*sin(t*0.1) + 3*cos(t*0.37)    # roughly 2-18°C
solar_radiation = max(0, 300 + 200*sin(t*0.05) + 50*cos(t*0.23))  # roughly 50-550 W/m²
```

#### Noisy mode

Adds gaussian perturbations to both weather generation and returned metrics:

| Metric | Noise amplitude |
|--------|----------------|
| Outside temperature | ±1.5 °C |
| Solar radiation | ±30 W/m² |
| Zone temperature (reported) | ±0.3 °C |
| Zone humidity (reported) | ±0.02 |
| Zone CO2 (reported) | ±15 ppm |
| Growth rate (reported) | ±0.02 |
| Energy (reported) | ±0.01 kWh |

#### Adversarial mode

All noise from noisy mode, plus random shocks with ~2% probability per tick:

| Shock type | Magnitude |
|------------|-----------|
| Temperature | ±10-15 °C |
| Solar radiation | ±200 W/m² |

These simulate cold snaps, heat waves, and cloud bursts. A shock at tick 200 might drop outside temperature from 15°C to -2°C while the optimizer was configured for warm weather, triggering guardrails and forcing rollback.

## Scenarios

Set via `GREENHOUSE_SCENARIO` environment variable:

| Scenario | Zones | Breeders | Purpose |
|----------|-------|----------|---------|
| `simple` (default) | 2 | 1 | Verify basic engine convergence |
| `medium` | 4 | 2-3 | Test cooperation (zones share walls) |
| `complex` | 6 | 4+ | Stress test with multiple interacting subsystems |

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/apply` | POST | Apply parameter set, run simulation, return metrics (JSON) |
| `/metrics` | GET | Current state in Prometheus exposition format |
| `/metrics/json` | GET | Current state as JSON |
| `/status` | GET | Full state including applied parameters |
| `/health` | GET | Liveness check (`{"status":"ok","zones":2,"tick":0}`) |
| `/reset` | POST | Reset greenhouse to initial conditions |

### Example: Apply parameters

```bash
curl -X POST http://localhost:8090/apply \
  -H 'Content-Type: application/json' \
  -d '{
    "heating_setpoints": [22, 20],
    "vent_openings": [0.3, 0.2],
    "shading": 0.1,
    "co2_injection": 5.0,
    "light_intensity": 100,
    "irrigation": 0.5,
    "sim_steps": 60
  }'
```

Response:
```json
{
  "zone_temps": [24.5, 22.1],
  "zone_humidities": [0.35, 0.42],
  "zone_co2_levels": [445.0, 430.0],
  "zone_growth_rates": [0.72, 0.68],
  "growth_rate": 0.70,
  "trial_energy_kwh": 0.45,
  "trial_water_liters": 0.30,
  "max_temp": 24.5,
  "min_temp": 22.1,
  "max_humidity": 0.42,
  "max_co2": 445.0,
  "outside_temp": 13.4,
  "solar_radiation": 355.0,
  "tick": 60
}
```

### Example: Prometheus metrics

```bash
curl http://localhost:8090/metrics
```

```
# HELP greenhouse_growth_rate Average growth rate across all zones
# TYPE greenhouse_growth_rate gauge
greenhouse_growth_rate 0.70
greenhouse_energy_kwh 0.45
greenhouse_zone_0_temp_celsius 24.5
greenhouse_zone_1_temp_celsius 22.1
...
```

## Integration with Godon

This bench plugs into the existing godon stack:

| Godon Component | Bench Integration |
|----------------|-------------------|
| **Effectuator** | HTTP effectuator (`effectuation/http.py`) calls `POST /apply` with the trial's suggested parameters |
| **Reconnaissance** | Prometheus reconnaissance (`reconnaissance/prometheus.py`) scrapes `GET /metrics` |
| **Strain** | A new `greenhouse` strain defines parameter ranges (see below) |
| **Guardrails** | `max_temp > 40`, `max_humidity > 0.9`, `max_co2 > 1500` |
| **Rollback** | Restore previous parameters on guardrail violation |

### Suggested strain parameters

For a `greenhouse` strain, the search space would be:

```yaml
settings:
  greenhouse:
    heating_setpoints:
      constraints:
        - {step: 1.0, lower: 10, upper: 35}    # Per zone or broadcast
    vent_openings:
      constraints:
        - {step: 0.05, lower: 0.0, upper: 1.0}  # Per zone or broadcast
    shading:
      constraints:
        - {step: 0.05, lower: 0.0, upper: 0.8}
    co2_injection:
      constraints:
        - {step: 0.5, lower: 0.0, upper: 15.0}
    light_intensity:
      constraints:
        - {step: 10, lower: 0, upper: 500}
    irrigation:
      constraints:
        - {step: 0.05, lower: 0.1, upper: 2.0}
```

## Building

Uses the shared Nix build system:

```bash
cd images/godon-bench-greenhouse
PROJECT_ROOT=../.. ../../build/build-container-nix.sh \
  --version <version> \
  --name godon-bench-greenhouse \
  --builder-file ../../build/Dockerfile.nix-builder
```

## Running

```bash
docker run -p 8090:8090 ghcr.io/godon-dev/godon-bench-greenhouse

# With weather mode and seed:
docker run -p 8090:8090 \
  -e GREENHOUSE_SCENARIO=complex \
  -e GREENHOUSE_WEATHER=adversarial \
  -e GREENHOUSE_SEED=42 \
  ghcr.io/godon-dev/godon-bench-greenhouse
```

## CI/CD

- **CI**: `.github/workflows/godon-bench-greenhouse-ci.yml` -- builds and tests all endpoints on PR
- **Release**: Push tag `godon-bench-greenhouse-X.Y.Z` to trigger `godon-bench-greenhouse-release.yml`
