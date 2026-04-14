// godon-bench-greenhouse -- Greenhouse Simulation Engine
//
// A simplified multi-zone greenhouse model for testing the godon optimization engine.
//
// PHYSICS MODEL
//
// Each zone in the greenhouse tracks four state variables:
//   - Temperature (°C): influenced by heating, ventilation, solar gain, wall conduction
//   - Humidity (ratio 0-1): influenced by plant transpiration and vent drying
//   - CO2 (ppm): influenced by injection, vent loss, and plant uptake
//   - Growth (accumulated): the objective -- plants grow when conditions are right
//
// Zones share walls and exchange heat through them. Weather (outside temperature,
// solar radiation) drifts over time using a Lissajous-like pattern, forcing the
// optimizer to continuously adapt.
//
// The model is intentionally simple (Newton's law of cooling style ODE) so that:
//   1. The optimizer's behavior is easy to reason about
//   2. Ground truth is intuitive (warm + lit + watered = good, overheated = bad)
//   3. Parameter interactions are real but not opaque
//
// SCENARIOS
//
//   Simple:  2 zones -- verify basic engine convergence
//   Medium:  4 zones -- test cooperation between breeders sharing the greenhouse
//   Complex: 6 zones -- stress test with more interacting subsystems

use crate::types::*;
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
}

