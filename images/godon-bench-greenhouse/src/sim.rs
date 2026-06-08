// godon-bench-greenhouse -- Greenhouse Simulation Engine
//
// A simplified multi-zone greenhouse model for testing the godon optimization engine.
//
// PHYSICS MODEL
//
// Each zone in the greenhouse tracks six state variables:
//   - Temperature (°C): influenced by heating, ventilation, solar gain, wall conduction
//   - Humidity (ratio 0-1): influenced by plant transpiration and vent drying
//   - CO2 (ppm): influenced by injection, vent loss, and plant uptake
//   - Growth (accumulated): the objective -- plants grow when conditions are right
//   - Damage factor (0.3-1.0): irreversible cap on growth from sustained extremes
//   - Extreme ticks: counter tracking how long a zone has been in dangerous territory
//
// Zones share walls and exchange heat through them. Weather (outside temperature,
// solar radiation) drifts over time using a Lissajous-like pattern, forcing the
// optimizer to continuously adapt.
//
// CROP AGING (Non-stationary optimum)
//
// Plants progress through four developmental phases, each with different optimal
// conditions. The optimizer cannot find one static parameter set -- it must discover
// a schedule that follows the crop through its lifecycle:
//
//   Seedling   (ticks 0-149):   prefers 18-24°C, low CO2 (400-600ppm), less water
//   Vegetative (ticks 150-399): prefers 20-28°C, medium CO2 (600-900ppm), normal water
//   Flowering  (ticks 400-699): prefers 22-30°C, high CO2 (1000-1400ppm), more water
//                                CO2 has 3× sensitivity -- critical window for optimization
//   Fruiting   (ticks 700+):    prefers 18-26°C, medium CO2 (600-900ppm), moderate water
//
// IRREVERSIBLE DAMAGE
//
// If a zone's temperature exceeds 42°C or drops below 3°C for sustained periods
// (>30 ticks), the zone accumulates permanent damage. The damage_factor (starting
// at 1.0) decreases by 0.002 per extreme tick, bottoming out at 0.3. This factor
// multiplies growth rate, permanently capping that zone's productivity. The counter
// slowly recovers when conditions improve, but lost damage_factor never returns.
// This creates real consequences for reckless optimization.
//
// CRITICAL WINDOWS
//
// The flowering phase (ticks 400-699) has amplified CO2 sensitivity (3×) and water
// sensitivity (1.5×). Getting CO2 right during this window is 3× as impactful as
// during other phases. Missing this window means suboptimal results that cannot
// be recovered later. This tests whether the optimizer can identify and exploit
// time-limited opportunities.
//
// INTER-GREENHOUSE COUPLING (Structural, Proximity-Based)
//
// Multiple greenhouse instances standing right next to each other share
// physical infrastructure at the structural level. Coupling is not a
// perturbation added after zone physics — it is a first-class part of
// the zone equations:
//
//   Shared wall:     The wall between greenhouses is a zone boundary with
//     5× the conductance of inter-zone walls (0.25). Thin shared wall
//     (polycarbonate/aluminum) transfers heat readily.
//
//   Air exchange:    A shared ventilation duct connects the greenhouses.
//     Mixing rate depends on BOTH greenhouses' vent openings — higher
//     activity = more exchange. CO2 and humidity move toward the mixed-air
//     average. Nonlinear: coupling strength depends on optimizer behavior.
//
//   Shared water tank: Finite reservoir (20L) with slow well influx
//     (0.01 L/tick). Both greenhouses drain it. When combined outflux
//     exceeds influx, the tank depletes. Effective irrigation scales
//     with remaining tank fraction. Genuine scarcity coupling.
//
// Coupling is controlled by COUPLING_FACTOR (0.0 = none, higher = stronger).
// All channels directly modify zone state, not outside conditions.

use crate::types::*;
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use std::sync::{Arc, Mutex};

/// State of a single greenhouse zone.
///
/// Each zone represents a distinct growing area with independent environmental
/// conditions. Zones interact through shared walls (heat conduction).
pub struct Zone {
    /// Current air temperature in °C. Plants survive 5-40°C, thrive at 15-30°C.
    pub temp: f64,
    /// Relative humidity as a ratio (0.0 - 1.0). High humidity (>0.9) promotes disease.
    pub humidity: f64,
    /// CO2 concentration in ppm. Ambient is ~420ppm, enrichment targets 800-1200ppm.
    pub co2: f64,
    /// Cumulative plant growth. Increases each timestep by the current growth rate.
    pub growth_accumulated: f64,
    /// Irreversible damage factor (0.3 - 1.0). Caps growth permanently after
    /// sustained extreme temperatures. Starts at 1.0 (undamaged), decreases by
    /// 0.002 per extreme tick below 0.3 floor.
    pub damage_factor: f64,
    /// Number of consecutive ticks where temp was >42°C or <3°C. Decrements
    /// by 1 per tick when conditions are normal. Triggers damage at >30 ticks.
    pub extreme_ticks: u64,
}

/// Weather dynamics mode -- determines how the environment behaves.
///
///   Smooth:      Deterministic Lissajous oscillations. Predictable, smooth drift.
///                Good for verifying basic convergence.
///
///   Noisy:       Smooth drift + gaussian noise on weather and sensor readings.
///                Tests optimizer robustness to measurement uncertainty.
///
///   Adversarial: Smooth drift + noise + random shocks (cold snaps, heat waves,
///                cloud bursts). Shocks can push zones into guardrail territory
///                even with reasonable parameters. Tests guardrails, rollback,
///                and re-convergence after disruption.
#[derive(Debug, Clone)]
pub enum WeatherMode {
    Smooth,
    Noisy,
    Adversarial,
}

