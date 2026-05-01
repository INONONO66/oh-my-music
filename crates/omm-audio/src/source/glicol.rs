use glicol::Engine;

use crate::dsp::SmoothedParam;
use crate::frame::{StereoFrame, db_to_gain};
use crate::source::AudioSource;

const GLICOL_BLOCK_SIZE: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum GlicolError {
    #[error("Failed to load Glicol code: {0}")]
    LoadFailed(String),
}

pub struct GlicolSource {
    engine: Engine<128>,
    residual: Vec<StereoFrame>,
    residual_offset: usize,
    residual_len: usize,
    enabled: bool,
    gain: SmoothedParam,
    code_loaded: bool,
}

impl GlicolSource {
    pub fn new(sample_rate: u32) -> Self {
        let mut engine = Engine::<128>::new();
        engine.set_sr(sample_rate as usize);

        let mut residual = Vec::with_capacity(GLICOL_BLOCK_SIZE);
        residual.resize(GLICOL_BLOCK_SIZE, StereoFrame::SILENCE);

        Self {
            engine,
            residual,
            residual_offset: 0,
            residual_len: 0,
            enabled: true,
            gain: SmoothedParam::new(0.0),
            code_loaded: false,
        }
    }

    pub fn load_code(&mut self, code: &str) -> Result<(), GlicolError> {
        self.engine
            .update_with_code(code)
            .map_err(|error| GlicolError::LoadFailed(error.to_string()))?;
        self.code_loaded = true;
        self.clear_residual();
        Ok(())
    }

    fn refill_residual(&mut self) {
        // Glicol's `next_block` consumes a `Vec<&[f32]>` of optional input
        // channels. This source is synth-only, so we hand it an empty vec.
        // `Vec::new()` is guaranteed by std to not allocate (capacity 0), and
        // Glicol's internal `is_empty()` guard skips the input copy entirely,
        // so this stays alloc-free in the audio render path.
        let block = self.engine.next_block(Vec::new());

        for frame_index in 0..GLICOL_BLOCK_SIZE {
            let left = block.first().map_or(0.0, |channel| channel[frame_index]);
            let right = block.get(1).map_or(left, |channel| channel[frame_index]);
            self.residual[frame_index] = StereoFrame::new(left, right);
        }

        self.residual_offset = 0;
        self.residual_len = GLICOL_BLOCK_SIZE;
    }

    fn clear_residual(&mut self) {
        self.residual_offset = 0;
        self.residual_len = 0;
    }

    fn write_silence(output: &mut [StereoFrame]) {
        for frame in output {
            *frame = StereoFrame::SILENCE;
        }
    }
}

impl AudioSource for GlicolSource {
    fn render(&mut self, output: &mut [StereoFrame]) {
        if output.is_empty() {
            return;
        }

        if !self.enabled || !self.code_loaded {
            Self::write_silence(output);
            return;
        }

        let mut output_offset = 0;
        while output_offset < output.len() {
            if self.residual_offset >= self.residual_len {
                self.refill_residual();
            }

            let available = self.residual_len.saturating_sub(self.residual_offset);
            let frames_to_copy = available.min(output.len() - output_offset);

            for frame_index in 0..frames_to_copy {
                let source = self.residual[self.residual_offset + frame_index];
                let gain = db_to_gain(self.gain.next_value());
                output[output_offset + frame_index] =
                    StereoFrame::new(source.left * gain, source.right * gain);
            }

            self.residual_offset += frames_to_copy;
            output_offset += frames_to_copy;

            if self.residual_offset >= self.residual_len {
                self.clear_residual();
            }
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
        self.gain.set_target(gain_db, ramp_frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 0.001;

    fn render_test_code(source: &mut GlicolSource) {
        let result = source.load_code("out: sin 440 >> mul 0.3");
        assert!(result.is_ok(), "valid Glicol code should load: {result:?}");
    }

    fn peak(frames: &[StereoFrame]) -> f32 {
        frames.iter().fold(0.0_f32, |current, frame| {
            current.max(frame.left.abs()).max(frame.right.abs())
        })
    }

    fn assert_silence(frames: &[StereoFrame]) {
        for frame in frames {
            assert!(
                frame.left.abs() <= EPSILON,
                "left should be silent: {}",
                frame.left
            );
            assert!(
                frame.right.abs() <= EPSILON,
                "right should be silent: {}",
                frame.right
            );
        }
    }

    fn assert_finite(frames: &[StereoFrame]) {
        for frame in frames {
            assert!(
                frame.left.is_finite(),
                "left should be finite: {}",
                frame.left
            );
            assert!(
                frame.right.is_finite(),
                "right should be finite: {}",
                frame.right
            );
        }
    }

    #[test]
    fn glicol_new_48000_does_not_panic() {
        let _source = GlicolSource::new(48_000);
    }

    #[test]
    fn glicol_loads_valid_code() {
        let mut source = GlicolSource::new(48_000);
        let result = source.load_code("out: sin 440 >> mul 0.3");
        assert!(result.is_ok(), "valid Glicol code should load: {result:?}");
    }

    #[test]
    fn glicol_invalid_code_returns_error_and_keeps_previous_rendering() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);

        let result = source.load_code("invalid garbage code !@#$%");
        assert!(result.is_err(), "invalid Glicol code should fail");

        let mut output = vec![StereoFrame::SILENCE; 256];
        source.render(&mut output);

        assert!(
            peak(&output) > 0.05,
            "previous valid code should keep rendering"
        );
        assert_finite(&output);
    }

    #[test]
    fn glicol_render_after_code_load_is_nonzero() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);

