use crate::dsp::{
    apply_gain_block, apply_pan_block, nan_guard_and_clamp, OnePoleHighpass,
    OnePoleLowpass, SafetyLimiter, SmoothedParam,
};
use crate::frame::StereoFrame;
use crate::meter::MeterSnapshot;
use crate::mixer::Mixer;
use crate::source::AudioSource;

#[derive(Debug, Clone, Copy)]
pub struct AudioRuntimeConfig {
    pub sample_rate: u32,
}

pub struct AudioRuntime {
    sample_rate: u32,
    sources: Vec<Box<dyn AudioSource>>,
    mixer: Mixer,
    master_gain_db: SmoothedParam,
    master_pan: SmoothedParam,
    highpass: OnePoleHighpass,
    lowpass: OnePoleLowpass,
    limiter: SafetyLimiter,
    last_meter: MeterSnapshot,
}

impl AudioRuntime {
    pub fn new(config: AudioRuntimeConfig) -> Self {
        Self {
            sample_rate: config.sample_rate,
            sources: Vec::new(),
            mixer: Mixer::new(),
            master_gain_db: SmoothedParam::new(0.0),
            master_pan: SmoothedParam::new(0.0),
            highpass: OnePoleHighpass::new(20.0, config.sample_rate),
            lowpass: OnePoleLowpass::new(20_000.0, config.sample_rate),
            limiter: SafetyLimiter::new(1.0),
            last_meter: MeterSnapshot::default(),
        }
    }

    pub fn add_source(&mut self, source: Box<dyn AudioSource>) {
        self.sources.push(source);
    }

    pub fn render_block(&mut self, output: &mut [StereoFrame]) {
        if output.is_empty() {
            return;
        }

        let mut refs: Vec<&mut dyn AudioSource> = self
            .sources
            .iter_mut()
            .map(|source| source.as_mut() as &mut dyn AudioSource)
            .collect();

        self.mixer.render(&mut refs, output);
        apply_gain_block(output, &mut self.master_gain_db);
        apply_pan_block(output, &mut self.master_pan);
        self.highpass.process(output);
        self.lowpass.process(output);
        self.limiter.process(output);
        nan_guard_and_clamp(output);
        self.last_meter = MeterSnapshot::compute(output);
    }

    pub fn meters(&self) -> &MeterSnapshot {
        &self.last_meter
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_master_gain_db(&mut self, db: f32, ramp_frames: u32) {
        self.master_gain_db.set_target(db, ramp_frames);
    }

    pub fn set_master_pan(&mut self, pan: f32, ramp_frames: u32) {
        self.master_pan.set_target(pan, ramp_frames);
    }

    pub fn set_lowpass_hz(&mut self, hz: f32) {
        self.lowpass.set_cutoff(hz);
    }

    pub fn set_highpass_hz(&mut self, hz: f32) {
        self.highpass.set_cutoff(hz);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{GlicolSource, TestToneSource};

    const SAMPLE_RATE: u32 = 48_000;
    const FRAME_COUNT: usize = 128;

    struct LoudSource {
        value: f32,
        sign: f32,
    }

    impl LoudSource {
        fn new(value: f32) -> Self {
            Self { value, sign: 1.0 }
        }
    }

    impl AudioSource for LoudSource {
        fn render(&mut self, output: &mut [StereoFrame]) {
            for frame in output.iter_mut() {
                let sample = self.value * self.sign;
                *frame = StereoFrame::new(sample, sample);
                self.sign = -self.sign;
            }
        }

        fn set_enabled(&mut self, enabled: bool) {
            let _ = enabled;
        }

        fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
            let _ = (gain_db, ramp_frames);
        }
    }

    fn peak(frames: &[StereoFrame]) -> f32 {
        frames.iter().fold(0.0_f32, |current, frame| {
            current.max(frame.left.abs()).max(frame.right.abs())
        })
    }

    fn assert_all_in_unit_range(frames: &[StereoFrame]) {
        for (index, frame) in frames.iter().enumerate() {
            assert!(
                frame.left >= -1.0 && frame.left <= 1.0,
                "left out of range at {index}: {}",
                frame.left
            );
            assert!(
                frame.right >= -1.0 && frame.right <= 1.0,
                "right out of range at {index}: {}",
                frame.right
            );
        }
    }

    #[test]
    fn runtime_without_sources_renders_silence() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        let mut output = vec![StereoFrame::new(0.5, -0.5); FRAME_COUNT];

        runtime.render_block(&mut output);

        for (index, frame) in output.iter().enumerate() {
            assert_eq!(frame.left, 0.0, "left non-zero at {index}: {}", frame.left);
            assert_eq!(frame.right, 0.0, "right non-zero at {index}: {}", frame.right);
        }
    }

    #[test]
    fn runtime_with_test_tone_source_renders_nonzero_signal() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.add_source(Box::new(TestToneSource::new(440.0, 48000)));
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p > 0.5, "expected peak > 0.5, got {p}");
    }

    #[test]
    fn runtime_master_gain_minus_sixty_is_nearly_silent() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.set_master_gain_db(-60.0, 0);
        runtime.add_source(Box::new(TestToneSource::new(440.0, 48000)));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p < 0.01, "expected near silence, got peak {p}");
    }

    #[test]
    fn runtime_master_gain_zero_keeps_normal_amplitude() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.set_master_gain_db(0.0, 0);
        runtime.add_source(Box::new(TestToneSource::new(440.0, 48000)));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p > 0.5, "expected normal amplitude, got peak {p}");
    }

    #[test]
    fn runtime_output_is_always_clamped_to_unit_range() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.add_source(Box::new(TestToneSource::new(440.0, 48000)));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        assert_all_in_unit_range(&output);
    }

    #[test]
    fn runtime_limiter_clamps_excessive_source_signal() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.add_source(Box::new(LoudSource::new(10_000.0)));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        assert_all_in_unit_range(&output);
        let p = peak(&output);
        assert!((p - 1.0).abs() < 0.001, "expected limiter clamp at 1.0, got {p}");
    }

    #[test]
    fn runtime_meters_return_latest_render_snapshot() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.add_source(Box::new(TestToneSource::new(440.0, 48000)));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let meters = runtime.meters();
        assert!(meters.peak_left > 0.0, "left peak should update");
        assert!(meters.peak_right > 0.0, "right peak should update");
    }

    #[test]
    fn runtime_empty_output_slice_does_not_panic() {
        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.add_source(Box::new(TestToneSource::new(440.0, 48000)));
        let mut output: Vec<StereoFrame> = Vec::new();

        runtime.render_block(&mut output);

        assert!(output.is_empty());
    }

    #[test]
    fn runtime_renders_glicol_source_nonzero() {
        let mut source = GlicolSource::new(SAMPLE_RATE);
        let load_result = source.load_code("out: sin 440 >> mul 0.3");
        assert!(load_result.is_ok(), "Glicol code should load: {load_result:?}");

        let mut runtime = AudioRuntime::new(AudioRuntimeConfig { sample_rate: SAMPLE_RATE });
        runtime.add_source(Box::new(source));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p > 0.05, "expected Glicol non-zero output, got peak {p}");
        assert_all_in_unit_range(&output);
    }
}
