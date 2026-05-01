use std::f32::consts::TAU;

use crate::dsp::SmoothedParam;
use crate::frame::{StereoFrame, db_to_gain};
use crate::source::AudioSource;

pub struct TestToneSource {
    phase: f32,
    phase_increment: f32,
    enabled: bool,
    gain: SmoothedParam,
}

impl TestToneSource {
    pub fn new(freq_hz: f32, sample_rate: u32) -> Self {
        let phase_increment = TAU * freq_hz / sample_rate as f32;
        Self {
            phase: 0.0,
            phase_increment,
            enabled: true,
            gain: SmoothedParam::new(1.0),
        }
    }
}

impl AudioSource for TestToneSource {
    fn render(&mut self, output: &mut [StereoFrame]) {
        if !self.enabled {
            for frame in output.iter_mut() {
                *frame = StereoFrame::SILENCE;
            }
            return;
        }
        for frame in output.iter_mut() {
            let sample = self.phase.sin() * self.gain.next_value();
            *frame = StereoFrame::new(sample, sample);
            self.phase += self.phase_increment;
            if self.phase >= TAU {
                self.phase -= TAU;
            }
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
        let target = db_to_gain(gain_db);
        self.gain.set_target(target, ramp_frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;
    const FREQ_HZ: f32 = 440.0;
    const FRAME_COUNT: usize = 256;

    fn render_buffer(source: &mut TestToneSource, frames: usize) -> Vec<StereoFrame> {
        let mut buf = vec![StereoFrame::SILENCE; frames];
        source.render(&mut buf);
        buf
    }

    fn peak(buf: &[StereoFrame]) -> f32 {
        buf.iter()
            .fold(0.0_f32, |acc, f| acc.max(f.left.abs()).max(f.right.abs()))
    }

    #[test]
    fn test_new_creates_source() {
        let _src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
    }

    #[test]
    fn test_render_produces_nonzero_peak() {
        let mut src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
        let buf = render_buffer(&mut src, 128);
        let p = peak(&buf);
        assert!(p > 0.5, "expected peak > 0.5, got {}", p);
    }

    #[test]
    fn test_render_in_unit_range() {
        let mut src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
        let buf = render_buffer(&mut src, FRAME_COUNT);
        for (i, f) in buf.iter().enumerate() {
            assert!(
                f.left >= -1.0 && f.left <= 1.0,
                "left out of range at {}: {}",
                i,
                f.left
            );
            assert!(
                f.right >= -1.0 && f.right <= 1.0,
                "right out of range at {}: {}",
                i,
                f.right
            );
        }
    }

    #[test]
    fn test_left_equals_right() {
        let mut src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
        let buf = render_buffer(&mut src, FRAME_COUNT);
        for (i, f) in buf.iter().enumerate() {
            assert!(
                (f.left - f.right).abs() < 1e-6,
                "L != R at {}: L={}, R={}",
                i,
                f.left,
                f.right
            );
        }
    }

    #[test]
    fn test_zero_crossing_distance_matches_half_period() {
        // Period at 48kHz / 440Hz ≈ 109.09 samples → half ≈ 54.5
        let mut src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
        let buf = render_buffer(&mut src, FRAME_COUNT);

        let mut crossings = Vec::new();
        for i in 1..buf.len() {
            let prev = buf[i - 1].left;
            let curr = buf[i].left;
            if prev.signum() != curr.signum() && curr != 0.0 && prev != 0.0 {
                crossings.push(i);
            }
        }

        assert!(
            crossings.len() >= 2,
            "expected at least 2 zero crossings, got {}",
            crossings.len()
        );

        let distance = (crossings[1] - crossings[0]) as f32;
        let expected_half_period = SAMPLE_RATE as f32 / FREQ_HZ / 2.0;
        let diff = (distance - expected_half_period).abs();
        assert!(
            diff <= 2.0,
            "zero-cross distance {} too far from expected {} (diff {})",
            distance,
            expected_half_period,
            diff
        );
    }

    #[test]
    fn test_disabled_outputs_silence() {
        let mut src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
        src.set_enabled(false);
        let buf = render_buffer(&mut src, FRAME_COUNT);
        for (i, f) in buf.iter().enumerate() {
            assert_eq!(f.left, 0.0, "left non-zero at {}: {}", i, f.left);
            assert_eq!(f.right, 0.0, "right non-zero at {}: {}", i, f.right);
        }
    }

    #[test]
    fn test_set_gain_db_minus_six_immediate() {
        let mut src = TestToneSource::new(FREQ_HZ, SAMPLE_RATE);
        src.set_gain_db(-6.0, 0);
        let buf = render_buffer(&mut src, FRAME_COUNT);
        let p = peak(&buf);
        assert!(
            (p - 0.5).abs() < 0.05,
            "expected peak ≈ 0.5 (±0.05), got {}",
            p
        );
    }
}
