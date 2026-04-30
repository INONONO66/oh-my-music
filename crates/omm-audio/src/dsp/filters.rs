use crate::frame::StereoFrame;
use std::f32::consts::TAU;

/// One-pole IIR low-pass filter applied per channel.
///
/// Difference equation: `y[n] = y[n-1] + α * (x[n] - y[n-1])`
/// where `α = 1 - exp(-2π * fc / sr)`.
pub struct OnePoleLowpass {
    cutoff_hz: f32,
    sample_rate: u32,
    alpha: f32,
    last_left: f32,
    last_right: f32,
}

impl OnePoleLowpass {
    pub fn new(cutoff_hz: f32, sample_rate: u32) -> Self {
        let alpha = compute_lowpass_alpha(cutoff_hz, sample_rate);
        Self {
            cutoff_hz,
            sample_rate,
            alpha,
            last_left: 0.0,
            last_right: 0.0,
        }
    }

    pub fn set_cutoff(&mut self, cutoff_hz: f32) {
        self.cutoff_hz = cutoff_hz;
        self.alpha = compute_lowpass_alpha(cutoff_hz, self.sample_rate);
    }

    pub fn cutoff_hz(&self) -> f32 {
        self.cutoff_hz
    }

    pub fn process(&mut self, frames: &mut [StereoFrame]) {
        if frames.is_empty() {
            return;
        }
        let alpha = self.alpha;
        for frame in frames.iter_mut() {
            self.last_left += alpha * (frame.left - self.last_left);
            self.last_right += alpha * (frame.right - self.last_right);
            frame.left = self.last_left;
            frame.right = self.last_right;
        }
    }
}

fn compute_lowpass_alpha(cutoff_hz: f32, sample_rate: u32) -> f32 {
    if sample_rate == 0 || cutoff_hz <= 0.0 {
        return 0.0;
    }
    let sr = sample_rate as f32;
    let alpha = 1.0 - (-TAU * cutoff_hz / sr).exp();
    alpha.clamp(0.0, 1.0)
}

/// One-pole IIR high-pass filter applied per channel.
///
/// Difference equation: `y[n] = α * (y[n-1] + x[n] - x[n-1])`
/// where `α = exp(-2π * fc / sr)`.
pub struct OnePoleHighpass {
    cutoff_hz: f32,
    sample_rate: u32,
    alpha: f32,
    last_left_in: f32,
    last_left_out: f32,
    last_right_in: f32,
    last_right_out: f32,
}

impl OnePoleHighpass {
    pub fn new(cutoff_hz: f32, sample_rate: u32) -> Self {
        let alpha = compute_highpass_alpha(cutoff_hz, sample_rate);
        Self {
            cutoff_hz,
            sample_rate,
            alpha,
            last_left_in: 0.0,
            last_left_out: 0.0,
            last_right_in: 0.0,
            last_right_out: 0.0,
        }
    }

    pub fn set_cutoff(&mut self, cutoff_hz: f32) {
        self.cutoff_hz = cutoff_hz;
        self.alpha = compute_highpass_alpha(cutoff_hz, self.sample_rate);
    }

    pub fn cutoff_hz(&self) -> f32 {
        self.cutoff_hz
    }

    pub fn process(&mut self, frames: &mut [StereoFrame]) {
        if frames.is_empty() {
            return;
        }
        let alpha = self.alpha;
        for frame in frames.iter_mut() {
            let new_left_out = alpha * (self.last_left_out + frame.left - self.last_left_in);
            let new_right_out = alpha * (self.last_right_out + frame.right - self.last_right_in);
            self.last_left_in = frame.left;
            self.last_right_in = frame.right;
            self.last_left_out = new_left_out;
            self.last_right_out = new_right_out;
            frame.left = new_left_out;
            frame.right = new_right_out;
        }
    }
}

