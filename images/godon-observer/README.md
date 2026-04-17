# godon-observer

Optimization observability: Prometheus metrics proxy, trial history from Optuna storage, visualization dashboard.

## Endpoints

| Route | Description |
|-------|-------------|
| `GET /metrics` | Prometheus metrics from Push Gateway |
| `GET /health` | Health check (`DEGRADED` if DB unreachable) |
| `GET /dashboard` | Interactive visualization (heatmap, spider web, parallel coordinates) |
| `GET /api/breeders/<uuid>/trials/<study>?offset=0&limit=100` | Paginated trial history |
| `GET /api/breeders/<uuid>/studies` | List Optuna studies for a breeder |
| `GET /api/breeders/<uuid>/summary` | Trial count, directions, study attributes |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `8089` | HTTP port |
| `PUSH_GATEWAY_URL` | `http://pushgateway:9091` | Prometheus Push Gateway |
| `GODON_ARCHIVE_DB_USER` | `yugabyte` | YugabyteDB user |
| `GODON_ARCHIVE_DB_PASSWORD` | `yugabyte` | YugabyteDB password |
| `GODON_ARCHIVE_DB_SERVICE_HOST` | `yb-tserver-0` | YugabyteDB host |
| `GODON_ARCHIVE_DB_SERVICE_PORT` | `5433` | YugabyteDB port |

## Data Sources

- **Push Gateway** → `/metrics` (live aggregates: trial counts, best values, durations)
- **YugabyteDB** (Optuna schema) → trial API (per-trial parameters, values, state, timing)

The OptunaReader (`src/optuna_reader.rs`) reads from Optuna's stable RDB tables (`trials`, `trial_params`, `trial_values`, `study_directions`, `study_user_attributes`). Pure Rust, no Python.

## Replaces

Supersedes `godon-metrics-exporter`. Push Gateway proxy preserved in `/metrics`.
