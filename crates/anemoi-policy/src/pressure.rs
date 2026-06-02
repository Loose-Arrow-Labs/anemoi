//! Resource pressure model.
//!
//! Turns a runtime memory snapshot, candidate requirements, and active-request
//! load into normalized pressure readings plus scoring penalties with plain,
//! structured explanation reasons. Unknown capacity stays unknown: missing
//! data never collapses into zero pressure (which would manufacture false
//! confidence that there is room to load a model).

use anemoi_core::RuntimeMemorySnapshot;

/// A normalized resource pressure reading.
///
/// `Known(fraction)` is the current utilization in the range `0.0..=1.0+`
/// (it can exceed `1.0` when projected usage overcommits capacity).
/// `Unknown` means the runtime did not report the capacity needed to compute
/// pressure, so no value is assumed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pressure {
    Known(f64),
    Unknown,
}

impl Pressure {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Pressure::Unknown)
    }

    pub fn value(&self) -> Option<f64> {
        match self {
            Pressure::Known(value) => Some(*value),
            Pressure::Unknown => None,
        }
    }
}

/// Inputs required to assess pressure for a single candidate placement.
#[derive(Debug, Clone)]
pub struct PressureInputs<'a> {
    pub memory: &'a RuntimeMemorySnapshot,
    pub vram_required_mb: Option<u64>,
    pub ram_required_mb: Option<u64>,
    /// Whether placing this candidate would load the model fresh (and thus
    /// add its memory requirement to the runtime). Reusing a resident model
    /// does not increase memory pressure.
    pub is_cold_load: bool,
    pub active_request_count: usize,
}

/// A single structured pressure reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PressureReason {
    pub code: String,
    pub detail: String,
    pub impact: i32,
}

/// The outcome of a pressure assessment: per-dimension readings, the total
/// scoring penalty, and the structured reasons that explain it.
#[derive(Debug, Clone)]
pub struct PressureAssessment {
    /// Current VRAM pressure from the snapshot (`used / total`).
    pub vram: Pressure,
    /// Current RAM pressure from the snapshot (`used / total`).
    pub ram: Pressure,
    pub active_request_count: usize,
    pub penalty: i32,
    pub reasons: Vec<PressureReason>,
}

/// Configurable thresholds and penalties for the pressure model.
#[derive(Debug, Clone)]
pub struct PressureModel {
    /// Projected utilization at or above this fraction is "high" pressure.
    pub high_threshold: f64,
    /// Projected utilization at or above this fraction (but below high) is
    /// "elevated" pressure.
    pub elevated_threshold: f64,
    /// Penalty applied to a cold-load candidate under high pressure.
    pub high_penalty: i32,
    /// Penalty applied to a cold-load candidate under elevated pressure.
    pub elevated_penalty: i32,
    /// Penalty applied per active request on the target runtime. The cumulative
    /// active-request penalty is linear in the request count (`each * count`) so
    /// a busier runtime always scores lower than a less busy one.
    pub active_request_penalty_each: i32,
}

impl Default for PressureModel {
    fn default() -> Self {
        Self {
            high_threshold: 0.85,
            elevated_threshold: 0.70,
            high_penalty: -25,
            elevated_penalty: -10,
            active_request_penalty_each: -5,
        }
    }
}

impl PressureModel {
    pub fn assess(&self, inputs: &PressureInputs<'_>) -> PressureAssessment {
        let mut reasons = Vec::new();
        let mut penalty = 0;

        let vram = current_pressure(inputs.memory.vram_used_mb, inputs.memory.vram_total_mb);
        let projected_vram = projected_pressure(
            inputs.memory.vram_used_mb,
            inputs.memory.vram_total_mb,
            inputs.is_cold_load,
            inputs.vram_required_mb,
        );
        penalty += self.dimension_reason(
            "vram",
            vram,
            projected_vram,
            inputs.is_cold_load,
            &mut reasons,
        );

        let ram = current_pressure(inputs.memory.ram_used_mb, inputs.memory.ram_total_mb);
        let projected_ram = projected_pressure(
            inputs.memory.ram_used_mb,
            inputs.memory.ram_total_mb,
            inputs.is_cold_load,
            inputs.ram_required_mb,
        );
        penalty +=
            self.dimension_reason("ram", ram, projected_ram, inputs.is_cold_load, &mut reasons);

        penalty += self.active_request_reason(inputs.active_request_count, &mut reasons);

        PressureAssessment {
            vram,
            ram,
            active_request_count: inputs.active_request_count,
            penalty,
            reasons,
        }
    }

    fn dimension_reason(
        &self,
        dimension: &str,
        current: Pressure,
        projected: Pressure,
        is_cold_load: bool,
        reasons: &mut Vec<PressureReason>,
    ) -> i32 {
        match projected {
            Pressure::Unknown => {
                reasons.push(PressureReason {
                    code: format!("pressure.{dimension}"),
                    detail: format!(
                        "{dimension} capacity is unknown; treating residency as unproven and assigning no pressure credit"
                    ),
                    impact: 0,
                });
                0
            }
            Pressure::Known(projected_value) => {
                let current_pct = current.value().map(percent).unwrap_or(0);
                let projected_pct = percent(projected_value);
                // Only a fresh load adds memory, so only a cold load is
                // penalized for projected overcommit. Reusing a resident model
                // occupies memory already counted in current usage.
                let impact = if !is_cold_load {
                    0
                } else if projected_value >= self.high_threshold {
                    self.high_penalty
                } else if projected_value >= self.elevated_threshold {
                    self.elevated_penalty
                } else {
                    0
                };
                reasons.push(PressureReason {
                    code: format!("pressure.{dimension}"),
                    detail: format!(
                        "{dimension} pressure is {current_pct}% now and {projected_pct}% projected after this placement"
                    ),
                    impact,
                });
                impact
            }
        }
    }

    fn active_request_reason(&self, count: usize, reasons: &mut Vec<PressureReason>) -> i32 {
        // Linear in the request count: a runtime serving more requests is always
        // penalized more. A previous `.max(floor)` capped this at the floor, so
        // counts beyond the floor/each boundary all scored identically.
        let impact = self.active_request_penalty_each * count as i32;
        reasons.push(PressureReason {
            code: "pressure.active_requests".to_string(),
            detail: format!("runtime is serving {count} active request(s)"),
            impact,
        });
        impact
    }
}

fn current_pressure(used: Option<u64>, total: Option<u64>) -> Pressure {
    match total {
        Some(total) if total > 0 => {
            let used = used.unwrap_or(0);
            Pressure::Known(used as f64 / total as f64)
        }
        _ => Pressure::Unknown,
    }
}

fn projected_pressure(
    used: Option<u64>,
    total: Option<u64>,
    is_cold_load: bool,
    required: Option<u64>,
) -> Pressure {
    match total {
        Some(total) if total > 0 => {
            let used = used.unwrap_or(0);
            let extra = if is_cold_load {
                required.unwrap_or(0)
            } else {
                0
            };
            Pressure::Known((used + extra) as f64 / total as f64)
        }
        _ => Pressure::Unknown,
    }
}

fn percent(fraction: f64) -> u64 {
    (fraction * 100.0).round() as u64
}