impl WeatherMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            WeatherMode::Smooth => "smooth",
            WeatherMode::Noisy => "noisy",
            WeatherMode::Adversarial => "adversarial",
        }
    }
}

/// Complexity scenario -- determines zone count and problem difficulty.
///
/// Godon's breeder engine can run multiple workers. In multi-zone scenarios,
/// different breeders can control different zones and must cooperate (or at
/// least not fight each other through shared walls).
#[derive(Debug, Clone)]
pub enum Scenario {
    /// 2 zones, 1 breeder. Verify engine convergence on a simple problem.
    Simple,
    /// 4 zones, 2-3 breeders. Test cooperation -- zones share walls.
    Medium,
    /// 6 zones, 4+ breeders. Stress test -- multiple interacting subsystems.
    Complex,
}

impl Scenario {
    pub fn zone_count(&self) -> usize {
        match self {
            Scenario::Simple => 2,
            Scenario::Medium => 4,
            Scenario::Complex => 6,
        }
    }
}

/// Parsed status from a neighbor greenhouse, used to compute coupling deltas.
/// Fetched from each neighbor's GET /status endpoint.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NeighborStatus {
    pub zones: Vec<crate::types::ZoneSnapshot>,
    pub trial_energy_kwh: f64,
    pub trial_water_liters: f64,
}

/// Shared coupling state between adjacent greenhouses.
///
/// Two greenhouses right next to each other share physical infrastructure
/// at the structural level — not as perturbations added after zone physics,
/// but as first-class parts of the zone equations:
///
/// Coupling channels:
///   - Shared wall:     Zone boundary conduction to neighbor zones (thermal)
///   - Air exchange:    Shared ventilation duct mixing air between greenhouses
///   - Water tank:      Finite shared reservoir with slow well influx
///   - Humidity:        Moisture transfer through semi-permeable shared wall
pub struct CouplingState {
    /// URLs of neighbor greenhouses (comma-separated from COUPLING_NEIGHBORS env var).
    pub neighbors: Vec<String>,
    /// Coupling strength multiplier (0.0 = no coupling, 0.05 = weak, 0.2 = strong).
    pub factor: f64,
    /// Last computed thermal coupling delta (°C shift to zone temp).
    pub last_delta_temp: f64,
    /// Last computed CO2 air exchange delta (ppm shift to zone CO2).
    pub last_delta_co2: f64,
    /// Removed (power sag). Kept at 0.0 for backward compatibility.
    pub last_delta_light: f64,
    /// Last computed humidity exchange delta (ratio shift to zone humidity).
    pub last_delta_humidity: f64,
    /// Last computed water pressure reduction from shared tank depletion.
    pub last_delta_water: f64,
    /// Shared water tank level (liters). Starts at capacity, depletes
    /// when combined draw exceeds influx. Slowly refills from well.
    pub shared_water_tank: f64,
    /// Shared water tank capacity (liters).
    pub shared_water_capacity: f64,
    /// Well influx rate (liters per tick). Slow refill.
    pub shared_water_influx: f64,
    /// Neighbor's reported water consumption for current tick cycle.
    neighbor_water_draw: f64,
}

impl CouplingState {
    pub fn new(neighbors: Vec<String>, factor: f64) -> Self {
        let capacity = 20.0;
        Self {
            neighbors,
            factor,
            last_delta_temp: 0.0,
            last_delta_co2: 0.0,
            last_delta_light: 0.0,
            last_delta_humidity: 0.0,
            last_delta_water: 0.0,
            shared_water_tank: capacity,
            shared_water_capacity: capacity,
            shared_water_influx: 0.01, // slow well refill: 0.01 liters/tick
            neighbor_water_draw: 0.0,
        }
    }
}

/// Top-level greenhouse state, shared via Arc<Mutex> across HTTP handlers.
///
/// Maintains the full simulation: zones, weather, cumulative resource usage,
/// coupling state, and the last applied parameter set (for the breeder's
/// effectuation cycle).
pub struct Greenhouse {
    /// The climate-controlled zones in this greenhouse.
    pub zones: Vec<Zone>,
    /// Simulation tick counter. Each tick = 0.1 simulated hours (6 minutes).
    pub tick: u64,
    /// Current outside temperature in °C. Drifts with weather model + coupling.
    pub outside_temp: f64,
    /// Current outside CO2 in ppm. Base 420ppm + coupling exhaust delta.
    pub outside_co2: f64,
    /// Current outside humidity ratio. Base 0.4 + coupling humidity delta.
    pub outside_humidity: f64,
    /// Current solar radiation in W/m². Varies with weather model.
    pub solar_radiation: f64,
    /// Cumulative energy consumed this trial (kWh). Sum of heating, venting, lighting.
    pub trial_energy_kwh: f64,
    /// Cumulative water consumed this trial (liters).
    pub trial_water_liters: f64,
    /// The last parameter set applied via POST /apply. Used for growth calculation.
    pub last_params: Option<ApplyRequest>,
    /// Which scenario configuration is active.
    #[allow(dead_code)]
    pub scenario: Scenario,
    /// Weather dynamics mode (smooth, noisy, adversarial).
    pub weather_mode: WeatherMode,
    /// RNG seed for reproducibility. Same seed + same steps = same results.
    pub seed: u64,
    /// Seeded PRNG (ChaCha8). Deterministic given the same seed.
    rng: ChaCha8Rng,
    /// Hidden coupling state to neighbor greenhouses.
    pub coupling: CouplingState,
    /// Accumulated crop quality (0.0-1.0). Degrades with stress events,
    /// slowly recovers. Affected by damage_factor.
    pub crop_quality: f64,
}