        let mut output = vec![StereoFrame::SILENCE; 256];
        source.render(&mut output);

        assert!(
            peak(&output) > 0.05,
            "loaded sine should produce audible output"
        );
        assert_finite(&output);
    }

    #[test]
    fn glicol_render_before_code_load_is_silence() {
        let mut source = GlicolSource::new(48_000);
        let mut output = vec![StereoFrame::new(1.0, -1.0); 64];

        source.render(&mut output);

        assert_silence(&output);
    }

    #[test]
    fn glicol_block_adapter_reuses_residual_for_two_short_renders() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);

        let mut first = vec![StereoFrame::SILENCE; 100];
        source.render(&mut first);
        assert_eq!(first.len(), 100);
        assert_eq!(source.residual_len, GLICOL_BLOCK_SIZE);
        assert_eq!(source.residual_offset, 100);
        assert!(peak(&first) > 0.05);

        let mut second = vec![StereoFrame::SILENCE; 100];
        source.render(&mut second);
        assert_eq!(second.len(), 100);
        assert_eq!(source.residual_len, GLICOL_BLOCK_SIZE);
        assert_eq!(source.residual_offset, 72);
        assert!(peak(&second) > 0.05);
        assert_finite(&first);
        assert_finite(&second);
    }

    #[test]
    fn glicol_block_adapter_renders_four_blocks_for_512_frames() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);

        let mut output = vec![StereoFrame::SILENCE; 512];
        source.render(&mut output);

        assert_eq!(output.len(), 512);
        assert_eq!(source.residual_len, 0);
        assert_eq!(source.residual_offset, 0);
        assert!(peak(&output) > 0.05);
        assert_finite(&output);
    }

    #[test]
    fn glicol_disabled_renders_silence() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);
        source.set_enabled(false);

        let mut output = vec![StereoFrame::new(1.0, -1.0); 128];
        source.render(&mut output);

        assert_silence(&output);
    }

    #[test]
    fn glicol_immediate_minus_sixty_db_gain_is_nearly_silent() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);
        source.set_gain_db(-60.0, 0);

        let mut output = vec![StereoFrame::SILENCE; 256];
        source.render(&mut output);

        assert!(
            peak(&output) <= EPSILON,
            "-60 dB output should be nearly silent"
        );
        assert_finite(&output);
    }

    #[test]
    fn glicol_rendered_output_has_no_nan_or_inf() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);

        let mut output = vec![StereoFrame::SILENCE; 384];
        source.render(&mut output);

        assert_finite(&output);
    }

    #[test]
    fn glicol_repeated_render_does_not_grow_residual_capacity() {
        let mut source = GlicolSource::new(48_000);
        render_test_code(&mut source);

        let initial_capacity = source.residual.capacity();
        let mut output = vec![StereoFrame::SILENCE; 256];

        for _ in 0..1024 {
            source.render(&mut output);
        }

        assert_eq!(
            source.residual.capacity(),
            initial_capacity,
            "residual buffer should not reallocate during steady-state rendering"
        );
        assert_finite(&output);
    }
}