/// Top-level greenhouse state, shared via Arc<Mutex> across HTTP handlers.
///
/// Maintains the full simulation: zones, weather, cumulative resource usage,
/// and the last applied parameter set (for the breeder's effectuation cycle).
pub struct Greenhouse {
    /// The climate-controlled zones in this greenhouse.
    pub zones: Vec<Zone>,
    /// Simulation tick counter. Each tick = 0.1 simulated hours (6 minutes).
    pub tick: u64,
    /// Current outside temperature in °C. Drifts with weather model.
    pub outside_temp: f64,
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

impl Greenhouse {
    /// Create a new greenhouse with all zones at default comfortable conditions:
    /// 20°C, 50% humidity, 400ppm CO2 (slightly below ambient).
    pub fn new(scenario: Scenario) -> Self {
        let zone_count = scenario.zone_count();
        let zones = (0..zone_count)
            .map(|_| Zone {
                temp: 20.0,
                humidity: 0.5,
                co2: 400.0,
                growth_accumulated: 0.0,
            })
            .collect();

        Self {
            zones,
            tick: 0,
            outside_temp: 10.0,
            solar_radiation: 200.0,
            trial_energy_kwh: 0.0,
            trial_water_liters: 0.0,
            last_params: None,
            scenario,
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

    /// Advance the simulation by one timestep (dt = 0.1 simulated hours).
    ///
    /// For each zone, computes the net effect of:
    ///   - Solar gain (reduced by shading)
    ///   - Heat conduction through shared walls (Newton's law of cooling)
    ///   - Heat loss to outside through the greenhouse shell
    ///   - Ventilation cooling (exchanges inside air with cooler outside air)
    ///   - Active heating toward the setpoint
    ///   - Active cooling when temperature exceeds setpoint by 2°C deadband
    ///
    /// Then updates humidity (transpiration vs vent drying) and CO2
    /// (injection vs vent loss vs plant uptake).
    pub fn step(&mut self) {
        // Update weather before computing zone physics
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
        let outside_co2 = self.outside_co2();

        for (i, zone) in self.zones.iter_mut().enumerate() {
            let heating_setpoint = params.heating_setpoints[i];
            let vent = params.vent_openings[i].clamp(0.0, 1.0);
            let shading = params.shading.clamp(0.0, 1.0);
            let co2_inject = params.co2_injection;
            let light = params.light_intensity;
            let irrigation = params.irrigation;

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
            // Ventilation removes humidity by exchanging with drier outside air
            let vent_drying = vent * 0.3;
            zone.humidity += dt * (transpiration - vent_drying);
            zone.humidity = zone.humidity.clamp(0.1, 0.99);

            // --- CO2 dynamics ---

            // CO2 is lost through ventilation (exchanged with ambient ~420ppm)
            let co2_loss_vent = vent * (zone.co2 - outside_co2) * 0.2;
            // Plants consume CO2 proportional to their growth rate
            let co2_plant_uptake = zone.growth_rate_for() * 0.5;
            zone.co2 += dt * (co2_inject - co2_loss_vent - co2_plant_uptake);
            zone.co2 = zone.co2.clamp(100.0, 3000.0);

            // --- Growth accumulation ---

            // Calculate effective light (solar after shading + supplemental grow lights)
            let effective_light = self.solar_radiation * (1.0 - shading) + light;
            let growth = zone.growth_rate_for_params(
                effective_light,
                co2_inject,
                irrigation,
            );
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
        let _total_co2 = params.co2_injection * 0.0001;

        self.trial_energy_kwh += dt * (total_heating + total_venting + total_lighting);
        self.trial_water_liters += dt * params.irrigation * 0.1;

        self.tick += 1;
    }

    /// Run N simulation steps. Called by the /apply endpoint after receiving
    /// new parameters from the optimizer.
    pub fn run_steps(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
    }

    /// Update weather conditions. Uses Lissajous-like oscillations to create
    /// realistic-looking but deterministic weather patterns that drift over time.
    ///
    /// The drift is critical for testing the optimizer's ability to track
    /// moving targets. A static optimum would only test convergence, not
    /// continuous adaptation.
    fn update_weather(&mut self) {
        let t = self.tick as f64 * 0.01;
        // Temperature oscillates roughly 2-18°C (simulating day/night + seasonal)
        self.outside_temp =
            10.0 + 8.0 * (t * 0.1).sin() + 3.0 * (t * 0.37).cos();
        // Solar radiation oscillates roughly 50-550 W/m², never negative
        self.solar_radiation =
            (300.0 + 200.0 * (t * 0.05).sin() + 50.0 * (t * 0.23).cos()).max(0.0);
    }

    /// Ambient CO2 concentration outside the greenhouse (~420ppm as of 2024).
    fn outside_co2(&self) -> f64 {
        420.0
    }

    /// Compute the current metrics snapshot from the greenhouse state.
    ///
    /// This is what reconnaissance reads -- either via /metrics/json (JSON)
    /// or /metrics (Prometheus exposition format). The optimizer uses these
    /// values to evaluate how good the current parameter set is.
    pub fn growth_metrics(&self) -> MetricsResponse {
        let zone_temps: Vec<f64> = self.zones.iter().map(|z| z.temp).collect();
        let zone_humidities: Vec<f64> = self.zones.iter().map(|z| z.humidity).collect();
        let zone_co2_levels: Vec<f64> = self.zones.iter().map(|z| z.co2).collect();

        // Compute per-zone growth rate based on current zone state and applied params.
        // Growth rate is the instantaneous derivative, not the accumulated total.
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
                )
            })
            .collect();

        // Average across all zones -- the primary objective to maximize
        let avg_growth =
            zone_growth_rates.iter().sum::<f64>() / zone_growth_rates.len().max(1) as f64;

        MetricsResponse {
            zone_temps,
            zone_humidities,
            zone_co2_levels,
            zone_growth_rates,
            growth_rate: avg_growth,
            trial_energy_kwh: self.trial_energy_kwh,
            trial_water_liters: self.trial_water_liters,
            max_temp: self.zones.iter().map(|z| z.temp).fold(f64::NEG_INFINITY, f64::max),
            min_temp: self.zones.iter().map(|z| z.temp).fold(f64::INFINITY, f64::min),
            max_humidity: self.zones.iter().map(|z| z.humidity).fold(f64::NEG_INFINITY, f64::max),
            max_co2: self.zones.iter().map(|z| z.co2).fold(f64::NEG_INFINITY, f64::max),
            outside_temp: self.outside_temp,
            solar_radiation: self.solar_radiation,
            tick: self.tick,
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

    /// Compute the full growth rate considering all environmental factors.
    ///
    /// Growth is the product of five factors, each in [0, 1]:
    ///   - Temperature factor: 0 below 5°C or above 40°C, ramps up 5-15°C,
    ///     optimal 15-30°C, ramps down 30-40°C
    ///   - Light factor: low light is limiting, optimal 200-600 W/m²,
    ///     slight reduction above 600 (photoinhibition)
    ///   - CO2 factor: limiting below 200ppm, optimal 800-1200ppm,
    ///     slight reduction above 1200 (waste / diminishing returns)
    ///   - Water factor: limiting below 0.1, optimal 1.0-2.0,
    ///     slight reduction above 2.0 (overwatering)
    ///   - Humidity factor: penalty above 0.9 (disease risk),
    ///     penalty below 0.2 (drought stress)
    ///
    /// The multiplicative model means ANY single bad factor kills growth.
    /// This creates strong non-linearity that stresses the optimizer.
    fn growth_rate_for_params(&self, light: f64, _co2_inject: f64, irrigation: f64) -> f64 {
        let temp_factor = if self.temp < 5.0 || self.temp > 40.0 {
            0.0
        } else if self.temp < 15.0 {
            (self.temp - 5.0) / 10.0
        } else if self.temp > 30.0 {
            (40.0 - self.temp) / 10.0
        } else {
            1.0
        };

        let light_factor = if light < 50.0 {
            light / 50.0 * 0.3
        } else if light < 200.0 {
            0.3 + 0.7 * (light - 50.0) / 150.0
        } else if light < 600.0 {
            1.0
        } else {
            1.0 - (light - 600.0) / 1000.0 * 0.2
        };

        let co2_factor = if self.co2 < 200.0 {
            0.3
        } else if self.co2 < 800.0 {
            0.3 + 0.7 * (self.co2 - 200.0) / 600.0
        } else if self.co2 < 1200.0 {
            1.0
        } else {
            1.0 - (self.co2 - 1200.0) / 1000.0 * 0.3
        };

        let water_factor = if irrigation < 0.1 {
            irrigation / 0.1 * 0.4
        } else if irrigation < 1.0 {
            0.4 + 0.6 * (irrigation - 0.1) / 0.9
        } else if irrigation < 2.0 {
            1.0
        } else {
            0.8
        };

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