/// Crop developmental phase -- determines optimal temperature/CO2/water ranges
/// and sensitivity multipliers. The optimizer must adapt as the crop matures.
///
/// Phases are tick-based:
///   Seedling:   0-149   (young plants, delicate, low demands)
///   Vegetative: 150-399 (rapid growth, building biomass)
///   Flowering:  400-699 (critical window, CO2 has 3× effect)
///   Fruiting:   700+    (maturation, reduced demands)
#[derive(Debug)]
enum CropPhase {
    Seedling,
    Vegetative,
    Flowering,
    Fruiting,
}

impl Greenhouse {
    /// Create a new greenhouse without coupling (backward compatible).
    pub fn new(scenario: Scenario, weather_mode: WeatherMode, seed: u64) -> Self {
        Self::with_coupling(scenario, weather_mode, seed, Vec::new(), 0.0)
    }

    /// Create a new greenhouse with inter-greenhouse coupling.
    ///
    /// All zones start at default conditions: 20°C, 50% humidity, 400ppm CO2,
    /// no damage. The RNG is seeded for reproducibility. Coupling neighbors
    /// and factor are stored but have no effect until apply_coupling() is called.
    pub fn with_coupling(
        scenario: Scenario,
        weather_mode: WeatherMode,
        seed: u64,
        neighbors: Vec<String>,
        coupling_factor: f64,
    ) -> Self {
        let zone_count = scenario.zone_count();
        let zones = (0..zone_count)
            .map(|_| Zone {
                temp: 20.0,
                humidity: 0.5,
                co2: 400.0,
                growth_accumulated: 0.0,
                damage_factor: 1.0,
                extreme_ticks: 0,
            })
            .collect();

        Self {
            zones,
            tick: 0,
            outside_temp: 10.0,
            outside_co2: 420.0,
            outside_humidity: 0.4,
            solar_radiation: 200.0,
            trial_energy_kwh: 0.0,
            trial_water_liters: 0.0,
            last_params: None,
            scenario,
            weather_mode,
            seed,
            rng: ChaCha8Rng::seed_from_u64(seed),
            coupling: CouplingState::new(neighbors, coupling_factor),
            crop_quality: 1.0,
        }
    }

    /// Apply a parameter set from the optimizer.
    ///
    /// Accepts either one value per zone or a single value broadcast to all zones.
    /// This flexibility lets the same strain definition work across scenarios
    /// with different zone counts without restructuring.
    pub fn apply(&mut self, req: ApplyRequest) {
        let expected = self.zones.len();

        // Normalize heating setpoints: per-zone or broadcast
        let heating = if req.heating_setpoints.len() == expected {
            req.heating_setpoints.clone()
        } else if req.heating_setpoints.len() == 1 {
            vec![req.heating_setpoints[0]; expected]
        } else {
            vec![20.0; expected]
        };

        // Normalize vent openings: per-zone or broadcast
        let vents = if req.vent_openings.len() == expected {
            req.vent_openings.clone()
        } else if req.vent_openings.len() == 1 {
            vec![req.vent_openings[0]; expected]
        } else {
            vec![0.3; expected]
        };

        let req = ApplyRequest {
            heating_setpoints: heating,
            vent_openings: vents,
            ..req
        };

        self.last_params = Some(req);
    }

    /// Determine the current crop developmental phase based on tick count.
    ///
    /// The phase determines optimal ranges for temperature, CO2, and water,
    /// as well as sensitivity multipliers that create critical windows.
    fn crop_phase(&self) -> CropPhase {
        let tick = self.tick;
        if tick < 150 {
            CropPhase::Seedling
        } else if tick < 400 {
            CropPhase::Vegetative
        } else if tick < 700 {
            CropPhase::Flowering
        } else {
            CropPhase::Fruiting
        }
    }

