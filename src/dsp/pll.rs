//! 19 kHz pilot-tone PLL for FM broadcast stereo + RDS.
//!
//! Locks a 2nd-order PLL to the 19 kHz pilot in the demodulated MPX
//! signal. After lock the NCO can emit any harmonic in phase with the
//! broadcast — we need cos(2·φ) for the L−R DSB demodulator and
//! cos(3·φ) for the 57 kHz RDS BPSK demodulator.
//!
//! Convention: the PD is `pilot · cos(φ_nco)`. When the loop is locked
//! its DC is zero only when φ_nco tracks the pilot's argument so that
//! `sin(φ_nco)` aligns with the pilot itself — at that point
//! `cos(k·φ_nco)` is the natural in-phase reference for the k-th
//! harmonic subcarrier.

#[derive(Debug, Clone, Copy)]
pub struct PllRefs {
    pub sin_phi: f32,
    pub cos_phi: f32,
    /// In-phase reference for the L−R DSB subcarrier (38 kHz).
    /// The FM-stereo standard puts the L−R signal at −cos(2ωt) when
    /// the pilot is sin(ωt); we use cos(2·φ) and accept the resulting
    /// sign flip (which just swaps L and R at the output, audible only
    /// in the channel labels, not in the spectrum).
    pub cos_2phi: f32,
    /// In-phase reference for the RDS BPSK subcarrier (57 kHz).
    /// The RDS subcarrier is sin(3ωt) when the pilot is sin(ωt), so
    /// the coherent demodulator multiplies the MPX by sin(3·φ).
    pub sin_3phi: f32,
}

pub struct PilotPll {
    fs: f32,
    omega_free: f32,
    omega: f32,
    phase: f32,
    integ: f32,
    kp: f32,
    ki: f32,
    /// Low-passed in-phase product magnitude as a coarse lock indicator.
    lock_metric: f32,
}

impl PilotPll {
    pub fn new(fs: f32) -> Self {
        let omega_free = 2.0 * std::f32::consts::PI * 19_000.0 / fs;
        let zeta = 0.707f32;
        let bw = 75.0f32;
        let theta = bw * 2.0 * std::f32::consts::PI / fs;
        let denom = 1.0 + 2.0 * zeta * theta + theta * theta;
        let kp = 4.0 * zeta * theta / denom;
        let ki = 4.0 * theta * theta / denom;
        Self {
            fs,
            omega_free,
            omega: omega_free,
            phase: 0.0,
            integ: 0.0,
            kp,
            ki,
            lock_metric: 0.0,
        }
    }

    /// Advance one sample and return the references the downstream
    /// stereo and RDS decoders need.
    #[inline]
    pub fn step(&mut self, pilot_sample: f32) -> PllRefs {
        let cos_phi = self.phase.cos();
        let sin_phi = self.phase.sin();

        let pd = pilot_sample * cos_phi;
        self.integ += self.ki * pd;
        let control = self.kp * pd + self.integ;
        self.omega = self.omega_free + control;
        self.phase += self.omega;
        if self.phase > std::f32::consts::PI {
            self.phase -= 2.0 * std::f32::consts::PI;
        } else if self.phase < -std::f32::consts::PI {
            self.phase += 2.0 * std::f32::consts::PI;
        }

        let inphase = pilot_sample * sin_phi;
        self.lock_metric = 0.999 * self.lock_metric + 0.001 * inphase.abs();

        // Double-angle identities — no extra trig calls.
        let cos_2phi = cos_phi * cos_phi - sin_phi * sin_phi;
        // sin(3φ) = 3·sin(φ) − 4·sin³(φ).
        let sin_3phi = sin_phi * (3.0 - 4.0 * sin_phi * sin_phi);

        PllRefs {
            sin_phi,
            cos_phi,
            cos_2phi,
            sin_3phi,
        }
    }

    pub fn lock_metric(&self) -> f32 {
        self.lock_metric
    }

    pub fn fs(&self) -> f32 {
        self.fs
    }
}
