# godon observer dashboard — future vision

## current state (0.2.0)

single-breeder dashboard with:
- heatmap: rows = objectives + parameters, columns = trials, color = quality
- spider web: axes = objectives + parameters, trials colored by quality
- parallel coordinates: lines colored by trial quality
- top info bar: objective best values + guardrail status
- auto-discovery of studies via optuna reader
- guardrail threshold visualization
- api proxy to godon-api for breeder config (objectives, guardrails)

## next evolution: advisory dashboard

### principle: advise, don't just visualize

every other tuning tool shows data and makes you interpret it. godon should
compute and recommend. the dashboard answers "what do I do?" — not "here's data."

### three layers

1. **glance** — one visual anchor, no reading required
2. **explore** — interactive charts for investigation
3. **automate** — /diagnosis API for programmatic consumption

### single-breeder view

#### radial health (glance layer)

center: convergence state orb. color/size = how converged.
spokes: one per parameter/objective. color = health (boundary proximity,
sensitivity, guardrail status).

glance result: green orb + one yellow spoke = "mostly fine, check this."

example:
```
         latency ── ● green
                    \
    throughput ── ● green  \
                             ◉  BREEDER HEALTH
              cpu ── ● yellow /    78%
                          /
       buffer ── ● green

         memory ── ● green   guardrails: ○ ○ ○ ● ○
```

#### convergence engine

not a chart — a computed metric:
- fit exponential decay to running-best curve
- convergence % = how close to projected asymptote
- projected remaining improvement = asymptote - current best
- verdict: KEEP RUNNING / CONVERGING / CONVERGED / STUCK

#### boundary detection

flag parameters whose optimum sits at search space boundary:
- "cpu_count optimum at upper bound (8) → expand to 16?"
- actionable: tells you the search space is too narrow

#### parameter sensitivity

rank parameters by correlation with objective improvement:
- high influence → keep, tune carefully
- low influence → consider dropping to reduce dimensionality

#### guardrail effectiveness

for each guardrail:
- how many trials it blocked
- if never triggered → "loosen or remove?"
- if blocking too many → "tighten search space instead?"

### diagnosis API

endpoint: /api/breeders/<uuid>/diagnosis

returns JSON with:
- convergence state + percentage
- boundary hits
- parameter sensitivity ranking
- guardrail effectiveness
- recommendations list

consumed by: CLI, notifications, other tools

## multi-breeder view (future)

### why

even without cross-boundary optimization, operators need to see:
- are two breeders converging on the same optimum? (duplicate effort)
- which breeder deserves more budget? (resource allocation)
- are breeders interfering? (coupling hints)

### correlation view (passive)

shared wall-clock timeline, stacked objective values:

```
abc-123  ▂▃▅▆▇▆▅▃▂▂▂▂▂▃▅▆▇███
def-456  ▃▅▆▇██████████▇▆▅▅▅▅
ghi-789  ▂▂▃┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄
```

flags suspicious temporal overlaps. marks as "investigate" — NOT as
confirmed coupling. correlation is a hypothesis generator, not evidence.

### probe view (active / causal)

shows results of intentional perturbation experiments:

```
probe: abc-123 trial #47 (cpu: 4→6)
─────────────────────────────────────
  abc-123 latency  23ms → 19ms  ✓ expected
  def-456 latency  41ms → 67ms  ⚠ unexpected response
  ghi-789 latency  38ms → 38ms  — no effect
```

this IS causal evidence. one perturbation, measured responses,
clear directionality.

### coupling graph

only populated from probe data. shows confirmed directional
relationships. NOT from passive observation.

### architecture layers

1. correlation view — shared timeline, flags overlaps, "investigate?" prompts
2. probe view — results of perturbation experiments, causal links
3. coupling graph — populated only from probes, confirmed relationships

### honest boundary

passive observation can only show correlation. coupling (causation)
requires intervention. the dashboard should make this distinction clear
and offer "run a probe?" as the bridge between correlation and coupling.

## dashboard architecture summary

```
single breeder
├── radial health (glance)
├── convergence engine (computed)
├── explore charts
│   ├── heatmap (trials × params/objectives)
│   ├── spider web
│   └── parallel coordinates
├── diagnosis API (/diagnosis)
└── recommendations

multi breeder (future)
├── breeder matrix (convergence overview)
├── correlation timeline (shared wall clock)
├── probe view (causal experiments)
└── coupling graph (confirmed only)
```