    /// Human-readable crop phase name (exposed via status endpoint).
    pub fn crop_phase_name(&self) -> &'static str {
        match self.crop_phase() {
            CropPhase::Seedling => "seedling",
            CropPhase::Vegetative => "vegetative",
            CropPhase::Flowering => "flowering",
            CropPhase::Fruiting => "fruiting",
        }
    }

    /// CO2 sensitivity multiplier for the current crop phase.
    ///
    /// During flowering, CO2 has 3× sensitivity -- getting CO2 right during
    /// this window is dramatically more impactful than at other times. This
    /// creates a critical window that the optimizer must discover and exploit.
    fn co2_sensitivity(&self) -> f64 {
        match self.crop_phase() {
            CropPhase::Seedling => 1.0,
            CropPhase::Vegetative => 1.2,
            CropPhase::Flowering => 3.0,
            CropPhase::Fruiting => 1.0,
        }
    }

    /// Water sensitivity multiplier for the current crop phase.
    ///
    /// Flowering and fruiting phases need more precise irrigation. Over- or
    /// under-watering during flowering has 1.5× the impact.
    fn water_sensitivity(&self) -> f64 {
        match self.crop_phase() {
            CropPhase::Seedling => 0.6,
            CropPhase::Vegetative => 1.0,
            CropPhase::Flowering => 1.5,
            CropPhase::Fruiting => 1.2,
        }
    }

    /// Advance the simulation by one timestep (dt = 0.1 simulated hours).
    ///
    /// For each zone, computes the net effect of:
    ///   - Solar gain (reduced by shading)
    ///   - Heat conduction through shared walls (Newton's law of cooling)
    ///   - Heat loss to outside through the greenhouse shell
    ///   - Ventilation cooling (exchanges inside air with outside air)
    ///   - Active heating toward the setpoint
    ///   - Active cooling when temperature exceeds setpoint by 2°C deadband
    ///   - Irreversible damage accumulation from sustained extreme temperatures
    ///   - Coupling deltas modifying outside conditions and effective light
    ///
    /// Then updates humidity (transpiration vs vent drying), CO2 (injection vs
    /// vent loss vs plant uptake), and growth (phase-dependent multiplicative model).
    pub fn step(&mut self) {
        // Update weather (including coupling deltas) before computing zone physics
        self.update_weather();

        // No parameters applied yet -- nothing to simulate
        let params = match &self.last_params {
            Some(p) => p.clone(),
            None => return,
        };

        let dt = 0.1;
        let zone_count = self.zones.len();

        // Snapshot neighbor temperatures before mutation to satisfy borrow checker.
        // This also represents the physical reality that heat transfer in a single
        // timestep uses temperatures from the start of the timestep (explicit Euler).
        let neighbor_temps: Vec<f64> = self.zones.iter().map(|z| z.temp).collect();
        let outside_co2 = self.outside_co2;

        let crop_phase = self.crop_phase();
        let co2_sensitivity = self.co2_sensitivity();
        let water_sensitivity = self.water_sensitivity();

        for (i, zone) in self.zones.iter_mut().enumerate() {
            let heating_setpoint = params.heating_setpoints[i];
            let vent = params.vent_openings[i].clamp(0.0, 1.0);
            let shading = params.shading.clamp(0.0, 1.0);
            let co2_inject = params.co2_injection;
            let light = params.light_intensity;
            let irrigation = params.irrigation;

            // --- Damage tracking (slowly reversible) ---
            // Zones at >42°C or <3°C accumulate extreme ticks. After 30 ticks
            // of sustained extremes, the zone takes damage (damage_factor
            // decreases by 0.002 per extreme tick, floor 0.3).
            // Recovery: when conditions are normal, damage_factor slowly
            // regenerates (+0.0005 per tick). A brief excursion costs ~2-3
            // trials to recover. Sustained recklessness takes 50+ trials.
            // The counter slowly recovers when conditions improve.
            if zone.temp > 42.0 || zone.temp < 3.0 {
                zone.extreme_ticks += 1;
            } else {
                zone.extreme_ticks = zone.extreme_ticks.saturating_sub(1);
                zone.damage_factor = (zone.damage_factor + 0.0005).min(1.0);
            }

            if zone.extreme_ticks > 30 {
                zone.damage_factor = (zone.damage_factor - 0.002).max(0.3);
            }

            // --- Temperature dynamics ---

            // Solar energy entering the zone, reduced by shading (0 = no shade, 1 = full)
            let solar_gain = self.solar_radiation * (1.0 - shading) * 0.02;

            // Average temperature of adjacent zones (linear chain topology)
            let neighbor_avg = if zone_count > 1 {
                let mut sum = 0.0;
                let mut count = 0;
                if i > 0 {
                    sum += neighbor_temps[i - 1];
                    count += 1;
                }
                if i < zone_count - 1 {
                    sum += neighbor_temps[i + 1];
                    count += 1;
                }
                sum / count as f64
            } else {
                zone.temp
            };

            // Heat conduction through shared walls. Zones connected in a linear chain
            // so interior zones have two neighbors, edge zones have one.
            let wall_transfer = (neighbor_avg - zone.temp) * 0.05;

            // Heat loss through greenhouse shell to outside (insulation factor 0.1)
            let heat_loss_outside = (zone.temp - self.outside_temp) * 0.1;

            // Ventilation cooling: proportional to vent opening and temp difference.
            // Opening vents exchanges warm inside air with cooler outside air.
            // This is why the optimizer must balance ventilation vs CO2 loss.
            let vent_cooling = vent * (zone.temp - self.outside_temp) * 0.3;

            // Active heating: proportional controller toward setpoint
            let heating_power = if heating_setpoint > zone.temp {
                (heating_setpoint - zone.temp) * 0.5
            } else {
                0.0
            };

            // Active cooling: kicks in when zone exceeds setpoint by 2°C deadband.
            // The deadband prevents hunting around the setpoint and makes the
            // optimizer's job harder (can't just set setpoint to 0 for cooling).
            let cooling_power = if heating_setpoint < zone.temp - 2.0 {
                (zone.temp - heating_setpoint - 2.0) * 0.3
            } else {
                0.0
            };

            // Net temperature change this timestep
            zone.temp += dt
                * (solar_gain
                    + wall_transfer
                    - heat_loss_outside
                    - vent_cooling
                    + heating_power
                    - cooling_power);

            // --- Humidity dynamics ---

            // Plants release water vapor proportional to irrigation and temperature
            let transpiration = irrigation * zone.temp * 0.001;
            // Ventilation removes humidity by exchanging with outside air.
            // Outside humidity is affected by coupling (neighbor moisture migration).
            let vent_drying = vent * (zone.humidity - self.outside_humidity).max(0.0) * 0.3;
            zone.humidity += dt * (transpiration - vent_drying);
            zone.humidity = zone.humidity.clamp(0.1, 0.99);

            // --- CO2 dynamics ---

            // CO2 is lost through ventilation (exchanged with outside CO2).
            let co2_loss_vent = vent * (zone.co2 - outside_co2) * 0.2;
            // Plants consume CO2 proportional to their growth rate
            let co2_plant_uptake = zone.growth_rate_for() * 0.5;
            zone.co2 += dt * (co2_inject - co2_loss_vent - co2_plant_uptake);
            zone.co2 = zone.co2.clamp(100.0, 3000.0);

            // --- Structural coupling (direct zone-level) ---
            // Applied AFTER zone physics, BEFORE growth calculation.
            // Coupling is a first-class part of zone equations, not an afterthought.

            zone.temp += self.coupling.last_delta_temp * dt;
            zone.co2 += self.coupling.last_delta_co2 * dt;
            zone.co2 = zone.co2.clamp(100.0, 3000.0);
            zone.humidity += self.coupling.last_delta_humidity * dt;
            zone.humidity = zone.humidity.clamp(0.1, 0.99);

            // Shared water tank: effective irrigation scales with remaining water.
            // irrigation is the optimizer's requested rate; scarcity reduces it.
            let effective_irrigation = irrigation
                * (1.0 - self.coupling.last_delta_water);

            // --- Growth accumulation ---

            // Calculate effective light (solar after shading + supplemental grow lights).
            let effective_light = self.solar_radiation * (1.0 - shading) + light;

            // Growth rate depends on current crop phase (non-stationary optimum),
            // phase-dependent sensitivities (critical windows), and irreversible
            // damage factor. A damaged zone can never reach its former potential.
            let growth = zone.growth_rate_for_params(
                effective_light,
                co2_inject,
                effective_irrigation,
                &crop_phase,
                co2_sensitivity,
                water_sensitivity,
            ) * zone.damage_factor;
            zone.growth_accumulated += dt * growth;
        }

        // --- Resource consumption tracking ---

        // Energy: heating effort (proportional to setpoint excess), vent fan power,
        // and grow light power. These create the multi-objective tradeoff:
        // more heating/light = faster growth but higher energy cost.
        let total_heating: f64 = params
            .heating_setpoints
            .iter()
            .zip(self.zones.iter())
            .map(|(sp, z)| (*sp - z.temp).max(0.0) * 0.01)
            .sum();
        let total_venting: f64 = params.vent_openings.iter().map(|v| v * 0.005).sum();
        let total_lighting = params.light_intensity * 0.001;

        self.trial_energy_kwh += dt * (total_heating + total_venting + total_lighting);
        self.trial_water_liters += dt * params.irrigation * 0.1;

        // --- Shared water tank dynamics ---
        // Drain: my irrigation draw reduces the tank
        let my_draw = params.irrigation * 0.3 * dt; // liters this tick
        let neighbor_draw = self.coupling.neighbor_water_draw * 0.01 * dt; // scaled estimate
        let total_draw = my_draw + neighbor_draw;
        self.coupling.shared_water_tank -= total_draw;
        // Refill: slow well influx
        self.coupling.shared_water_tank += self.coupling.shared_water_influx * dt;
        // Clamp
        self.coupling.shared_water_tank = self.coupling.shared_water_tank
            .clamp(0.0, self.coupling.shared_water_capacity);

        // --- Crop quality tracking ---
        // Degrades with stress events, slowly recovers.
        let avg_damage: f64 = self.zones.iter().map(|z| z.damage_factor).sum::<f64>()
            / self.zones.len().max(1) as f64;
        let mut stress_count = 0;
        for zone in &self.zones {
            if zone.temp > 35.0 || zone.temp < 8.0 { stress_count += 1; }
            if zone.co2 < 200.0 || zone.co2 > 2500.0 { stress_count += 1; }
        }
        let irrigation = params.irrigation;
        if irrigation > 2.5 || irrigation < 0.1 { stress_count += 1; }

        // Degradation from stress events
        self.crop_quality -= 0.001 * stress_count as f64 * dt;
        // Slow recovery when no stress
        if stress_count == 0 {
            self.crop_quality += 0.0002 * dt;
        }
        // Scale by average damage factor
        self.crop_quality *= (0.999 + 0.001 * avg_damage);
        self.crop_quality = self.crop_quality.clamp(0.0, 1.0);

        self.tick += 1;
    }

    /// Run N simulation steps. Called by the /apply endpoint after receiving
    /// new parameters from the optimizer.
    pub fn run_steps(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
    }

    /// Update weather conditions based on the active weather mode.
    ///
    /// Smooth: deterministic Lissajous oscillations. Same tick always produces
    ///         the same weather. Good for verifying convergence.
    ///
    /// Noisy: smooth base + gaussian noise on temperature (σ=1.5°C) and
    ///        solar radiation (σ=30 W/m²). Simulates sensor/measurement noise
    ///        and natural variability. Tests optimizer robustness to uncertainty.
    ///
    /// Adversarial: smooth base + noise + random shocks. Shocks are sudden
    ///        step changes (±10-15°C temperature swings, ±200 W/m² radiation
    ///        drops lasting 20-60 ticks). These simulate cold snaps, heat waves,
    ///        and cloud bursts. They can push zones into guardrail territory
    ///        even with reasonable parameters, forcing rollback and re-convergence.
    ///
    /// Coupling no longer modifies weather — proximity coupling is applied
    /// directly to zone state in step().
    fn update_weather(&mut self) {
        let t = self.tick as f64 * 0.01;

        // Base weather: smooth Lissajous oscillations (always present)
        let base_temp = 10.0 + 8.0 * (t * 0.1).sin() + 3.0 * (t * 0.37).cos();
        let base_solar = (300.0 + 200.0 * (t * 0.05).sin() + 50.0 * (t * 0.23).cos()).max(0.0);

        match self.weather_mode {
            WeatherMode::Smooth => {
                self.outside_temp = base_temp;
                self.solar_radiation = base_solar;
            }
            WeatherMode::Noisy => {
                // Gaussian noise: temp ±1.5°C, solar ±30 W/m²
                let temp_noise = self.rng.random_range(-1.5..1.5);
                let solar_noise = self.rng.random_range(-30.0..30.0);
                self.outside_temp = base_temp + temp_noise;
                self.solar_radiation = (base_solar + solar_noise).max(0.0);
            }
            WeatherMode::Adversarial => {
                // Same noise as Noisy mode
                let temp_noise = self.rng.random_range(-1.5..1.5);
                let solar_noise = self.rng.random_range(-30.0..30.0);

                // Random shocks: ~2% chance per tick of a major weather event.
                let mut shock_temp = 0.0;
                let mut shock_solar = 0.0;

                if self.rng.random_bool(0.02) {
                    // Cold snap or heat wave: ±10-15°C shift
                    shock_temp = self.rng.random_range(-15.0..15.0);
                    // Cloud burst or sudden clear sky: ±200 W/m² shift
                    shock_solar = self.rng.random_range(-200.0..200.0);
                }

                self.outside_temp = base_temp + temp_noise + shock_temp;
                self.solar_radiation = (base_solar + solar_noise + shock_solar).max(0.0);
            }
        }

        // Coupling no longer modifies outside conditions.
        // Proximity-based coupling is applied directly to zone state in step().
        self.outside_co2 = 420.0;
        self.outside_humidity = 0.4;
    }

    /// Compute structural coupling from neighbor greenhouse statuses.
    ///
    /// Coupling is a first-class part of zone physics, not a perturbation
    /// added after the fact. Three structural channels:
    ///
    ///   SHARED WALL (thermal): The wall between greenhouses is treated as
    ///     a zone boundary with 5× the conductance of inter-zone walls (0.25).
    ///     Thin shared wall (polycarbonate/aluminum) transfers heat readily.
    ///
    ///   AIR EXCHANGE (CO2 + humidity): A shared ventilation duct connects
    ///     the two greenhouses. Air mixing rate depends on BOTH greenhouses'
    ///     vent openings — higher activity = more exchange. Nonlinear:
    ///     air_exchange = (my_vent + neighbor_vent) * factor * 0.3
    ///     CO2 and humidity move toward the mixed-air average.
    ///
    ///   SHARED WATER TANK: Finite reservoir (20L) with slow well influx
    ///     (0.01 L/tick). Both greenhouses drain it. When combined outflux
    ///     exceeds influx, the tank depletes. Effective irrigation scales
    ///     with remaining tank fraction: irrigation * (tank / capacity).
    ///     Neighbor's draw depletes the shared resource — genuine scarcity.
    ///
    /// Deltas are stored and applied in step().
    pub fn apply_coupling(&mut self, neighbor_statuses: &[NeighborStatus]) {
        let factor = self.coupling.factor;
        if factor <= 0.0 || neighbor_statuses.is_empty() {
            self.coupling.last_delta_temp = 0.0;
            self.coupling.last_delta_co2 = 0.0;
            self.coupling.last_delta_light = 0.0;
            self.coupling.last_delta_humidity = 0.0;
            self.coupling.last_delta_water = 0.0;
            self.coupling.neighbor_water_draw = 0.0;
            return;
        }

        let my_avg_temp = self.zones.iter().map(|z| z.temp).sum::<f64>()
            / self.zones.len().max(1) as f64;
        let my_avg_co2 = self.zones.iter().map(|z| z.co2).sum::<f64>()
            / self.zones.len().max(1) as f64;
        let my_avg_humidity = self.zones.iter().map(|z| z.humidity).sum::<f64>()
            / self.zones.len().max(1) as f64;
        let my_avg_vent = self.last_params.as_ref().map_or(0.0, |p| {
            p.vent_openings.iter().sum::<f64>() / p.vent_openings.len().max(1) as f64
        });

        let mut delta_temp = 0.0;
        let mut delta_co2 = 0.0;
        let mut delta_humidity = 0.0;
        let mut total_neighbor_water = 0.0;

        for ns in neighbor_statuses {
            let neighbor_avg_temp = if ns.zones.is_empty() {
                20.0
            } else {
                ns.zones.iter().map(|z| z.temp).sum::<f64>() / ns.zones.len() as f64
            };
            let neighbor_avg_co2 = if ns.zones.is_empty() {
                400.0
            } else {
                ns.zones.iter().map(|z| z.co2).sum::<f64>() / ns.zones.len() as f64
            };
            let neighbor_avg_humidity = if ns.zones.is_empty() {
                0.5
            } else {
                ns.zones.iter().map(|z| z.humidity).sum::<f64>() / ns.zones.len() as f64
            };
            total_neighbor_water += ns.trial_water_liters;

            // SHARED WALL (thermal): same conductance as inter-zone walls.
            // Factor scales the effective wall area. At factor=1.0,
            // neighbor zones conduct heat as strongly as own zones.
            delta_temp += (neighbor_avg_temp - my_avg_temp) * factor * 0.25;

            // AIR EXCHANGE (CO2 + humidity): shared ventilation duct.
            // Mixing rate depends on BOTH greenhouses' vent openings.
            // Higher activity = more air exchange = stronger coupling.
            let neighbor_avg_vent = if ns.zones.is_empty() {
                0.0
            } else {
                // Estimate from energy usage as proxy for activity level
                (ns.trial_energy_kwh / 10.0).min(1.0)
            };
            let air_exchange_rate = (my_avg_vent + neighbor_avg_vent) * factor * 0.3;

            // CO2: mixed air moves both toward the average
            let co2_mixed = (my_avg_co2 + neighbor_avg_co2) / 2.0;
            delta_co2 += (co2_mixed - my_avg_co2) * air_exchange_rate;

            // HUMIDITY: same air exchange carries moisture
            let hum_mixed = (my_avg_humidity + neighbor_avg_humidity) / 2.0;
            delta_humidity += (hum_mixed - my_avg_humidity) * air_exchange_rate;
        }

        // SHARED WATER TANK: finite reservoir with slow well influx.
        // Update tank level based on neighbor's reported water consumption.
        // My own consumption is deducted in step() each tick.
        self.coupling.neighbor_water_draw = total_neighbor_water;

        // Compute water scarcity factor: what fraction of irrigation is available
        let tank_fraction = (self.coupling.shared_water_tank
            / self.coupling.shared_water_capacity)
            .clamp(0.0, 1.0);
        let water_scarcity = 1.0 - tank_fraction; // 0 = full tank, 1 = empty

        self.coupling.last_delta_temp = delta_temp;
        self.coupling.last_delta_co2 = delta_co2;
        self.coupling.last_delta_light = 0.0;
        self.coupling.last_delta_humidity = delta_humidity;
        self.coupling.last_delta_water = water_scarcity;
    }

    /// Compute the current metrics snapshot from the greenhouse state.
    ///
    /// This is what reconnaissance reads -- either via /metrics/json (JSON)
    /// or /metrics (Prometheus exposition format). The optimizer uses these
    /// values to evaluate how good the current parameter set is.
    ///
    pub fn growth_metrics(&mut self) -> MetricsResponse {
        let zone_temps: Vec<f64> = self.zones.iter().map(|z| z.temp).collect();
        let zone_humidities: Vec<f64> = self.zones.iter().map(|z| z.humidity).collect();
        let zone_co2_levels: Vec<f64> = self.zones.iter().map(|z| z.co2).collect();
        let zone_damage: Vec<f64> = self.zones.iter().map(|z| z.damage_factor).collect();

        let crop_phase = self.crop_phase();
        let co2_sensitivity = self.co2_sensitivity();
        let water_sensitivity = self.water_sensitivity();

        let zone_growth_rates: Vec<f64> = self
            .zones
            .iter()
            .map(|z| {
                let params = self.last_params.as_ref();
                let effective_light = self.solar_radiation
                    * (1.0 - params.map_or(0.0, |p| p.shading))
                    + params.map_or(0.0, |p| p.light_intensity);
                z.growth_rate_for_params(
                    effective_light,
                    params.map_or(0.0, |p| p.co2_injection),
                    params.map_or(0.0, |p| p.irrigation),
                    &crop_phase,
                    co2_sensitivity,
                    water_sensitivity,
                ) * z.damage_factor
            })
            .collect();

        let avg_growth =
            zone_growth_rates.iter().sum::<f64>() / zone_growth_rates.len().max(1) as f64;

        let (noisy_temps, noisy_humidities, noisy_co2, noisy_growth, noisy_energy, noisy_water) =
            match self.weather_mode {
                WeatherMode::Smooth => (
                    zone_temps.clone(),
                    zone_humidities.clone(),
                    zone_co2_levels.clone(),
                    avg_growth,
                    self.trial_energy_kwh,
                    self.trial_water_liters,
                ),
                WeatherMode::Noisy | WeatherMode::Adversarial => {
                    let nt: Vec<f64> = zone_temps.iter()
                        .map(|t| t + self.rng.random_range(-0.3..0.3))
                        .collect();
                    let nh: Vec<f64> = zone_humidities.iter()
                        .map(|h| (h + self.rng.random_range(-0.02..0.02)).clamp(0.0, 1.0))
                        .collect();
                    let nc: Vec<f64> = zone_co2_levels.iter()
                        .map(|c| (c + self.rng.random_range(-15.0..15.0)).max(0.0))
                        .collect();
                    let ng = (avg_growth + self.rng.random_range(-0.02..0.02)).max(0.0);
                    let ne = self.trial_energy_kwh + self.rng.random_range(-0.01..0.01);
                    let nw = (self.trial_water_liters + self.rng.random_range(-0.005..0.005)).max(0.0);
                    (nt, nh, nc, ng, ne, nw)
                }
            };

        MetricsResponse {
            zone_temps: noisy_temps.clone(),
            zone_humidities: noisy_humidities.clone(),
            zone_co2_levels: noisy_co2.clone(),
            zone_growth_rates,
            zone_damage,
            growth_rate: noisy_growth,
            trial_energy_kwh: noisy_energy,
            trial_water_liters: noisy_water,
            water_efficiency: noisy_growth / noisy_water.max(0.001),
            crop_quality: self.crop_quality,
            max_temp: noisy_temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            min_temp: noisy_temps.iter().cloned().fold(f64::INFINITY, f64::min),
            max_humidity: noisy_humidities.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            max_co2: noisy_co2.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            outside_temp: self.outside_temp,
            outside_co2: self.outside_co2,
            outside_humidity: self.outside_humidity,
            solar_radiation: self.solar_radiation,
            tick: self.tick,
            crop_phase: format!("{:?}", crop_phase).to_lowercase(),
            coupling_delta_temp: self.coupling.last_delta_temp,
            coupling_delta_co2: self.coupling.last_delta_co2,
            coupling_delta_light: 0.0,
            coupling_delta_water: self.coupling.last_delta_water,
            coupling_delta_humidity: self.coupling.last_delta_humidity,
        }
    }
}

