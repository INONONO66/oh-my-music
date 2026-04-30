use crate::constants::MAX_BLOCK_FRAMES;
use crate::dsp::SmoothedParam;
use crate::frame::{db_to_gain, StereoFrame};
use crate::source::AudioSource;

/// Sums multiple `AudioSource` outputs and applies a smoothed master gain.
///
/// The master gain is stored in dB and converted per frame so a ramp produces
/// a constant-power dB-domain transition. `MAX_BLOCK_FRAMES` of scratch
/// capacity is pre-allocated so steady-state `render()` does not allocate.
pub struct Mixer {
    master_gain_db: SmoothedParam,
    scratch: Vec<StereoFrame>,
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            master_gain_db: SmoothedParam::new(0.0),
            scratch: Vec::with_capacity(MAX_BLOCK_FRAMES),
        }
    }

    /// Set the master gain in decibels. `ramp_frames == 0` applies immediately.
    pub fn set_master_gain_db(&mut self, db: f32, ramp_frames: u32) {
        self.master_gain_db.set_target(db, ramp_frames);
    }

    pub fn render(
        &mut self,
        sources: &mut [&mut dyn AudioSource],
        output: &mut [StereoFrame],
    ) {
        if output.is_empty() {
            return;
        }

        let n = output.len();

        for frame in output.iter_mut() {
            *frame = StereoFrame::SILENCE;
        }

        self.scratch.resize(n, StereoFrame::SILENCE);

        for source in sources.iter_mut() {
            source.render(&mut self.scratch[..n]);
            for (out_frame, src_frame) in output.iter_mut().zip(self.scratch[..n].iter()) {
                out_frame.left += src_frame.left;
                out_frame.right += src_frame.right;
            }
        }

        for frame in output.iter_mut() {
            let db = self.master_gain_db.next_value();
            let gain = db_to_gain(db);
            frame.left *= gain;
            frame.right *= gain;
        }
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TestToneSource;

    const SAMPLE_RATE: u32 = 48_000;
    const FRAME_COUNT: usize = 128;

    fn peak(buf: &[StereoFrame]) -> f32 {
        buf.iter()
            .fold(0.0_f32, |acc, f| acc.max(f.left.abs()).max(f.right.abs()))
    }

    #[test]
    fn test_render_zero_sources_produces_silence() {
        let mut mixer = Mixer::new();
        let mut output = vec![StereoFrame::new(0.9, -0.9); FRAME_COUNT];
        let mut sources: [&mut dyn AudioSource; 0] = [];
        mixer.render(&mut sources, &mut output);
        for (i, f) in output.iter().enumerate() {
            assert_eq!(f.left, 0.0, "left non-zero at {}: {}", i, f.left);
            assert_eq!(f.right, 0.0, "right non-zero at {}: {}", i, f.right);
        }
    }

    #[test]
    fn test_render_single_source_produces_signal() {
        let mut mixer = Mixer::new();
        let mut tone = TestToneSource::new(440.0, SAMPLE_RATE);
        let mut sources: [&mut dyn AudioSource; 1] = [&mut tone];
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        mixer.render(&mut sources, &mut output);
        let p = peak(&output);
        assert!(p > 0.5, "expected peak > 0.5, got {}", p);
    }

    #[test]
    fn test_render_two_sources_sum() {
        let mut mixer = Mixer::new();
        let mut s1 = TestToneSource::new(440.0, SAMPLE_RATE);
        let mut s2 = TestToneSource::new(880.0, SAMPLE_RATE);
        let mut sources: [&mut dyn AudioSource; 2] = [&mut s1, &mut s2];
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        mixer.render(&mut sources, &mut output);
        let p = peak(&output);
        assert!(p > 0.5, "expected summed peak > 0.5, got {}", p);
    }

    #[test]
    fn test_master_gain_minus_six_db_immediate() {
        let mut mixer = Mixer::new();
        mixer.set_master_gain_db(-6.0, 0);
        let mut tone = TestToneSource::new(440.0, SAMPLE_RATE);
        let mut sources: [&mut dyn AudioSource; 1] = [&mut tone];
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        mixer.render(&mut sources, &mut output);
        let p = peak(&output);
        assert!(
            (p - 0.5012).abs() < 0.05,
            "expected peak ≈ 0.5012 (±0.05), got {}",
            p
        );
    }

    #[test]
    fn test_render_empty_output_does_not_panic() {
        let mut mixer = Mixer::new();
        let mut tone = TestToneSource::new(440.0, SAMPLE_RATE);
        let mut sources: [&mut dyn AudioSource; 1] = [&mut tone];
        let mut output: Vec<StereoFrame> = Vec::new();
        mixer.render(&mut sources, &mut output);
        assert!(output.is_empty());
    }

    #[test]
    fn test_default_constructs_mixer() {
        let _mixer = Mixer::default();
    }
}