fn compute_highpass_alpha(cutoff_hz: f32, sample_rate: u32) -> f32 {
    if sample_rate == 0 || cutoff_hz <= 0.0 {
        return 1.0;
    }
    let sr = sample_rate as f32;
    let alpha = (-TAU * cutoff_hz / sr).exp();
    alpha.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(freq_hz: f32, sample_rate: u32, n_samples: usize) -> Vec<StereoFrame> {
        let sr = sample_rate as f32;
        (0..n_samples)
            .map(|i| {
                let t = i as f32 / sr;
                let s = (TAU * freq_hz * t).sin();
                StereoFrame::new(s, s)
            })
            .collect()
    }

    fn peak_amplitude(frames: &[StereoFrame]) -> f32 {
        frames
            .iter()
            .fold(0.0_f32, |acc, f| acc.max(f.left.abs()).max(f.right.abs()))
    }

    #[test]
    fn lowpass_passes_50hz_with_1khz_cutoff() {
        let mut lp = OnePoleLowpass::new(1000.0, 48_000);
        let mut samples = sine_wave(50.0, 48_000, 1000);
        lp.process(&mut samples);
        let peak = peak_amplitude(&samples[800..]);
        assert!(
            peak > 0.85,
            "50Hz should pass with 1kHz LP cutoff, peak = {}",
            peak
        );
    }

    #[test]
    fn lowpass_attenuates_10khz_with_1khz_cutoff() {
        let mut lp = OnePoleLowpass::new(1000.0, 48_000);
        let mut samples = sine_wave(10_000.0, 48_000, 1000);
        lp.process(&mut samples);
        let peak = peak_amplitude(&samples[800..]);
        assert!(
            peak < 0.3,
            "10kHz should be attenuated with 1kHz LP cutoff, peak = {}",
            peak
        );
    }

    #[test]
    fn lowpass_near_nyquist_passes_lower_frequencies() {
        let mut lp = OnePoleLowpass::new(20_000.0, 48_000);
        let mut samples = sine_wave(1_000.0, 48_000, 1000);
        lp.process(&mut samples);
        let peak = peak_amplitude(&samples[800..]);
        assert!(
            peak > 0.85,
            "1kHz should pass with 20kHz LP cutoff, peak = {}",
            peak
        );
    }

    #[test]
    fn lowpass_set_cutoff_updates_alpha() {
        let mut lp = OnePoleLowpass::new(1000.0, 48_000);
        let alpha_before = lp.alpha;
        lp.set_cutoff(5000.0);
        assert!(lp.alpha > alpha_before, "raising cutoff should raise alpha");
        assert!((lp.cutoff_hz() - 5000.0).abs() < 0.001);
    }

    #[test]
    fn lowpass_empty_slice_does_not_panic() {
        let mut lp = OnePoleLowpass::new(1000.0, 48_000);
        let mut empty: Vec<StereoFrame> = Vec::new();
        lp.process(&mut empty);
    }

    #[test]
    fn highpass_passes_5khz_with_100hz_cutoff() {
        let mut hp = OnePoleHighpass::new(100.0, 48_000);
        let mut samples = sine_wave(5_000.0, 48_000, 1000);
        hp.process(&mut samples);
        let peak = peak_amplitude(&samples[800..]);
        assert!(
            peak > 0.7,
            "5kHz should pass with 100Hz HP cutoff, peak = {}",
            peak
        );
    }

    #[test]
    fn highpass_attenuates_50hz_with_1khz_cutoff() {
        let mut hp = OnePoleHighpass::new(1000.0, 48_000);
        let mut samples = sine_wave(50.0, 48_000, 1000);
        hp.process(&mut samples);
        let peak = peak_amplitude(&samples[800..]);
        assert!(
            peak < 0.3,
            "50Hz should be attenuated with 1kHz HP cutoff, peak = {}",
            peak
        );
    }

    #[test]
    fn highpass_set_cutoff_updates_alpha() {
        let mut hp = OnePoleHighpass::new(1000.0, 48_000);
        let alpha_before = hp.alpha;
        hp.set_cutoff(100.0);
        // Lower cutoff → alpha closer to 1.0 (more pass-through above cutoff)
        assert!(
            hp.alpha > alpha_before,
            "lowering cutoff should raise alpha"
        );
        assert!((hp.cutoff_hz() - 100.0).abs() < 0.001);
    }

    #[test]
    fn highpass_empty_slice_does_not_panic() {
        let mut hp = OnePoleHighpass::new(1000.0, 48_000);
        let mut empty: Vec<StereoFrame> = Vec::new();
        hp.process(&mut empty);
    }
}