impl Zone {
    /// Compute a simplified growth rate based on temperature and humidity only.
    /// Used for CO2 plant uptake calculation (which doesn't need the full model).
    fn growth_rate_for(&self) -> f64 {
        let temp_factor = if self.temp < 5.0 || self.temp > 40.0 {
            0.0
        } else if self.temp < 15.0 {
            (self.temp - 5.0) / 10.0
        } else if self.temp > 30.0 {
            (40.0 - self.temp) / 10.0
        } else {
            1.0
        };

        let hum_factor = if self.humidity > 0.9 {
            0.3
        } else if self.humidity < 0.2 {
            0.5
        } else {
            1.0
        };

        temp_factor * hum_factor
    }

    /// Compute the full growth rate considering all environmental factors
    /// and the current crop developmental phase.
    ///
    /// Growth is the product of five factors, each in [0, 1]:
    ///   - Temperature factor: phase-dependent optimal range.
    ///     Seedling: 18-24°C, Vegetative: 20-28°C, Flowering: 22-30°C, Fruiting: 18-26°C
    ///   - Light factor: low light is limiting, optimal 200-600 W/m²,
    ///     slight reduction above 600 (photoinhibition)
    ///   - CO2 factor: phase-dependent optimal range with sensitivity multiplier.
    ///     During flowering, CO2 sensitivity is 3× -- the critical window effect.
    ///     Seedling: 400-600ppm, Vegetative: 600-900ppm, Flowering: 1000-1400ppm
    ///   - Water factor: phase-dependent with sensitivity multiplier.
    ///     Flowering needs 1.5× more precise irrigation.
    ///   - Humidity factor: penalty above 0.9 (disease risk),
    ///     penalty below 0.2 (drought stress)
    ///
    /// The multiplicative model means ANY single bad factor kills growth.
    /// Combined with phase-dependent optima, the optimizer must discover a
    /// time-varying policy, not a static parameter set.
    fn growth_rate_for_params(
        &self,
        light: f64,
        _co2_inject: f64,
        irrigation: f64,
        phase: &CropPhase,
        co2_sensitivity: f64,
        water_sensitivity: f64,
    ) -> f64 {
        // Temperature factor: optimal range shifts with crop phase
        let (opt_temp_lo, opt_temp_hi) = match phase {
            CropPhase::Seedling => (18.0, 24.0),
            CropPhase::Vegetative => (20.0, 28.0),
            CropPhase::Flowering => (22.0, 30.0),
            CropPhase::Fruiting => (18.0, 26.0),
        };

        let temp_factor = if self.temp < 5.0 || self.temp > 40.0 {
            0.0
        } else if self.temp < opt_temp_lo {
            (self.temp - 5.0) / (opt_temp_lo - 5.0)
        } else if self.temp > opt_temp_hi {
            (40.0 - self.temp) / (40.0 - opt_temp_hi)
        } else {
            1.0
        };

        // Light factor: unchanged across phases
        let light_factor = if light < 50.0 {
            light / 50.0 * 0.3
        } else if light < 200.0 {
            0.3 + 0.7 * (light - 50.0) / 150.0
        } else if light < 600.0 {
            1.0
        } else {
            1.0 - (light - 600.0) / 1000.0 * 0.2
        };

        // CO2 factor: optimal range shifts with crop phase, amplified by sensitivity.
        // During flowering (3× sensitivity), getting CO2 into the 1000-1400ppm
        // range is dramatically more impactful than at other times.
        let (opt_co2_lo, opt_co2_hi) = match phase {
            CropPhase::Seedling => (400.0, 600.0),
            CropPhase::Vegetative => (600.0, 900.0),
            CropPhase::Flowering => (1000.0, 1400.0),
            CropPhase::Fruiting => (600.0, 900.0),
        };

        let co2_base = if self.co2 < 200.0 {
            0.3
        } else if self.co2 < opt_co2_lo {
            0.3 + 0.7 * (self.co2 - 200.0) / (opt_co2_lo - 200.0)
        } else if self.co2 < opt_co2_hi {
            1.0
        } else {
            1.0 - (self.co2 - opt_co2_hi) / 1000.0 * 0.3
        };

        // Sensitivity amplification, capped at 1.0 (can't exceed perfect).
        // A 3× sensitivity during flowering means getting CO2 right is 3× as
        // important -- but getting it wrong is also 3× as bad.
        let co2_factor = (co2_base * co2_sensitivity).min(1.0);

        // Water factor: phase-dependent sensitivity
        let water_base = if irrigation < 0.1 {
            irrigation / 0.1 * 0.4
        } else if irrigation < 1.0 {
            0.4 + 0.6 * (irrigation - 0.1) / 0.9
        } else if irrigation < 2.0 {
            1.0
        } else {
            0.8
        };
        let water_factor = (water_base * water_sensitivity).min(1.0);

        // Humidity factor: unchanged across phases
        let hum_factor = if self.humidity > 0.9 {
            0.3
        } else if self.humidity < 0.2 {
            0.5
        } else {
            1.0
        };

        temp_factor * light_factor * co2_factor * water_factor * hum_factor
    }
}

/// Thread-safe shared reference to the greenhouse simulation.
/// The simulation state is protected by a Mutex -- only one request modifies
/// it at a time, which is fine since optimization trials are sequential.
pub type SharedGreenhouse = Arc<Mutex<Greenhouse>>;
