use std::sync::{Arc, Mutex};

/// Simple noise gate that learns background noise level then suppresses it.
///
/// Workflow:
/// 1. Call `start_calibration()` — user stays silent for ~3 seconds
/// 2. Audio samples are fed via `feed_calibration()`
/// 3. Call `finish_calibration()` — computes noise floor RMS
/// 4. After calibration, `process()` gates audio below the learned threshold
///
/// The gate uses a smooth attack/release to avoid harsh clicks:
/// - When signal > threshold: gate opens quickly (attack)
/// - When signal < threshold: gate closes slowly (release)

#[derive(Clone)]
pub struct NoiseGate {
    /// Whether the gate is enabled
    pub enabled: bool,
    /// Learned noise floor RMS (linear)
    noise_floor_rms: f32,
    /// Gate threshold = noise_floor_rms * headroom_multiplier
    threshold: f32,
    /// Current gate level (0.0 = closed, 1.0 = open)
    gate_level: f32,
    /// How much above noise floor to set the threshold (default 1.5x)
    pub headroom: f32,
}

/// Shared state for calibration (fed from audio thread)
pub struct CalibrationState {
    pub active: bool,
    pub samples: Vec<f32>,
}

impl NoiseGate {
    pub fn new() -> Self {
        Self {
            enabled: false,
            noise_floor_rms: 0.0,
            threshold: 0.0,
            gate_level: 1.0,
            headroom: 1.5,
        }
    }

    /// Finish calibration: compute noise floor from collected samples
    pub fn finish_calibration(&mut self, samples: &[f32]) -> Result<f32, &'static str> {
        if samples.len() < 4800 {
            return Err("Not enough audio captured");
        }

        // Compute RMS of the noise samples
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        let rms = (sum_sq / samples.len() as f32).sqrt();

        if rms < 0.00001 {
            return Err("No audio detected — check your microphone");
        }

        self.noise_floor_rms = rms;
        self.threshold = rms * self.headroom;
        self.enabled = true;

        let db = 20.0 * rms.log10();
        Ok(db)
    }

    /// Process a single mono sample through the gate.
    /// Returns the gated sample.
    #[inline]
    pub fn process(&mut self, sample: f32) -> f32 {
        if !self.enabled || self.threshold == 0.0 {
            return sample;
        }

        let abs = sample.abs();

        // Smoothed gate: fast attack (open quickly), slow release (close gently)
        if abs > self.threshold {
            // Attack: open gate quickly
            self.gate_level = (self.gate_level + 0.1).min(1.0);
        } else {
            // Release: close gate slowly to avoid clicks
            self.gate_level = (self.gate_level - 0.0005).max(0.0);
        }

        sample * self.gate_level
    }

    /// Get the current threshold in dBFS
    pub fn threshold_db(&self) -> f32 {
        if self.threshold > 0.0 {
            20.0 * self.threshold.log10()
        } else {
            -80.0
        }
    }

    /// Get the noise floor in dBFS
    pub fn noise_floor_db(&self) -> f32 {
        if self.noise_floor_rms > 0.0 {
            20.0 * self.noise_floor_rms.log10()
        } else {
            -80.0
        }
    }

    /// Whether calibration has been done
    pub fn is_calibrated(&self) -> bool {
        self.noise_floor_rms > 0.0
    }

    /// Reset the gate
    pub fn reset(&mut self) {
        self.enabled = false;
        self.noise_floor_rms = 0.0;
        self.threshold = 0.0;
        self.gate_level = 1.0;
    }
}

/// Create shared calibration state
pub fn new_calibration_state() -> Arc<Mutex<CalibrationState>> {
    Arc::new(Mutex::new(CalibrationState {
        active: false,
        samples: Vec::new(),
    }))
}
