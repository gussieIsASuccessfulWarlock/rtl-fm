//! Single-pole IIR for 50/75 us FM deemphasis.

/// Discrete-time deemphasis filter:
///   y[n] = a * x[n] + (1-a) * y[n-1]
/// with a = 1 - exp(-1 / (fs * tau)).
pub struct Deemphasis {
    a: f32,
    one_minus_a: f32,
    y: f32,
}

impl Deemphasis {
    pub fn new(tau_seconds: f32, fs: f32) -> Self {
        let a = 1.0 - (-1.0 / (fs * tau_seconds)).exp();
        Self {
            a,
            one_minus_a: 1.0 - a,
            y: 0.0,
        }
    }

    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        self.y = self.a * x + self.one_minus_a * self.y;
        self.y
    }

    pub fn process_inplace(&mut self, buf: &mut [f32]) {
        for v in buf.iter_mut() {
            *v = self.process_sample(*v);
        }
    }
}

/// EU (50 µs) / NA (75 µs) deemphasis time constant.
pub const TAU_50US: f32 = 50e-6;
pub const TAU_75US: f32 = 75e-6;
